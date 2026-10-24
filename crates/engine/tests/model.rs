//! Runs only with a real model: `AXIOM_ENGINE_MODEL=ollama:qwen2.5:3b-instruct cargo test -p engine`.
use core_ai::NpuBackend;
use engine::{resolve, Engine};

/// (text, seconds to the first piece, finished) for one slice.
fn slice(e: &Engine, prompt: &str, prefix: &str, n: usize) -> (String, f64, bool) {
    let (t, mut first, mut text) = (std::time::Instant::now(), None, String::new());
    let done = e.generate_stream(2, prompt, prefix, n, &mut |p| {
        first.get_or_insert(t.elapsed().as_secs_f64());
        text += p;
        true
    }).unwrap();
    (text, first.unwrap_or(0.0), done)
}

#[test]
fn paused_generations_resume_without_rereading_their_prompt() {
    let Ok(spec) = std::env::var("AXIOM_ENGINE_MODEL") else {
        eprintln!("skipped: set AXIOM_ENGINE_MODEL to run");
        return;
    };
    let e = Engine::load(Some(resolve(&spec).unwrap()), None).unwrap();
    let long = format!("{}\nIn one sentence, what is this text about?", "The lighthouse keeper logged every passing ship. ".repeat(60));
    let (a, prefill, _) = slice(&e, &long, "", 8);
    assert!(!a.is_empty());
    // another generation runs while the first is paused
    assert!(!e.generate("Name a colour.").unwrap().is_empty());
    let (b, resume, _) = slice(&e, &long, &a, 8);
    assert!(!b.is_empty(), "the paused answer continues");
    assert!(resume < prefill / 2.0, "resume took {resume:.3}s, the first slice {prefill:.3}s: the prompt was read again");
    let mut whole = a + &b;
    loop {
        let (more, _, done) = slice(&e, &long, &whole, 8);
        whole += &more;
        if done {
            break;
        }
    }
    assert!(whole.len() > 20, "{whole}");
}
