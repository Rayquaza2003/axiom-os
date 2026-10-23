//! P6 runtime: third-party apps run in an OS sandbox and reach the system only through this broker.
//!
//! Deny by default: an app declares `requires` in its manifest, the user grants each capability at
//! install, and every call is checked against the grant table at call time (so `revoke` bites
//! immediately). Wire protocol: the app writes one JSON request per line to stdout
//! (`{"id":1,"call":"search","args":{"q":"..."}}`) and reads one JSON reply per line from stdin.
//!
//! Calls: `search`, `tag`, `generate`, `fetch` (needs `egress:<host>` AND passes the egress firewall),
//! plus `log` (no capability). The OS sandbox blocks network, writes and reads of the Axiom home.
//!
//! Scoped capabilities: `search:tag=<t>` only sees objects tagged `t`; `tag:tag=<t>` only tags them.
//! Model-context capabilities (`generate`) expire after `CAP_TTL_S`; the rest last until revoked.
//!
//! Two runtimes: `cmd` runs a native process under the OS sandbox; `wasm` runs a WebAssembly module
//! in-process (wasmi) with no imports but `axiom.call` / `axiom.reply`, so it has no ambient authority
//! on any host. Both speak the same JSON requests.
//!
//! Signed bundles: `signature.json` holds an ed25519 signature over every file. A signed bundle must
//! come from a trusted publisher; an update must be signed by the publisher of the installed version.
//! Unsigned bundles install (development) but can never replace a signed one.
use crate::Axiom;
use anyhow::{anyhow, bail, Result};
use core_ai::egress::host_key;
use core_ai::{execute_in, BACKGROUND};
use core_data::norm_tag;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// (capability, seconds a grant lives). Decision P0.3: short TTL for model-context caps.
const CAP_TTL_S: &[(&str, f64)] = &[("generate", 3600.0)];

const SCHEMA: &str = "
create table if not exists grants(app text, cap text, ts real, primary key(app, cap));
create table if not exists calls(ts real, app text, call text, decision text);
create table if not exists installs(app text primary key, publisher text);
create table if not exists trusted(publisher text primary key);
";

const SIG_FILE: &str = "signature.json";
/// Instruction budget for one wasm run (~2s of CPU); a runaway loop traps instead of spinning forever.
/// ponytail: fixed budget, no per-app setting; wasmi has no wall-clock interrupt, so fuel is the only stop.
const WASM_FUEL: u64 = 500_000_000;
const WASM_MEMORY: usize = 256 << 20;

pub struct Manifest {
    pub name: String,
    /// Native command (empty for a wasm app).
    pub cmd: Vec<String>,
    /// Wasm module file in the app directory (binary or text format).
    pub wasm: Option<String>,
    pub requires: Vec<String>,
}

pub struct Installed {
    pub name: String,
    pub granted: Vec<String>,
    pub required: Vec<String>,
    /// Hex ed25519 key of the signer; `None` = unsigned.
    pub publisher: Option<String>,
}

#[derive(Debug, Default)]
pub struct RunResult {
    pub exit: Option<i32>,
    pub log: Vec<String>,
    /// (call, allowed)
    pub calls: Vec<(String, bool)>,
}

fn db(home: &Path) -> Result<Connection> {
    let c = Connection::open(home.join("apps.db"))?;
    c.execute_batch(SCHEMA)?;
    Ok(c)
}

fn app_dir(home: &Path, name: &str) -> PathBuf {
    home.join("apps").join(name)
}

fn valid_cap(c: &str) -> bool {
    let scoped = |base: &str| c.strip_prefix(base).and_then(|r| r.strip_prefix(":tag=")).is_some_and(|t| !t.is_empty() && norm_tag(t) == t);
    matches!(c, "search" | "tag" | "generate") || scoped("search") || scoped("tag")
        || c.strip_prefix("egress:").is_some_and(|h| !h.is_empty() && !h.contains('/'))
}

pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let v: Value = serde_json::from_slice(&platform::read_file(&dir.join("manifest.json"))?)?;
    let strs = |k: &str| -> Result<Vec<String>> {
        v[k].as_array().ok_or_else(|| anyhow!("manifest.{k} must be a list"))?
            .iter().map(|x| x.as_str().map(String::from).ok_or_else(|| anyhow!("manifest.{k} must hold strings"))).collect()
    };
    let name = v["name"].as_str().ok_or_else(|| anyhow!("manifest.name missing"))?.to_string();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        bail!("app name must be lowercase letters, digits, - or _");
    }
    let cmd = if v["cmd"].is_null() { vec![] } else { strs("cmd")? };
    let (wasm, requires) = (v["wasm"].as_str().map(String::from), strs("requires")?);
    match (&wasm, cmd.is_empty()) {
        (Some(_), false) => bail!("manifest has both cmd and wasm"),
        (None, true) => bail!("manifest.cmd is empty"),
        (Some(w), _) if w.is_empty() || w.starts_with('.') || w.contains('/') || w.contains('\\') => {
            bail!("manifest.wasm must be a file name inside the app directory")
        }
        _ => {}
    }
    if let Some(bad) = requires.iter().find(|c| !valid_cap(c)) {
        bail!("unknown capability {bad:?}");
    }
    Ok(Manifest { name, cmd, wasm, requires })
}

// --- signing ---------------------------------------------------------------------------------

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Result<[u8; N]> {
    let s = s.trim();
    if s.len() != 2 * N || !s.is_ascii() {
        bail!("expected {} hex characters", 2 * N);
    }
    let v = (0..N).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16)).collect::<Result<Vec<_>, _>>()?;
    Ok(v.try_into().unwrap())
}

/// Blake3 over (path, bytes) of every file except the signature, in path order.
fn bundle_digest(dir: &Path) -> Result<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    for p in platform::walk_all(dir)? {
        let rel = p.strip_prefix(dir)?.to_string_lossy().to_string();
        if rel == SIG_FILE {
            continue;
        }
        let data = platform::read_file(&p)?;
        for part in [rel.as_bytes(), &data] {
            h.update(&(part.len() as u64).to_le_bytes());
            h.update(part);
        }
    }
    Ok(*h.finalize().as_bytes())
}

/// This device's publisher key, created on first use. Returns (signing key, public hex).
pub fn publisher_key(home: &Path) -> Result<(SigningKey, String)> {
    let p = home.join("publisher.key");
    if !platform::exists(&p) {
        platform::write_secret(&p, &platform::random_bytes::<32>()?)?;
    }
    let seed: [u8; 32] = platform::read_file(&p)?.try_into().map_err(|_| anyhow!("{} is not a 32-byte key", p.display()))?;
    let k = SigningKey::from_bytes(&seed);
    let public = hex(k.verifying_key().as_bytes());
    Ok((k, public))
}

/// Writes `signature.json` into `dir`. Returns the publisher key (hex).
pub fn sign(home: &Path, dir: &Path) -> Result<String> {
    read_manifest(dir)?;
    let (k, public) = publisher_key(home)?;
    let sig = k.sign(&bundle_digest(dir)?);
    platform::write_file(&dir.join(SIG_FILE), json!({"publisher": public, "sig": hex(&sig.to_bytes())}).to_string().as_bytes())?;
    Ok(public)
}

/// `Ok(None)` = unsigned; `Err` = a signature that does not verify.
fn verify(dir: &Path) -> Result<Option<String>> {
    let p = dir.join(SIG_FILE);
    if !platform::exists(&p) {
        return Ok(None);
    }
    let v: Value = serde_json::from_slice(&platform::read_file(&p)?)?;
    let (publisher, sig) = (v["publisher"].as_str().unwrap_or(""), v["sig"].as_str().unwrap_or(""));
    let key = VerifyingKey::from_bytes(&unhex::<32>(publisher)?).map_err(|_| anyhow!("bad publisher key"))?;
    key.verify_strict(&bundle_digest(dir)?, &Signature::from_bytes(&unhex::<64>(sig)?))
        .map_err(|_| anyhow!("signature does not match the bundle (modified after signing?)"))?;
    Ok(Some(publisher.to_string()))
}

pub fn trust(home: &Path, publisher: &str) -> Result<()> {
    unhex::<32>(publisher)?;
    db(home)?.execute("insert or ignore into trusted values(?)", [publisher.trim()])?;
    Ok(())
}

/// Copy the app in and ask `approve(app, cap)` for each required capability. Re-installing resets grants.
/// The bundle is verified after copying (so it cannot change between check and use).
pub fn install(ax: &Axiom, src: &Path, approve: &dyn Fn(&str, &str) -> bool) -> Result<(String, Vec<String>)> {
    let staging = ax.home.join("apps").join(".staging");
    platform::remove_dir_all(&staging)?;
    platform::copy_tree(src, &staging)?;
    let checked = check_bundle(&ax.home, &staging);
    let (m, publisher) = match checked {
        Ok(v) => v,
        Err(e) => {
            platform::remove_dir_all(&staging)?;
            return Err(e);
        }
    };
    let dest = app_dir(&ax.home, &m.name);
    platform::remove_dir_all(&dest)?;
    platform::rename(&staging, &dest)?;
    let c = db(&ax.home)?;
    c.execute("insert or replace into installs values(?,?)", params![m.name, publisher])?;
    c.execute("delete from grants where app=?", [&m.name])?;
    let mut granted = vec![];
    for cap in &m.requires {
        if approve(&m.name, cap) {
            c.execute("insert or replace into grants values(?,?,?)", params![m.name, cap, platform::now()])?;
            granted.push(cap.clone());
        }
    }
    Ok((m.name, granted))
}

fn check_bundle(home: &Path, dir: &Path) -> Result<(Manifest, Option<String>)> {
    let m = read_manifest(dir)?;
    let publisher = verify(dir)?;
    let c = db(home)?;
    if let Some(p) = &publisher {
        if c.query_row("select 1 from trusted where publisher=?", [p], |_| Ok(())).optional()?.is_none() {
            bail!("untrusted publisher {p} (run `axiom app trust {p}` if you trust it)");
        }
    }
    let prev: Option<Option<String>> = c.query_row("select publisher from installs where app=?", [&m.name], |r| r.get(0)).optional()?;
    if let Some(Some(prev)) = prev {
        if publisher.as_ref() != Some(&prev) {
            bail!("{} is installed from publisher {prev}; an update must be signed by the same publisher", m.name);
        }
    }
    Ok((m, publisher))
}

pub fn granted(home: &Path, app: &str) -> Result<Vec<String>> {
    let c = db(home)?;
    let mut st = c.prepare("select cap from grants where app=? order by cap")?;
    let v = st.query_map([app], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
    Ok(v)
}

pub fn revoke(home: &Path, app: &str, cap: Option<&str>) -> Result<usize> {
    let c = db(home)?;
    Ok(match cap {
        Some(cap) => c.execute("delete from grants where app=? and cap=?", [app, cap])?,
        None => c.execute("delete from grants where app=?", [app])?,
    })
}

pub fn list(home: &Path) -> Result<Vec<Installed>> {
    let c = db(home)?;
    let mut out = vec![];
    for d in platform::list_dir(&home.join("apps")) {
        if d.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        if let Ok(m) = read_manifest(&d) {
            let publisher = c.query_row("select publisher from installs where app=?", [&m.name], |r| r.get(0)).optional()?.flatten();
            out.push(Installed { granted: granted(home, &m.name)?, name: m.name, required: m.requires, publisher });
        }
    }
    Ok(out)
}

fn call(ax: &Axiom, app: &str, name: &str, args: &Value, now: f64) -> Result<Value, String> {
    let e = |x: anyhow::Error| x.to_string();
    let need = match name {
        "search" | "tag" | "generate" => name.to_string(),
        "fetch" => format!("egress:{}", host_key(args["url"].as_str().unwrap_or(""))),
        _ => return Err(format!("unknown call {name:?}")),
    };
    let c = db(&ax.home).map_err(e)?;
    let mut st = c.prepare("select cap, ts from grants where app=?").map_err(|x| x.to_string())?;
    let grants: Vec<(String, f64)> = st.query_map([app], |r| Ok((r.get(0)?, r.get(1)?))).and_then(|r| r.collect())
        .map_err(|x| x.to_string())?;
    // an unscoped grant beats a scoped one
    let scoped = |g: &&(String, f64)| matches!(name, "search" | "tag") && g.0.strip_prefix(&need).is_some_and(|r| r.starts_with(":tag="));
    let Some((cap, granted_at)) = grants.iter().find(|g| g.0 == need).or_else(|| grants.iter().find(scoped)) else {
        return Err(format!("capability denied: {need}"));
    };
    if CAP_TTL_S.iter().any(|(c, ttl)| *c == need && now - granted_at > *ttl) {
        return Err(format!("capability expired: {need} (reinstall to grant again)"));
    }
    let scope = cap.split_once(":tag=").map(|(_, t)| t.to_string());
    let s = |k: &str| args[k].as_str().ok_or_else(|| format!("missing string arg {k:?}"));
    match name {
        "search" => {
            let r = execute_in(&ax.store, &ax.nrm, s("q")?, now, 5, scope.as_deref()).map_err(e)?;
            Ok(json!(r.hits.iter().map(|h| json!({
                "id": h.obj, "title": ax.store.title(&h.obj), "score": h.score,
                "text": h.text.chars().take(300).collect::<String>(),
            })).collect::<Vec<_>>()))
        }
        "tag" => {
            let id = ax.store.resolve(s("obj")?).map_err(e)?;
            if let Some(t) = &scope {
                let inside = ax.store.db.query_row("select 1 from edges where src=? and type='tagged' and dst=?",
                                                   [&id, &format!("tag:{t}")], |_| Ok(())).is_ok();
                if !inside {
                    return Err(format!("object outside capability scope (tag:{t})"));
                }
            }
            ax.store.tag(&id, s("tag")?, 0.8, &format!("app:{app}")).map_err(e)?;
            Ok(json!("ok"))
        }
        "generate" => {
            if !ax.nrm.backend.has_llm() {
                return Err("no LLM configured (set AXIOM_LLM_MODEL)".into());
            }
            ax.nrm.generate(BACKGROUND, s("prompt")?.to_string()).map(Value::from).map_err(e)
        }
        _ => ax.egress.send(s("url")?, &args["payload"], "app").map_err(e), // fetch
    }
}

/// Run an installed app until it exits, closes stdout, or exceeds `timeout`.
pub fn run(ax: &Axiom, name: &str, args: &[String], now: f64, timeout: Duration) -> Result<RunResult> {
    let dir = app_dir(&ax.home, name);
    let m = read_manifest(&dir).map_err(|e| anyhow!("app {name:?} is not installed ({e})"))?;
    let mut p: Box<dyn AppIo> = match &m.wasm {
        Some(w) => Box::new(WasmApp::spawn(&platform::read_file(&dir.join(w))?)?),
        None => Box::new(platform::spawn_sandboxed(&[m.cmd.clone(), args.to_vec()].concat(), &dir, &ax.home)?),
    };
    let (mut res, deadline) = (RunResult::default(), Instant::now() + timeout);
    let audit = db(&ax.home)?;
    while let Some(line) = deadline.checked_duration_since(Instant::now()).and_then(|left| p.recv_line(left)) {
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                res.log.push(format!("(malformed line ignored: {})", line.chars().take(80).collect::<String>()));
                continue;
            }
        };
        let (id, name) = (req["id"].clone(), req["call"].as_str().unwrap_or("").to_string());
        let reply = if name == "log" {
            res.log.push(req["args"]["msg"].as_str().unwrap_or("").to_string());
            json!({"id": id, "ok": "ok"})
        } else {
            let r = call(ax, &m.name, &name, &req["args"], now);
            audit.execute("insert into calls values(?,?,?,?)", params![platform::now(), m.name, name, if r.is_ok() { "allow" } else { "deny" }])?;
            res.calls.push((name, r.is_ok()));
            match r {
                Ok(v) => json!({"id": id, "ok": v}),
                Err(msg) => json!({"id": id, "error": msg}),
            }
        };
        p.send_line(&reply.to_string())?;
    }
    res.exit = p.finish();
    Ok(res)
}

// --- runtimes ----------------------------------------------------------------------------------

/// One request line out, one reply line in: what the broker loop needs from either runtime.
trait AppIo {
    fn recv_line(&mut self, timeout: Duration) -> Option<String>;
    fn send_line(&mut self, line: &str) -> Result<()>;
    fn finish(&mut self) -> Option<i32>;
}

impl AppIo for platform::Proc {
    fn recv_line(&mut self, timeout: Duration) -> Option<String> {
        platform::Proc::recv_line(self, timeout)
    }
    fn send_line(&mut self, line: &str) -> Result<()> {
        platform::Proc::send_line(self, line)
    }
    fn finish(&mut self) -> Option<i32> {
        platform::Proc::finish(self)
    }
}

/// A wasm module on its own thread. Its two host functions block on the broker, like a process on a pipe.
struct WasmApp {
    to_app: Option<Sender<String>>,
    from_app: Receiver<String>,
    exit: Receiver<i32>,
}

struct WasmHost {
    to_broker: Sender<String>,
    from_broker: Receiver<String>,
    reply: Vec<u8>,
    limits: wasmi::StoreLimits,
}

fn memory(c: &wasmi::Caller<WasmHost>) -> Result<wasmi::Memory, wasmi::Error> {
    c.get_export("memory").and_then(wasmi::Extern::into_memory).ok_or_else(|| wasmi::Error::new("module exports no memory"))
}

impl WasmApp {
    /// Module ABI: export `memory` and `run()`; import `axiom.call(ptr, len) -> reply_len` (request JSON in
    /// memory) and `axiom.reply(ptr)` (copies the last reply to `ptr`).
    fn spawn(bytes: &[u8]) -> Result<WasmApp> {
        let mut cfg = wasmi::Config::default();
        cfg.consume_fuel(true);
        let engine = wasmi::Engine::new(&cfg);
        let module = wasmi::Module::new(&engine, bytes).map_err(|e| anyhow!("invalid wasm module: {e}"))?;
        let ((to_broker, from_app), (to_app, from_broker), (exit_tx, exit)) = (channel(), channel(), channel());
        // the interpreter recurses on its native stack; the 2 MiB thread default is not enough
        std::thread::Builder::new().stack_size(16 << 20).spawn(move || {
            let limits = wasmi::StoreLimitsBuilder::new().memory_size(WASM_MEMORY).build();
            let host = WasmHost { to_broker, from_broker, reply: vec![], limits };
            let _ = exit_tx.send(if Self::run(&engine, &module, host).is_ok() { 0 } else { 1 });
        })?;
        Ok(WasmApp { to_app: Some(to_app), from_app, exit })
    }

    fn run(engine: &wasmi::Engine, module: &wasmi::Module, host: WasmHost) -> Result<(), wasmi::Error> {
        let mut store = wasmi::Store::new(engine, host);
        store.limiter(|h| &mut h.limits);
        store.set_fuel(WASM_FUEL)?;
        let mut linker = wasmi::Linker::<WasmHost>::new(engine);
        linker.func_wrap("axiom", "call", |mut c: wasmi::Caller<WasmHost>, ptr: u32, len: u32| -> Result<u32, wasmi::Error> {
            let mut req = vec![0u8; len as usize];
            memory(&c)?.read(&c, ptr as usize, &mut req).map_err(|e| wasmi::Error::new(e.to_string()))?;
            let req = String::from_utf8(req).map_err(|_| wasmi::Error::new("request is not UTF-8"))?;
            c.data().to_broker.send(req.replace('\n', " ")).map_err(|_| wasmi::Error::new("broker gone"))?;
            let reply = c.data().from_broker.recv().map_err(|_| wasmi::Error::new("broker gone"))?;
            c.data_mut().reply = reply.into_bytes();
            Ok(c.data().reply.len() as u32)
        }).map_err(|e| wasmi::Error::new(e.to_string()))?;
        linker.func_wrap("axiom", "reply", |mut c: wasmi::Caller<WasmHost>, ptr: u32| -> Result<(), wasmi::Error> {
            let reply = std::mem::take(&mut c.data_mut().reply);
            memory(&c)?.write(&mut c, ptr as usize, &reply).map_err(|e| wasmi::Error::new(e.to_string()))
        }).map_err(|e| wasmi::Error::new(e.to_string()))?;
        let instance = linker.instantiate_and_start(&mut store, module)?;
        instance.get_typed_func::<(), ()>(&store, "run")?.call(&mut store, ())
    }
}

impl AppIo for WasmApp {
    fn recv_line(&mut self, timeout: Duration) -> Option<String> {
        self.from_app.recv_timeout(timeout).ok()
    }
    fn send_line(&mut self, line: &str) -> Result<()> {
        self.to_app.as_ref().ok_or_else(|| anyhow!("app finished"))?.send(line.to_string()).map_err(|_| anyhow!("app exited"))
    }
    /// Dropping the reply channel traps a module blocked in `call`; a busy one stops when its fuel runs out.
    fn finish(&mut self) -> Option<i32> {
        self.to_app = None;
        self.exit.recv_timeout(Duration::from_secs(2)).ok()
    }
}
