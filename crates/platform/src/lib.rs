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

/// POSTs `payload` and hands each line of the response to `line` as it arrives, until `line` returns false
/// or the response ends. Stopping early closes the connection, which tells the server to stop as well.
pub fn http_lines(url: &str, payload: &Value, timeout_s: u64, line: &mut dyn FnMut(&str) -> bool) -> Result<()> {
    use std::io::BufRead;
    let resp = ureq::post(url).timeout(Duration::from_secs(timeout_s)).send_json(payload).map_err(|e| anyhow!("{url}: {e}"))?;
    for l in std::io::BufReader::new(resp.into_reader()).lines() {
        if !line(&l.map_err(|e| anyhow!("{url}: {e}"))?) {
            break;
        }
    }
    Ok(())
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

pub fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    Ok(std::fs::write(path, bytes)?)
}

pub fn rename(from: &Path, to: &Path) -> Result<()> {
    Ok(std::fs::rename(from, to)?)
}

/// Every regular file under `root`, dot-entries included, symlinks skipped (what `copy_tree` copies).
pub fn walk_all(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = vec![];
    for e in std::fs::read_dir(root)?.flatten() {
        let ft = e.file_type()?;
        if ft.is_dir() {
            out.extend(walk_all(&e.path())?);
        } else if ft.is_file() {
            out.push(e.path());
        }
    }
    out.sort();
    Ok(out)
}

pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    Ok(std::fs::read(path)?)
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}

// --- sockets ----------------------------------------------------------------

const NET_TIMEOUT: Duration = Duration::from_secs(30);

/// TCP stream: length-prefixed messages (mesh), or one HTTP request and response (the axiomd API).
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

/// Connects to the first reachable address `addr` resolves to, 5s per attempt.
pub fn dial(addr: &str) -> Result<Conn> {
    use std::net::ToSocketAddrs;
    let mut last = anyhow!("{addr}: resolves to no address");
    for a in addr.to_socket_addrs()? {
        match std::net::TcpStream::connect_timeout(&a, Duration::from_secs(5)) {
            Ok(s) => return conn(s),
            Err(e) => last = anyhow!("{addr}: {e}"),
        }
    }
    Err(last)
}

/// Pipes bytes both ways between two connections until either side closes.
pub fn splice(a: Conn, b: Conn) -> Result<()> {
    use std::net::Shutdown;
    let (mut ar, mut bw) = (a.0.try_clone()?, b.0.try_clone()?);
    let t = std::thread::spawn(move || {
        let _ = std::io::copy(&mut ar, &mut bw);
        let _ = bw.shutdown(Shutdown::Write);
    });
    let (mut br, mut aw) = (b.0, a.0);
    let _ = std::io::copy(&mut br, &mut aw);
    let _ = aw.shutdown(Shutdown::Write);
    let _ = t.join();
    Ok(())
}

impl Listener {
    pub fn local_addr(&self) -> Result<String> {
        Ok(self.0.local_addr()?.to_string())
    }

    pub fn accept(&self) -> Result<Conn> {
        conn(self.0.accept()?.0)
    }
}

fn write_msg(w: &mut impl std::io::Write, msg: &[u8]) -> Result<()> {
    w.write_all(&(msg.len() as u32).to_be_bytes())?;
    Ok(w.write_all(msg)?)
}

fn read_msg(r: &mut impl std::io::Read, max: usize) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_be_bytes(len) as usize;
    if n > max {
        return Err(anyhow!("message of {n} bytes exceeds limit {max}"));
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    Ok(b)
}

impl Conn {
    pub fn send(&mut self, msg: &[u8]) -> Result<()> {
        write_msg(&mut self.0, msg)
    }

    /// Reads one message; rejects anything larger than `max` bytes before allocating.
    pub fn recv(&mut self, max: usize) -> Result<Vec<u8>> {
        read_msg(&mut self.0, max)
    }

    /// Like `recv`, but `None` if nothing arrives within `wait`.
    pub fn recv_within(&mut self, max: usize, wait: Duration) -> Result<Option<Vec<u8>>> {
        use std::io::ErrorKind::{TimedOut, WouldBlock};
        self.0.set_read_timeout(Some(wait))?;
        let r = self.0.peek(&mut [0u8; 1]);
        self.0.set_read_timeout(Some(NET_TIMEOUT))?;
        match r {
            Err(e) if matches!(e.kind(), WouldBlock | TimedOut) => Ok(None),
            Err(e) => Err(e.into()),
            Ok(0) => Err(anyhow!("connection closed")),
            Ok(_) => self.recv(max).map(Some),
        }
    }

    /// Reads one HTTP/1.x request: head capped at 16 KiB, body at `max_body`. No chunked bodies.
    pub fn http_request(&mut self, max_body: usize) -> Result<HttpRequest> {
        use std::io::{BufRead, Read, Write};
        const MAX_HEAD: u64 = 16 << 10;
        let mut r = std::io::BufReader::new(&self.0);
        let mut head = String::new();
        while !head.ends_with("\n\r\n") && !head.ends_with("\n\n") {
            if (&mut r).take(MAX_HEAD - head.len() as u64).read_line(&mut head)? == 0 {
                return Err(anyhow!("HTTP head incomplete or over 16 KiB"));
            }
        }
        let mut lines = head.lines();
        let mut first = lines.next().unwrap_or("").split(' ');
        let (method, path) = match (first.next(), first.next(), first.next()) {
            (Some(m), Some(p), Some(v)) if v.starts_with("HTTP/1.") => (m.to_string(), p.to_string()),
            _ => return Err(anyhow!("not an HTTP/1.x request")),
        };
        let headers = lines.filter(|l| !l.is_empty())
            .map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())))
            .collect::<Option<Vec<_>>>().ok_or_else(|| anyhow!("malformed HTTP header"))?;
        let mut req = HttpRequest { method, path, headers, body: vec![] };
        if req.header("transfer-encoding").is_some() {
            return Err(anyhow!("chunked request bodies are not supported (send Content-Length)"));
        }
        let len: usize = req.header("content-length").unwrap_or("0").parse().map_err(|_| anyhow!("bad Content-Length"))?;
        if len > max_body {
            return Err(anyhow!("body of {len} bytes exceeds limit {max_body}"));
        }
        if req.header("expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue")) {
            (&self.0).write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        }
        // grows as bytes arrive: a client announcing a large body must also send it
        r.take(len as u64).read_to_end(&mut req.body)?;
        if req.body.len() < len {
            return Err(anyhow!("body shorter than Content-Length"));
        }
        Ok(req)
    }

    /// Writes a whole response; the connection closes after it (no keep-alive).
    pub fn http_respond(&mut self, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
        use std::io::Write;
        write!(self.0, "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reason(status), body.len())?;
        Ok(self.0.write_all(body)?)
    }

    /// Starts a response whose body follows in `http_chunk` calls (chunked transfer encoding).
    pub fn http_start(&mut self, status: u16, content_type: &str) -> Result<()> {
        use std::io::Write;
        write!(self.0, "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n", reason(status))?;
        Ok(self.0.flush()?)
    }

    /// Sends one piece of a started response at once; an empty piece ends it. Fails once the client is gone.
    pub fn http_chunk(&mut self, data: &[u8]) -> Result<()> {
        use std::io::Write;
        write!(self.0, "{:x}\r\n", data.len())?;
        self.0.write_all(data)?;
        self.0.write_all(b"\r\n")?;
        Ok(self.0.flush()?)
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

pub struct HttpRequest {
    pub method: String,
    pub path: String,
    /// Names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

// --- local IPC ------------------------------------------------------------------

/// Unix socket with the `Conn` message framing. The socket file is mode 0600, so only its owner can connect.
pub struct LocalConn(std::os::unix::net::UnixStream);

pub struct LocalListener(std::os::unix::net::UnixListener);

/// Binds `path`, replacing a stale socket file; fails while a live listener owns it.
pub fn listen_local(path: &Path) -> Result<LocalListener> {
    use std::os::unix::fs::PermissionsExt;
    if dial_local(path).is_ok() {
        return Err(anyhow!("{} is in use (already running?)", path.display()));
    }
    let _ = std::fs::remove_file(path);
    let l = std::os::unix::net::UnixListener::bind(path).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(LocalListener(l))
}

pub fn dial_local(path: &Path) -> Result<LocalConn> {
    Ok(LocalConn(std::os::unix::net::UnixStream::connect(path).map_err(|e| anyhow!("{}: {e}", path.display()))?))
}

impl LocalListener {
    pub fn accept(&self) -> Result<LocalConn> {
        Ok(LocalConn(self.0.accept()?.0))
    }
}

impl LocalConn {
    pub fn send(&mut self, msg: &[u8]) -> Result<()> {
        write_msg(&mut self.0, msg)
    }

    pub fn recv(&mut self, max: usize) -> Result<Vec<u8>> {
        read_msg(&mut self.0, max)
    }
}

// --- login services -------------------------------------------------------------

fn launch_agent(label: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents").join(format!("{label}.plist")))
}

fn launchctl(args: &[&str]) -> Result<()> {
    let o = std::process::Command::new("/bin/launchctl").args(args).output()?;
    if o.status.success() {
        return Ok(());
    }
    Err(anyhow!("launchctl {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim()))
}

/// launchd domain of the user owning `plist` (std has no getuid; we wrote the file, so it is ours).
fn gui_domain(plist: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    Ok(format!("gui/{}", std::fs::metadata(plist)?.uid()))
}

/// Runs `program` at login and restarts it if it exits (a macOS LaunchAgent), starting it now.
/// launchd passes no shell environment, so `env` is all the service sees beyond its defaults.
pub fn service_install(label: &str, program: &Path, env: &[(String, String)], log: &Path) -> Result<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Err(anyhow!("login services are macOS-only for now (launchd)"));
    }
    let esc = |v: &str| v.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let s = |v: &str| format!("<string>{}</string>", esc(v));
    let env: String = env.iter().map(|(k, v)| format!("<key>{}</key>{}", esc(k), s(v))).collect();
    let (prog, log) = (s(&program.to_string_lossy()), s(&log.to_string_lossy()));
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>Label</key>{}\n<key>ProgramArguments</key><array>{prog}</array>\n\
         <key>EnvironmentVariables</key><dict>{env}</dict>\n\
         <key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n\
         <key>StandardOutPath</key>{log}\n<key>StandardErrorPath</key>{log}\n\
         </dict></plist>\n", s(label));
    let path = launch_agent(label)?;
    makedirs(path.parent().unwrap())?;
    std::fs::write(&path, plist)?;
    let domain = gui_domain(&path)?;
    let _ = launchctl(&["bootout", &format!("{domain}/{label}")]); // a previous install, if any
    launchctl(&["bootstrap", &domain, &path.to_string_lossy()])?;
    Ok(path)
}

/// Stops the service and removes it from login. `false` if it was not installed.
pub fn service_uninstall(label: &str) -> Result<bool> {
    let path = launch_agent(label)?;
    if !path.exists() {
        return Ok(false);
    }
    let _ = launchctl(&["bootout", &format!("{}/{label}", gui_domain(&path)?)]);
    std::fs::remove_file(&path)?;
    Ok(true)
}

// --- clipboard ------------------------------------------------------------------

const CLIP_GET: &[&[&str]] = &[&["pbpaste"], &["wl-paste", "--no-newline"], &["xclip", "-selection", "clipboard", "-o"]];
const CLIP_SET: &[&[&str]] = &[&["pbcopy"], &["wl-copy"], &["xclip", "-selection", "clipboard"]];

pub fn clipboard_get() -> Result<String> {
    for c in CLIP_GET {
        if let Ok(o) = std::process::Command::new(c[0]).args(&c[1..]).output() {
            if o.status.success() {
                return Ok(String::from_utf8_lossy(&o.stdout).into_owned());
            }
        }
    }
    Err(anyhow!("no clipboard tool found (pbpaste, wl-paste or xclip)"))
}

pub fn clipboard_set(text: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    for c in CLIP_SET {
        let Ok(mut child) = Command::new(c[0]).args(&c[1..]).stdin(Stdio::piped()).spawn() else { continue };
        child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?.write_all(text.as_bytes())?;
        if child.wait()?.success() {
            return Ok(());
        }
    }
    Err(anyhow!("no clipboard tool found (pbcopy, wl-copy or xclip)"))
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
/// `hidden` except `cwd` itself. macOS: sandbox-exec. Linux: bubblewrap (read-only root, `hidden`
/// masked by an empty tmpfs, no network namespace).
///
/// ponytail: deny-list profile on macOS. A full allow-list needs per-interpreter paths;
/// fails closed when the host has no sandbox, never runs the app unconfined.
pub fn spawn_sandboxed(cmd: &[String], cwd: &Path, hidden: &Path) -> Result<Proc> {
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    let mut c = if Path::new("/usr/bin/sandbox-exec").exists() {
        let profile = format!(
            "(version 1)(allow default)(deny network*)(deny file-write*)\
             (allow file-write* (literal \"/dev/null\") (literal \"/dev/tty\"))\
             (deny file-read* (subpath \"{}\"))(allow file-read* (subpath \"{}\"))",
            sbpath(hidden), sbpath(cwd)
        );
        let mut c = Command::new("/usr/bin/sandbox-exec");
        c.arg("-p").arg(profile);
        c
    } else if let Some(bwrap) = ["/usr/bin/bwrap", "/bin/bwrap"].into_iter().find(|p| Path::new(p).exists()) {
        let (hidden, cwd) = (std::fs::canonicalize(hidden)?, std::fs::canonicalize(cwd)?);
        let mut c = Command::new(bwrap);
        c.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs"]).arg(&hidden)
            .arg("--ro-bind").arg(&cwd).arg(&cwd)
            .args(["--unshare-net", "--unshare-pid", "--die-with-parent", "--new-session", "--"]);
        c
    } else {
        return Err(anyhow!("no OS sandbox available on this host (refusing to run app unconfined)"));
    };
    let mut child = c.args(cmd).current_dir(cwd)
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

// --- process accounting -----------------------------------------------------------

/// Cumulative counters of one process.
pub struct ProcStat {
    pub name: String,
    pub cpu_ns: u64,
    /// CPU energy only: macOS attributes no GPU energy to processes.
    pub cpu_energy_nj: u64,
    /// Memory charged to the process; on unified memory this includes its GPU buffers.
    pub footprint: u64,
    /// Neural Engine memory.
    pub neural: u64,
}

#[cfg(target_os = "macos")]
mod libproc {
    use std::ffi::c_void;

    #[repr(C)]
    pub struct Timebase {
        pub numer: u32,
        pub denom: u32,
    }

    unsafe extern "C" {
        pub fn proc_listallpids(buf: *mut c_void, bytes: i32) -> i32;
        pub fn proc_name(pid: i32, buf: *mut c_void, bytes: u32) -> i32;
        pub fn proc_pid_rusage(pid: i32, flavor: i32, buf: *mut c_void) -> i32;
        pub fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
}

/// Counters of every process this user may inspect (macOS denies other users' processes; they are skipped).
pub fn proc_stats() -> Result<std::collections::HashMap<i32, ProcStat>> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::c_void;
        let mut pids = vec![0i32; 1 << 14];
        let n = unsafe { libproc::proc_listallpids(pids.as_mut_ptr() as *mut c_void, (pids.len() * 4) as i32) };
        if n < 0 {
            return Err(anyhow!("proc_listallpids failed"));
        }
        let mut tb = libproc::Timebase { numer: 1, denom: 1 };
        unsafe { libproc::mach_timebase_info(&mut tb) };
        let mut out = std::collections::HashMap::new();
        for &pid in pids.iter().take(n as usize).filter(|&&p| p > 0) {
            // rusage_info_v6 (sys/resource.h): a 16-byte uuid, then 56 u64 fields; indexes count u64s from the start
            let mut ri = [0u64; 58];
            if unsafe { libproc::proc_pid_rusage(pid, 6, ri.as_mut_ptr() as *mut c_void) } != 0 {
                continue;
            }
            let mut name = [0u8; 64];
            let len = unsafe { libproc::proc_name(pid, name.as_mut_ptr() as *mut c_void, name.len() as u32) }.max(0) as usize;
            let ticks = (ri[2] + ri[3]) as u128; // ri_user_time + ri_system_time, in mach ticks
            out.insert(pid, ProcStat {
                name: String::from_utf8_lossy(&name[..len.min(name.len())]).into_owned(),
                cpu_ns: (ticks * tb.numer as u128 / tb.denom.max(1) as u128) as u64,
                cpu_energy_nj: ri[42], // ri_energy_nj
                footprint: ri[9],      // ri_phys_footprint
                neural: ri[46],        // ri_neural_footprint
            });
        }
        Ok(out)
    }
    #[cfg(not(target_os = "macos"))]
    Err(anyhow!("process accounting is macOS-only for now"))
}

/// Cumulative GPU busy time (ns) and name per process, from the IOKit registry. Covers every user's
/// processes without root. Apple GPUs only (`AppUsage` of each GPU client).
pub fn gpu_ns() -> Result<std::collections::HashMap<i32, (u64, String)>> {
    let o = std::process::Command::new("/usr/sbin/ioreg").args(["-l", "-w0", "-r", "-c", "IOAccelerator"]).output()?;
    Ok(parse_gpu_ns(&String::from_utf8_lossy(&o.stdout)))
}

fn parse_gpu_ns(ioreg: &str) -> std::collections::HashMap<i32, (u64, String)> {
    let mut out = std::collections::HashMap::new();
    for entry in ioreg.split("+-o ").skip(1) {
        let Some((pid, rest)) = entry.split("\"IOUserClientCreator\" = \"pid ").nth(1).and_then(|c| c.split_once(", ")) else { continue };
        let Ok(pid) = pid.parse::<i32>() else { continue };
        let ns: u64 = entry.split("\"accumulatedGPUTime\"=").skip(1)
            .filter_map(|s| s.split(|c: char| !c.is_ascii_digit()).next()?.parse::<u64>().ok()).sum();
        out.entry(pid).or_insert((0, rest.split('"').next().unwrap_or("").to_string())).0 += ns;
    }
    out
}

/// Command line of every process, by pid.
pub fn command_lines() -> Result<std::collections::HashMap<i32, String>> {
    let o = std::process::Command::new("/bin/ps").args(["-axo", "pid=,command="]).output()?;
    Ok(String::from_utf8_lossy(&o.stdout).lines()
        .filter_map(|l| {
            let (pid, cmd) = l.trim_start().split_once(' ')?;
            Some((pid.parse().ok()?, cmd.trim().to_string()))
        })
        .collect())
}

/// Files that `pids` have open or memory-mapped, as (pid, path).
pub fn open_files(pids: &[i32]) -> Result<Vec<(i32, PathBuf)>> {
    if pids.is_empty() {
        return Ok(vec![]);
    }
    let list: Vec<String> = pids.iter().map(i32::to_string).collect();
    // lsof exits non-zero when a pid is gone; what it printed still holds
    let o = std::process::Command::new("/usr/sbin/lsof").args(["-a", "-n", "-P", "-F", "pn", "-p", &list.join(",")]).output()?;
    let mut pid = 0;
    let mut out = vec![];
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        match l.split_at_checked(1) {
            Some(("p", v)) => pid = v.parse().unwrap_or(0),
            Some(("n", v)) if v.starts_with('/') => out.push((pid, PathBuf::from(v))),
            _ => {}
        }
    }
    Ok(out)
}

/// Up to `n` leading bytes of a file; `None` if it cannot be read.
pub fn read_head(path: &Path, n: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut b = Vec::with_capacity(n);
    std::fs::File::open(path).ok()?.take(n as u64).read_to_end(&mut b).ok()?;
    Some(b)
}

/// Size of a regular file; `None` for anything else.
pub fn file_len(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len())
}

#[cfg(test)]
mod tests {
    #[test]
    fn gpu_time_sums_per_process() {
        let ioreg = r#"+-o AGXAcceleratorG13G_B0  <class AGXAcceleratorG13G_B0>
  +-o AGXDeviceUserClient  <class AGXDeviceUserClient>
  |     "AppUsage" = ({"API"="Metal","lastSubmittedTime"=5,"accumulatedGPUTime"=100},{"API"="Metal","accumulatedGPUTime"=20})
  |     "IOUserClientCreator" = "pid 439, WindowServer"
  +-o AGXDeviceUserClient  <class AGXDeviceUserClient>
  |     "AppUsage" = ({"API"="Metal","accumulatedGPUTime"=3})
  |     "IOUserClientCreator" = "pid 439, WindowServer"
  +-o AGXDeviceUserClient  <class AGXDeviceUserClient>
  |     "AppUsage" = ()
  |     "IOUserClientCreator" = "pid 77, llama-server"
"#;
        let g = super::parse_gpu_ns(ioreg);
        assert_eq!(g[&439], (123, "WindowServer".to_string()));
        assert_eq!(g[&77], (0, "llama-server".to_string()));
        assert_eq!(g.len(), 2, "the accelerator itself has no creator pid");
    }
}
