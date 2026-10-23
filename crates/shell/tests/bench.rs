//! Bench gate: against a model server that serves one request at a time, axiomd keeps interactive requests
//! fast under background load. Run: cargo test
use axiom_shell::bench::{self, Workload};
use core_ai::{NpuBackend, BACKGROUND, INTERACTIVE};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

/// One slot and a FIFO queue, like llama-server with `-np 1`: 5 ms per text.
struct OneSlot(Mutex<mpsc::Sender<(usize, mpsc::Sender<()>)>>);

impl OneSlot {
    fn new() -> OneSlot {
        let (tx, rx) = mpsc::channel::<(usize, mpsc::Sender<()>)>();
        std::thread::spawn(move || {
            for (texts, done) in rx {
                std::thread::sleep(Duration::from_millis(5 * texts as u64));
                let _ = done.send(());
            }
        });
        OneSlot(Mutex::new(tx))
    }
}

impl NpuBackend for OneSlot {
    fn dim(&self) -> usize { 1 }
    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let (tx, rx) = mpsc::channel();
        self.0.lock().unwrap().send((texts.len(), tx))?;
        rx.recv()?;
        Ok(texts.iter().map(|_| vec![1.0]).collect())
    }
    fn has_llm(&self) -> bool { false }
    fn generate(&self, _: &str) -> anyhow::Result<String> { unreachable!() }
}

/// 64 texts per background request (320 ms on the server), one-text probes every 100 ms.
const WORK: Workload = Workload {
    name: "embed",
    clients: 1,
    load: |b, _| Ok(b.embed_at(BACKGROUND, &vec!["passage".to_string(); 64])?.len()),
    probe: |b| b.embed_at(INTERACTIVE, &["query".into()]).map(drop),
    probe_every: Duration::from_millis(100),
    unit: "texts",
};

#[test]
fn axiomd_keeps_interactive_requests_fast_under_load() {
    let tmp = tempfile::tempdir().unwrap();
    let phases = bench::run(tmp.path(), std::sync::Arc::new(OneSlot::new()), &[&WORK], 2.0, &|_| {}).unwrap().remove(0);
    let p95 = |i: usize| axiom_shell::daemon::percentile(&mut phases[i].probes.clone(), 0.95);
    let labels: Vec<&str> = phases.iter().map(|p| p.label).collect();
    assert_eq!(labels, ["no load", "direct", "through axiomd"]);
    assert!(p95(0) < 0.05, "an idle server answers a probe at once: {}", p95(0));
    // direct: a probe waits out the 64-text batch ahead of it; through axiomd: at most one 16-text chunk
    assert!(p95(1) > 2.0 * p95(2), "direct p95 {:.3}s vs axiomd p95 {:.3}s", p95(1), p95(2));
    assert!(phases[1].units > 0 && phases[2].units > 0, "background work still progresses");
    assert!(bench::render(&WORK, &phases).contains("through axiomd vs direct: interactive p95"));
}
