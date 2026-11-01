//! Neural Resource Manager: one worker, strict priority tiers with aging.
//!
//! Preemption is cooperative. `embed` queues one job per `EMBED_CHUNK` texts, so a higher-priority job
//! waits for at most one chunk. `generate` streams in slices of `SLICE_TOKENS`: between slices, if a
//! higher-tier job is waiting, the generation goes back in the queue and later resumes after the text
//! it already produced. It ages from when it went back: had it kept the age of its request, a background
//! answer older than `aging_s` would win the queue against every fresh interactive job, yield after one
//! slice, and win again (measured: an interactive request waited 11.7 s behind a 110 s background chat). Slices end cleanly on the server, so nothing generated is
//! thrown away (cancelling a stream instead costs Ollama 0.4-1.3 s). Backends that cannot stream (the
//! `generate_stream` default) run to the end.
//!
//! Reading a long prompt (prefill) cannot be sliced by tokens, and at ~90 tokens/s a 10k-token agent prompt
//! would hold the worker for ~110 s. So a BACKGROUND job reads its prompt in steps of about `PREFILL_S`: each
//! step sends the prompt cut there and asks for one token, which leaves the read part in the server's prefix
//! cache (Ollama keeps it across other requests), and checks for higher-tier work between steps. Steps are
//! sized by time, not characters: digit-heavy text runs ~2 chars a token against ~4 for prose, and prefill
//! slows as the context grows (measured on an M1 with 8 GB: 90 tokens/s near empty, 50 at 2k tokens).
//!
//! ponytail: a resumed generation re-sends its prompt and partial answer, and the server re-reads them
//! when other work used its cache meanwhile; an engine that keeps paused sequences in memory avoids that.
//!
//! With axion running, the NRM that matters is the daemon's: a client process's backend forwards each
//! job's tier there, so all processes on the machine share one queue.
use crate::NpuBackend;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Condvar, Mutex};

pub const REALTIME: u8 = 0;
pub const INTERACTIVE: u8 = 1;
pub const BACKGROUND: u8 = 2;

const EMBED_CHUNK: usize = 16;

struct Job {
    tier: u8,
    /// When this job entered the queue; priority ages from here.
    enqueued: f64,
    run: Box<dyn FnOnce() + Send>,
}

type Queue = Arc<(Mutex<Vec<Job>>, Condvar)>;

/// Finished jobs kept for `recent`.
const LOG: usize = 1024;

pub struct Nrm {
    pub backend: Arc<dyn NpuBackend>,
    q: Queue,
    done: Arc<Mutex<VecDeque<(u8, f64, f64)>>>,
}

/// Tokens a generation runs before it checks for higher-tier work: about 0.4 s for a 3B model on an M1.
/// Measured on Ollama, 16-token slices of an uninterrupted answer cost nothing (its prompt cache continues
/// them) and give the same text.
const SLICE_TOKENS: usize = 8;

/// Seconds a BACKGROUND prefill step aims to take: what a higher tier may wait for it.
const PREFILL_S: f64 = 2.0;
/// Characters of the first prefill step, and the least of any.
const PREFILL_FIRST: usize = 512;
const PREFILL_MIN: usize = 128;

/// How far a BACKGROUND job has read its prompt, and its next step in chars.
#[derive(Clone, Copy)]
struct Prefill {
    read: usize,
    step: usize,
}

impl Default for Prefill {
    fn default() -> Self {
        Prefill { read: 0, step: PREFILL_FIRST }
    }
}

impl Prefill {
    /// The next cut of a `len`-char prompt, or `None` when the generation should read the rest itself. Only
    /// BACKGROUND jobs split their prompt; the others would pay its round trips for nothing.
    fn next_cut(&self, tier: u8, len: usize) -> Option<usize> {
        (tier == BACKGROUND && self.read + self.step < len).then_some(self.read + self.step)
    }

    /// Records a step that read up to `cut` in `took` s, and sizes the next to take `PREFILL_S` at that
    /// speed. It at most doubles, since a step the server had cached is fast without saying anything.
    fn done(&mut self, cut: usize, took: f64) {
        self.read = cut;
        self.step = ((self.step as f64 * PREFILL_S / took.max(1e-3)) as usize).clamp(PREFILL_MIN, self.step * 2);
    }
}

/// Characters of text in a chat's messages.
fn chat_chars(body: &Value) -> usize {
    body["messages"].as_array().map_or(0, |m| m.iter().map(|m| m["content"].as_str().map_or(0, str::len)).sum())
}

/// `body` with its messages cut after `n` chars of text, asking for one token: a prefill step.
fn chat_cut(body: &Value, mut n: usize) -> Value {
    let mut b = body.clone();
    let mut kept = vec![];
    for m in body["messages"].as_array().into_iter().flatten() {
        let mut m = m.clone();
        if let Some(c) = m["content"].as_str() {
            if c.len() >= n {
                m["content"] = json!(&c[..c.floor_char_boundary(n)]);
                kept.push(m);
                break;
            }
            n -= c.len();
        }
        kept.push(m);
    }
    b["messages"] = json!(kept);
    b["max_tokens"] = json!(1);
    b
}

/// Where a sliced chat stands between jobs.
#[derive(Default)]
struct ChatState {
    prefill: Prefill,
    /// Answer text so far.
    prefix: String,
    /// Tokens generated so far, against the client's `max_tokens`.
    used: usize,
}

/// Runs a chat's steps until it ends (`Some(choice)`) or a higher tier waits (`None`).
fn chat_steps(b: &dyn NpuBackend, q: &Queue, tier: u8, body: &Value, st: &mut ChatState) -> Result<Option<Value>> {
    let limit = body["max_tokens"].as_u64().map_or(usize::MAX, |n| n as usize);
    loop {
        if let Some(cut) = st.prefill.next_cut(tier, chat_chars(body)).filter(|_| st.prefix.is_empty()) {
            let t = platform::now();
            b.chat(tier, &chat_cut(body, cut))?;
            st.prefill.done(cut, platform::now() - t);
        } else {
            let (left, mut req) = (limit - st.used, body.clone());
            if !st.prefix.is_empty() {
                req["messages"].as_array_mut().ok_or_else(|| anyhow!("messages must be a list"))?
                    .push(json!({"role": "assistant", "content": st.prefix}));
            }
            let slice = SLICE_TOKENS.min(left);
            req["max_tokens"] = json!(slice);
            let mut choice = b.chat(tier, &req)?;
            let piece = choice["message"]["content"].as_str().unwrap_or("").to_string();
            let cut_off = choice["finish_reason"] == "length" && slice < left;
            if cut_off && piece.is_empty() && choice["message"]["tool_calls"].is_null() {
                // the slice ended inside a tool call, which the server drops when cut: run this one to its end
                match body.get("max_tokens") {
                    Some(_) => req["max_tokens"] = json!(left),
                    None => drop(req.as_object_mut().unwrap().remove("max_tokens")),
                }
                choice = b.chat(tier, &req)?;
            } else if cut_off {
                st.prefix += &piece;
                st.used += slice;
                if outranked(q, tier) {
                    return Ok(None);
                }
                continue;
            }
            let rest = choice["message"]["content"].as_str().unwrap_or("").to_string();
            choice["message"]["content"] = json!(st.prefix.clone() + &rest);
            return Ok(Some(choice));
        }
        if outranked(q, tier) {
            return Ok(None);
        }
    }
}

fn push_log(log: &Mutex<VecDeque<(u8, f64, f64)>>, job: (u8, f64, f64)) {
    let mut log = log.lock().unwrap();
    if log.len() == LOG {
        log.pop_front();
    }
    log.push_back(job);
}

/// Lower runs first: the tier, minus one per `aging_s` waited.
fn rank(tier: u8, enqueued: f64, now: f64, aging_s: f64) -> f64 {
    tier as f64 - (now - enqueued) / aging_s
}

/// Whether a job of a strictly higher tier is waiting. Aging only orders the queue: an old background
/// generation still yields to a fresh interactive one, and a whole slice runs every time it is picked.
fn outranked(q: &Queue, tier: u8) -> bool {
    q.0.lock().unwrap().iter().any(|j| j.tier < tier)
}

impl Nrm {
    /// A queued job gains one tier of priority per `aging_s` waited, so BACKGROUND cannot starve.
    pub fn new(backend: Arc<dyn NpuBackend>, aging_s: f64) -> Nrm {
        let q: Queue = Arc::new((Mutex::new(vec![]), Condvar::new()));
        let done = Arc::new(Mutex::new(VecDeque::with_capacity(LOG)));
        let (worker, log) = (q.clone(), done.clone());
        std::thread::spawn(move || loop {
            let (job, t) = {
                let mut jobs = worker.0.lock().unwrap();
                while jobs.is_empty() {
                    jobs = worker.1.wait(jobs).unwrap();
                }
                let t = platform::now();
                let key = |j: &Job| rank(j.tier, j.enqueued, t, aging_s);
                let i = (0..jobs.len()).min_by(|&a, &b| key(&jobs[a]).total_cmp(&key(&jobs[b]))).unwrap();
                (jobs.remove(i), t)
            };
            let (tier, waited) = (job.tier, t - job.enqueued);
            let _ = catch_unwind(AssertUnwindSafe(job.run)); // a panicking job must not kill the worker
            push_log(&log, (tier, waited, platform::now() - t));
        });
        Nrm { backend, q, done }
    }

    fn log(&self, tier: u8, waited: f64, ran: f64) {
        push_log(&self.done, (tier, waited, ran));
    }

    /// The last 1024 finished jobs as (tier, seconds queued, seconds running). A preempted generation
    /// counts once per run.
    pub fn recent(&self) -> Vec<(u8, f64, f64)> {
        self.done.lock().unwrap().iter().copied().collect()
    }

    pub fn submit<T: Send + 'static>(&self, tier: u8, f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
        let (tx, rx) = channel();
        let run = Box::new(move || {
            let _ = tx.send(f());
        });
        self.q.0.lock().unwrap().push(Job { tier, enqueued: platform::now(), run });
        self.q.1.notify_one();
        rx
    }

    pub fn embed(&self, tier: u8, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let jobs: Vec<_> = texts.chunks(EMBED_CHUNK).map(|c| {
            let (b, c) = (self.backend.clone(), c.to_vec());
            self.submit(tier, move || b.embed_at(tier, &c))
        }).collect();
        let mut out = vec![];
        for rx in jobs {
            out.extend(rx.recv().map_err(|_| anyhow!("embed job failed"))??);
        }
        Ok(out)
    }

    pub fn generate(&self, tier: u8, prompt: String) -> Result<String> {
        self.generate_with(tier, prompt, &mut |_| true)
    }

    /// `generate` that hands each piece of the answer to `on_piece` as it arrives; `on_piece` returns false
    /// to cancel (the caller went away). Preemption is invisible here: the pieces just pause.
    pub fn generate_with(&self, tier: u8, prompt: String, on_piece: &mut dyn FnMut(&str) -> bool) -> Result<String> {
        let (since, mut out) = (platform::now(), String::new());
        if self.backend.batches() {
            // it runs this alongside other generations and orders them by tier itself
            let mut cancelled = false;
            let done = self.backend.generate_stream(tier, &prompt, "", usize::MAX, &mut |piece| {
                out += piece;
                cancelled = !on_piece(piece);
                !cancelled
            });
            self.log(tier, 0.0, platform::now() - since);
            if cancelled {
                bail!("generation cancelled");
            }
            return if done? { Ok(out) } else { Err(anyhow!("the answer stopped early")) };
        }
        let mut pf = Prefill::default();
        loop {
            let (tx, rx) = channel::<String>();
            let (b, p, mut prefix, q) = (self.backend.clone(), prompt.clone(), out.clone(), self.q.clone());
            let job = self.submit(tier, move || -> Result<(bool, Prefill)> {
                while let Some(cut) = pf.next_cut(tier, p.len()).filter(|_| prefix.is_empty()) {
                    let (cut, t) = (p.floor_char_boundary(cut), platform::now());
                    b.generate_stream(tier, &p[..cut], "", 1, &mut |_| true)?;
                    pf.done(cut, platform::now() - t);
                    if outranked(&q, tier) {
                        return Ok((false, pf));
                    }
                }
                loop {
                    let (mut got, mut gone) = (String::new(), false);
                    let done = b.generate_stream(tier, &p, &prefix, SLICE_TOKENS, &mut |piece| {
                        got += piece;
                        gone = tx.send(piece.to_string()).is_err();
                        !gone
                    })?;
                    if done || gone || outranked(&q, tier) {
                        return Ok((done, pf));
                    }
                    if got.is_empty() {
                        bail!("the model stopped making progress");
                    }
                    prefix += &got;
                }
            });
            let mut cancelled = false;
            for piece in rx {
                out += &piece;
                if !on_piece(&piece) {
                    cancelled = true;
                    break; // drops the receiver, so the stream stops at its next piece
                }
            }
            let finished;
            (finished, pf) = job.recv().map_err(|_| anyhow!("generate job failed"))??;
            if cancelled {
                bail!("generation cancelled");
            }
            if finished {
                return Ok(out);
            }
        }
    }

    /// A tool-calling chat (`NpuBackend::chat`), sliced like `generate`: the answer runs `SLICE_TOKENS` at a
    /// time and resumes after its text. A tool call cannot resume (a cut one comes back empty), so once a
    /// slice ends inside one, that call runs to its end: a few seconds for a 3B model.
    pub fn chat(&self, tier: u8, body: Value) -> Result<Value> {
        let mut st = ChatState::default();
        loop {
            let (b, q, body) = (self.backend.clone(), self.q.clone(), body.clone());
            let job = self.submit(tier, move || {
                let r = chat_steps(&*b, &q, tier, &body, &mut st);
                (r, st)
            });
            let r;
            (r, st) = job.recv().map_err(|_| anyhow!("chat job failed"))?;
            if let Some(choice) = r? {
                return Ok(choice);
            }
        }
    }

    /// Jobs waiting per tier: [REALTIME, INTERACTIVE, BACKGROUND].
    pub fn queued(&self) -> [usize; 3] {
        let mut n = [0; 3];
        for j in self.q.0.lock().unwrap().iter() {
            n[(j.tier as usize).min(2)] += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Streams the answer word by word (a word is a token), 10 ms a word, resuming after any prefix it is
    /// given. "count" answers "1 2 ... 60"; anything else answers "ready". Records (prompt, prefix) per call.
    struct Words(Mutex<Vec<(String, String)>>);

    impl NpuBackend for Words {
        fn dim(&self) -> usize { 1 }
        fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> { unreachable!() }
        fn has_llm(&self) -> bool { true }
        fn generate(&self, _: &str) -> Result<String> { unreachable!("the NRM streams") }
        fn generate_stream(&self, _: u8, prompt: &str, prefix: &str, max_tokens: usize, piece: &mut dyn FnMut(&str) -> bool) -> Result<bool> {
            self.0.lock().unwrap().push((prompt.into(), prefix.into()));
            let words: Vec<String> = if prompt == "count" { (1..=60).map(|i| format!("{i} ")).collect() } else { vec!["ready".into()] };
            let (mut at, mut sent) = (0, 0);
            for w in words {
                at += w.len();
                if at <= prefix.len() {
                    continue;
                }
                if sent == max_tokens {
                    return Ok(false);
                }
                std::thread::sleep(Duration::from_millis(10));
                sent += 1;
                if !piece(&w) {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        /// The same answers as a chat; "tool" answers a 20-token tool call, which comes back empty when cut.
        fn chat(&self, tier: u8, body: &Value) -> Result<Value> {
            let m = body["messages"].as_array().unwrap();
            let last = m.last().unwrap();
            let (prompt, prefix) = match last["role"] == "assistant" {
                true => (m[m.len() - 2]["content"].as_str().unwrap(), last["content"].as_str().unwrap()),
                false => (last["content"].as_str().unwrap(), ""),
            };
            let max = body["max_tokens"].as_u64().map_or(usize::MAX, |n| n as usize);
            if prompt == "tool" {
                self.0.lock().unwrap().push((format!("tool/{max}"), prefix.into()));
                return Ok(match max < 20 {
                    true => json!({"message": {"role": "assistant", "content": ""}, "finish_reason": "length"}),
                    false => json!({"message": {"role": "assistant", "content": "", "tool_calls": [{"id": "c"}]}, "finish_reason": "tool_calls"}),
                });
            }
            let mut out = String::new();
            let done = self.generate_stream(tier, prompt, prefix, max, &mut |p| {
                out += p;
                true
            })?;
            Ok(json!({"message": {"role": "assistant", "content": out}, "finish_reason": if done { "stop" } else { "length" }}))
        }
    }

    fn user(text: &str) -> Value {
        json!({"messages": [{"role": "user", "content": text}]})
    }

    #[test]
    fn a_background_chat_yields_between_slices_and_a_cut_tool_call_reruns_whole() {
        let words = Arc::new(Words(Mutex::new(vec![])));
        let nrm = Arc::new(Nrm::new(words.clone(), 0.05));
        let n2 = nrm.clone();
        let bg = std::thread::spawn(move || n2.chat(BACKGROUND, user("count")).unwrap());
        // aging 0.05 s: the chat is 5 periods old when the interactive request comes
        std::thread::sleep(Duration::from_millis(250));
        let t = platform::now();
        assert_eq!(nrm.generate(INTERACTIVE, "word".into()).unwrap(), "ready");
        assert!(platform::now() - t < 0.1, "interactive waited {:.3}s behind a background chat", platform::now() - t);
        let full: String = (1..=60).map(|i| format!("{i} ")).collect();
        let choice = bg.join().unwrap();
        assert_eq!((choice["message"]["content"].as_str().unwrap(), &choice["finish_reason"]), (full.as_str(), &json!("stop")));

        words.0.lock().unwrap().clear();
        let choice = nrm.chat(INTERACTIVE, user("tool")).unwrap();
        assert_eq!(choice["message"]["tool_calls"][0]["id"], "c");
        let calls: Vec<String> = words.0.lock().unwrap().iter().map(|c| c.0.clone()).collect();
        assert_eq!(calls, [format!("tool/{SLICE_TOKENS}"), format!("tool/{}", usize::MAX)], "the cut call reran without a limit");
    }

    #[test]
    fn prefill_steps_follow_the_measured_speed() {
        let mut pf = Prefill::default();
        pf.done(512, 8.0); // 4x too slow: a quarter
        assert_eq!((pf.read, pf.step), (512, 128));
        pf.done(640, 30.0);
        assert_eq!(pf.step, PREFILL_MIN);
        pf.done(768, 0.5); // 4x too fast: only doubles
        assert_eq!(pf.step, 256);
        pf.done(1024, 1.0);
        assert_eq!(pf.step, 512);
    }

    #[test]
    fn a_long_background_prompt_is_read_in_steps() {
        let words = Arc::new(Words(Mutex::new(vec![])));
        let nrm = Nrm::new(words.clone(), 30.0);
        let long = format!("a{}", "é".repeat(1500)); // 3001 bytes: a cut at an even byte falls inside a char
        let lens = || words.0.lock().unwrap().drain(..).map(|c| c.0.len()).collect::<Vec<_>>();
        nrm.generate(BACKGROUND, long.clone()).unwrap();
        // instant steps: each one doubles
        assert_eq!(lens(), [511, 1535, 3001]);
        nrm.chat(BACKGROUND, user(&long)).unwrap();
        assert_eq!(lens(), [511, 1535, 3001]);
        nrm.generate(INTERACTIVE, long).unwrap();
        assert_eq!(lens(), [3001], "only background jobs split their prompt");
    }

    /// A 600 ms background answer; an interactive request `after` seconds in. Returns (interactive wait, calls).
    fn preempt(after: f64, aging_s: f64) -> (f64, Vec<(String, String)>) {
        let words = Arc::new(Words(Mutex::new(vec![])));
        let nrm = Arc::new(Nrm::new(words.clone(), aging_s));
        let n2 = nrm.clone();
        let bg = std::thread::spawn(move || n2.generate(BACKGROUND, "count".into()).unwrap());
        std::thread::sleep(Duration::from_secs_f64(after));
        let t = platform::now();
        assert_eq!(nrm.generate(INTERACTIVE, "word".into()).unwrap(), "ready");
        let waited = platform::now() - t;
        let full: String = (1..=60).map(|i| format!("{i} ")).collect();
        assert_eq!(bg.join().unwrap(), full, "the resumed answer is whole and in order");
        let calls = words.0.lock().unwrap().clone();
        (waited, calls)
    }

    #[test]
    fn interactive_generation_preempts_background_and_background_resumes() {
        let (waited, calls) = preempt(0.25, 30.0);
        // a slice is 8 words (80 ms): the interactive answer waits at most for the slice in progress
        assert!(waited < 0.1, "interactive waited {waited:.3}s behind a 600 ms background answer");
        let i = calls.iter().position(|c| c.0 == "word").unwrap();
        assert!(i > 0 && calls[0] == ("count".into(), "".into()), "{calls:?}");
        assert!(calls[i + 1..].iter().all(|c| c.0 == "count" && !c.1.is_empty()), "the background resumed after its prefix: {calls:?}");
        assert!(calls.len() >= 60 / SLICE_TOKENS, "a long answer runs in slices: {calls:?}");
    }

    #[test]
    fn a_resumed_background_answer_does_not_keep_its_age() {
        // aging 0.05 s: the answer is 5 periods old when the others come. A realtime job holds the worker so
        // the resumed answer and the interactive request are both queued when it ends; interactive goes next.
        let words = Arc::new(Words(Mutex::new(vec![])));
        let nrm = Arc::new(Nrm::new(words.clone(), 0.05));
        let n2 = nrm.clone();
        let bg = std::thread::spawn(move || n2.generate(BACKGROUND, "count".into()).unwrap());
        std::thread::sleep(Duration::from_millis(250));
        let w2 = words.clone();
        let rt = nrm.submit(REALTIME, move || {
            std::thread::sleep(Duration::from_millis(200));
            w2.0.lock().unwrap().push(("rt".into(), "".into()));
        });
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(nrm.generate(INTERACTIVE, "word".into()).unwrap(), "ready");
        rt.recv().unwrap();
        bg.join().unwrap();
        let calls: Vec<String> = words.0.lock().unwrap().iter().map(|c| c.0.clone()).collect();
        let i = calls.iter().position(|c| c == "rt").unwrap();
        assert_eq!(calls[i + 1], "word", "{calls:?}");
    }

    #[test]
    fn a_caller_that_goes_away_cancels_the_generation() {
        let words = Arc::new(Words(Mutex::new(vec![])));
        let nrm = Nrm::new(words.clone(), 30.0);
        let (mut seen, t) = (0, platform::now());
        let r = nrm.generate_with(INTERACTIVE, "count".into(), &mut |_| {
            seen += 1;
            seen < 3
        });
        assert!(r.is_err() && seen == 3);
        assert!(platform::now() - t < 0.15, "the 300 ms stream stopped early");
        assert_eq!(words.0.lock().unwrap().len(), 1, "no resume after a cancel");
    }
}
