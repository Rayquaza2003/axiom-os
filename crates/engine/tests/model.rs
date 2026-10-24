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
    let e = Engine::load(Some(resolve(&spec).unwrap()), None, std::time::Duration::from_secs(2)).unwrap();
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

    // idle past 2 s frees the model; the next request loads it again
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(!e.generate("Name a colour.").unwrap().is_empty(), "the model reloads on demand");
}

#[test]
fn generations_run_side_by_side_in_one_batch() {
    let Ok(spec) = std::env::var("AXIOM_ENGINE_MODEL") else {
        eprintln!("skipped: set AXIOM_ENGINE_MODEL to run");
        return;
    };
    let e = std::sync::Arc::new(Engine::load(Some(resolve(&spec).unwrap()), None, std::time::Duration::from_secs(60)).unwrap());
    e.generate("Say hi.").unwrap(); // load the model
    let t = std::time::Instant::now();
    let alone = { e.generate("Reply with one word: ready").unwrap(); t.elapsed().as_secs_f64() };
    let bg: Vec<_> = (0..3).map(|i| {
        let e = e.clone();
        std::thread::spawn(move || {
            let r = e.generate(&format!("Write a 150-word story about river number {i}.")).unwrap();
            (r, std::time::Instant::now())
        })
    }).collect();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let t = std::time::Instant::now();
    e.generate("Reply with one word: ready").unwrap();
    let (busy, answered) = (t.elapsed().as_secs_f64(), std::time::Instant::now());
    for h in bg {
        let (story, done) = h.join().unwrap();
        assert!(story.split_whitespace().count() > 50, "{story}");
        assert!(done > answered, "the quick answer came while the stories were still running");
    }
    assert!(busy < alone * 4.0 + 0.5, "with 3 answers running it took {busy:.2}s, alone {alone:.2}s: it waited instead of joining the batch");
}

#[test]
fn long_prompts_use_the_whole_context() {
    let Ok(spec) = std::env::var("AXIOM_ENGINE_MODEL") else {
        eprintln!("skipped: set AXIOM_ENGINE_MODEL to run");
        return;
    };
    let e = Engine::load(Some(resolve(&spec).unwrap()), None, std::time::Duration::from_secs(60)).unwrap();
    // about 3000 tokens: past the old 2048 share per answer
    let notes = "Note: the harbour office opens at nine and the ferry leaves at noon. ".repeat(190);
    let answer = e.generate(&format!("{notes}\nWhen does the ferry leave? Answer in a few words.")).unwrap();
    assert!(answer.to_lowercase().contains("noon") || answer.contains("12"), "{answer}");
    let too_long = "word ".repeat(40_000);
    let err = e.generate(&too_long).unwrap_err().to_string();
    assert!(err.contains("tokens"), "{err}");
}

#[test]
fn batched_embeddings_match_one_at_a_time() {
    let Ok(spec) = std::env::var("AXIOM_ENGINE_EMBED") else {
        eprintln!("skipped: set AXIOM_ENGINE_EMBED to run");
        return;
    };
    let e = Engine::load(None, Some(resolve(&spec).unwrap()), std::time::Duration::from_secs(60)).unwrap();
    let texts: Vec<String> = (0..32).map(|i| format!("Passage {i}: revenue, hiring and the roadmap for item {i}.")).collect();
    e.embed(&texts).unwrap(); // load the model and size its buffers for a full batch
    let t = std::time::Instant::now();
    let one: Vec<Vec<f32>> = texts.iter().map(|x| e.embed(std::slice::from_ref(x)).unwrap().remove(0)).collect();
    let alone = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    let many = e.embed(&texts).unwrap();
    let batched = t.elapsed().as_secs_f64();
    assert_eq!(many.len(), texts.len());
    for (a, b) in one.iter().zip(&many) {
        let cos: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        assert!(cos > 0.999, "batched and single embeddings differ: cosine {cos}");
    }
    assert!(batched < alone / 2.0, "32 texts took {batched:.3}s batched, {alone:.3}s one by one");
}
