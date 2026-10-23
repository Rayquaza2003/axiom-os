//! AI monitor gate: a process holding a model shows up with its counters. Run: cargo test
use axiom_shell::monitor;
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn a_process_holding_a_model_shows_up() {
    // an extensionless GGUF file, like an Ollama blob, named on a command line like `llama-server --model`
    let tmp = tempfile::tempdir().unwrap();
    let blob = tmp.path().join("sha256-test");
    let mut h = b"GGUF".to_vec();
    h.extend(3u32.to_le_bytes());
    h.extend(0u64.to_le_bytes());
    h.extend(1u64.to_le_bytes());
    for (i, s) in ["general.name", "Test Model"].into_iter().enumerate() {
        h.extend((s.len() as u64).to_le_bytes());
        h.extend(s.as_bytes());
        if i == 0 {
            h.extend(8u32.to_le_bytes());
        }
    }
    h.resize(2 << 20, 0);
    std::fs::write(&blob, &h).unwrap();
    let mut child = Command::new("sh").args(["-c", "read x"]).arg(&blob).stdin(Stdio::piped()).spawn().unwrap();

    let a = monitor::sample().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let b = monitor::sample().unwrap();
    let r = monitor::report(&a, &b, false).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();

    let row = r.rows.iter().find(|x| x.pid == child.id() as i32).expect("the model holder is listed");
    assert_eq!(row.model.as_deref(), Some("Test Model (GGUF 2 MB)"));
    assert!(!row.name.is_empty() && row.memory > 0, "counters come from proc_pid_rusage");
    assert!(r.window_s >= 0.2 && monitor::render(&r).contains("Test Model (GGUF 2 MB)"));
}
