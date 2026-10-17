//! Neural Resource Manager: one worker, strict priority tiers with aging.
//!
//! Preemption is cooperative: `embed` queues one job per `EMBED_CHUNK` texts, so a higher-priority job
//! waits for at most one chunk. ponytail: a `generate` call is one job and runs to the end; preempting it
//! needs a streaming backend that can stop and resume mid-generation.
use crate::NpuBackend;
use anyhow::{anyhow, Result};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Condvar, Mutex};

pub const REALTIME: u8 = 0;
pub const INTERACTIVE: u8 = 1;
pub const BACKGROUND: u8 = 2;

const EMBED_CHUNK: usize = 16;

struct Job {
    tier: u8,
    enqueued: f64,
    run: Box<dyn FnOnce() + Send>,
}

type Queue = Arc<(Mutex<Vec<Job>>, Condvar)>;

pub struct Nrm {
    pub backend: Arc<dyn NpuBackend>,
    q: Queue,
}

impl Nrm {
    /// A queued job gains one tier of priority per `aging_s` waited, so BACKGROUND cannot starve.
    pub fn new(backend: Arc<dyn NpuBackend>, aging_s: f64) -> Nrm {
        let q: Queue = Arc::new((Mutex::new(vec![]), Condvar::new()));
        let worker = q.clone();
        std::thread::spawn(move || loop {
            let job = {
                let mut jobs = worker.0.lock().unwrap();
                while jobs.is_empty() {
                    jobs = worker.1.wait(jobs).unwrap();
                }
                let t = platform::now();
                let key = |j: &Job| j.tier as f64 - (t - j.enqueued) / aging_s;
                let i = (0..jobs.len()).min_by(|&a, &b| key(&jobs[a]).total_cmp(&key(&jobs[b]))).unwrap();
                jobs.remove(i)
            };
            let _ = catch_unwind(AssertUnwindSafe(job.run)); // a panicking job must not kill the worker
        });
        Nrm { backend, q }
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
            self.submit(tier, move || b.embed(&c))
        }).collect();
        let mut out = vec![];
        for rx in jobs {
            out.extend(rx.recv().map_err(|_| anyhow!("embed job failed"))??);
        }
        Ok(out)
    }

    pub fn generate(&self, tier: u8, prompt: String) -> Result<String> {
        let b = self.backend.clone();
        self.submit(tier, move || b.generate(&prompt)).recv().map_err(|_| anyhow!("generate job failed"))?
    }
}
