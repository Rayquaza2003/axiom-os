//! SDK for Axiom apps. `call` works the same in a native app (JSON lines over stdin/stdout, so the app
//! must not print anything else to stdout) and in a wasm32 app (the `axiom.call` / `axiom.reply` imports;
//! export `#[unsafe(no_mangle)] pub extern "C" fn run()`).
//!
//! Every call is checked against the capabilities the user granted at install; a denial comes back as `Err`.
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn call(name: &str, args: Value) -> Result<Value, String> {
    let req = json!({"id": NEXT_ID.fetch_add(1, Ordering::Relaxed), "call": name, "args": args});
    parse_reply(&transport(&req.to_string())?)
}

pub fn log(msg: &str) {
    let _ = call("log", json!({"msg": msg}));
}

/// Needs `search` (or `search:tag=<t>`). Returns `[{id, title, score, text}]`.
pub fn search(q: &str) -> Result<Value, String> {
    call("search", json!({"q": q}))
}

/// Needs `tag` (or `tag:tag=<t>`). `obj` is a path or id prefix.
pub fn tag(obj: &str, tag: &str) -> Result<Value, String> {
    call("tag", json!({"obj": obj, "tag": tag}))
}

/// Needs `generate` (expires an hour after it was granted).
pub fn generate(prompt: &str) -> Result<String, String> {
    call("generate", json!({"prompt": prompt})).map(|v| v.as_str().unwrap_or("").to_string())
}

/// Needs `egress:<host>`, and the egress firewall must also approve.
pub fn fetch(url: &str, payload: Value) -> Result<Value, String> {
    call("fetch", json!({"url": url, "payload": payload}))
}

fn parse_reply(line: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("bad reply from broker: {e}"))?;
    match v.get("error") {
        Some(e) => Err(e.as_str().unwrap_or("error").to_string()),
        None => Ok(v["ok"].clone()),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn transport(req: &str) -> Result<String, String> {
    use std::io::{BufRead, Write};
    let mut out = std::io::stdout().lock();
    writeln!(out, "{req}").and_then(|_| out.flush()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) => Err("broker closed the connection".into()),
        Ok(_) => Ok(line),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(target_arch = "wasm32")]
fn transport(req: &str) -> Result<String, String> {
    #[link(wasm_import_module = "axiom")]
    unsafe extern "C" {
        fn call(ptr: *const u8, len: u32) -> u32;
        fn reply(ptr: *mut u8);
    }
    // SAFETY: the host reads `len` bytes at `ptr`, then writes exactly the returned length into `buf`.
    let buf = unsafe {
        let n = call(req.as_ptr(), req.len() as u32);
        let mut buf = vec![0u8; n as usize];
        reply(buf.as_mut_ptr());
        buf
    };
    String::from_utf8(buf).map_err(|_| "reply is not UTF-8".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn replies_map_to_results() {
        assert_eq!(super::parse_reply(r#"{"id":1,"ok":[1,2]}"#), Ok(serde_json::json!([1, 2])));
        assert_eq!(super::parse_reply(r#"{"id":1,"error":"capability denied: search"}"#), Err("capability denied: search".into()));
        assert!(super::parse_reply("garbage").is_err());
    }
}
