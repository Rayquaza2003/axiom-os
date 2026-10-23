//! P5 minimal shell: ties store, NRM, planner and egress together. Used by the CLI, the TUI and the gate test.
//!
//! Session state (workspaces, the active workspace, the clipboard) lives in the graph, so it roams to
//! paired devices with every mesh sync.
use anyhow::{anyhow, bail, Result};
use chrono::{Local, TimeZone};
use core_ai::egress::{Approve, Egress, Transport};
use core_ai::{execute, summarize, LocalBackend, Mode, Nrm, NpuBackend, QueryResult};
use core_data::Store;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

pub mod apps;
pub mod daemon;
pub mod tui;

pub const AGING_S: f64 = 30.0;

/// This device's inference: `AXIOM_OFFLOAD=host:port` (or `relay://...`, comma list fails over) runs it on a
/// paired device, otherwise `LocalBackend` (env-configured).
pub fn default_backend(home: &Path) -> Result<Arc<dyn NpuBackend>> {
    let Some(addr) = std::env::var("AXIOM_OFFLOAD").ok().filter(|v| !v.is_empty()) else {
        return Ok(Arc::new(LocalBackend::from_env()?));
    };
    let (key, log, dest) = (mesh::load_or_create_key(home)?, home.join("egress.db"), addr.clone());
    let audit = Box::new(move |n| {
        if let Ok(e) = Egress::new(&log, None, None) {
            let _ = e.record(&dest, "mesh-offload", n, "allow", "");
        }
    });
    Ok(Arc::new(mesh::RemoteBackend::connect(&addr, key, audit)?))
}

pub struct Axiom {
    pub home: std::path::PathBuf,
    pub store: Store,
    pub nrm: Nrm,
    pub egress: Egress,
}

impl Axiom {
    pub fn open(home: &Path, backend: Arc<dyn NpuBackend>, approve: Option<Approve>, transport: Option<Transport>) -> Result<Axiom> {
        platform::makedirs(home)?;
        let ax = Axiom {
            home: home.to_path_buf(),
            store: Store::open(&home.join("axiom.db"), backend.dim())?,
            nrm: Nrm::new(backend, AGING_S),
            egress: Egress::new(&home.join("egress.db"), approve, transport)?,
        };
        // workspaces used to live in a local table that did not sync
        let old: Vec<(String, String)> = ax.store.db.prepare("select name, query from workspaces")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        for (n, q) in old {
            ax.save_workspace(&n, &q)?;
        }
        ax.store.db.execute("delete from workspaces", [])?;
        Ok(ax)
    }

    /// Inference goes to axiomd when it runs (one scheduler and one loaded model for the machine),
    /// otherwise to `default_backend` in this process.
    pub fn open_default(approve: Option<Approve>) -> Result<Axiom> {
        let home = platform::home()?;
        let backend: Arc<dyn NpuBackend> = match daemon::connect(&home)? {
            Some(d) => Arc::new(d),
            None => default_backend(&home)?,
        };
        Axiom::open(&home, backend, approve, None)
    }

    /// Speaks locally, or on a paired device when `AXIOM_AUDIO_SINK=host:port` is set (audio routing).
    pub fn speak(&self, text: &str) -> Result<()> {
        let Some(sink) = std::env::var("AXIOM_AUDIO_SINK").ok().filter(|v| !v.is_empty()) else {
            platform::speak(text);
            return Ok(());
        };
        self.egress.record(&sink, "mesh", text.len(), "allow", "speak")?;
        mesh::remote_speak(&sink, &mesh::load_or_create_key(&self.home)?, text)
    }

    pub fn embedder(&self) -> impl Fn(&[String]) -> Result<Vec<Vec<f32>>> + '_ {
        |texts| self.nrm.embed(core_ai::BACKGROUND, texts.to_vec())
    }

    pub fn import(&self, dir: &Path, project: Option<&str>) -> Result<usize> {
        self.store.import_dir(dir, &self.embedder(), project)
    }

    /// Answer `q`. `cloud_url` sends the context through the egress firewall as model-context.
    /// `save` writes the answer back as an object derived_from its sources.
    pub fn ask(&self, q: &str, now: f64, cloud_url: Option<&str>, save: bool) -> Result<(QueryResult, String)> {
        let r = execute(&self.store, &self.nrm, q, now, 8)?;
        let mut text = match cloud_url {
            Some(url) if !r.hits.is_empty() => {
                let mut ctx = String::new();
                for h in &r.hits {
                    ctx += &format!("[{}]\n{}\n\n", self.store.title(&h.obj), self.store.text(&h.obj, 1500)?);
                }
                let reply = self.egress.send(url, &json!({"prompt": format!("Summarize:\n{ctx}")}), "model-context")?;
                reply["response"].as_str().unwrap_or("").to_string()
            }
            _ if r.plan.summarize => summarize(&self.store, &self.nrm, &r.hits)?,
            _ if r.hits.is_empty() => "No matching documents.".into(),
            _ => r.hits.iter().map(|h| format!("- {}  ({:.2})", self.store.title(&h.obj), h.score)).collect::<Vec<_>>().join("\n"),
        };
        if save && !r.hits.is_empty() {
            let oid = self.store.put(text.as_bytes(), now)?;
            self.store.index(&oid, &text, &self.embedder())?;
            self.store.tag(&oid, "summary", 0.9, "ai")?;
            for h in &r.hits {
                self.store.link(&oid, "derived_from", &h.obj, 1.0, "user")?;
            }
            text += &format!("\n[saved {}, derived_from {} objects]", &oid[..12], r.hits.len());
        }
        Ok((r, text))
    }

    pub fn save_workspace(&self, name: &str, query: &str) -> Result<()> {
        self.store.relink(&format!("workspace:{name}"), "query", query)
    }

    /// (name, query), newest query per workspace.
    pub fn workspaces(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.store.db.prepare(
            "select substr(src,11), dst from edges e where type='query' and src like 'workspace:%' \
             and ts=(select max(ts) from edges where src=e.src and type='query') group by src order by src")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn active_workspace(&self) -> Option<String> {
        self.store.target("session:current", "active").and_then(|w| w.strip_prefix("workspace:").map(String::from))
    }

    /// Opens `name` (default: the active workspace, possibly set on another device) and makes it active.
    pub fn open_workspace(&self, name: Option<&str>, now: f64) -> Result<(String, String)> {
        let name = match name {
            Some(n) => n.to_string(),
            None => self.active_workspace().ok_or_else(|| anyhow!("no active workspace"))?,
        };
        let (_, q) = self.workspaces()?.into_iter().find(|(n, _)| *n == name).ok_or_else(|| anyhow!("no such workspace {name:?}"))?;
        self.store.relink("session:current", "active", &format!("workspace:{name}"))?;
        Ok((name, self.ask(&q, now, None, false)?.1))
    }

    /// Universal clipboard: stores `text` as an object and points `clipboard:current` at it.
    pub fn clip_push(&self, text: &str, now: f64) -> Result<String> {
        if text.is_empty() {
            bail!("clipboard is empty");
        }
        let oid = self.store.put(text.as_bytes(), now)?;
        self.store.index(&oid, text, &self.embedder())?;
        self.store.tag(&oid, "clipboard", 1.0, "user")?;
        self.store.relink("clipboard:current", "holds", &oid)?;
        Ok(oid)
    }

    pub fn clip_current(&self) -> Result<Option<String>> {
        let Some(oid) = self.store.target("clipboard:current", "holds") else { return Ok(None) };
        Ok(Some(String::from_utf8_lossy(&self.store.get(&oid)?).into_owned()))
    }

    pub fn brief(&self, now: f64) -> Result<String> {
        let date = Local.timestamp_opt(now as i64, 0).single().map(|t| t.format("%A %d %b").to_string()).unwrap_or_default();
        let tags = self.store.top_tags(5)?;
        let mut lines = vec![
            format!("{date}: {} new objects this week", self.store.count_since(now - 7.0 * 86400.0)?),
            format!("top tags: {}", if tags.is_empty() { "none".into() } else {
                tags.iter().map(|(t, c)| format!("{t}({c})")).collect::<Vec<_>>().join(", ") }),
        ];
        for (name, q) in self.workspaces()? {
            lines.push(format!("[{name}]"));
            for h in execute(&self.store, &self.nrm, &q, now, 3)?.hits {
                lines.push(format!("  - {}", self.store.title(&h.obj)));
            }
        }
        Ok(lines.join("\n"))
    }
}

pub fn mode_name(m: &Mode) -> &'static str {
    match m {
        Mode::Hybrid => "hybrid",
        Mode::Keyword => "keyword",
    }
}
