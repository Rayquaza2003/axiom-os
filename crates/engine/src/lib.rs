//! In-process inference on llama.cpp (Metal on Apple Silicon): axiomd owns the model instead of fronting a
//! model server. One thread owns llama.cpp; callers talk to it over channels, so the engine is safe to call
//! from any thread.
//!
//! Every generation is its own llama.cpp sequence. When the NRM pauses one between slices, its KV cache
//! stays in memory and the next slice continues from it without re-reading the prompt or the partial answer
//! (a model server re-reads both once other work has used its cache). Up to `SEQS` paused generations are
//! kept; the least recently used one is dropped first, and a dropped one is rebuilt from its prefix.
//!
//! Config: `AXIOM_ENGINE_MODEL` (chat model) and `AXIOM_ENGINE_EMBED` (embedding model), each a GGUF path or
//! `ollama:NAME[:TAG]`, which reuses a model Ollama has already downloaded.
use anyhow::{anyhow, bail, Result};
use core_ai::NpuBackend;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use serde_json::Value;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

/// Paused generations kept in memory at once.
const SEQS: usize = 4;
/// KV cells shared by all sequences (about 300 MB for a 3B model at f16).
const CTX_TOKENS: u32 = 8192;
/// Tokens per prefill batch, and the longest text an embedding sees.
const BATCH: usize = 2048;

static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();

/// Path of the GGUF file a config value names: a path, or `ollama:NAME[:TAG]` from Ollama's manifests.
pub fn resolve(spec: &str) -> Result<PathBuf> {
    resolve_in(&PathBuf::from(std::env::var("HOME")?).join(".ollama/models"), spec)
}

fn resolve_in(root: &std::path::Path, spec: &str) -> Result<PathBuf> {
    let Some(name) = spec.strip_prefix("ollama:") else { return Ok(PathBuf::from(spec)) };
    let (name, tag) = name.split_once(':').unwrap_or((name, "latest"));
    let repo = if name.contains('/') { name.to_string() } else { format!("library/{name}") };
    let manifest = root.join("manifests/registry.ollama.ai").join(repo).join(tag);
    let m: Value = serde_json::from_slice(&platform::read_file(&manifest).map_err(|e| anyhow!("{spec}: no Ollama manifest ({e})"))?)?;
    let digest = m["layers"].as_array().into_iter().flatten()
        .find(|l| l["mediaType"] == "application/vnd.ollama.image.model")
        .and_then(|l| l["digest"].as_str())
        .ok_or_else(|| anyhow!("{spec}: the manifest names no model layer"))?;
    Ok(root.join("blobs").join(digest.replace(':', "-")))
}

enum Req {
    Generate { prompt: String, prefix: String, max_tokens: usize, out: Sender<Result<Option<String>>> },
    Embed { texts: Vec<String>, out: Sender<Result<Vec<Vec<f32>>>> },
    /// Free the models; the reply comes once they are gone.
    Stop(Sender<()>),
}

/// Every engine, so process exit can free their GPU memory before ggml tears Metal down (ggml asserts
/// that nothing is still resident).
static ENGINES: Mutex<Vec<Sender<Req>>> = Mutex::new(Vec::new());

extern "C" fn release_gpu() {
    for tx in ENGINES.lock().unwrap().drain(..) {
        let (ack, done) = channel();
        if tx.send(Req::Stop(ack)).is_ok() {
            let _ = done.recv_timeout(std::time::Duration::from_secs(5));
        }
    }
}

pub struct Engine {
    tx: Mutex<Sender<Req>>,
    dim: usize,
    llm: bool,
}

impl Engine {
    /// Loads the models named by `AXIOM_ENGINE_MODEL` / `AXIOM_ENGINE_EMBED`; `None` when neither is set.
    pub fn from_env() -> Result<Option<Engine>> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let (chat, embed) = (env("AXIOM_ENGINE_MODEL"), env("AXIOM_ENGINE_EMBED"));
        if chat.is_none() && embed.is_none() {
            return Ok(None);
        }
        Engine::load(chat.map(|s| resolve(&s)).transpose()?, embed.map(|s| resolve(&s)).transpose()?).map(Some)
    }

    /// Without an embedding model, embeddings are the offline lexical ones.
    pub fn load(chat: Option<PathBuf>, embed: Option<PathBuf>) -> Result<Engine> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let llm = chat.is_some();
        std::thread::Builder::new().name("engine".into()).spawn(move || match serve(chat, embed, rx, &ready_tx) {
            Ok(Some(ack)) => drop(ack.send(())), // models dropped when `serve` returned
            Ok(None) => {}
            Err(e) => drop(ready_tx.send(Err(e))),
        })?;
        let dim = ready_rx.recv().map_err(|_| anyhow!("engine thread died while loading"))??;
        // registered after ggml set Metal up, so it runs before ggml's own teardown
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(|| platform::at_exit(release_gpu));
        ENGINES.lock().unwrap().push(tx.clone());
        Ok(Engine { tx: Mutex::new(tx), dim, llm })
    }

    fn send(&self, r: Req) -> Result<()> {
        self.tx.lock().unwrap().send(r).map_err(|_| anyhow!("engine thread is gone"))
    }
}

impl NpuBackend for Engine {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let (out, rx) = channel();
        self.send(Req::Embed { texts: texts.to_vec(), out })?;
        rx.recv().map_err(|_| anyhow!("engine thread is gone"))?
    }

    fn has_llm(&self) -> bool {
        self.llm
    }

    fn generate(&self, prompt: &str) -> Result<String> {
        let mut text = String::new();
        while !self.generate_stream(0, prompt, &text.clone(), 256, &mut |p| {
            text += p;
            true
        })? {}
        Ok(text)
    }

    fn generate_stream(&self, _tier: u8, prompt: &str, prefix: &str, max_tokens: usize, piece: &mut dyn FnMut(&str) -> bool) -> Result<bool> {
        if !self.llm {
            bail!("no chat model loaded (set AXIOM_ENGINE_MODEL)");
        }
        let (out, rx) = channel();
        self.send(Req::Generate { prompt: prompt.into(), prefix: prefix.into(), max_tokens, out })?;
        for p in rx {
            match p? {
                Some(s) => {
                    if !piece(&s) {
                        return Ok(false); // dropping `rx` pauses the sequence at its next token
                    }
                }
                None => return Ok(true),
            }
        }
        Ok(false) // the slice ended
    }
}

/// A generation in progress, paused or running.
struct Seq {
    prompt: String,
    /// Everything emitted so far.
    text: String,
    /// Bytes of a character split across tokens, not yet emitted.
    partial: Vec<u8>,
    /// Next KV position.
    pos: i32,
    /// Sampled and emitted but not yet decoded: decoding it gives the logits for the next token.
    pending: Option<LlamaToken>,
    sampler: LlamaSampler,
    used: u64,
}

/// Serves until every sender is gone (`None`) or `Stop` comes (its reply, sent by the caller once the
/// models dropped here are freed).
fn serve(chat: Option<PathBuf>, embed: Option<PathBuf>, rx: Receiver<Req>, ready: &Sender<Result<usize>>) -> Result<Option<Sender<()>>> {
    let backend = BACKEND.get_or_init(|| {
        // llama.cpp and ggml (Metal setup included) log to stderr unless redirected
        llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default().with_logs_enabled(false));
        LlamaBackend::init().expect("llama.cpp initialises once per process")
    });
    let gpu = LlamaModelParams::default().with_n_gpu_layers(999);
    let load = |p: &PathBuf| LlamaModel::load_from_file(backend, p, &gpu).map_err(|e| anyhow!("{}: {e}", p.display()));
    let chat_model = chat.as_ref().map(load).transpose()?;
    let embed_model = embed.as_ref().map(load).transpose()?;
    let mut gen_ctx = chat_model.as_ref().map(|m| {
        let p = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(CTX_TOKENS)).with_n_seq_max(SEQS as u32)
            .with_kv_unified(true).with_n_batch(BATCH as u32);
        m.new_context(backend, p).map_err(|e| anyhow!("chat context: {e}"))
    }).transpose()?;
    let mut emb_ctx = embed_model.as_ref().map(|m| {
        let p = LlamaContextParams::default().with_embeddings(true).with_n_ctx(NonZeroU32::new(BATCH as u32))
            .with_n_batch(BATCH as u32).with_n_ubatch(BATCH as u32);
        m.new_context(backend, p).map_err(|e| anyhow!("embedding context: {e}"))
    }).transpose()?;
    let probe = match emb_ctx.as_mut() {
        Some(ctx) => embed_one(ctx, embed_model.as_ref().unwrap(), "probe")?.len(),
        None => core_ai::LocalBackend::offline().dim(),
    };
    let _ = ready.send(Ok(probe));

    let mut seqs: Vec<Option<Seq>> = (0..SEQS).map(|_| None).collect();
    let mut tick = 0u64;
    for req in rx {
        tick += 1;
        match req {
            Req::Embed { texts, out } => {
                let r = match (emb_ctx.as_mut(), embed_model.as_ref()) {
                    (Some(ctx), Some(m)) => texts.iter().map(|t| embed_one(ctx, m, t)).collect(),
                    _ => core_ai::LocalBackend::offline().embed(&texts),
                };
                let _ = out.send(r);
            }
            Req::Stop(ack) => return Ok(Some(ack)),
            Req::Generate { prompt, prefix, max_tokens, out } => {
                let (Some(ctx), Some(m)) = (gen_ctx.as_mut(), chat_model.as_ref()) else {
                    let _ = out.send(Err(anyhow!("no chat model loaded")));
                    continue;
                };
                if let Err(e) = generate(ctx, m, &mut seqs, tick, &prompt, &prefix, max_tokens, &out) {
                    let _ = out.send(Err(e));
                }
            }
        }
    }
    Ok(None)
}

/// Runs up to `max_tokens` of the answer to `prompt` after `prefix`, sending each piece, then `None` when the
/// answer ends. Stops early, keeping the sequence, when the receiver goes away.
#[allow(clippy::too_many_arguments)]
fn generate(ctx: &mut LlamaContext, m: &LlamaModel, seqs: &mut [Option<Seq>], tick: u64, prompt: &str, prefix: &str,
            max_tokens: usize, out: &Sender<Result<Option<String>>>) -> Result<()> {
    let id = match seqs.iter().position(|s| s.as_ref().is_some_and(|s| s.prompt == prompt && s.text == prefix)) {
        Some(id) => id, // paused here: its KV cache is still in memory
        None => {
            // a free slot, else the least recently used one
            let id = (0..seqs.len()).min_by_key(|&i| seqs[i].as_ref().map_or(0, |s| s.used)).unwrap();
            ctx.kv_cache_seq_rm(id as i32, None, None)?;
            seqs[id] = None;
            let tmpl = m.chat_template(None).map_err(|e| anyhow!("model has no chat template: {e}"))?;
            let text = m.apply_chat_template(&tmpl, &[LlamaChatMessage::new("user".into(), prompt.into())?], true)? + prefix;
            let tokens = m.vocab().tokenize(text.as_bytes(), true, true);
            let mut pos = 0;
            for (n, chunk) in tokens.chunks(BATCH).enumerate() {
                let mut batch = LlamaBatch::new(chunk.len(), 1);
                let last = n * BATCH + chunk.len() == tokens.len();
                for (i, t) in chunk.iter().enumerate() {
                    batch.add(*t, pos, &[id as i32], last && i + 1 == chunk.len())?;
                    pos += 1;
                }
                ctx.decode(&mut batch)?;
            }
            let sampler = LlamaSampler::chain_simple([LlamaSampler::top_k(40), LlamaSampler::top_p(0.95, 1),
                                                      LlamaSampler::temp(0.8), LlamaSampler::dist(tick as u32)]);
            seqs[id] = Some(Seq { prompt: prompt.into(), text: prefix.into(), partial: vec![], pos, pending: None, sampler, used: tick });
            id
        }
    };
    let s = seqs[id].as_mut().unwrap();
    s.used = tick;
    for _ in 0..max_tokens {
        let mut idx = -1; // the prefill left this sequence's logits as the last output
        if let Some(t) = s.pending.take() {
            let mut batch = LlamaBatch::new(1, 1);
            batch.add(t, s.pos, &[id as i32], true)?;
            ctx.decode(&mut batch)?;
            s.pos += 1;
            idx = 0;
        }
        let t = s.sampler.sample(ctx, idx);
        if m.vocab().is_eog(t) || s.pos as u32 >= CTX_TOKENS / 2 {
            ctx.kv_cache_seq_rm(id as i32, None, None)?;
            seqs[id] = None;
            let _ = out.send(Ok(None));
            return Ok(());
        }
        s.pending = Some(t);
        s.partial.extend(m.vocab().token_to_piece(t, false, None));
        // emit whole characters only; a split one waits for its next token
        let valid = match std::str::from_utf8(&s.partial) {
            Ok(v) => v.len(),
            Err(e) => e.valid_up_to(),
        };
        let piece: String = String::from_utf8(s.partial.drain(..valid).collect()).unwrap();
        if piece.is_empty() {
            continue;
        }
        s.text += &piece;
        if out.send(Ok(Some(piece))).is_err() {
            return Ok(()); // the caller left; the sequence stays paused
        }
    }
    Ok(())
}

/// Mean-pooled (or the model's own pooling), L2-normalised embedding of one text.
fn embed_one(ctx: &mut LlamaContext, m: &LlamaModel, text: &str) -> Result<Vec<f32>> {
    let mut tokens = m.vocab().tokenize(text.as_bytes(), true, false);
    tokens.truncate(BATCH);
    ctx.kv_cache_seq_rm(0, None, None)?;
    let mut batch = LlamaBatch::new(tokens.len(), 1);
    for (i, t) in tokens.iter().enumerate() {
        batch.add(*t, i as i32, &[0], true)?;
    }
    ctx.decode(&mut batch)?;
    let v = ctx.embeddings_seq_ith(0).map_err(|e| anyhow!("no pooled embedding: {e}"))?;
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    Ok(v.iter().map(|x| x / n).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn ollama_names_resolve_to_their_model_blob() {
        let tmp = std::env::temp_dir().join(format!("engine-resolve-{}", std::process::id()));
        let dir = tmp.join("manifests/registry.ollama.ai/library/qwen2.5");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("3b"), r#"{"layers":[{"mediaType":"application/vnd.ollama.image.template","digest":"sha256:aa"},
            {"mediaType":"application/vnd.ollama.image.model","digest":"sha256:bb"}]}"#).unwrap();
        assert_eq!(super::resolve_in(&tmp, "ollama:qwen2.5:3b").unwrap(), tmp.join("blobs/sha256-bb"));
        assert!(super::resolve_in(&tmp, "ollama:qwen2.5").is_err(), "the default tag is latest, which is missing");
        assert_eq!(super::resolve_in(&tmp, "/m/x.gguf").unwrap(), std::path::PathBuf::from("/m/x.gguf"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
