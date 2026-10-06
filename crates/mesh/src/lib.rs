//! P4 mesh: encrypted two-device replication of the object store and graph.
//!
//! Objects are content-addressed, so replication is a set union and cannot conflict. Edges merge
//! last-writer-wins per (src, type, dst). Devices pair by sharing one 32-byte key.
//!
//! ponytail: pre-shared key + full id/edge exchange (O(n) per sync) instead of Noise NK + QR join and
//! vector-clock deltas; no relay/NAT traversal (dial a reachable address). Replace when more than a
//! handful of devices or ~1M objects are in play.
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key as AeadKey, Nonce};
use core_data::{EdgeRow, ObjectBundle, Store};
use platform::{Conn, Listener};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

pub type Key = [u8; 32];

const MAX_FRAME: usize = 64 << 20;
const BATCH: usize = 2000;

#[derive(Serialize, Deserialize)]
struct WEdge(String, String, String, f64, String, f64);

#[derive(Serialize, Deserialize)]
struct WObj {
    id: String,
    size: i64,
    created: f64,
    chunks: Vec<(String, String)>,
    passages: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t")]
enum Msg {
    Hello { dim: usize },
    Ids { ids: Vec<String> },
    Edges { rows: Vec<WEdge> },
    Obj { o: WObj },
    End { want: Vec<String> },
    Ack { objects: usize },
}

#[derive(Debug, Default, PartialEq)]
pub struct Stats {
    pub objects_in: usize,
    pub objects_out: usize,
    pub edges_in: usize,
}

// --- pairing --------------------------------------------------------------------

pub fn key_path(home: &Path) -> std::path::PathBuf {
    home.join("mesh.key")
}

pub fn load_or_create_key(home: &Path) -> Result<Key> {
    let p = key_path(home);
    if !platform::exists(&p) {
        platform::write_secret(&p, &platform::random_bytes::<32>()?)?;
    }
    platform::read_file(&p)?.try_into().map_err(|_| anyhow!("{} is not a 32-byte key", p.display()))
}

pub fn key_to_hex(k: &Key) -> String {
    k.iter().map(|b| format!("{b:02x}")).collect()
}

/// Install a key copied from another device. Refuses to overwrite an existing key.
pub fn join(home: &Path, hex: &str) -> Result<()> {
    let hex = hex.trim();
    if hex.len() != 64 || !hex.is_ascii() {
        bail!("key must be 64 hex characters");
    }
    let bytes = (0..32).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)).collect::<Result<Vec<_>, _>>()?;
    platform::write_secret(&key_path(home), &bytes)
}

// --- encrypted channel ------------------------------------------------------------

struct Chan {
    conn: Conn,
    cipher: ChaCha20Poly1305,
}

impl Chan {
    fn new(conn: Conn, key: &Key) -> Chan {
        Chan { conn, cipher: ChaCha20Poly1305::new(&AeadKey::from(*key)) }
    }

    fn send(&mut self, m: &Msg) -> Result<()> {
        let nonce = platform::random_bytes::<12>()?;
        let ct = self.cipher.encrypt(&Nonce::from(nonce), serde_json::to_vec(m)?.as_slice())
            .map_err(|_| anyhow!("encrypt failed"))?;
        self.conn.send(&[nonce.as_slice(), &ct].concat())
    }

    fn recv(&mut self) -> Result<Msg> {
        let b = self.conn.recv(MAX_FRAME)?;
        if b.len() < 12 {
            bail!("short frame");
        }
        let nonce: [u8; 12] = b[..12].try_into()?;
        let pt = self.cipher.decrypt(&Nonce::from(nonce), &b[12..]).map_err(|_| anyhow!("authentication failed (wrong key?)"))?;
        Ok(serde_json::from_slice(&pt)?)
    }
}

// --- wire conversion ----------------------------------------------------------------

fn to_wire(b: ObjectBundle) -> WObj {
    WObj {
        id: b.id, size: b.size, created: b.created,
        chunks: b.chunks.into_iter().map(|(h, d)| (h, B64.encode(d))).collect(),
        passages: b.passages.into_iter().map(|(t, e)| (t, B64.encode(e))).collect(),
    }
}

fn from_wire(o: WObj) -> Result<ObjectBundle> {
    let dec = |s: String| B64.decode(s).map_err(|e| anyhow!("bad base64: {e}"));
    Ok(ObjectBundle {
        id: o.id, size: o.size, created: o.created,
        chunks: o.chunks.into_iter().map(|(h, d)| Ok((h, dec(d)?))).collect::<Result<_>>()?,
        passages: o.passages.into_iter().map(|(t, e)| Ok((t, dec(e)?))).collect::<Result<_>>()?,
    })
}

fn send_ids_edges(c: &mut Chan, store: &Store) -> Result<()> {
    for ids in store.object_ids()?.chunks(BATCH) {
        c.send(&Msg::Ids { ids: ids.to_vec() })?;
    }
    send_edges(c, store)
}

fn send_edges(c: &mut Chan, store: &Store) -> Result<()> {
    let rows: Vec<WEdge> = store.edge_rows()?.into_iter().map(|e| WEdge(e.src, e.ty, e.dst, e.conf, e.source, e.ts)).collect();
    for r in rows.chunks(BATCH) {
        c.send(&Msg::Edges { rows: r.iter().map(|e| WEdge(e.0.clone(), e.1.clone(), e.2.clone(), e.3, e.4.clone(), e.5)).collect() })?;
    }
    Ok(())
}

fn send_objects(c: &mut Chan, store: &Store, ids: &[String]) -> Result<()> {
    for id in ids {
        c.send(&Msg::Obj { o: to_wire(store.export_object(id)?) })?;
    }
    Ok(())
}

fn merge_edges(store: &Store, rows: Vec<WEdge>) -> Result<usize> {
    let n = rows.len();
    for WEdge(src, ty, dst, conf, source, ts) in rows {
        store.merge_edge(&EdgeRow { src, ty, dst, conf, source, ts })?;
    }
    Ok(n)
}

/// Read frames until `End`, applying objects and edges. Returns (peer ids, want list, stats).
fn receive(c: &mut Chan, store: &Store, stats: &mut Stats) -> Result<(HashSet<String>, Vec<String>)> {
    let mut ids = HashSet::new();
    loop {
        match c.recv()? {
            Msg::Ids { ids: v } => ids.extend(v),
            Msg::Edges { rows } => stats.edges_in += merge_edges(store, rows)?,
            Msg::Obj { o } => {
                store.import_object(&from_wire(o)?)?;
                stats.objects_in += 1;
            }
            Msg::End { want } => return Ok((ids, want)),
            _ => bail!("unexpected message"),
        }
    }
}

fn check_dim(store: &Store, m: Msg) -> Result<()> {
    match m {
        Msg::Hello { dim } if dim == store.dim() => Ok(()),
        Msg::Hello { dim } => bail!("peer uses {dim}-dim embeddings, this store uses {}", store.dim()),
        _ => bail!("expected hello"),
    }
}

// --- protocol ---------------------------------------------------------------------------

/// Dial a peer and make both stores equal.
pub fn sync(addr: &str, store: &Store, key: &Key) -> Result<Stats> {
    let mut c = Chan::new(platform::dial(addr)?, key);
    c.send(&Msg::Hello { dim: store.dim() })?;
    send_ids_edges(&mut c, store)?;
    c.send(&Msg::End { want: vec![] })?;
    check_dim(store, c.recv()?)?;
    let mut stats = Stats::default();
    let (_, want) = store.atomic(|| receive(&mut c, store, &mut stats))?;
    send_objects(&mut c, store, &want)?;
    stats.objects_out = want.len();
    c.send(&Msg::End { want: vec![] })?;
    match c.recv()? {
        Msg::Ack { .. } => Ok(stats),
        _ => bail!("peer did not acknowledge"),
    }
}

/// Accept one peer and sync with it.
pub fn serve_one(l: &Listener, store: &Store, key: &Key) -> Result<Stats> {
    let mut c = Chan::new(l.accept()?, key);
    check_dim(store, c.recv()?)?;
    let mut stats = Stats::default();
    let (theirs, _) = store.atomic(|| receive(&mut c, store, &mut stats))?;
    let mine: HashSet<String> = store.object_ids()?.into_iter().collect();
    let missing_there: Vec<String> = mine.difference(&theirs).cloned().collect();
    let want: Vec<String> = theirs.difference(&mine).cloned().collect();
    c.send(&Msg::Hello { dim: store.dim() })?;
    send_edges(&mut c, store)?;
    send_objects(&mut c, store, &missing_there)?;
    stats.objects_out = missing_there.len();
    c.send(&Msg::End { want })?;
    let mut pushed = Stats::default();
    store.atomic(|| receive(&mut c, store, &mut pushed))?;
    stats.objects_in += pushed.objects_in;
    c.send(&Msg::Ack { objects: pushed.objects_in })?;
    Ok(stats)
}
