//! P5 minimal shell: ties store, NRM, planner and egress together. Used by the CLI, the TUI and the gate test.
use anyhow::Result;
use chrono::{Local, TimeZone};
use core_ai::egress::{Approve, Egress, Transport};
use core_ai::{execute, summarize, LocalBackend, Mode, Nrm, NpuBackend, QueryResult};
use core_data::Store;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

pub mod apps;
pub mod tui;

pub const AGING_S: f64 = 30.0;

pub struct Axiom {
    pub home: std::path::PathBuf,
    pub store: Store,
    pub nrm: Nrm,
    pub egress: Egress,
}

impl Axiom {
    pub fn open(home: &Path, backend: Arc<dyn NpuBackend>, approve: Option<Approve>, transport: Option<Transport>) -> Result<Axiom> {
        platform::makedirs(home)?;
        Ok(Axiom {
            home: home.to_path_buf(),
            store: Store::open(&home.join("axiom.db"), backend.dim())?,
            nrm: Nrm::new(backend, AGING_S),
            egress: Egress::new(&home.join("egress.db"), approve, transport)?,
        })
    }

    pub fn open_default(approve: Option<Approve>) -> Result<Axiom> {
        Axiom::open(&platform::home()?, Arc::new(LocalBackend::from_env()?), approve, None)
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

    pub fn workspaces(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.store.db.prepare("select name, query from workspaces order by name")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
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
