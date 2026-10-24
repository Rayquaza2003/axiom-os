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
//! Before a chat model loads, the engine plans for this machine: its GGUF metadata gives the weights and
//! the context's memory, and the budget is the memory available now (halved under memory pressure; capped
//! by `AXIOM_ENGINE_MEMORY` MB) and the GPU's working set. Too little memory puts fewer layers on the GPU;
//! the rest run on the CPU from the memory-mapped file, which the OS reads and drops as needed. The context
//! shrinks only when its KV cache alone would crowd memory.
//!
//! Models load when a request first needs one (an embedding never loads the chat model). Idle for
//! `AXIOM_ENGINE_IDLE` seconds (default 120), the contexts are freed but the models stay mapped; idle for
//! `AXIOM_ENGINE_UNLOAD` seconds (default 900), or as soon as macOS reports memory pressure, the models go
//! too. Their files stay in the OS file cache, so the next load reads memory, not disk.
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
    /// What is loaded and why; written by the engine thread.
    state: std::sync::Arc<Mutex<String>>,
}

/// How long an idle engine keeps things: contexts (KV caches, compute buffers) and models.
#[derive(Clone, Copy)]
pub struct Idle {
    pub context: std::time::Duration,
    pub model: std::time::Duration,
}

/// What a chat model needs, from its GGUF metadata.
#[derive(Debug)]
pub struct Need {
    pub bytes: u64,
    pub layers: u32,
    pub kv_per_token: u64,
}

/// What this machine can give now.
#[derive(Debug)]
pub struct Budget {
    /// The GPU's free working set (0 without a GPU).
    pub gpu_free: u64,
    /// Memory available without pushing other programs to swap.
    pub available: u64,
    /// 1 normal, 2 warning, 4 critical.
    pub pressure: u8,
}

#[derive(Debug, PartialEq)]
pub struct Plan {
    pub gpu_layers: u32,
    pub ctx: u32,
    pub note: String,
}

/// Compute buffers and the rest of a loaded model besides weights and KV cache.
const OVERHEAD: u64 = 512 << 20;
/// The context never shrinks below this.
const MIN_CTX: u32 = 2048;

/// How to load a model that needs `need` on a machine that has `b`, wanting `want_ctx` tokens of context.
pub fn plan(need: &Need, b: &Budget, want_ctx: u32) -> Plan {
    let room = if b.pressure >= 2 { b.available / 2 } else { b.available }.saturating_sub(OVERHEAD);
    let kv = |c: u32| need.kv_per_token * c as u64;
    // The weights are mapped from their file, which the OS can page, so a model too big for memory gets
    // fewer GPU layers rather than a shorter context. Only a context whose KV cache would take more than a
    // quarter of the room shrinks: it is a real allocation, and shrinking it breaks long prompts.
    let mut ctx = want_ctx.max(MIN_CTX);
    while ctx > MIN_CTX && kv(ctx) > room / 4 {
        ctx /= 2;
    }
    let ctx_note = if ctx < want_ctx { format!("{ctx}-token context (shrunk from {want_ctx} to fit)") } else { format!("{ctx}-token context") };
    let gpu = room.min(b.gpu_free);
    if need.bytes + kv(ctx) <= gpu {
        return Plan { gpu_layers: need.layers, ctx, note: format!("all {} layers on the GPU, {ctx_note}", need.layers) };
    }
    let per_layer = (need.bytes / need.layers.max(1) as u64).max(1);
    let layers = (gpu.saturating_sub(kv(ctx)) / per_layer).min(need.layers as u64) as u32;
    let note = match layers {
        0 => format!("no room on the GPU: all {} layers run on the CPU, read from disk as needed (slow; a smaller model or AXIOM_OFFLOAD helps), {ctx_note}", need.layers),
        n => format!("{n} of {} layers on the GPU, the rest on the CPU read from disk as needed, {ctx_note}", need.layers),
    };
    Plan { gpu_layers: layers, ctx, note }
}

/// A chat model's needs, read from its header without loading weights.
fn need(backend: &LlamaBackend, path: &PathBuf) -> Result<Need> {
    let m = LlamaModel::load_from_file(backend, path, &LlamaModelParams::default().with_vocab_only(true))
        .map_err(|e| anyhow!("{}: {e}", path.display()))?;
    let arch = m.meta_val_str("general.architecture")?;
    let get = |k: &str| m.meta_val_str(&format!("{arch}.{k}")).ok().and_then(|v| v.parse::<u64>().ok());
    let layers = get("block_count").ok_or_else(|| anyhow!("{}: no layer count", path.display()))?;
    let heads = get("attention.head_count").unwrap_or(1).max(1);
    let head_dim = get("attention.key_length").or_else(|| Some(get("embedding_length")? / heads)).unwrap_or(128);
    let kv_heads = get("attention.head_count_kv").unwrap_or(heads);
    Ok(Need {
        bytes: platform::file_len(path).unwrap_or(0),
        layers: layers as u32,
        kv_per_token: 2 * layers * kv_heads * head_dim * 2, // K and V, f16
    })
}

fn budget() -> Budget {
    let mem = platform::memory().unwrap_or(platform::Memory { total: 0, available: 0, pressure: 1 });
    let gpu_free = llama_cpp_2::list_llama_ggml_backend_devices().iter()
        .filter(|d| !d.backend.eq_ignore_ascii_case("CPU")).map(|d| d.memory_free as u64).max().unwrap_or(0);
    // AXIOM_ENGINE_MEMORY (MB) caps what the engine may take, below what the machine has
    let cap = std::env::var("AXIOM_ENGINE_MEMORY").ok().and_then(|v| v.parse::<u64>().ok()).map_or(u64::MAX, |mb| mb << 20);
    Budget { gpu_free: gpu_free.min(cap), available: mem.available.min(cap), pressure: mem.pressure }
}

impl Engine {
    /// Loads the models named by `AXIOM_ENGINE_MODEL` / `AXIOM_ENGINE_EMBED`; `None` when neither is set.
    pub fn from_env() -> Result<Option<Engine>> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let (chat, embed) = (env("AXIOM_ENGINE_MODEL"), env("AXIOM_ENGINE_EMBED"));
        if chat.is_none() && embed.is_none() {
            return Ok(None);
        }
        let secs = |k: &str, d: f64| std::time::Duration::from_secs_f64(env(k).and_then(|v| v.parse().ok()).unwrap_or(d));
        let idle = Idle { context: secs("AXIOM_ENGINE_IDLE", 120.0), model: secs("AXIOM_ENGINE_UNLOAD", 900.0) };
        let (chat, embed) = (chat.map(|s| resolve(&s)).transpose()?, embed.map(|s| resolve(&s)).transpose()?);
        Engine::load(chat, embed, idle).map(Some)
    }

    /// Without an embedding model, embeddings are the offline lexical ones.
    pub fn load(chat: Option<PathBuf>, embed: Option<PathBuf>, idle: Idle) -> Result<Engine> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let llm = chat.is_some();
        let state = std::sync::Arc::new(Mutex::new("nothing loaded yet".to_string()));
        let st = state.clone();
        std::thread::Builder::new().name("engine".into()).spawn(move || match serve(chat, embed, idle, rx, &ready_tx, &st) {
            Ok(Some(ack)) => drop(ack.send(())), // models dropped when `serve` returned
            Ok(None) => {}
            Err(e) => drop(ready_tx.send(Err(e))),
        })?;
        let dim = ready_rx.recv().map_err(|_| anyhow!("engine thread died while loading"))??;
        ENGINES.lock().unwrap().push(tx.clone());
        Ok(Engine { tx: Mutex::new(tx), dim, llm, state })
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

    fn describe(&self) -> String {
        format!("engine: {}", self.state.lock().unwrap())
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
/// models dropped here are freed).
fn serve(chat: Option<PathBuf>, embed: Option<PathBuf>, idle: Idle, rx: Receiver<Req>, ready: &Sender<Result<usize>>,
         state: &Mutex<String>) -> Result<Option<Sender<()>>> {
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
        match run(backend, &chat, &embed, first, &rx, idle, state) {
            Exit::Idle => *state.lock().unwrap() = "models unloaded; their files stay in the OS cache for a quick reload".into(),
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

fn load(backend: &LlamaBackend, path: &PathBuf, gpu_layers: u32) -> Result<LlamaModel> {
    let m = LlamaModel::load_from_file(backend, path, &LlamaModelParams::default().with_n_gpu_layers(gpu_layers))
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

/// Serves requests, starting with `first`. Models and contexts load when a request first needs them.
/// Idle past `idle.context` the contexts go; past `idle.model`, or under memory pressure, this returns and
/// the models go too.
fn run(backend: &LlamaBackend, chat: &Option<PathBuf>, embed: &Option<PathBuf>, first: Req, rx: &Receiver<Req>,
       idle: Idle, state: &Mutex<String>) -> Exit {
    use std::cell::OnceCell;
    use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
    use std::time::{Duration, Instant};
    let (chat_model, embed_model) = (OnceCell::<LlamaModel>::new(), OnceCell::<LlamaModel>::new());
    let (mut live, mut emb_ctx): (Option<Chat>, Option<LlamaContext>) = (None, None);
    let (mut tick, mut next, mut last) = (0u64, Some(first), Instant::now());
    let mut chat_plan: Option<Plan> = None;
    let set = |s: String| *state.lock().unwrap() = s;
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
            // idle: wake every 10 s to free what has waited long enough, or everything under pressure
            None => match rx.recv_timeout(Duration::from_secs(10).min(idle.context)) {
                Ok(r) => Some(r),
                Err(RecvTimeoutError::Timeout) => {
                    let waited = last.elapsed();
                    if waited >= idle.model || platform::memory_pressure() >= 2 {
                        return Exit::Idle;
                    }
                    if waited >= idle.context && (live.is_some() || emb_ctx.is_some()) {
                        (live, emb_ctx) = (None, None);
                        set(format!("idle: contexts freed, models kept mapped ({})", chat_plan.as_ref().map_or("embeddings only", |p| &p.note)));
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return Exit::Closed,
            },
        };
        last = Instant::now();
        let Some(req) = req else {
            last = Instant::now(); // idle time counts from the last token, not the last request
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
                            let _ = embed_model.set(load(backend, path, 999)?);
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
                            let want = std::env::var("AXIOM_ENGINE_CTX").ok().and_then(|v| v.parse().ok()).unwrap_or(CTX_TOKENS);
                            let (pl, t) = (plan(&need(backend, path)?, &budget(), want), Instant::now());
                            let _ = chat_model.set(load(backend, path, pl.gpu_layers)?);
                            set(format!("chat model loaded in {:.1} s: {}", t.elapsed().as_secs_f64(), pl.note));
                            chat_plan = Some(pl);
                        }
                        let m = chat_model.get().unwrap();
                        let cells = chat_plan.as_ref().map_or(CTX_TOKENS, |p| p.ctx);
                        let p = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(cells)).with_n_seq_max(SEQS as u32)
                            .with_kv_unified(true).with_n_batch(BATCH as u32);
                        let ctx = m.new_context(backend, p).map_err(|e| anyhow!("chat context: {e}"))?;
                        let (cells, limit) = (ctx.n_ctx() as i32, ctx.n_ctx().min(m.n_ctx_train()) as i32);
                        set(format!("in use: {}", chat_plan.as_ref().map_or("", |p| &p.note)));
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
    use super::{plan, Budget, Need};

    const GB: u64 = 1 << 30;

    #[test]
    fn plans_fit_the_model_to_the_machine() {
        // a 3B model: 1.8 GB of weights, 36 layers, 36 KB of KV cache per token
        let need = Need { bytes: 1_800_000_000, layers: 36, kv_per_token: 36_864 };
        let roomy = plan(&need, &Budget { gpu_free: 5 * GB, available: 6 * GB, pressure: 1 }, 8192);
        assert_eq!((roomy.gpu_layers, roomy.ctx), (36, 8192), "{}", roomy.note);
        // 2.5 GB free: the 300 MB context still fits next to the weights
        let tight = plan(&need, &Budget { gpu_free: 5 * GB, available: 5 * GB / 2, pressure: 1 }, 8192);
        assert_eq!(tight.gpu_layers, 36, "{}", tight.note);
        // 2.2 GB free: some layers stay on the CPU, the context stays whole
        let tighter = plan(&need, &Budget { gpu_free: 5 * GB, available: 2_200_000_000, pressure: 1 }, 8192);
        assert!(tighter.ctx == 8192 && tighter.gpu_layers < 36 && tighter.gpu_layers > 0, "{tighter:?}");
        assert!(tighter.note.contains("of 36 layers on the GPU"), "{}", tighter.note);
        // 1.2 GB free: the KV cache would crowd memory, so the context shrinks too
        let small = plan(&need, &Budget { gpu_free: 5 * GB, available: 1_200_000_000, pressure: 1 }, 8192);
        assert!(small.ctx < 8192 && small.note.contains("shrunk"), "{small:?}");
        // under memory pressure only half the available memory counts
        let pressed = plan(&need, &Budget { gpu_free: 5 * GB, available: 3 * GB, pressure: 2 }, 8192);
        assert!(pressed.gpu_layers < 36, "{pressed:?}");
        let none = plan(&need, &Budget { gpu_free: 0, available: GB, pressure: 1 }, 8192);
        assert_eq!((none.gpu_layers, none.ctx), (0, 2048));
        assert!(none.note.contains("run on the CPU"), "{}", none.note);
    }

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
