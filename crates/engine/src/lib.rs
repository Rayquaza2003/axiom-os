//! In-process inference on llama.cpp (Metal on Apple Silicon): axion owns the model instead of fronting a
//! model server. One thread owns llama.cpp; callers talk to it over channels, so the engine is safe to call
//! from any thread.
//!
//! Generations run side by side: each is its own llama.cpp sequence, and every step decodes one token for
//! all running sequences in a single batch, which costs a decode-bound GPU little more than one. A new
//! request is prefilled between steps and joins the batch at once, so it never waits for another answer.
//! When all `SEQS` slots are busy, waiting requests start in tier order. A paused generation (a slice
//! used up, or a caller gone) keeps its KV cache, so it resumes without re-reading its prompt; the least
//! recently used paused one is dropped first, and a dropped one is rebuilt from its prefix.
//!
//! Models load when a request first needs one (an embedding never loads the chat model) and everything is
//! freed after `AXIOM_ENGINE_IDLE` seconds without requests (default 120).
//!
//! Config: `AXIOM_ENGINE_MODEL` (chat model) and `AXIOM_ENGINE_EMBED` (embedding model), each a GGUF path or
//! `ollama:NAME[:TAG]`, which reuses a model Ollama has already downloaded.
use anyhow::{anyhow, bail, Result};
use core_ai::{NpuBackend, INTERACTIVE};
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

/// Generations running or paused at once.
const SEQS: usize = 4;
/// KV cells shared by all sequences unless `AXIOM_ENGINE_CTX` says otherwise (about 300 MB for a 3B model
/// at f16). Any one answer may use all of them, up to the model's trained length.
const CTX_TOKENS: u32 = 8192;
/// Tokens per prefill batch, and the longest text an embedding sees.
const BATCH: usize = 2048;
/// Texts embedded together in one batch.
const EMBED_SEQS: usize = 16;

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
    Generate { tier: u8, prompt: String, prefix: String, max_tokens: usize, out: Sender<Result<Option<String>>> },
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
        let idle = env("AXIOM_ENGINE_IDLE").and_then(|v| v.parse().ok()).unwrap_or(120.0);
        let (chat, embed) = (chat.map(|s| resolve(&s)).transpose()?, embed.map(|s| resolve(&s)).transpose()?);
        Engine::load(chat, embed, std::time::Duration::from_secs_f64(idle)).map(Some)
    }

    /// Without an embedding model, embeddings are the offline lexical ones. Loaded weights are freed after
    /// `idle` without requests.
    pub fn load(chat: Option<PathBuf>, embed: Option<PathBuf>, idle: std::time::Duration) -> Result<Engine> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let llm = chat.is_some();
        std::thread::Builder::new().name("engine".into()).spawn(move || match serve(chat, embed, idle, rx, &ready_tx) {
            Ok(Some(ack)) => drop(ack.send(())), // models dropped when `serve` returned
            Ok(None) => {}
            Err(e) => drop(ready_tx.send(Err(e))),
        })?;
        let dim = ready_rx.recv().map_err(|_| anyhow!("engine thread died while loading"))??;
        ENGINES.lock().unwrap().push(tx.clone());
        Ok(Engine { tx: Mutex::new(tx), dim, llm })
    }

    fn send(&self, r: Req) -> Result<()> {
        self.tx.lock().unwrap().send(r).map_err(|_| anyhow!("engine thread is gone"))
    }
}

/// Frees the models now rather than at the idle timeout (the exit hook's copy of the sender would keep
/// the thread alive otherwise).
impl Drop for Engine {
    fn drop(&mut self) {
        let (ack, done) = channel();
        if self.send(Req::Stop(ack)).is_ok() {
            let _ = done.recv_timeout(std::time::Duration::from_secs(5));
        }
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
        if !self.generate_stream(INTERACTIVE, prompt, "", usize::MAX, &mut |p| {
            text += p;
            true
        })? {
            bail!("the answer stopped early");
        }
        Ok(text)
    }

    fn batches(&self) -> bool {
        self.llm
    }

    fn generate_stream(&self, tier: u8, prompt: &str, prefix: &str, max_tokens: usize, piece: &mut dyn FnMut(&str) -> bool) -> Result<bool> {
        if !self.llm {
            bail!("no chat model loaded (set AXIOM_ENGINE_MODEL)");
        }
        let (out, rx) = channel();
        self.send(Req::Generate { tier, prompt: prompt.into(), prefix: prefix.into(), max_tokens, out })?;
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
/// models dropped here are freed). Weights load on first use and are freed after `idle` without requests,
/// so an idle engine holds no GPU memory and does no work.
fn serve(chat: Option<PathBuf>, embed: Option<PathBuf>, idle: std::time::Duration, rx: Receiver<Req>, ready: &Sender<Result<usize>>) -> Result<Option<Sender<()>>> {
    let backend = BACKEND.get_or_init(|| {
        // llama.cpp and ggml (Metal setup included) log to stderr unless redirected
        llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default().with_logs_enabled(false));
        LlamaBackend::init().expect("llama.cpp initialises once per process")
    });
    // the embedding size comes from the GGUF metadata alone, so no weights load at start
    let dim = match &embed {
        Some(p) => {
            let m = LlamaModel::load_from_file(backend, p, &LlamaModelParams::default().with_vocab_only(true))
                .map_err(|e| anyhow!("{}: {e}", p.display()))?;
            let arch = m.meta_val_str("general.architecture")?;
            m.meta_val_str(&format!("{arch}.embedding_length"))?.parse()?
        }
        None => core_ai::LocalBackend::offline().dim(),
    };
    let _ = ready.send(Ok(dim));
    while let Ok(first) = rx.recv() {
        match run(backend, &chat, &embed, first, &rx, idle) {
            Exit::Idle => {} // everything loaded in `run` is freed
            Exit::Stop(ack) => return Ok(Some(ack)),
            Exit::Closed => return Ok(None),
        }
    }
    Ok(None)
}

enum Exit {
    Idle,
    Stop(Sender<()>),
    Closed,
}

fn load(backend: &LlamaBackend, path: &PathBuf) -> Result<LlamaModel> {
    let m = LlamaModel::load_from_file(backend, path, &LlamaModelParams::default().with_n_gpu_layers(999))
        .map_err(|e| anyhow!("{}: {e}", path.display()))?;
    // registered after ggml set Metal up, so it runs before ggml's own teardown
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| platform::at_exit(release_gpu));
    Ok(m)
}

type Out = Sender<Result<Option<String>>>;

/// A generation being streamed to a caller: it advances one token per batch step.
struct Stream {
    seq: usize,
    /// Tokens left in this request.
    left: usize,
    out: Out,
}

/// A request waiting for a sequence slot.
struct Wait {
    tier: u8,
    arrived: u64,
    prompt: String,
    prefix: String,
    max_tokens: usize,
    out: Out,
}

/// The chat model's context, its sequences, the streams running on them and the requests waiting for one.
struct Chat<'m> {
    m: &'m LlamaModel,
    ctx: LlamaContext<'m>,
    /// KV cells in the context.
    cells: i32,
    /// Longest prompt plus answer: the context or the model's trained length, whichever is smaller.
    limit: i32,
    seqs: Vec<Option<Seq>>,
    streams: Vec<Stream>,
    waiting: Vec<Wait>,
}

impl Chat<'_> {
    /// Starts waiting requests while a slot is free, higher tiers (then earlier ones) first.
    fn admit(&mut self, tick: u64) {
        self.waiting.sort_by_key(|w| (w.tier, w.arrived));
        while let Some(w) = self.waiting.first() {
            let busy = |i: usize| self.streams.iter().any(|st| st.seq == i);
            let paused = (0..SEQS).find(|&i| !busy(i) && self.seqs[i].as_ref()
                .is_some_and(|s| s.prompt == w.prompt && s.text == w.prefix && s.pending.is_some()));
            // else a free slot, else the least recently used paused one
            let Some(id) = paused.or_else(|| (0..SEQS).filter(|&i| !busy(i)).min_by_key(|&i| self.seqs[i].as_ref().map_or(0, |s| s.used))) else {
                break;
            };
            let w = self.waiting.remove(0);
            let st = Stream { seq: id, left: w.max_tokens, out: w.out };
            if paused.is_some() {
                self.seqs[id].as_mut().unwrap().used = tick;
                self.streams.push(st);
                continue;
            }
            match self.prefill(id, &w.prompt, &w.prefix, tick) {
                Ok(t) => {
                    // a failure here closes the stream; its caller sees the answer stop early
                    if let Ok(Some(st)) = self.deliver(st, t) {
                        self.streams.push(st);
                    }
                }
                Err(e) => drop(st.out.send(Err(e))),
            }
        }
    }

    fn free(&mut self, id: usize) {
        let _ = self.ctx.kv_cache_seq_rm(id as i32, None, None);
        self.seqs[id] = None;
    }

    /// Makes room for `n` more KV cells by dropping paused sequences, least recently used first.
    /// False when the running ones alone leave no room.
    fn room(&mut self, n: i32) -> bool {
        loop {
            let used: i32 = self.seqs.iter().flatten().map(|s| s.pos).sum();
            if used + n <= self.cells {
                return true;
            }
            let running = |i: usize| self.streams.iter().any(|st| st.seq == i);
            let Some(id) = (0..SEQS).filter(|&i| !running(i) && self.seqs[i].is_some()).min_by_key(|&i| self.seqs[i].as_ref().unwrap().used) else {
                return false;
            };
            self.free(id);
        }
    }

    /// Reads prompt and prefix into sequence `id` and samples its first token.
    fn prefill(&mut self, id: usize, prompt: &str, prefix: &str, tick: u64) -> Result<LlamaToken> {
        self.free(id);
        let tmpl = self.m.chat_template(None).map_err(|e| anyhow!("model has no chat template: {e}"))?;
        let text = self.m.apply_chat_template(&tmpl, &[LlamaChatMessage::new("user".into(), prompt.into())?], true)? + prefix;
        let tokens = self.m.vocab().tokenize(text.as_bytes(), true, true);
        if tokens.len() as i32 >= self.limit {
            bail!("the prompt is {} tokens; this model and context hold {}", tokens.len(), self.limit);
        }
        if !self.room(tokens.len() as i32) {
            bail!("the context is full with running answers; try again shortly");
        }
        let mut pos = 0;
        for (n, chunk) in tokens.chunks(BATCH).enumerate() {
            let mut batch = LlamaBatch::new(chunk.len(), 1);
            let last = n * BATCH + chunk.len() == tokens.len();
            for (i, t) in chunk.iter().enumerate() {
                batch.add(*t, pos, &[id as i32], last && i + 1 == chunk.len())?;
                pos += 1;
            }
            self.ctx.decode(&mut batch)?;
        }
        let mut sampler = LlamaSampler::chain_simple([LlamaSampler::top_k(40), LlamaSampler::top_p(0.95, 1),
                                                      LlamaSampler::temp(0.8), LlamaSampler::dist(tick as u32)]);
        let first = sampler.sample(&self.ctx, -1);
        self.seqs[id] = Some(Seq { prompt: prompt.into(), text: prefix.into(), partial: vec![], pos, pending: None, sampler, used: tick });
        Ok(first)
    }

    /// One token for every running stream, in a single batch.
    fn step(&mut self) -> Result<()> {
        // with no paused answer left to drop, the longest running one ends to make room
        while !self.room(self.streams.len() as i32) {
            let k = (0..self.streams.len()).max_by_key(|&k| self.seqs[self.streams[k].seq].as_ref().unwrap().pos).unwrap();
            let st = self.streams.remove(k);
            self.free(st.seq);
            let _ = st.out.send(Err(anyhow!("the context is full; the answer was cut off")));
        }
        if self.streams.is_empty() {
            return Ok(());
        }
        let mut batch = LlamaBatch::new(self.streams.len(), 1);
        for st in &self.streams {
            let s = self.seqs[st.seq].as_mut().unwrap();
            batch.add(s.pending.take().expect("a running stream has a token to decode"), s.pos, &[st.seq as i32], true)?;
            s.pos += 1;
        }
        self.ctx.decode(&mut batch)?;
        for (i, st) in std::mem::take(&mut self.streams).into_iter().enumerate() {
            let t = self.seqs[st.seq].as_mut().unwrap().sampler.sample(&self.ctx, i as i32);
            if let Some(st) = self.deliver(st, t)? {
                self.streams.push(st);
            }
        }
        Ok(())
    }

    /// Hands sampled token `t` to its stream's caller; returns the stream while it keeps running. A finished
    /// answer frees its sequence; a used-up request or a gone caller leaves it paused.
    fn deliver(&mut self, mut st: Stream, t: LlamaToken) -> Result<Option<Stream>> {
        let s = self.seqs[st.seq].as_mut().unwrap();
        if self.m.vocab().is_eog(t) || s.pos >= self.limit {
            self.free(st.seq);
            let _ = st.out.send(Ok(None));
            return Ok(None);
        }
        s.pending = Some(t);
        s.partial.extend(self.m.vocab().token_to_piece(t, false, None));
        // emit whole characters only; a split one waits for its next token
        let valid = match std::str::from_utf8(&s.partial) {
            Ok(v) => v.len(),
            Err(e) => e.valid_up_to(),
        };
        let piece = String::from_utf8(s.partial.drain(..valid).collect()).unwrap();
        if !piece.is_empty() {
            s.text += &piece;
            if st.out.send(Ok(Some(piece))).is_err() {
                return Ok(None);
            }
        }
        st.left = st.left.saturating_sub(1);
        Ok((st.left > 0).then_some(st))
    }

    /// After a failed step: tell every running caller and drop their sequences.
    fn fail(&mut self, e: &anyhow::Error) {
        for st in std::mem::take(&mut self.streams) {
            self.free(st.seq);
            let _ = st.out.send(Err(anyhow!("{e}")));
        }
    }
}

/// Serves requests, starting with `first`, until none comes for `idle` and nothing is generating. Models
/// and contexts load when a request first needs them and are dropped on return.
fn run(backend: &LlamaBackend, chat: &Option<PathBuf>, embed: &Option<PathBuf>, first: Req, rx: &Receiver<Req>,
       idle: std::time::Duration) -> Exit {
    use std::cell::OnceCell;
    use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
    let (chat_model, embed_model) = (OnceCell::<LlamaModel>::new(), OnceCell::<LlamaModel>::new());
    let (mut live, mut emb_ctx): (Option<Chat>, Option<LlamaContext>) = (None, None);
    let (mut tick, mut next) = (0u64, Some(first));
    loop {
        // requests come in between steps; wait for one only when nothing is generating
        let busy = live.as_ref().is_some_and(|c| !c.streams.is_empty() || !c.waiting.is_empty());
        let req = match next.take() {
            Some(r) => Some(r),
            None if busy => match rx.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return Exit::Closed,
            },
            None => match rx.recv_timeout(idle) {
                Ok(r) => Some(r),
                Err(RecvTimeoutError::Timeout) => return Exit::Idle,
                Err(RecvTimeoutError::Disconnected) => return Exit::Closed,
            },
        };
        let Some(req) = req else {
            let c = live.as_mut().unwrap();
            c.admit(tick);
            if !c.streams.is_empty() {
                if let Err(e) = c.step() {
                    c.fail(&e);
                }
            }
            continue;
        };
        tick += 1;
        match req {
            Req::Stop(ack) => return Exit::Stop(ack),
            Req::Embed { texts, out } => {
                let r = (|| -> Result<Vec<Vec<f32>>> {
                    let Some(path) = embed else { return core_ai::LocalBackend::offline().embed(&texts) };
                    if emb_ctx.is_none() {
                        if embed_model.get().is_none() {
                            let _ = embed_model.set(load(backend, path)?);
                        }
                        let p = LlamaContextParams::default().with_embeddings(true).with_n_ctx(NonZeroU32::new(BATCH as u32))
                            .with_n_batch(BATCH as u32).with_n_ubatch(BATCH as u32).with_n_seq_max(EMBED_SEQS as u32)
                            .with_kv_unified(true); // any one text may use the whole context
                        emb_ctx = Some(embed_model.get().unwrap().new_context(backend, p).map_err(|e| anyhow!("embedding context: {e}"))?);
                    }
                    let (ctx, m) = (emb_ctx.as_mut().unwrap(), embed_model.get().unwrap());
                    embed_all(ctx, m, &texts)
                })();
                let _ = out.send(r);
            }
            Req::Generate { tier, prompt, prefix, max_tokens, out } => {
                let r = (|| -> Result<()> {
                    let path = chat.as_ref().ok_or_else(|| anyhow!("no chat model (set AXIOM_ENGINE_MODEL)"))?;
                    if live.is_none() {
                        if chat_model.get().is_none() {
                            let _ = chat_model.set(load(backend, path)?);
                        }
                        let m = chat_model.get().unwrap();
                        let cells = std::env::var("AXIOM_ENGINE_CTX").ok().and_then(|v| v.parse().ok()).unwrap_or(CTX_TOKENS);
                        let p = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(cells)).with_n_seq_max(SEQS as u32)
                            .with_kv_unified(true).with_n_batch(BATCH as u32);
                        let ctx = m.new_context(backend, p).map_err(|e| anyhow!("chat context: {e}"))?;
                        let (cells, limit) = (ctx.n_ctx() as i32, ctx.n_ctx().min(m.n_ctx_train()) as i32);
                        live = Some(Chat { m, ctx, cells, limit, seqs: (0..SEQS).map(|_| None).collect(), streams: vec![], waiting: vec![] });
                    }
                    Ok(())
                })();
                match r {
                    Ok(()) => live.as_mut().unwrap().waiting.push(Wait { tier, arrived: tick, prompt, prefix, max_tokens, out }),
                    Err(e) => drop(out.send(Err(e))),
                }
            }
        }
    }
}

/// Embeddings of `texts` (each cut to `BATCH` tokens), L2-normalised, with the model's own pooling. Texts
/// share a decode: up to `EMBED_SEQS` of them per batch, one sequence each.
fn embed_all(ctx: &mut LlamaContext, m: &LlamaModel, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let tokens: Vec<_> = texts.iter().map(|t| {
        let mut v = m.vocab().tokenize(t.as_bytes(), true, false);
        v.truncate(BATCH);
        v
    }).collect();
    let mut out = Vec::with_capacity(texts.len());
    let mut i = 0;
    while i < tokens.len() {
        // as many texts as fit in one batch
        let (mut n, mut size) = (0, 0);
        while i + n < tokens.len() && n < EMBED_SEQS && size + tokens[i + n].len() <= BATCH {
            size += tokens[i + n].len();
            n += 1;
        }
        ctx.clear_kv_cache();
        let mut batch = LlamaBatch::new(size.max(1), 1);
        for (seq, toks) in tokens[i..i + n].iter().enumerate() {
            for (pos, t) in toks.iter().enumerate() {
                batch.add(*t, pos as i32, &[seq as i32], true)?;
            }
        }
        ctx.decode(&mut batch)?;
        for seq in 0..n {
            let v = ctx.embeddings_seq_ith(seq as i32).map_err(|e| anyhow!("no pooled embedding: {e}"))?;
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            out.push(v.iter().map(|x| x / norm).collect());
        }
        i += n;
    }
    Ok(out)
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
