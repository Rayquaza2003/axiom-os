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

pub fn remove_dir_all(p: &Path) -> Result<()> {
    if p.exists() {
        std::fs::remove_dir_all(p)?;
    }
    Ok(())
}

/// Recursive copy that skips symlinks (never follows a link out of the source tree).
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    makedirs(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let ft = e.file_type()?;
        if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// --- randomness -------------------------------------------------------------

/// Cryptographic random bytes from the OS. ponytail: /dev/urandom (unix only); getrandom crate if a non-unix host appears.
pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    use std::io::Read;
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

pub fn write_secret(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    Ok(f.write_all(bytes)?)
}

pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    Ok(std::fs::read(path)?)
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}

// --- sockets ----------------------------------------------------------------

const NET_TIMEOUT: Duration = Duration::from_secs(30);

/// Length-prefixed message stream over TCP.
pub struct Conn(std::net::TcpStream);

pub struct Listener(std::net::TcpListener);

fn conn(s: std::net::TcpStream) -> Result<Conn> {
    s.set_read_timeout(Some(NET_TIMEOUT))?;
    s.set_write_timeout(Some(NET_TIMEOUT))?;
    Ok(Conn(s))
}

pub fn listen(addr: &str) -> Result<Listener> {
    Ok(Listener(std::net::TcpListener::bind(addr)?))
}

pub fn dial(addr: &str) -> Result<Conn> {
    conn(std::net::TcpStream::connect(addr)?)
}

impl Listener {
    pub fn local_addr(&self) -> Result<String> {
        Ok(self.0.local_addr()?.to_string())
    }

    pub fn accept(&self) -> Result<Conn> {
        conn(self.0.accept()?.0)
    }
}

impl Conn {
    pub fn send(&mut self, msg: &[u8]) -> Result<()> {
        use std::io::Write;
        self.0.write_all(&(msg.len() as u32).to_be_bytes())?;
        Ok(self.0.write_all(msg)?)
    }

    /// Reads one message; rejects anything larger than `max` bytes before allocating.
    pub fn recv(&mut self, max: usize) -> Result<Vec<u8>> {
        use std::io::Read;
        let mut len = [0u8; 4];
        self.0.read_exact(&mut len)?;
        let n = u32::from_be_bytes(len) as usize;
        if n > max {
            return Err(anyhow!("message of {n} bytes exceeds limit {max}"));
        }
        let mut b = vec![0u8; n];
        self.0.read_exact(&mut b)?;
        Ok(b)
    }
}

// --- sandboxed child processes ------------------------------------------------

/// A child process spoken to in newline-delimited text. Killed on drop.
pub struct Proc {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
}

fn sbpath(p: &Path) -> String {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")
}

/// Run `cmd` (working directory `cwd`) under the OS sandbox: no network, no file writes, no reads of
/// `hidden` except `cwd` itself.
///
/// ponytail: deny-list profile (macOS sandbox-exec). A full allow-list needs per-interpreter paths;
/// fails closed when the host has no sandbox, never runs the app unconfined.
pub fn spawn_sandboxed(cmd: &[String], cwd: &Path, hidden: &Path) -> Result<Proc> {
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    if !Path::new("/usr/bin/sandbox-exec").exists() {
        return Err(anyhow!("no OS sandbox available on this host (refusing to run app unconfined)"));
    }
    let profile = format!(
        "(version 1)(allow default)(deny network*)(deny file-write*)\
         (allow file-write* (literal \"/dev/null\") (literal \"/dev/tty\"))\
         (deny file-read* (subpath \"{}\"))(allow file-read* (subpath \"{}\"))",
        sbpath(hidden), sbpath(cwd)
    );
    let mut child = Command::new("/usr/bin/sandbox-exec")
        .arg("-p").arg(profile).args(cmd).current_dir(cwd)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
    let out = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let (tx, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for l in std::io::BufReader::new(out).lines().map_while(|l| l.ok()) {
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    Ok(Proc { child, stdin, lines })
}

impl Proc {
    pub fn send_line(&mut self, line: &str) -> Result<()> {
        use std::io::Write;
        writeln!(self.stdin, "{line}")?;
        Ok(self.stdin.flush()?)
    }

    /// `None` on EOF (app exited or closed stdout) or timeout.
    pub fn recv_line(&mut self, timeout: Duration) -> Option<String> {
        self.lines.recv_timeout(timeout).ok()
    }

    /// Exit code once the child has stopped; a child still running after ~2s is killed (`None`).
    pub fn finish(&mut self) -> Option<i32> {
        for _ in 0..20 {
            if let Ok(Some(s)) = self.child.try_wait() {
                return s.code();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        self.child.wait().ok().and_then(|s| s.code())
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
