//! axiomd gate: one NRM for every process, OpenAI-compatible API behind per-client keys. Run: cargo test
use axiom_shell::{daemon, Axiom};
use core_ai::{LocalBackend, NpuBackend, BACKGROUND, INTERACTIVE, REALTIME};
use serde_json::Value;
use std::io::{Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// Offline embeddings; an LLM that echoes its prompt and records it. The prompt "hold" blocks until released.
struct Echo {
    calls: Mutex<Vec<String>>,
    hold: Mutex<Option<mpsc::Receiver<()>>>,
}

impl NpuBackend for Echo {
    fn dim(&self) -> usize { LocalBackend::offline().dim() }
    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> { LocalBackend::offline().embed(texts) }
    fn has_llm(&self) -> bool { true }
    fn generate(&self, p: &str) -> anyhow::Result<String> {
        if p == "hold" {
            let rx = self.hold.lock().unwrap().take().unwrap();
            rx.recv().unwrap();
        }
        self.calls.lock().unwrap().push(p.to_string());
        Ok(format!("echo: {p}"))
    }
}

/// One raw HTTP exchange: (status, body).
fn http(addr: &str, method: &str, path: &str, key: Option<&str>, body: &str) -> (u16, String) {
    let auth = key.map(|k| format!("Authorization: Bearer {k}\r\n")).unwrap_or_default();
    raw(addr, &format!("{method} {path} HTTP/1.1\r\nHost: x\r\n{auth}Content-Length: {}\r\n\r\n{body}", body.len()))
}

fn raw(addr: &str, req: &str) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    (head[9..12].parse().unwrap(), body.to_string())
}

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

#[test]
fn daemon_shares_one_nrm_and_gates_the_api() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (release, held) = mpsc::channel();
    let echo = Arc::new(Echo { calls: Mutex::new(vec![]), hold: Mutex::new(Some(held)) });
    assert!(daemon::connect(&home).unwrap().is_none(), "no daemon yet: processes run inference themselves");
    let addr = daemon::start(&home, echo.clone(), Some("127.0.0.1:0")).unwrap().unwrap();
    assert!(daemon::start(&home, echo.clone(), None).is_err(), "one daemon per home");

    // an Axiom process sends its inference to the daemon
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("q3-revenue.md"), "Q3 revenue grew 12 percent.").unwrap();
    let client = Arc::new(daemon::connect(&home).unwrap().expect("daemon reachable"));
    assert_eq!((client.dim(), client.has_llm()), (echo.dim(), true));
    let ax = Axiom::open(&home, client.clone(), None, None).unwrap();
    ax.import(&docs, None).unwrap();
    let (r, text) = ax.ask("summarize revenue", platform::now(), None, false).unwrap();
    assert!(!r.hits.is_empty() && text.starts_with("echo: Summarize these documents"), "{text}");

    // tiers cross processes: with the worker busy, a later INTERACTIVE job beats an earlier BACKGROUND one
    echo.calls.lock().unwrap().clear();
    let job = |tier: u8, p: &'static str| {
        let c = daemon::connect(&home).unwrap().unwrap();
        let t = std::thread::spawn(move || c.generate_at(tier, p).unwrap());
        std::thread::sleep(Duration::from_millis(100));
        t
    };
    let jobs = [job(REALTIME, "hold"), job(BACKGROUND, "bg"), job(INTERACTIVE, "ia")];
    let s = daemon::status(&home).unwrap().unwrap();
    assert_eq!((&s["queued"]["background"], &s["queued"]["interactive"]), (&1.into(), &1.into()), "{s}");
    release.send(()).unwrap();
    jobs.into_iter().for_each(|t| drop(t.join().unwrap()));
    assert_eq!(*echo.calls.lock().unwrap(), ["hold", "ia", "bg"]);
    let s = daemon::status(&home).unwrap().unwrap();
    let wait = |t: &str| s["recent"][t]["wait_p95"].as_f64().unwrap();
    assert!(wait("background") > wait("interactive"), "the earlier background job waited longer: {s}");

    // HTTP API: no key, no service
    assert_eq!(http(&addr, "GET", "/v1/models", None, "").0, 401);
    assert_eq!(http(&addr, "GET", "/v1/models", Some("ax-guess"), "").0, 401);
    assert!(daemon::issue_key(&home, "local").is_err(), "\"local\" names Axiom processes");
    let key = daemon::issue_key(&home, "editor").unwrap();
    let k = Some(key.as_str());
    let (st, body) = http(&addr, "GET", "/v1/models", k, "");
    assert_eq!((st, json(&body)["data"][0]["id"].as_str()), (200, Some("axiom")));

    let (st, body) = http(&addr, "POST", "/v1/embeddings", k, r#"{"model":"x","input":["q3 revenue","lisbon"]}"#);
    let v = json(&body);
    assert_eq!((st, v["data"].as_array().unwrap().len()), (200, 2), "{body}");
    assert_eq!(v["data"][1]["embedding"].as_array().unwrap().len(), echo.dim());

    let (st, body) = http(&addr, "POST", "/v1/chat/completions", k, r#"{"model":"x","messages":[{"role":"user","content":"hi"}]}"#);
    assert_eq!((st, json(&body)["choices"][0]["message"]["content"].as_str()), (200, Some("echo: hi")), "{body}");
    let chat = r#"{"messages":[{"role":"system","content":"be brief"},{"role":"user","content":[{"type":"text","text":"hi"}]}],"stream":true}"#;
    let (st, body) = http(&addr, "POST", "/v1/chat/completions", k, chat);
    assert!(st == 200 && body.ends_with("data: [DONE]\n\n"), "{body}");
    let first = json(body.lines().next().unwrap().strip_prefix("data: ").unwrap());
    assert_eq!(first["choices"][0]["delta"]["content"], "echo: system: be brief\n\nuser: hi\n\nassistant:");

    assert_eq!(http(&addr, "POST", "/v1/chat/completions", k, r#"{"messages":[]}"#).0, 400);
    assert_eq!(http(&addr, "POST", "/v1/embeddings", k, "not json").0, 400);
    assert_eq!(http(&addr, "GET", "/v1/nothing", k, "").0, 404);
    assert_eq!(raw(&addr, "garbage\r\n\r\n").0, 400);
    assert_eq!(raw(&addr, "POST /v1/embeddings HTTP/1.1\r\nContent-Length: 999999999999\r\n\r\n").0, 400);

    let s = daemon::status(&home).unwrap().unwrap();
    assert_eq!(s["clients"]["editor"]["requests"], 3, "{s}");
    assert!(s["clients"]["local"]["requests"].as_u64().unwrap() >= 5 && s["clients"]["local"]["seconds"].as_f64().unwrap() > 0.0, "{s}");

    // revoking a key bites on the next request
    assert!(daemon::revoke_key(&home, "editor").unwrap());
    assert_eq!(http(&addr, "GET", "/v1/models", k, "").0, 401);
}
