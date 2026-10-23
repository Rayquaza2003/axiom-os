//! axiomd: AI as a system resource. One daemon per user owns the model and the NRM. Axiom processes
//! reach it over a unix socket (mode 0600: this user only); other programs over an OpenAI-compatible
//! HTTP API, each with its own API key.
//!
//! One scheduler for the machine: a background import in one process and a question asked in another
//! share one priority queue, and the model is loaded once. `Axiom::open_default` uses the daemon when it
//! runs and falls back to in-process inference when it does not. The store stays in each process
//! (SQLite coordinates them); only inference goes through here.
//!
//! Socket protocol: one length-prefixed JSON request per connection, `{"op": "info" | "status" | "embed" |
//! "generate", "tier": 0..=2, ...}`, answered with `{"ok": ...}` or `{"error": "..."}`.
//! API keys: only a hash is stored (`daemon.db`); a key reaches inference, never the user's data.
use crate::{apps, AGING_S};
use anyhow::{anyhow, bail, Result};
use core_ai::{NpuBackend, Nrm, BACKGROUND, INTERACTIVE, REALTIME};
use platform::{Conn, HttpRequest, LocalConn};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const SOCK: &str = "axiomd.sock";
/// launchd label of the login service.
pub const LABEL: &str = "dev.axiom.axiomd";
const HTTP_DEFAULT: &str = "127.0.0.1:7779";
const MAX_MSG: usize = 64 << 20;
const MAX_BODY: usize = 32 << 20;
/// The one model the HTTP API lists. Requests may name any model; axiomd serves the one it was started with.
const MODEL: &str = "axiom";

/// Address of the OpenAI-compatible API: `AXIOM_HTTP`, default 127.0.0.1:7779.
pub fn http_addr() -> String {
    std::env::var("AXIOM_HTTP").unwrap_or_else(|_| HTTP_DEFAULT.into())
}

/// Nearest-rank percentile (`p` in 0..=1); 0 for no samples.
pub fn percentile(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[((p * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1]
}

/// "420 ms" below a second, "2.31 s" from there.
pub fn fmt_secs(s: f64) -> String {
    if s < 1.0 { format!("{:.0} ms", s * 1e3) } else { format!("{s:.2} s") }
}

// --- server ------------------------------------------------------------------------------------

struct Daemon {
    home: PathBuf,
    nrm: Nrm,
    started: f64,
    http: Option<String>,
    /// Per client ("local" = Axiom processes; others = API key names): requests, and seconds spent on them.
    clients: Mutex<BTreeMap<String, (u64, f64)>>,
}

/// Serves `home` on its unix socket, and on `http` the OpenAI-compatible API. Listeners run on their own
/// threads. Returns the bound HTTP address. Fails if a daemon already serves `home`.
pub fn start(home: &Path, backend: Arc<dyn NpuBackend>, http: Option<&str>) -> Result<Option<String>> {
    let local = platform::listen_local(&home.join(SOCK))?;
    // a busy port must not take the socket (and every Axiom process) down with it
    let http = http.and_then(|a| platform::listen(a).map_err(|e| eprintln!("axiomd: no HTTP API on {a}: {e}")).ok());
    let d = Arc::new(Daemon {
        home: home.to_path_buf(),
        nrm: Nrm::new(backend, AGING_S),
        started: platform::now(),
        http: http.as_ref().map(|l| l.local_addr()).transpose()?,
        clients: Mutex::default(),
    });
    let addr = d.http.clone();
    accept_loop(d.clone(), move || local.accept(), Daemon::serve_local);
    if let Some(l) = http {
        accept_loop(d, move || l.accept(), Daemon::serve_http);
    }
    Ok(addr)
}

/// ponytail: one thread per connection; a pool if many clients connect at once.
fn accept_loop<C: Send + 'static>(d: Arc<Daemon>, accept: impl Fn() -> Result<C> + Send + 'static, serve: fn(&Daemon, C)) {
    std::thread::spawn(move || loop {
        match accept() {
            Ok(c) => {
                let d = d.clone();
                std::thread::spawn(move || serve(&d, c));
            }
            Err(e) => {
                eprintln!("axiomd: accept: {e}");
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    });
}

/// (status, content type, body)
type Reply = (u16, &'static str, Vec<u8>);

fn ok(v: Value) -> Reply {
    (200, "application/json", v.to_string().into_bytes())
}

fn error(status: u16, msg: &str) -> Reply {
    (status, "application/json", json!({"error": {"message": msg, "type": "axiom"}}).to_string().into_bytes())
}

impl Daemon {
    fn serve_local(&self, mut c: LocalConn) {
        let r = c.recv(MAX_MSG).and_then(|b| Ok(serde_json::from_slice::<Value>(&b)?)).and_then(|req| self.op(&req));
        let reply = match r {
            Ok(v) => json!({"ok": v}),
            Err(e) => json!({"error": e.to_string()}),
        };
        let _ = c.send(reply.to_string().as_bytes());
    }

    fn op(&self, req: &Value) -> Result<Value> {
        let tier = req["tier"].as_u64().map_or(INTERACTIVE, |t| t.min(BACKGROUND as u64) as u8);
        let b = &self.nrm.backend;
        Ok(match req["op"].as_str().unwrap_or("") {
            "info" => json!({"dim": b.dim(), "llm": b.has_llm()}),
            "status" => self.status(),
            "embed" => json!(self.embed("local", tier, serde_json::from_value(req["texts"].clone())?)?),
            "generate" => json!(self.generate("local", tier, req["prompt"].as_str().ok_or_else(|| anyhow!("prompt required"))?)?),
            op => bail!("unknown op {op:?}"),
        })
    }

    fn embed(&self, client: &str, tier: u8, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.timed(client, || self.nrm.embed(tier, texts))
    }

    fn generate(&self, client: &str, tier: u8, prompt: &str) -> Result<String> {
        self.timed(client, || self.nrm.generate(tier, prompt.to_string()))
    }

    fn timed<T>(&self, client: &str, f: impl FnOnce() -> T) -> T {
        let t = platform::now();
        let r = f();
        let mut clients = self.clients.lock().unwrap();
        let c = clients.entry(client.to_string()).or_default();
        *c = (c.0 + 1, c.1 + platform::now() - t);
        r
    }

    fn status(&self) -> Value {
        let [rt, ia, bg] = self.nrm.queued();
        let recent = self.nrm.recent();
        let tier = |t: u8| {
            let mut waits: Vec<f64> = recent.iter().filter(|j| j.0 == t).map(|j| j.1).collect();
            json!({
                "jobs": waits.len(),
                "wait_p50": percentile(&mut waits, 0.5),
                "wait_p95": percentile(&mut waits, 0.95),
                "run_s": recent.iter().filter(|j| j.0 == t).map(|j| j.2).sum::<f64>(),
            })
        };
        let clients: serde_json::Map<String, Value> = self.clients.lock().unwrap().iter()
            .map(|(k, (n, s))| (k.clone(), json!({"requests": n, "seconds": s}))).collect();
        json!({
            "uptime_s": platform::now() - self.started,
            "dim": self.nrm.backend.dim(),
            "llm": self.nrm.backend.has_llm(),
            "http": self.http,
            "queued": {"realtime": rt, "interactive": ia, "background": bg},
            "recent": {"realtime": tier(REALTIME), "interactive": tier(INTERACTIVE), "background": tier(BACKGROUND)},
            "clients": clients,
        })
    }

    fn serve_http(&self, mut c: Conn) {
        let reply = match c.http_request(MAX_BODY) {
            Ok(req) => self.route(&req, &mut c),
            Err(e) => Some(error(400, &e.to_string())),
        };
        // None: the handler streamed its own response
        let Some((status, ctype, body)) = reply else { return };
        if status >= 500 {
            eprintln!("axiomd: HTTP {status}: {}", String::from_utf8_lossy(&body));
        }
        let _ = c.http_respond(status, ctype, &body);
    }

    fn route(&self, req: &HttpRequest, c: &mut Conn) -> Option<Reply> {
        let key = req.header("authorization").and_then(|a| a.strip_prefix("Bearer ")).map(str::trim);
        let Some(client) = key.and_then(|k| key_owner(&self.home, k)) else {
            return Some(error(401, "missing or unknown API key (create one with `axiom daemon key NAME`)"));
        };
        let body: Value = match req.body.is_empty() {
            true => Value::Null,
            false => match serde_json::from_slice(&req.body) {
                Ok(v) => v,
                Err(e) => return Some(error(400, &format!("body is not JSON: {e}"))),
            },
        };
        match (req.method.as_str(), req.path.split('?').next().unwrap_or("")) {
            ("GET", "/v1/models") => Some(ok(json!({"object": "list", "data": [{"id": MODEL, "object": "model", "owned_by": "axiom"}]}))),
            ("POST", "/v1/embeddings") => Some(self.embeddings(&client, &body)),
            ("POST", "/v1/chat/completions") => self.chat(&client, &body, c),
            (m, p) => Some(error(404, &format!("no route {m} {p}"))),
        }
    }

    // ponytail: every API client runs at INTERACTIVE; a per-key tier when a bulk client shows up.
    fn embeddings(&self, client: &str, body: &Value) -> Reply {
        let texts: Vec<String> = match &body["input"] {
            Value::String(s) => vec![s.clone()],
            v => match serde_json::from_value(v.clone()) {
                Ok(t) => t,
                Err(_) => return error(400, "input must be a string or a list of strings"),
            },
        };
        match self.embed(client, INTERACTIVE, texts) {
            Ok(v) => ok(json!({
                "object": "list",
                "model": MODEL,
                "data": v.into_iter().enumerate().map(|(i, e)| json!({"object": "embedding", "index": i, "embedding": e})).collect::<Vec<_>>(),
                "usage": {"prompt_tokens": 0, "total_tokens": 0},
            })),
            Err(e) => error(500, &e.to_string()),
        }
    }

    /// `None` when it streamed the answer itself (`stream: true`).
    fn chat(&self, client: &str, body: &Value, c: &mut Conn) -> Option<Reply> {
        if !self.nrm.backend.has_llm() {
            return Some(error(503, "no LLM configured (start axiomd with AXIOM_LLM_MODEL set)"));
        }
        let Some(prompt) = prompt_of(&body["messages"]) else {
            return Some(error(400, "messages must be a non-empty list of {role, content}"));
        };
        let (id, created) = (format!("chatcmpl-{}", (platform::now() * 1e6) as u64), platform::now() as i64);
        if body["stream"] != true {
            return Some(match self.generate(client, INTERACTIVE, &prompt) {
                Ok(text) => ok(json!({"id": id, "object": "chat.completion", "created": created, "model": MODEL,
                                      "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}]})),
                Err(e) => error(500, &e.to_string()),
            });
        }
        // pieces go out as the model makes them; a client that hangs up cancels the generation
        let event = |delta: Value, finish: Value| format!("data: {}\n\n", json!({
            "id": id, "object": "chat.completion.chunk", "created": created, "model": MODEL,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        }));
        c.http_start(200, "text/event-stream").ok()?;
        c.http_chunk(event(json!({"role": "assistant", "content": ""}), Value::Null).as_bytes()).ok()?;
        let r = self.timed(client, || {
            self.nrm.generate_with(INTERACTIVE, prompt, &mut |piece| c.http_chunk(event(json!({"content": piece}), Value::Null).as_bytes()).is_ok())
        });
        let end = match r {
            Ok(_) => event(json!({}), json!("stop")),
            Err(e) => format!("data: {}\n\n", json!({"error": {"message": e.to_string(), "type": "axiom"}})),
        };
        let _ = c.http_chunk(format!("{end}data: [DONE]\n\n").as_bytes());
        let _ = c.http_chunk(b"");
        None
    }
}

/// One prompt from a chat. A lone message passes through as is.
/// ponytail: the backend takes one prompt, so roles become text and the server's chat template sees a
/// single user turn; pass messages through once a backend takes them.
fn prompt_of(messages: &Value) -> Option<String> {
    let msgs = messages.as_array().filter(|m| !m.is_empty())?;
    let text = |c: &Value| match c {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => Some(parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n")),
        _ => None,
    };
    if let [m] = msgs.as_slice() {
        return text(&m["content"]);
    }
    let turns = msgs.iter().map(|m| Some(format!("{}: {}", m["role"].as_str()?, text(&m["content"])?))).collect::<Option<Vec<_>>>()?;
    Some(turns.join("\n\n") + "\n\nassistant:")
}

// --- API keys ------------------------------------------------------------------------------------

fn db(home: &Path) -> Result<Connection> {
    let c = Connection::open(home.join("daemon.db"))?;
    c.execute_batch("create table if not exists keys(client text primary key, hash text not null, ts real)")?;
    Ok(c)
}

fn key_hash(key: &str) -> String {
    blake3::hash(key.as_bytes()).to_hex().to_string()
}

/// A new API key for `client`, replacing its old one. Only the hash is kept, so the key is shown once.
pub fn issue_key(home: &Path, client: &str) -> Result<String> {
    if client == "local" || client.is_empty() || !client.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
        bail!("client name must be letters, digits, '-', '_' or '.' (and not \"local\")");
    }
    let key = format!("ax-{}", apps::hex(&platform::random_bytes::<32>()?));
    db(home)?.execute("insert or replace into keys values(?,?,?)", params![client, key_hash(&key), platform::now()])?;
    Ok(key)
}

/// `false` if `client` had no key.
pub fn revoke_key(home: &Path, client: &str) -> Result<bool> {
    Ok(db(home)?.execute("delete from keys where client=?", [client])? > 0)
}

/// Read per request, so `key` and `revoke` act without a restart.
fn key_owner(home: &Path, key: &str) -> Option<String> {
    db(home).ok()?.query_row("select client from keys where hash=?", [key_hash(key)], |r| r.get(0)).ok()
}

// --- client ----------------------------------------------------------------------------------------

/// This process's handle on axiomd: embed and generate run there, at the caller's NRM tier.
pub struct DaemonBackend {
    sock: PathBuf,
    dim: usize,
    llm: bool,
}

fn request(mut c: LocalConn, req: &Value) -> Result<Value> {
    c.send(req.to_string().as_bytes())?;
    let mut reply: Value = serde_json::from_slice(&c.recv(MAX_MSG)?)?;
    match reply["error"].as_str() {
        Some(e) => Err(anyhow!("axiomd: {e}")),
        None => Ok(reply["ok"].take()),
    }
}

/// The daemon serving `home`, or `None` when none is running.
pub fn connect(home: &Path) -> Result<Option<DaemonBackend>> {
    let sock = home.join(SOCK);
    let Ok(c) = platform::dial_local(&sock) else { return Ok(None) };
    let info = request(c, &json!({"op": "info"}))?;
    let dim = info["dim"].as_u64().ok_or_else(|| anyhow!("axiomd sent no embedding dim"))? as usize;
    Ok(Some(DaemonBackend { sock, dim, llm: info["llm"] == true }))
}

/// `None` when no daemon serves `home`.
pub fn status(home: &Path) -> Result<Option<Value>> {
    let Ok(c) = platform::dial_local(&home.join(SOCK)) else { return Ok(None) };
    request(c, &json!({"op": "status"})).map(Some)
}

impl DaemonBackend {
    fn call(&self, req: Value) -> Result<Value> {
        request(platform::dial_local(&self.sock)?, &req)
    }
}

impl NpuBackend for DaemonBackend {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_at(INTERACTIVE, texts)
    }

    fn has_llm(&self) -> bool {
        self.llm
    }

    fn generate(&self, prompt: &str) -> Result<String> {
        self.generate_at(INTERACTIVE, prompt)
    }

    fn embed_at(&self, tier: u8, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(serde_json::from_value(self.call(json!({"op": "embed", "tier": tier, "texts": texts}))?)?)
    }

    fn generate_at(&self, tier: u8, prompt: &str) -> Result<String> {
        Ok(serde_json::from_value(self.call(json!({"op": "generate", "tier": tier, "prompt": prompt}))?)?)
    }
}
