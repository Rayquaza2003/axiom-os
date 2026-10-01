//! All OS access lives here. Core crates call this, never `std::fs`, `std::process` or sockets directly.
//!
//! Porting to another kernel = rewrite this crate only.
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn home() -> Result<PathBuf> {
    let p = match std::env::var_os("AXIOM_HOME") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME not set"))?).join(".axiom"),
    };
    makedirs(&p)?;
    Ok(p)
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn makedirs(p: &Path) -> Result<()> {
    Ok(std::fs::create_dir_all(p)?)
}

pub fn abspath(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

pub fn copy(src: &Path, dst: &Path) -> Result<()> {
    std::fs::copy(src, dst)?;
    Ok(())
}

pub fn list_dir(p: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
    v.sort();
    v
}

pub struct Entry {
    pub path: PathBuf,
    pub mtime: f64,
}

/// Every file under `root`, skipping dot-directories.
pub fn walk(root: &Path) -> Vec<Entry> {
    let (mut out, mut stack) = (vec![], vec![root.to_path_buf()]);
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                if !e.file_name().to_string_lossy().starts_with('.') {
                    stack.push(p);
                }
            } else if let Ok(m) = md.modified() {
                out.push(Entry { path: p, mtime: m.duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0) });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Raw bytes + text for small UTF-8 files, `None` for anything else.
pub fn read_text(path: &Path) -> Option<(Vec<u8>, String)> {
    const MAX: u64 = 2_000_000;
    if std::fs::metadata(path).ok()?.len() > MAX {
        return None;
    }
    let b = std::fs::read(path).ok()?;
    if b[..b.len().min(1024)].contains(&0) {
        return None;
    }
    let s = String::from_utf8(b.clone()).ok()?;
    Some((b, s))
}

/// Raw transport. Callers decide whether the destination is local or must go through the egress firewall.
pub fn http_json(url: &str, payload: Option<&Value>, timeout_s: u64) -> Result<Value> {
    let req = if payload.is_some() { ureq::post(url) } else { ureq::get(url) }.timeout(Duration::from_secs(timeout_s));
    let resp = match payload {
        Some(p) => req.send_json(p),
        None => req.call(),
    };
    resp.map_err(|e| anyhow!("{url}: {e}"))?.into_json().map_err(|e| anyhow!("{url}: {e}"))
}

/// Text-to-speech through the host. Best effort: silent if no synthesizer exists.
pub fn speak(text: &str) {
    for cmd in ["say", "espeak"] {
        if std::process::Command::new(cmd).arg(text).status().is_ok() {
            return;
        }
    }
}
