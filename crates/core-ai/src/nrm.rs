//! Neural Resource Manager: one worker, strict priority tiers with aging.
//!
//! Preemption is cooperative. `embed` queues one job per `EMBED_CHUNK` texts, so a higher-priority job
//! waits for at most one chunk. `generate` streams in slices of `SLICE_TOKENS`: between slices, if a
//! higher-tier job is waiting, the generation goes back in the queue with its original age and later
//! resumes after the text it already produced. Slices end cleanly on the server, so nothing generated is
//! thrown away (cancelling a stream instead costs Ollama 0.4-1.3 s). Backends that cannot stream (the
//! `generate_stream` default) run to the end.
//!
//! ponytail: a resumed generation re-sends its prompt and partial answer, and the server re-reads them
//! when other work used its cache meanwhile; an engine that keeps paused sequences in memory avoids that.
//!
//! With axiomd running, the NRM that matters is the daemon's: a client process's backend forwards each
//! job's tier there, so all processes on the machine share one queue.
use crate::NpuBackend;
use anyhow::{anyhow, bail, Result};
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
    /// When its work was first asked for; priority ages from here, across preemptions.
    since: f64,
    /// When this job entered the queue.
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

/// Lower runs first: the tier, minus one per `aging_s` waited.
fn rank(tier: u8, since: f64, now: f64, aging_s: f64) -> f64 {
    tier as f64 - (now - since) / aging_s
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
                let key = |j: &Job| rank(j.tier, j.since, t, aging_s);
                let i = (0..jobs.len()).min_by(|&a, &b| key(&jobs[a]).total_cmp(&key(&jobs[b]))).unwrap();
                (jobs.remove(i), t)
            };
            let (tier, waited) = (job.tier, t - job.enqueued);
            let _ = catch_unwind(AssertUnwindSafe(job.run)); // a panicking job must not kill the worker
            let mut log = log.lock().unwrap();
            if log.len() == LOG {
                log.pop_front();
            }
            log.push_back((tier, waited, platform::now() - t));
        });
        Nrm { backend, q, done }
    }

    /// The last 1024 finished jobs as (tier, seconds queued, seconds running). A preempted generation
    /// counts once per run.
    pub fn recent(&self) -> Vec<(u8, f64, f64)> {
        self.done.lock().unwrap().iter().copied().collect()
    }

    pub fn submit<T: Send + 'static>(&self, tier: u8, f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
        self.submit_since(tier, platform::now(), f)
    }

    fn submit_since<T: Send + 'static>(&self, tier: u8, since: f64, f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
        let (tx, rx) = channel();
        let run = Box::new(move || {
            let _ = tx.send(f());
        });
        self.q.0.lock().unwrap().push(Job { tier, since, enqueued: platform::now(), run });
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
        loop {
            let (tx, rx) = channel::<String>();
            let (b, p, mut prefix, q) = (self.backend.clone(), prompt.clone(), out.clone(), self.q.clone());
            let job = self.submit_since(tier, since, move || -> Result<bool> {
                loop {
                    let (mut got, mut gone) = (String::new(), false);
                    let done = b.generate_stream(tier, &p, &prefix, SLICE_TOKENS, &mut |piece| {
                        got += piece;
                        gone = tx.send(piece.to_string()).is_err();
                        !gone
                    })?;
                    if done || gone || outranked(&q, tier) {
                        return Ok(done);
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
            let finished = job.recv().map_err(|_| anyhow!("generate job failed"))??;
            if cancelled {
                bail!("generation cancelled");
            }
            if finished {
                return Ok(out);
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
    }

    /// A 600 ms background answer; an interactive request `after` seconds in. Returns (interactive wait, calls).
    fn preempt(after: f64) -> (f64, Vec<(String, String)>) {
        let words = Arc::new(Words(Mutex::new(vec![])));
        let nrm = Arc::new(Nrm::new(words.clone(), 30.0));
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
        let (waited, calls) = preempt(0.25);
        // a slice is 8 words (80 ms): the interactive answer waits at most for the slice in progress
        assert!(waited < 0.1, "interactive waited {waited:.3}s behind a 600 ms background answer");
        let i = calls.iter().position(|c| c.0 == "word").unwrap();
        assert!(i > 0 && calls[0] == ("count".into(), "".into()), "{calls:?}");
        assert!(calls[i + 1..].iter().all(|c| c.0 == "count" && !c.1.is_empty()), "the background resumed after its prefix: {calls:?}");
        assert!(calls.len() >= 60 / SLICE_TOKENS, "a long answer runs in slices: {calls:?}");
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
