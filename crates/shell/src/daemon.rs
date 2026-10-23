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
use core_ai::{NpuBackend, Nrm, BACKGROUND, INTERACTIVE};
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

// --- server ------------------------------------------------------------------------------------

struct Daemon {
    home: PathBuf,
    nrm: Nrm,
    started: f64,
    http: Option<String>,
    /// Requests served per client ("local" = Axiom processes; others = API key names).
    requests: Mutex<BTreeMap<String, u64>>,
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
        requests: Mutex::default(),
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
        self.count(client);
        self.nrm.embed(tier, texts)
    }

    fn generate(&self, client: &str, tier: u8, prompt: &str) -> Result<String> {
        self.count(client);
        self.nrm.generate(tier, prompt.to_string())
    }

    fn count(&self, client: &str) {
        *self.requests.lock().unwrap().entry(client.to_string()).or_default() += 1;
    }

    fn status(&self) -> Value {
        let [rt, ia, bg] = self.nrm.queued();
        json!({
            "uptime_s": platform::now() - self.started,
            "dim": self.nrm.backend.dim(),
            "llm": self.nrm.backend.has_llm(),
            "http": self.http,
            "queued": {"realtime": rt, "interactive": ia, "background": bg},
            "requests": self.requests.lock().unwrap().clone(),
        })
    }

    fn serve_http(&self, mut c: Conn) {
        let (status, ctype, body) = match c.http_request(MAX_BODY) {
            Ok(req) => self.route(&req),
            Err(e) => error(400, &e.to_string()),
        };
        if status >= 500 {
            eprintln!("axiomd: HTTP {status}: {}", String::from_utf8_lossy(&body));
        }
        let _ = c.http_respond(status, ctype, &body);
    }

    fn route(&self, req: &HttpRequest) -> Reply {
        let key = req.header("authorization").and_then(|a| a.strip_prefix("Bearer ")).map(str::trim);
        let Some(client) = key.and_then(|k| key_owner(&self.home, k)) else {
            return error(401, "missing or unknown API key (create one with `axiom daemon key NAME`)");
        };
        let body: Value = match req.body.is_empty() {
            true => Value::Null,
            false => match serde_json::from_slice(&req.body) {
                Ok(v) => v,
                Err(e) => return error(400, &format!("body is not JSON: {e}")),
            },
        };
        match (req.method.as_str(), req.path.split('?').next().unwrap_or("")) {
            ("GET", "/v1/models") => ok(json!({"object": "list", "data": [{"id": MODEL, "object": "model", "owned_by": "axiom"}]})),
            ("POST", "/v1/embeddings") => self.embeddings(&client, &body),
            ("POST", "/v1/chat/completions") => self.chat(&client, &body),
            (m, p) => error(404, &format!("no route {m} {p}")),
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

    fn chat(&self, client: &str, body: &Value) -> Reply {
        if !self.nrm.backend.has_llm() {
            return error(503, "no LLM configured (start axiomd with AXIOM_LLM_MODEL set)");
        }
        let Some(prompt) = prompt_of(&body["messages"]) else {
            return error(400, "messages must be a non-empty list of {role, content}");
        };
        let text = match self.generate(client, INTERACTIVE, &prompt) {
            Ok(t) => t,
            Err(e) => return error(500, &e.to_string()),
        };
        let (id, created) = (format!("chatcmpl-{}", (platform::now() * 1e6) as u64), platform::now() as i64);
        let msg = json!({"role": "assistant", "content": text});
        if body["stream"] != true {
            return ok(json!({"id": id, "object": "chat.completion", "created": created, "model": MODEL,
                             "choices": [{"index": 0, "message": msg, "finish_reason": "stop"}]}));
        }
        // ponytail: the whole answer is one chunk; real token streaming needs a streaming backend
        let chunk = |delta: Value, finish: Value| format!("data: {}\n\n", json!({
            "id": id, "object": "chat.completion.chunk", "created": created, "model": MODEL,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        }));
        let sse = chunk(msg, Value::Null) + &chunk(json!({}), json!("stop")) + "data: [DONE]\n\n";
        (200, "text/event-stream", sse.into_bytes())
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
