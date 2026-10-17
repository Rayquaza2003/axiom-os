//! P4 mesh: encrypted two-device replication of the object store and graph.
//!
//! Objects are content-addressed, so replication is a set union and cannot conflict. Edges merge
//! last-writer-wins per (src, type, dst); deletions travel as tombstones. Devices pair by sharing one
//! 32-byte key, used as the PSK of a Noise NNpsk0 handshake (mutual auth, forward secrecy, no replay).
//!
//! Addresses: `host:port` dials directly; `relay://host:port` meets the peer at a relay (both sides dial
//! out, so NAT is no obstacle; the relay only pipes Noise ciphertext). A comma-separated list fails over
//! in order, e.g. `192.168.1.5:7777,relay://relay.example:7778`.
//!
//! Resource broker: a serving device also runs `Embed` / `Generate` / `Speak` jobs for its peers at
//! BACKGROUND priority (its own user always wins), so a laptop can borrow a desktop's model or speakers.
//!
//! ponytail: full id/edge exchange (O(n) per sync) instead of vector-clock deltas; no UDP hole punching
//! (the relay covers NAT); more than two devices converge by pairwise syncs. Replace when more than a
//! handful of devices or ~1M objects are in play.
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use core_ai::{NpuBackend, Nrm, BACKGROUND};
use core_data::{EdgeRow, ObjectBundle, Store, Tombstone};
use platform::{Conn, Listener};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub type Key = [u8; 32];

const MAX_FRAME: usize = 64 << 20;
const BATCH: usize = 2000;
const NOISE: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
const NOISE_MAX: usize = 65535;
/// Plaintext per Noise message: max length minus the AEAD tag and our one-byte "more follows" flag.
const PIECE: usize = NOISE_MAX - 16 - 1;
const ROLE_SERVE: u8 = 0;
const ROLE_DIAL: u8 = 1;
const MAX_ROOMS: usize = 1024;

#[derive(Serialize, Deserialize)]
struct WEdge(String, String, String, f64, String, f64);

#[derive(Serialize, Deserialize)]
struct WTomb(String, String, String, f64);

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
    Tombs { rows: Vec<WTomb> },
    Obj { o: WObj },
    End { want: Vec<String> },
    Ack { objects: usize },
    // resource broker
    Info,
    Caps { dim: usize, llm: bool },
    Embed { texts: Vec<String> },
    Vectors { v: Vec<Vec<f32>> },
    Generate { prompt: String },
    Speak { text: String },
    Text { s: String },
    Fail { error: String },
}

#[derive(Debug, Default, PartialEq)]
pub struct Stats {
    pub objects_in: usize,
    pub objects_out: usize,
    pub edges_in: usize,
    /// Broker jobs run for the peer (the credit it used on this device).
    pub jobs: usize,
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

// --- addressing: direct, relay, failover ------------------------------------------------

/// The relay sees only this, never the key.
fn room(key: &Key) -> [u8; 32] {
    blake3::derive_key("axiom mesh relay room v1", key)
}

fn dial_one(addr: &str, key: &Key) -> Result<Conn> {
    let Some(relay) = addr.strip_prefix("relay://") else { return platform::dial(addr) };
    let mut c = platform::dial(relay)?;
    c.send(&[room(key).as_slice(), &[ROLE_DIAL]].concat())?;
    match c.recv(1)?.as_slice() {
        b"P" => Ok(c),
        _ => bail!("no peer waiting at relay {relay}"),
    }
}

/// Tries each comma-separated address in order; the first that connects wins.
fn connect(addrs: &str, key: &Key) -> Result<Chan> {
    let mut errs = vec![];
    for a in addrs.split(',').map(str::trim).filter(|a| !a.is_empty()) {
        match dial_one(a, key) {
            Ok(c) => return Chan::handshake(c, key, true),
            Err(e) => errs.push(e.to_string()),
        }
    }
    bail!("no address reachable: {}", if errs.is_empty() { "none given".into() } else { errs.join("; ") })
}

/// Run a relay: parks serving devices by room and pipes each dialer to the matching one.
pub fn relay(l: &Listener) -> Result<()> {
    let parked: Arc<Mutex<HashMap<[u8; 32], Conn>>> = Arc::default();
    loop {
        let Ok(c) = l.accept() else { continue };
        let parked = parked.clone();
        std::thread::spawn(move || relay_one(c, &parked));
    }
}

fn relay_one(mut c: Conn, parked: &Mutex<HashMap<[u8; 32], Conn>>) -> Result<()> {
    let h = c.recv(33)?;
    let (Ok(room), Some(&role)) = (<[u8; 32]>::try_from(&h[..h.len().min(32)]), h.get(32)) else { bail!("bad relay hello") };
    if role == ROLE_SERVE {
        let mut p = parked.lock().unwrap();
        if p.len() >= MAX_ROOMS && !p.contains_key(&room) {
            bail!("relay full");
        }
        p.insert(room, c); // a newer serve replaces a stale one
        return Ok(());
    }
    let peer = parked.lock().unwrap().remove(&room);
    let Some(mut s) = peer else { return c.send(b"N") };
    s.send(b"P")?;
    c.send(b"P")?;
    platform::splice(c, s)
}

// --- encrypted channel ------------------------------------------------------------

struct Chan {
    conn: Conn,
    noise: snow::TransportState,
    buf: Vec<u8>,
}

impl Chan {
    /// NNpsk0: -> psk, e  /  <- e, ee. A wrong key fails the first read on the responder.
    fn handshake(mut conn: Conn, key: &Key, initiator: bool) -> Result<Chan> {
        let b = snow::Builder::new(NOISE.parse()?).psk(0, key)?;
        let mut hs = if initiator { b.build_initiator()? } else { b.build_responder()? };
        let mut buf = vec![0u8; NOISE_MAX];
        let auth = |_| anyhow!("authentication failed (wrong key?)");
        if initiator {
            let n = hs.write_message(&[], &mut buf)?;
            conn.send(&buf[..n])?;
            hs.read_message(&conn.recv(NOISE_MAX)?, &mut buf).map_err(auth)?;
        } else {
            hs.read_message(&conn.recv(NOISE_MAX)?, &mut buf).map_err(auth)?;
            let n = hs.write_message(&[], &mut buf)?;
            conn.send(&buf[..n])?;
        }
        Ok(Chan { conn, noise: hs.into_transport_mode()?, buf })
    }

    fn send(&mut self, m: &Msg) -> Result<()> {
        let pt = serde_json::to_vec(m)?;
        let mut pieces = pt.chunks(PIECE).peekable();
        while let Some(p) = pieces.next() {
            let n = self.noise.write_message(&[&[pieces.peek().is_some() as u8], p].concat(), &mut self.buf)?;
            self.conn.send(&self.buf[..n])?;
        }
        Ok(())
    }

    fn recv(&mut self) -> Result<Msg> {
        let mut pt = vec![];
        loop {
            let ct = self.conn.recv(NOISE_MAX)?;
            let n = self.noise.read_message(&ct, &mut self.buf).map_err(|_| anyhow!("authentication failed"))?;
            if n == 0 {
                bail!("empty frame");
            }
            pt.extend_from_slice(&self.buf[1..n]);
            if pt.len() > MAX_FRAME {
                bail!("message exceeds {MAX_FRAME} bytes");
            }
            if self.buf[0] == 0 {
                return Ok(serde_json::from_slice(&pt)?);
            }
        }
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
    let tombs: Vec<WTomb> = store.tombstone_rows()?.into_iter().map(|t| WTomb(t.src, t.ty, t.dst, t.ts)).collect();
    for r in tombs.chunks(BATCH) {
        c.send(&Msg::Tombs { rows: r.iter().map(|t| WTomb(t.0.clone(), t.1.clone(), t.2.clone(), t.3)).collect() })?;
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
            Msg::Tombs { rows } => {
                stats.edges_in += rows.len();
                for WTomb(src, ty, dst, ts) in rows {
                    store.merge_tombstone(&Tombstone { src, ty, dst, ts })?;
                }
            }
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

/// Dial a peer (comma-separated addresses fail over in order) and make both stores equal.
pub fn sync(addrs: &str, store: &Store, key: &Key) -> Result<Stats> {
    let mut c = connect(addrs, key)?;
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

/// Accept one peer and sync with it (or run its broker job if `nrm` is given).
pub fn serve_one(l: &Listener, store: &Store, key: &Key, nrm: Option<&Nrm>) -> Result<Stats> {
    serve_conn(l.accept()?, store, key, nrm)
}

/// Park at a relay until a peer arrives (`None` after `wait` with no peer), then serve it.
pub fn serve_relay(relay: &str, store: &Store, key: &Key, nrm: Option<&Nrm>, wait: Duration) -> Result<Option<Stats>> {
    let relay = relay.strip_prefix("relay://").unwrap_or(relay);
    let mut c = platform::dial(relay)?;
    c.send(&[room(key).as_slice(), &[ROLE_SERVE]].concat())?;
    match c.recv_within(1, wait)? {
        None => Ok(None),
        Some(b) if b == b"P" => serve_conn(c, store, key, nrm).map(Some),
        Some(_) => bail!("unexpected relay reply"),
    }
}

fn serve_conn(conn: Conn, store: &Store, key: &Key, nrm: Option<&Nrm>) -> Result<Stats> {
    let mut c = Chan::handshake(conn, key, false)?;
    let first = c.recv()?;
    if !matches!(first, Msg::Hello { .. }) {
        return run_job(&mut c, nrm, first);
    }
    check_dim(store, first)?;
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

// --- resource broker ------------------------------------------------------------------------

fn run_job(c: &mut Chan, nrm: Option<&Nrm>, m: Msg) -> Result<Stats> {
    let Some(nrm) = nrm else {
        c.send(&Msg::Fail { error: "this device does not run jobs for peers".into() })?;
        bail!("peer asked for a job; jobs are disabled");
    };
    let reply = match m {
        Msg::Info => Ok(Msg::Caps { dim: nrm.backend.dim(), llm: nrm.backend.has_llm() }),
        Msg::Embed { texts } => nrm.embed(BACKGROUND, texts).map(|v| Msg::Vectors { v }),
        Msg::Generate { prompt } => nrm.generate(BACKGROUND, prompt).map(|s| Msg::Text { s }),
        Msg::Speak { text } => {
            platform::speak(&text);
            Ok(Msg::Text { s: String::new() })
        }
        _ => bail!("unexpected message"),
    };
    c.send(&reply.unwrap_or_else(|e| Msg::Fail { error: e.to_string() }))?;
    Ok(Stats { jobs: 1, ..Stats::default() })
}

fn request(addrs: &str, key: &Key, m: Msg) -> Result<Msg> {
    let mut c = connect(addrs, key)?;
    c.send(&m)?;
    match c.recv()? {
        Msg::Fail { error } => bail!("peer: {error}"),
        r => Ok(r),
    }
}

/// Play `text` on a peer's speakers (audio routing).
pub fn remote_speak(addrs: &str, key: &Key, text: &str) -> Result<()> {
    request(addrs, key, Msg::Speak { text: text.into() }).map(|_| ())
}

/// An `NpuBackend` that runs every job on a peer device. `audit(bytes)` is called before each send.
pub struct RemoteBackend {
    addrs: String,
    key: Key,
    dim: usize,
    llm: bool,
    audit: Box<dyn Fn(usize) + Send + Sync>,
}

impl RemoteBackend {
    pub fn connect(addrs: &str, key: Key, audit: Box<dyn Fn(usize) + Send + Sync>) -> Result<RemoteBackend> {
        match request(addrs, &key, Msg::Info)? {
            Msg::Caps { dim, llm } => Ok(RemoteBackend { addrs: addrs.into(), key, dim, llm, audit }),
            _ => bail!("peer did not report its capabilities"),
        }
    }
}

impl NpuBackend for RemoteBackend {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        (self.audit)(texts.iter().map(String::len).sum());
        match request(&self.addrs, &self.key, Msg::Embed { texts: texts.to_vec() })? {
            Msg::Vectors { v } if v.len() == texts.len() && v.iter().all(|x| x.len() == self.dim) => Ok(v),
            _ => bail!("peer sent a malformed embedding reply"),
        }
    }

    fn has_llm(&self) -> bool {
        self.llm
    }

    fn generate(&self, prompt: &str) -> Result<String> {
        (self.audit)(prompt.len());
        match request(&self.addrs, &self.key, Msg::Generate { prompt: prompt.into() })? {
            Msg::Text { s } => Ok(s),
            _ => bail!("peer sent a malformed generate reply"),
        }
    }
}
