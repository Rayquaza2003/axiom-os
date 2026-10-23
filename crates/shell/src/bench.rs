//! `axiom bench`: does the shared AI service help? Each workload runs three ways: with no load; with
//! background load sent straight to the model server, the way every app calls it today; and with the same
//! load through axiomd at background priority. Reported: latency of a small interactive request (p50, p95,
//! max), background throughput, and the GPU busy share and AI memory from the activity monitor.
use crate::daemon::{fmt_secs, percentile};
use crate::{daemon, monitor};
use anyhow::{anyhow, Result};
use core_ai::{NpuBackend, BACKGROUND, INTERACTIVE};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Workload {
    pub name: &'static str,
    /// Background clients, each sending requests back to back.
    pub clients: usize,
    /// One background request; `n` varies its content so no cache answers it. Returns the units done.
    pub load: fn(&dyn NpuBackend, usize) -> Result<usize>,
    /// The interactive request whose latency is measured.
    pub probe: fn(&dyn NpuBackend) -> Result<()>,
    pub probe_every: Duration,
    pub unit: &'static str,
}

/// An indexer embedding 128 passages per request, against a search box embedding one query.
pub const EMBED: Workload = Workload {
    name: "embed",
    clients: 1,
    load: index_passages,
    probe: search_query,
    probe_every: Duration::from_millis(500),
    unit: "texts",
};

/// Three apps writing in the background, against a one-word chat reply.
pub const GENERATE: Workload = Workload {
    name: "generate",
    clients: 3,
    load: write_sentences,
    probe: one_word,
    probe_every: Duration::from_secs(1),
    unit: "replies",
};

fn index_passages(b: &dyn NpuBackend, n: usize) -> Result<usize> {
    let texts: Vec<String> = (0..128)
        .map(|i| format!("Passage {n}-{i}: the quarterly report covers revenue, hiring, pricing and the roadmap. ").repeat(3))
        .collect();
    Ok(b.embed_at(BACKGROUND, &texts)?.len())
}

fn search_query(b: &dyn NpuBackend) -> Result<()> {
    b.embed_at(INTERACTIVE, &["what did we decide about pricing".into()]).map(drop)
}

fn write_sentences(b: &dyn NpuBackend, n: usize) -> Result<usize> {
    b.generate_at(BACKGROUND, &format!("Write three sentences about river number {n}.")).map(|_| 1)
}

fn one_word(b: &dyn NpuBackend) -> Result<()> {
    b.generate_at(INTERACTIVE, "Reply with one word: ready").map(drop)
}

pub struct Phase {
    pub label: &'static str,
    pub loaded: bool,
    /// Seconds each interactive request took.
    pub probes: Vec<f64>,
    /// Background units finished within the phase.
    pub units: usize,
    pub secs: f64,
    /// GPU busy share of the whole machine, percent.
    pub gpu: f64,
    /// Memory of every process holding a model.
    pub ai_memory: u64,
}

/// Runs each workload with no load, with load straight on `backend`, and with load through a private axiomd
/// (serving from `home/bench`) on the same backend. `secs` is the length of each loaded phase.
pub fn run(home: &Path, backend: Arc<dyn NpuBackend>, workloads: &[&Workload], secs: f64, progress: &dyn Fn(&str)) -> Result<Vec<Vec<Phase>>> {
    let dir = home.join("bench");
    platform::makedirs(&dir)?;
    daemon::start(&dir, backend.clone(), None)?;
    let via: Arc<dyn NpuBackend> = Arc::new(daemon::connect(&dir)?.ok_or_else(|| anyhow!("bench daemon unreachable"))?);
    workloads.iter().map(|w| {
        progress(&format!("{}: warming up", w.name));
        (w.probe)(&*backend)?;
        (w.load)(&*backend, 0)?;
        let mut phases = vec![];
        for (label, b, loaded, s) in [("no load", &backend, false, secs.min(5.0)), ("direct", &backend, true, secs), ("through axiomd", &via, true, secs)] {
            progress(&format!("{}: {label}, {s:.0}s", w.name));
            phases.push(phase(label, b.clone(), w, loaded, s)?);
        }
        Ok(phases)
    }).collect()
}

fn phase(label: &'static str, b: Arc<dyn NpuBackend>, w: &Workload, loaded: bool, secs: f64) -> Result<Phase> {
    let before = monitor::sample()?;
    let (stop, end) = (Arc::new(AtomicBool::new(false)), platform::now() + secs);
    let load = w.load;
    let loaders: Vec<_> = (0..if loaded { w.clients } else { 0 }).map(|c| {
        let (b, stop) = (b.clone(), stop.clone());
        std::thread::spawn(move || -> Result<usize> {
            let mut units = 0;
            for n in (c + 1) * 1_000_000.. {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let u = load(&*b, n)?;
                if platform::now() <= end {
                    units += u;
                }
            }
            Ok(units)
        })
    }).collect();
    let mut probes = vec![];
    while platform::now() < end {
        std::thread::sleep(w.probe_every); // also lets the load fill the queue before the first probe
        let t = platform::now();
        (w.probe)(&*b)?;
        probes.push(platform::now() - t);
    }
    stop.store(true, Ordering::Relaxed);
    let mut units = 0;
    for h in loaders {
        units += h.join().map_err(|_| anyhow!("background client panicked"))??;
    }
    let r = monitor::report(&before, &monitor::sample()?, false)?;
    Ok(Phase { label, loaded, probes, units, secs, gpu: r.gpu_all, ai_memory: r.rows.iter().map(|x| x.memory).sum() })
}

pub fn render(w: &Workload, phases: &[Phase]) -> String {
    let p95 = |p: &Phase| percentile(&mut p.probes.clone(), 0.95);
    let rate = |p: &Phase| p.units as f64 / p.secs;
    let mut out = format!("{}: {} background client(s), one interactive request every {}\n", w.name, w.clients, fmt_secs(w.probe_every.as_secs_f64()));
    out += &format!("  {:<15} {:>6} {:>9} {:>9} {:>9} {:>18} {:>9} {:>10}\n", "", "probes", "p50", "p95", "max", "background", "GPU busy", "AI memory");
    for p in phases {
        let mut v = p.probes.clone();
        let bg = if p.loaded { format!("{:.1} {}/s", rate(p), w.unit) } else { "-".into() };
        out += &format!("  {:<15} {:>6} {:>9} {:>9} {:>9} {:>18} {:>8.0}% {:>10}\n", p.label, v.len(), fmt_secs(percentile(&mut v, 0.5)),
                        fmt_secs(percentile(&mut v, 0.95)), fmt_secs(percentile(&mut v, 1.0)), bg, p.gpu, monitor::size(p.ai_memory));
    }
    if let [_, direct, via] = phases {
        let (r, t) = (p95(direct) / p95(via).max(1e-6), rate(via) / rate(direct).max(1e-9) - 1.0);
        let speed = if r >= 1.0 { format!("{r:.1}x lower") } else { format!("{:.1}x higher", 1.0 / r) };
        out += &format!("  through axiomd vs direct: interactive p95 {speed}, background throughput {:+.0}%\n", t * 100.0);
    }
    out
}
