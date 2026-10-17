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
//! Model-context capabilities (`generate`) expire after `CAP_TTL_S`; the rest last until revoked.
//!
//! ponytail: capability = coarse name, no per-object scoping; native/WASM SDKs and a signed app
//! store are not built. Add scoping (e.g. `search:tag=finance`) when a real third-party app needs it.
use crate::Axiom;
use anyhow::{anyhow, bail, Result};
use core_ai::egress::host_key;
use core_ai::{execute, BACKGROUND};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// (capability, seconds a grant lives). Decision P0.3: short TTL for model-context caps.
const CAP_TTL_S: &[(&str, f64)] = &[("generate", 3600.0)];

const SCHEMA: &str = "
create table if not exists grants(app text, cap text, ts real, primary key(app, cap));
create table if not exists calls(ts real, app text, call text, decision text);
";

pub struct Manifest {
    pub name: String,
    pub cmd: Vec<String>,
    pub requires: Vec<String>,
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
    matches!(c, "search" | "tag" | "generate") || c.strip_prefix("egress:").is_some_and(|h| !h.is_empty() && !h.contains('/'))
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
    let (cmd, requires) = (strs("cmd")?, strs("requires")?);
    if cmd.is_empty() {
        bail!("manifest.cmd is empty");
    }
    if let Some(bad) = requires.iter().find(|c| !valid_cap(c)) {
        bail!("unknown capability {bad:?}");
    }
    Ok(Manifest { name, cmd, requires })
}

/// Copy the app in and ask `approve(app, cap)` for each required capability. Re-installing resets grants.
pub fn install(ax: &Axiom, src: &Path, approve: &dyn Fn(&str, &str) -> bool) -> Result<(String, Vec<String>)> {
    let m = read_manifest(src)?;
    let dest = app_dir(&ax.home, &m.name);
    platform::remove_dir_all(&dest)?;
    platform::copy_tree(src, &dest)?;
    let c = db(&ax.home)?;
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

/// (app, granted, required)
pub fn list(home: &Path) -> Result<Vec<(String, Vec<String>, Vec<String>)>> {
    let mut out = vec![];
    for d in platform::list_dir(&home.join("apps")) {
        if let Ok(m) = read_manifest(&d) {
            out.push((m.name.clone(), granted(home, &m.name)?, m.requires));
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
    let granted_at: Option<f64> = db(&ax.home).map_err(e)?
        .query_row("select ts from grants where app=? and cap=?", [app, &need], |r| r.get(0)).ok();
    let Some(granted_at) = granted_at else { return Err(format!("capability denied: {need}")) };
    if CAP_TTL_S.iter().any(|(c, ttl)| *c == need && now - granted_at > *ttl) {
        return Err(format!("capability expired: {need} (reinstall to grant again)"));
    }
    let s = |k: &str| args[k].as_str().ok_or_else(|| format!("missing string arg {k:?}"));
    match name {
        "search" => {
            let r = execute(&ax.store, &ax.nrm, s("q")?, now, 5).map_err(e)?;
            Ok(json!(r.hits.iter().map(|h| json!({
                "id": h.obj, "title": ax.store.title(&h.obj), "score": h.score,
                "text": h.text.chars().take(300).collect::<String>(),
            })).collect::<Vec<_>>()))
        }
        "tag" => {
            let id = ax.store.resolve(s("obj")?).map_err(e)?;
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
    let mut p = platform::spawn_sandboxed(&[m.cmd, args.to_vec()].concat(), &dir, &ax.home)?;
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
