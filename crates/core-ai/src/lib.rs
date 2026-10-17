//! P3 AI core: inference backend, NRM scheduler, planner, summarizer, egress firewall.
pub mod egress;
pub mod nrm;
pub mod plan;

use anyhow::Result;
use core_data::{Filter, Hit, Store};
use serde_json::{json, Value};

pub use nrm::{Nrm, BACKGROUND, INTERACTIVE, REALTIME};
pub use plan::{plan, Plan};

/// Inference backend. A vendor NPU backend (RKNN, QNN, MPS) implements this same trait
/// (see specs/build/hardware-support.md); `LocalBackend` is the offline default.
pub trait NpuBackend: Send + Sync {
    fn dim(&self) -> usize;
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn has_llm(&self) -> bool;
    fn generate(&self, prompt: &str) -> Result<String>;
}

/// Offline feature-hashing embedder (lexical, not semantic) plus optional OpenAI-compatible HTTP endpoints
/// (llama-server, Ollama, ...) for real embeddings and summaries.
///
/// Env: AXIOM_LLM_MODEL (enables summaries), AXIOM_EMBED_MODEL (enables semantic vectors),
/// AXIOM_LLM_URL / AXIOM_EMBED_URL (default http://localhost:11434).
pub struct LocalBackend {
    llm: Option<(String, String)>,
    embed: Option<(String, String)>,
    dim: usize,
}

const HASH_DIM: usize = 256;

impl LocalBackend {
    pub fn from_env() -> Result<Self> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let url = |k: &str| env(k).unwrap_or_else(|| "http://localhost:11434".into());
        let mut b = LocalBackend {
            llm: env("AXIOM_LLM_MODEL").map(|m| (url("AXIOM_LLM_URL"), m)),
            embed: env("AXIOM_EMBED_MODEL").map(|m| (url("AXIOM_EMBED_URL"), m)),
            dim: HASH_DIM,
        };
        if b.embed.is_some() {
            b.dim = b.embed(&["probe".to_string()])?[0].len();
        }
        Ok(b)
    }

    pub fn offline() -> Self {
        LocalBackend { llm: None, embed: None, dim: HASH_DIM }
    }

    fn hash_embed(text: &str) -> Vec<f32> {
        let mut v = vec![0f32; HASH_DIM];
        let lower = text.to_lowercase();
        for w in lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) {
            let w = if w.len() > 3 && w.ends_with('s') { &w[..w.len() - 1] } else { w };
            let h = crc32fast::hash(w.as_bytes());
            v[h as usize % HASH_DIM] += if (h >> 16) & 1 == 1 { 1.0 } else { -1.0 };
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            v.iter_mut().for_each(|x| *x /= n);
        }
        v
    }
}

impl NpuBackend for LocalBackend {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let Some((url, model)) = &self.embed else {
            return Ok(texts.iter().map(|t| Self::hash_embed(t)).collect());
        };
        let r = platform::http_json(&format!("{url}/v1/embeddings"), Some(&json!({"model": model, "input": texts})), 120)?;
        r["data"].as_array().ok_or_else(|| anyhow::anyhow!("bad embeddings reply"))?.iter()
            .map(|d| Ok(d["embedding"].as_array().ok_or_else(|| anyhow::anyhow!("bad embedding"))?
                .iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect()))
            .collect()
    }

    fn has_llm(&self) -> bool {
        self.llm.is_some()
    }

    fn generate(&self, prompt: &str) -> Result<String> {
        let (url, model) = self.llm.as_ref().ok_or_else(|| anyhow::anyhow!("no LLM configured"))?;
        let body = json!({"model": model, "messages": [{"role": "user", "content": prompt}], "stream": false});
        let r: Value = platform::http_json(&format!("{url}/v1/chat/completions"), Some(&body), 300)?;
        Ok(r["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string())
    }
}

#[derive(Debug, PartialEq)]
pub enum Mode {
    Hybrid,
    Keyword,
}

pub struct QueryResult {
    pub plan: Plan,
    pub mode: Mode,
    pub hits: Vec<Hit>,
}

/// <0.7 keyword fallback; >=0.7 hybrid. Read-only: writes (saving a summary) need an explicit flag upstream.
pub fn execute(store: &Store, nrm: &Nrm, q: &str, now: f64, k: usize) -> Result<QueryResult> {
    let known = store.known_tags()?;
    let mut p = plan(q, &known, now);
    if nrm.backend.has_llm() {
        // An LLM failure is not fatal: the rule plan stands.
        if let Ok(reply) = nrm.generate(INTERACTIVE, plan::llm_prompt(q, &known)) {
            p = plan::refine(p, &reply, &known);
        }
    }
    if p.confidence < 0.7 {
        let hits = store.keyword(&p.terms.join(" "), &Filter::default(), k)?;
        return Ok(QueryResult { plan: p, mode: Mode::Keyword, hits });
    }
    // With search terms a tag only boosts ranking (a folder name like "data" must not hide other docs).
    // With no terms the tag is the only signal left, so it filters.
    let (tags, boost) = if p.terms.is_empty() { (p.tags.clone(), vec![]) } else { (vec![], p.tags.clone()) };
    let f = Filter { tags, boost, project: None, span: p.t0.zip(p.t1) };
    let qvec = if p.terms.is_empty() { None } else { Some(nrm.embed(INTERACTIVE, vec![p.terms.join(" ")])?.remove(0)) };
    let hits = store.search(qvec.as_deref(), &f, k)?;
    Ok(QueryResult { plan: p, mode: Mode::Hybrid, hits })
}

pub fn summarize(store: &Store, nrm: &Nrm, hits: &[Hit]) -> Result<String> {
    if hits.is_empty() {
        return Ok("No matching documents.".into());
    }
    if !nrm.backend.has_llm() {
        let lines: Vec<String> = hits.iter().map(|h| {
            let body = h.text.lines().find(|l| !l.trim().is_empty() && !l.starts_with('#')).unwrap_or(&h.text);
            format!("- {}: {}", store.title(&h.obj), body.trim().chars().take(200).collect::<String>())
        }).collect();
        return Ok(format!("(extractive; set AXIOM_LLM_MODEL for a real summary)\n{}", lines.join("\n")));
    }
    let mut ctx = String::new();
    for h in hits {
        ctx += &format!("[{}]\n{}\n\n", store.title(&h.obj), store.text(&h.obj, 1500)?);
    }
    nrm.generate(INTERACTIVE, format!("Summarize these documents concisely, citing titles in brackets.\n\n{ctx}"))
}
