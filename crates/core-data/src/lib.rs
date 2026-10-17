//! P2 data plane: content-defined-chunked objects (Blake3) + relationship graph + passage vectors in one SQLite file.
//!
//! One transaction covers objects, edges and vectors. A snapshot is a file clone (copy-on-write on APFS/btrfs).
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, params_from_iter, types::Value, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const SCHEMA: &str = "
create table if not exists meta(k text primary key, v text);
create table if not exists blobs(hash text primary key, data blob not null);
create table if not exists objects(id text primary key, size integer, created real);
create table if not exists object_chunks(obj text, ord integer, blob text, primary key(obj, ord));
create table if not exists names(path text primary key, obj text, mtime real);
create table if not exists edges(src text, type text, dst text, conf real default 1.0,
  source text default 'user', ts real, primary key(src, type, dst));
create index if not exists edges_dst on edges(dst, type);
create table if not exists tombstones(src text, type text, dst text, ts real, primary key(src, type, dst));
create table if not exists passages(id integer primary key, obj text, ord integer, text text, emb blob);
create index if not exists passages_obj on passages(obj);
create virtual table if not exists fts using fts5(text, content='passages', content_rowid='id');
create table if not exists workspaces(name text primary key, query text);
";

/// Added to the cosine score of objects carrying a boost tag. Small on purpose: a tag nudges, never excludes.
pub const TAG_BOOST: f32 = 0.1;

pub type Embed<'a> = &'a dyn Fn(&[String]) -> Result<Vec<Vec<f32>>>;

#[derive(Default, Clone)]
pub struct Filter {
    /// Hard filter: object must carry every tag.
    pub tags: Vec<String>,
    /// Soft signal: objects carrying any of these tags rank higher (see `TAG_BOOST`).
    pub boost: Vec<String>,
    pub project: Option<String>,
    pub span: Option<(f64, f64)>,
}

pub struct Hit {
    pub obj: String,
    pub text: String,
    pub score: f32,
}

pub fn norm_tag(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join("-").to_lowercase()
}

pub fn passages(text: &str, size: usize) -> Vec<String> {
    let (mut out, mut cur) = (vec![], String::new());
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if !cur.is_empty() && cur.len() + para.len() > size {
            out.push(std::mem::take(&mut cur));
        }
        if cur.is_empty() {
            cur = para.to_string();
        } else {
            cur = format!("{cur}\n\n{para}");
        }
        while cur.len() > size {
            let mut cut = size;
            while !cur.is_char_boundary(cut) {
                cut -= 1;
            }
            out.push(cur[..cut].to_string());
            cur = cur[cut..].to_string();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn from_bytes(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na.sqrt() * nb.sqrt()) }
}

fn where_clause(f: &Filter) -> (String, Vec<Value>) {
    let (mut sql, mut p) = (String::new(), vec![]);
    for t in &f.tags {
        sql += " and exists(select 1 from edges e where e.src=p.obj and e.type='tagged' and e.dst=?)";
        p.push(Value::Text(format!("tag:{t}")));
    }
    if let Some(proj) = &f.project {
        sql += " and exists(select 1 from edges e where e.src=p.obj and e.type='in_project' and e.dst=?)";
        p.push(Value::Text(format!("project:{}", norm_tag(proj))));
    }
    if let Some((a, b)) = f.span {
        sql += " and o.created>=? and o.created<?";
        p.push(Value::Real(a));
        p.push(Value::Real(b));
    }
    (sql, p)
}

pub struct Store {
    pub db: Connection,
    path: PathBuf,
    dim: usize,
}

impl Store {
    pub fn open(path: &Path, dim: usize) -> Result<Store> {
        let db = Connection::open(path)?;
        db.execute_batch(SCHEMA)?;
        let have: Option<String> = db.query_row("select v from meta where k='dim'", [], |r| r.get(0)).ok();
        match have {
            None => {
                db.execute("insert into meta values('dim', ?)", [dim.to_string()])?;
            }
            Some(d) if d != dim.to_string() => bail!("store was built with {d}-dim embeddings, embedder gives {dim}"),
            _ => {}
        }
        let s = Store { db, path: path.to_path_buf(), dim };
        s.backfill_names()?;
        Ok(s)
    }

    /// File names live in the local `names` table, which does not replicate; a `named` edge does.
    /// Adds the edge for objects imported before it existed.
    fn backfill_names(&self) -> Result<()> {
        let mut st = self.db.prepare(
            "select path, obj from names n where not exists(select 1 from edges where src=n.obj and type='named')")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        for (path, obj) in rows {
            self.name(&obj, Path::new(&path))?;
        }
        Ok(())
    }

    fn name(&self, oid: &str, path: &Path) -> Result<()> {
        match path.file_name() {
            Some(f) => self.link(oid, "named", &format!("name:{}", f.to_string_lossy()), 1.0, "user"),
            None => Ok(()),
        }
    }

    // --- objects -----------------------------------------------------------
    pub fn put(&self, data: &[u8], created: f64) -> Result<String> {
        let id = blake3::hash(data).to_hex().to_string();
        if self.db.query_row("select 1 from objects where id=?", [&id], |_| Ok(())).is_ok() {
            return Ok(id);
        }
        for (i, c) in fastcdc::v2020::FastCDC::new(data, 2048, 8192, 32768).enumerate() {
            let bytes = &data[c.offset..c.offset + c.length];
            let h = blake3::hash(bytes).to_hex().to_string();
            self.db.execute("insert or ignore into blobs values(?,?)", params![h, bytes])?;
            self.db.execute("insert into object_chunks values(?,?,?)", params![id, i as i64, h])?;
        }
        self.db.execute("insert into objects values(?,?,?)", params![id, data.len() as i64, created])?;
        Ok(id)
    }

    pub fn get(&self, id: &str) -> Result<Vec<u8>> {
        let mut st = self.db.prepare(
            "select b.data from object_chunks c join blobs b on b.hash=c.blob where c.obj=? order by c.ord")?;
        let mut out = vec![];
        for r in st.query_map([id], |r| r.get::<_, Vec<u8>>(0))? {
            out.extend(r?);
        }
        Ok(out)
    }

    pub fn index(&self, oid: &str, text: &str, embed: Embed) -> Result<bool> {
        if self.db.query_row("select 1 from passages where obj=?", [oid], |_| Ok(())).is_ok() {
            return Ok(false);
        }
        let ps = passages(text, 800);
        if ps.is_empty() {
            return Ok(false);
        }
        for (i, (p, v)) in ps.iter().zip(embed(&ps)?).enumerate() {
            self.db.execute("insert into passages(obj, ord, text, emb) values(?,?,?,?)",
                            params![oid, i as i64, p, to_bytes(&v)])?;
            self.db.execute("insert into fts(rowid, text) values(?,?)", params![self.db.last_insert_rowid(), p])?;
        }
        Ok(true)
    }

    /// Atomic: the whole import is one transaction.
    pub fn import_dir(&self, root: &Path, embed: Embed, project: Option<&str>) -> Result<usize> {
        self.db.execute_batch("BEGIN")?;
        match self.import_inner(root, embed, project) {
            Ok(n) => {
                self.db.execute_batch("COMMIT")?;
                Ok(n)
            }
            Err(e) => {
                let _ = self.db.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn import_inner(&self, root: &Path, embed: Embed, project: Option<&str>) -> Result<usize> {
        let mut n = 0;
        for e in platform::walk(root) {
            let path = platform::abspath(&e.path);
            let key = path.to_string_lossy().to_string();
            let seen: Option<f64> = self.db.query_row("select mtime from names where path=?", [&key], |r| r.get(0)).ok();
            if seen == Some(e.mtime) {
                continue;
            }
            let Some((raw, text)) = platform::read_text(&path) else { continue };
            let oid = self.put(&raw, e.mtime)?;
            self.db.execute("insert or replace into names values(?,?,?)", params![key, oid, e.mtime])?;
            self.name(&oid, &path)?;
            self.index(&oid, &text, embed)?;
            let parent = path.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string());
            if let Some(parent) = parent {
                self.tag(&oid, &parent, 0.5, "heuristic")?;
            }
            if let Some(p) = project {
                self.link(&oid, "in_project", &format!("project:{}", norm_tag(p)), 1.0, "user")?;
            }
            n += 1;
        }
        Ok(n)
    }

    // --- graph -------------------------------------------------------------
    pub fn link(&self, src: &str, ty: &str, dst: &str, conf: f64, source: &str) -> Result<()> {
        self.db.execute("insert or replace into edges values(?,?,?,?,?,?)",
                        params![src, ty, dst, conf, source, platform::now()])?;
        self.db.execute("delete from tombstones where src=? and type=? and dst=?", [src, ty, dst])?;
        Ok(())
    }

    /// Deletes an edge and leaves a tombstone so the deletion replicates. Returns false if no such edge.
    pub fn unlink(&self, src: &str, ty: &str, dst: &str) -> Result<bool> {
        if self.db.execute("delete from edges where src=? and type=? and dst=?", [src, ty, dst])? == 0 {
            return Ok(false);
        }
        self.db.execute("insert or replace into tombstones values(?,?,?,?)", params![src, ty, dst, platform::now()])?;
        Ok(true)
    }

    /// Makes `dst` the only target of (src, ty). Replaced edges leave tombstones, so the change replicates.
    pub fn relink(&self, src: &str, ty: &str, dst: &str) -> Result<()> {
        let mut st = self.db.prepare("select dst from edges where src=? and type=? and dst<>?")?;
        let old = st.query_map([src, ty, dst], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        for d in old {
            self.unlink(src, ty, &d)?;
        }
        self.link(src, ty, dst, 1.0, "user")
    }

    /// Newest target of (src, ty). Two devices may each have set one before syncing; the later wins.
    pub fn target(&self, src: &str, ty: &str) -> Option<String> {
        self.db.query_row("select dst from edges where src=? and type=? order by ts desc limit 1", [src, ty], |r| r.get(0)).ok()
    }

    pub fn untag(&self, oid: &str, tag: &str) -> Result<bool> {
        self.unlink(oid, "tagged", &format!("tag:{}", norm_tag(tag)))
    }

    pub fn tag(&self, oid: &str, tag: &str, conf: f64, source: &str) -> Result<()> {
        self.link(oid, "tagged", &format!("tag:{}", norm_tag(tag)), conf, source)
    }

    pub fn known_tags(&self) -> Result<Vec<String>> {
        let mut st = self.db.prepare("select distinct substr(dst,5) from edges where type='tagged'")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
        Ok(v)
    }

    pub fn top_tags(&self, n: usize) -> Result<Vec<(String, i64)>> {
        let mut st = self.db.prepare(
            "select substr(dst,5), count(*) c from edges where type='tagged' group by dst order by c desc limit ?")?;
        let v = st.query_map([n as i64], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn resolve(&self, x: &str) -> Result<String> {
        let abs = platform::abspath(Path::new(x)).to_string_lossy().to_string();
        if let Ok(o) = self.db.query_row("select obj from names where path=?", [&abs], |r| r.get::<_, String>(0)) {
            return Ok(o);
        }
        let mut st = self.db.prepare("select id from objects where id like ?")?;
        let ids = st.query_map([format!("{x}%")], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        match ids.as_slice() {
            [one] => Ok(one.clone()),
            _ => Err(anyhow!("cannot resolve {x:?} to one object")),
        }
    }

    pub fn title(&self, oid: &str) -> String {
        if let Ok(p) = self.db.query_row("select path from names where obj=? limit 1", [oid], |r| r.get::<_, String>(0)) {
            return Path::new(&p).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or(p);
        }
        if let Ok(n) = self.db.query_row("select substr(dst,6) from edges where src=? and type='named' order by ts desc limit 1",
                                         [oid], |r| r.get::<_, String>(0)) {
            return n;
        }
        match self.db.query_row("select text from passages where obj=? order by ord limit 1", [oid], |r| r.get::<_, String>(0)) {
            Ok(t) => t.lines().next().unwrap_or("").chars().take(60).collect(),
            Err(_) => oid.chars().take(12).collect(),
        }
    }

    pub fn text(&self, oid: &str, limit: usize) -> Result<String> {
        let mut st = self.db.prepare("select text from passages where obj=? order by ord")?;
        let parts = st.query_map([oid], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        Ok(parts.join("\n\n").chars().take(limit).collect())
    }

    pub fn count_since(&self, t0: f64) -> Result<i64> {
        Ok(self.db.query_row("select count(*) from objects where created>=?", [t0], |r| r.get(0))?)
    }

    // --- queries -----------------------------------------------------------
    /// Hybrid: graph/time filters in SQL, then vector rank. `qvec=None` -> newest first.
    ///
    /// ponytail: linear scan over filtered passages, fine to ~1M passages; add an ANN index
    /// (sqlite-vec / HNSW) when that is measured to be slow.
    pub fn search(&self, qvec: Option<&[f32]>, f: &Filter, k: usize) -> Result<Vec<Hit>> {
        let (w, mut p) = where_clause(f);
        let Some(q) = qvec else {
            p.push(Value::Integer(k as i64));
            let sql = format!("select p.obj, p.text from passages p join objects o on o.id=p.obj where 1{w} \
                               group by p.obj order by o.created desc limit ?");
            let mut st = self.db.prepare(&sql)?;
            let rows = st.query_map(params_from_iter(p), |r| Ok(Hit { obj: r.get(0)?, text: r.get(1)?, score: 1.0 }))?;
            return Ok(rows.collect::<Result<_, _>>()?);
        };
        if q.len() != self.dim {
            bail!("query vector has {} dims, store has {}", q.len(), self.dim);
        }
        let sql = format!("select p.obj, p.text, p.emb from passages p join objects o on o.id=p.obj where 1{w}");
        let mut st = self.db.prepare(&sql)?;
        let boosted = self.tagged_objects(&f.boost)?;
        let mut best: HashMap<String, Hit> = HashMap::new();
        let mut rows = st.query(params_from_iter(p))?;
        while let Some(r) = rows.next()? {
            let (obj, text, emb): (String, String, Vec<u8>) = (r.get(0)?, r.get(1)?, r.get(2)?);
            let score = cosine(q, &from_bytes(&emb)) + if boosted.contains(&obj) { TAG_BOOST } else { 0.0 };
            if best.get(&obj).map_or(true, |h| score > h.score) {
                best.insert(obj.clone(), Hit { obj, text, score });
            }
        }
        let mut v: Vec<Hit> = best.into_values().collect();
        v.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.obj.cmp(&b.obj))); // deterministic on ties
        v.truncate(k);
        Ok(v)
    }

    fn tagged_objects(&self, tags: &[String]) -> Result<std::collections::HashSet<String>> {
        let mut out = std::collections::HashSet::new();
        for t in tags {
            let mut st = self.db.prepare("select src from edges where type='tagged' and dst=?")?;
            for r in st.query_map([format!("tag:{t}")], |r| r.get::<_, String>(0))? {
                out.insert(r?);
            }
        }
        Ok(out)
    }

    pub fn keyword(&self, q: &str, f: &Filter, k: usize) -> Result<Vec<Hit>> {
        let toks: Vec<String> = q.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|t| !t.is_empty()).map(|t| format!("\"{t}\"")).collect();
        if toks.is_empty() {
            return Ok(vec![]);
        }
        let (w, p) = where_clause(f);
        let mut params = vec![Value::Text(toks.join(" OR "))];
        params.extend(p);
        params.push(Value::Integer((k * 5) as i64));
        let sql = format!("select p.obj, p.text, bm25(fts) s from fts join passages p on p.id=fts.rowid \
                           join objects o on o.id=p.obj where fts match ?{w} order by s limit ?");
        let mut st = self.db.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(params), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?)))?;
        let (mut seen, mut out) = (std::collections::HashSet::new(), vec![]);
        for r in rows {
            let (obj, text, s) = r?;
            if seen.insert(obj.clone()) {
                out.push(Hit { obj, text, score: -s as f32 });
            }
        }
        out.truncate(k);
        Ok(out)
    }

    // --- snapshots ---------------------------------------------------------
    fn snapdir(&self) -> Result<PathBuf> {
        let d = self.path.parent().unwrap_or(Path::new(".")).join("snapshots");
        platform::makedirs(&d)?;
        Ok(d)
    }

    /// Clones the DB file. `platform::copy` is copy-on-write on APFS/btrfs, so this is O(1) in store size there.
    pub fn snapshot(&self, label: &str) -> Result<PathBuf> {
        let path = self.snapdir()?.join(format!("{}-{}.db", (platform::now() * 1000.0) as u64, norm_tag(label)));
        // The write lock keeps other connections from committing mid-copy; with a rollback journal
        // (not WAL) the main file then holds exactly the last committed state.
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let r = platform::copy(&self.path, &path);
        self.db.execute_batch("COMMIT")?;
        r.map(|_| path)
    }

    pub fn snapshots(&self) -> Result<Vec<PathBuf>> {
        Ok(platform::list_dir(&self.snapdir()?).into_iter().filter(|p| p.extension().is_some_and(|e| e == "db")).collect())
    }

    pub fn rollback(&mut self, snap: &Path) -> Result<()> {
        let tmp = Connection::open_in_memory()?; // release the file handle before overwriting
        drop(std::mem::replace(&mut self.db, tmp));
        platform::copy(snap, &self.path)?;
        *self = Store::open(&self.path.clone(), self.dim)?;
        Ok(())
    }
}

// --- replication (used by the mesh) -------------------------------------------

/// Everything needed to recreate one object on another device.
pub struct ObjectBundle {
    pub id: String,
    pub size: i64,
    pub created: f64,
    /// (blake3 hash, bytes) in order.
    pub chunks: Vec<(String, Vec<u8>)>,
    /// (text, embedding bytes) in order.
    pub passages: Vec<(String, Vec<u8>)>,
}

/// A deleted edge. Deletion wins ties against an edge with the same timestamp.
pub struct Tombstone {
    pub src: String,
    pub ty: String,
    pub dst: String,
    pub ts: f64,
}

pub struct EdgeRow {
    pub src: String,
    pub ty: String,
    pub dst: String,
    pub conf: f64,
    pub source: String,
    pub ts: f64,
}

impl Store {
    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn object_ids(&self) -> Result<Vec<String>> {
        let mut st = self.db.prepare("select id from objects order by id")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
        Ok(v)
    }

    pub fn edge_rows(&self) -> Result<Vec<EdgeRow>> {
        let mut st = self.db.prepare("select src, type, dst, conf, source, ts from edges")?;
        let rows = st.query_map([], |r| Ok(EdgeRow {
            src: r.get(0)?, ty: r.get(1)?, dst: r.get(2)?, conf: r.get(3)?, source: r.get(4)?, ts: r.get(5)?,
        }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn tombstone_rows(&self) -> Result<Vec<Tombstone>> {
        let mut st = self.db.prepare("select src, type, dst, ts from tombstones")?;
        let rows = st.query_map([], |r| Ok(Tombstone { src: r.get(0)?, ty: r.get(1)?, dst: r.get(2)?, ts: r.get(3)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn export_object(&self, id: &str) -> Result<ObjectBundle> {
        let (size, created): (i64, f64) = self.db.query_row("select size, created from objects where id=?", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut st = self.db.prepare("select c.blob, b.data from object_chunks c join blobs b on b.hash=c.blob where c.obj=? order by c.ord")?;
        let chunks = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        let mut st = self.db.prepare("select text, emb from passages where obj=? order by ord")?;
        let passages = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(ObjectBundle { id: id.to_string(), size, created, chunks, passages })
    }

    /// Untrusted input: verifies every hash and vector size before writing anything. Idempotent.
    pub fn import_object(&self, b: &ObjectBundle) -> Result<()> {
        if self.db.query_row("select 1 from objects where id=?", [&b.id], |_| Ok(())).is_ok() {
            return Ok(());
        }
        let mut whole = blake3::Hasher::new();
        for (h, data) in &b.chunks {
            if blake3::hash(data).to_hex().as_str() != h {
                bail!("chunk hash mismatch in object {}", b.id);
            }
            whole.update(data);
        }
        if whole.finalize().to_hex().as_str() != b.id {
            bail!("object {} does not match its id", b.id);
        }
        if b.passages.iter().any(|(_, e)| e.len() != self.dim * 4) {
            bail!("object {} has embeddings of the wrong dimension", b.id);
        }
        for (i, (h, data)) in b.chunks.iter().enumerate() {
            self.db.execute("insert or ignore into blobs values(?,?)", params![h, data])?;
            self.db.execute("insert into object_chunks values(?,?,?)", params![b.id, i as i64, h])?;
        }
        self.db.execute("insert into objects values(?,?,?)", params![b.id, b.size, b.created])?;
        for (i, (text, emb)) in b.passages.iter().enumerate() {
            self.db.execute("insert into passages(obj, ord, text, emb) values(?,?,?,?)", params![b.id, i as i64, text, emb])?;
            self.db.execute("insert into fts(rowid, text) values(?,?)", params![self.db.last_insert_rowid(), text])?;
        }
        Ok(())
    }

    /// Last-writer-wins per (src, type, dst); loses to a tombstone at least as new.
    pub fn merge_edge(&self, e: &EdgeRow) -> Result<()> {
        self.db.execute(
            "insert into edges select ?1,?2,?3,?4,?5,?6 where not exists(select 1 from tombstones \
             where src=?1 and type=?2 and dst=?3 and ts>=?6) on conflict(src, type, dst) do update set \
             conf=excluded.conf, source=excluded.source, ts=excluded.ts where excluded.ts > edges.ts",
            params![e.src, e.ty, e.dst, e.conf, e.source, e.ts])?;
        self.db.execute("delete from tombstones where src=? and type=? and dst=? and ts<?", params![e.src, e.ty, e.dst, e.ts])?;
        Ok(())
    }

    /// Deletes the edge unless it is strictly newer than the tombstone. Order-independent with `merge_edge`.
    pub fn merge_tombstone(&self, t: &Tombstone) -> Result<()> {
        self.db.execute("delete from edges where src=? and type=? and dst=? and ts<=?", params![t.src, t.ty, t.dst, t.ts])?;
        self.db.execute(
            "insert into tombstones select ?1,?2,?3,?4 where not exists(select 1 from edges \
             where src=?1 and type=?2 and dst=?3 and ts>?4) on conflict(src, type, dst) do update set \
             ts=excluded.ts where excluded.ts > tombstones.ts",
            params![t.src, t.ty, t.dst, t.ts])?;
        Ok(())
    }

    pub fn atomic<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.db.execute_batch("BEGIN")?;
        match f() {
            Ok(v) => {
                self.db.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.db.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}
