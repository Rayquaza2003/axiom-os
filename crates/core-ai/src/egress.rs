//! Egress firewall (userspace for the prototype). Every outbound byte goes through `Egress::send`.
//!
//! Deny by default. Per-destination allowlist for ordinary traffic. Model-context traffic is never
//! allowlisted: it needs a daily byte budget (default 0 = blocked) AND a per-request approval.
//! The audit log lives in its own DB so a data rollback cannot erase it.
use anyhow::Result;
use chrono::{Local, TimeZone};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::Path;

const SCHEMA: &str = "
create table if not exists audit(ts real, dest text, kind text, bytes integer, decision text, reason text);
create table if not exists allow(dest text primary key);
create table if not exists settings(k text primary key, v text);
";

#[derive(Debug)]
pub struct Denied(pub String);

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Denied {}

/// approve(dest, kind, nbytes, preview) -> "y" | "n" | "always"
pub type Approve = Box<dyn Fn(&str, &str, usize, &str) -> String + Send>;
pub type Transport = Box<dyn Fn(&str, &Value) -> Result<Value> + Send>;

pub struct AuditRow {
    pub ts: f64,
    pub dest: String,
    pub kind: String,
    pub bytes: i64,
    pub decision: String,
    pub reason: String,
}

pub struct Egress {
    db: Connection,
    pub approve: Option<Approve>,
    transport: Transport,
}

pub fn host_key(dest: &str) -> String {
    dest.split("://").last().unwrap_or(dest).split('/').next().unwrap_or(dest).to_string()
}

impl Egress {
    pub fn new(path: &Path, approve: Option<Approve>, transport: Option<Transport>) -> Result<Egress> {
        let db = Connection::open(path)?;
        db.execute_batch(SCHEMA)?;
        let transport = transport.unwrap_or_else(|| Box::new(|url, p| platform::http_json(url, Some(p), 120)));
        Ok(Egress { db, approve, transport })
    }

    pub fn setting(&self, k: &str, default: &str) -> String {
        self.db.query_row("select v from settings where k=?", [k], |r| r.get(0)).unwrap_or_else(|_| default.to_string())
    }

    pub fn set_setting(&self, k: &str, v: &str) -> Result<()> {
        self.db.execute("insert or replace into settings values(?,?)", [k, v])?;
        Ok(())
    }

    pub fn allow(&self, dest: &str) -> Result<()> {
        self.db.execute("insert or ignore into allow values(?)", [host_key(dest)])?;
        Ok(())
    }

    pub fn log(&self) -> Result<Vec<AuditRow>> {
        let mut st = self.db.prepare("select ts, dest, kind, bytes, decision, reason from audit order by rowid")?;
        let rows = st.query_map([], |r| Ok(AuditRow {
            ts: r.get(0)?, dest: r.get(1)?, kind: r.get(2)?, bytes: r.get(3)?, decision: r.get(4)?, reason: r.get(5)?,
        }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn used_today(&self, kind: &str) -> i64 {
        let n = Local.timestamp_opt(platform::now() as i64, 0).single().map(|t| t.date_naive().and_hms_opt(0, 0, 0).unwrap());
        let t0 = n.and_then(|d| Local.from_local_datetime(&d).earliest()).map(|t| t.timestamp() as f64).unwrap_or(0.0);
        self.db.query_row(
            "select coalesce(sum(bytes),0) from audit where kind=? and decision='allow' and ts>=?",
            params![kind, t0], |r| r.get(0)).unwrap_or(0)
    }

    /// Append an audit row. Public so other planes (mesh) can log their own traffic.
    pub fn record(&self, dest: &str, kind: &str, n: usize, decision: &str, reason: &str) -> Result<()> {
        self.db.execute("insert into audit values(?,?,?,?,?,?)", params![platform::now(), dest, kind, n as i64, decision, reason])?;
        Ok(())
    }

    fn deny(&self, dest: &str, kind: &str, n: usize, reason: &str) -> Result<Value> {
        self.record(dest, kind, n, "deny", reason)?;
        Err(Denied(format!("blocked {kind} egress to {dest}: {reason}")).into())
    }

    pub fn send(&self, dest: &str, payload: &Value, kind: &str) -> Result<Value> {
        let body = payload.to_string();
        let n = body.len();
        let needs_ask = if kind == "model-context" {
            let budget: i64 = self.setting("budget_model_context", "0").parse().unwrap_or(0);
            if self.used_today(kind) + n as i64 > budget {
                return self.deny(dest, kind, n, &format!("daily model-context budget {budget}B exceeded"));
            }
            true
        } else {
            self.db.query_row("select 1 from allow where dest=?", [host_key(dest)], |_| Ok(())).is_err()
        };
        if needs_ask {
            let preview: String = body.chars().take(200).collect();
            let answer = self.approve.as_ref().map(|a| a(dest, kind, n, &preview)).unwrap_or_else(|| "n".into());
            if answer != "y" && answer != "always" {
                return self.deny(dest, kind, n, "not approved");
            }
            if answer == "always" && kind != "model-context" {
                self.allow(dest)?;
            }
        }
        self.record(dest, kind, n, "allow", "")?;
        (self.transport)(dest, payload)
    }
}
