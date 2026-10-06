//! Prototype gate. Run: cargo test
use axiom_shell::Axiom;
use chrono::{Local, NaiveDate, TimeZone};
use core_ai::egress::Denied;
use core_ai::{LocalBackend, Nrm, BACKGROUND, REALTIME};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime};

fn local(y: i32, m: u32, d: u32, h: u32) -> f64 {
    let t = NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, 0, 0).unwrap();
    Local.from_local_datetime(&t).earliest().unwrap().timestamp() as f64
}

fn write(root: &Path, rel: &str, text: &str, when: f64) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    let f = std::fs::File::options().write(true).open(&p).unwrap();
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(when)).unwrap();
}

fn denied(r: anyhow::Result<Value>) -> String {
    r.expect_err("egress should be denied").downcast_ref::<Denied>().expect("Denied error").0.clone()
}

#[test]
fn gate() {
    let now = local(2026, 10, 1, 12);
    let tmp = tempfile::tempdir().unwrap();
    let docs = tmp.path().join("docs");
    let (q3, old) = (local(2026, 8, 15, 12), local(2026, 2, 3, 12));
    write(&docs, "finance/q3-revenue.md", "Q3 revenue grew 12 percent driven by enterprise renewals.", q3);
    write(&docs, "finance/q3-budget.md", "Budget review: marketing spend cut, hiring paused.", q3);
    write(&docs, "travel/trip.md", "Flights to Lisbon booked for the offsite.", q3);
    write(&docs, "finance/q1-plan.md", "Q1 plan: launch pricing experiment.", old);
    write(&docs, "data/sync.md", "Sync protocol replicates objects between devices.", old);
    write(&docs, "security/egress.md", "The egress firewall blocks model context from leaving the device.", old);

    let sent = Arc::new(Mutex::new(vec![]));
    let s2 = sent.clone();
    let transport: core_ai::egress::Transport = Box::new(move |d, _| {
        s2.lock().unwrap().push(d.to_string());
        Ok(json!({"response": "cloud"}))
    });
    let mut ax = Axiom::open(&tmp.path().join("home"), Arc::new(LocalBackend::offline()), None, Some(transport)).unwrap();
    let titles = |ax: &Axiom, hits: &[core_data::Hit]| {
        let mut t: Vec<String> = hits.iter().map(|h| ax.store.title(&h.obj)).collect();
        t.sort();
        t
    };

    // data plane: import, dedupe, incremental, chunked storage round-trips
    assert_eq!(ax.import(&docs, None).unwrap(), 6);
    assert_eq!(ax.import(&docs, None).unwrap(), 0);
    let oid = ax.store.resolve(docs.join("travel/trip.md").to_str().unwrap()).unwrap();
    assert_eq!(ax.store.get(&oid).unwrap(), b"Flights to Lisbon booked for the offsite.");

    // large file: content-defined chunks dedupe a shared prefix and reassemble byte-exact
    let big: Vec<u8> = (0..400_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let (a, b) = (ax.store.put(&big, now).unwrap(), ax.store.put(&[&big[..300_000], b"tail"].concat(), now).unwrap());
    assert_eq!(ax.store.get(&a).unwrap(), big);
    let (total, unique): (i64, i64) = ax.store.db
        .query_row("select count(*), count(distinct blob) from object_chunks where obj in (?,?)", [&a, &b], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert!(unique < total, "shared prefix should dedupe chunks ({unique} of {total})");

    // AI core: "Summarize Q3 docs" offline; the time filter excludes Q1
    let (r, _) = ax.ask("Summarize Q3 docs", now, None, false).unwrap();
    assert!(r.plan.confidence >= 0.7 && format!("{:?}", r.mode) == "Hybrid");
    assert_eq!(titles(&ax, &r.hits), ["q3-budget.md", "q3-revenue.md", "trip.md"]);
    let (r, _) = ax.ask("finance docs about revenue", now, None, false).unwrap(); // tag + vector
    assert_eq!(ax.store.title(&r.hits[0].obj), "q3-revenue.md");
    assert_eq!(r.hits.len(), 6, "tag boosts ranking but never excludes untagged docs");
    // a tag word inside a real question must not hide the right doc (regression: "data" folder tag)
    let (r, _) = ax.ask("how does the egress firewall protect my data", now, None, false).unwrap();
    assert!(r.plan.tags.contains(&"data".to_string()));
    assert_eq!(ax.store.title(&r.hits[0].obj), "egress.md");
    let (r, _) = ax.ask("lisbon", now, None, false).unwrap(); // low confidence -> keyword fallback
    assert_eq!(format!("{:?}", r.mode), "Keyword");
    assert_eq!(ax.store.title(&r.hits[0].obj), "trip.md");

    // save writes derived_from edges
    let (r, text) = ax.ask("summarize finance", now, None, true).unwrap();
    assert!(text.contains("saved"));
    let n: i64 = ax.store.db.query_row("select count(*) from edges where type='derived_from'", [], |r| r.get(0)).unwrap();
    assert_eq!(n as usize, r.hits.len());

    // snapshot + rollback
    let snap = ax.store.snapshot("before").unwrap();
    ax.store.tag(&oid, "vip", 1.0, "user").unwrap();
    assert!(ax.store.known_tags().unwrap().contains(&"vip".to_string()));
    ax.store.rollback(&snap).unwrap();
    assert!(!ax.store.known_tags().unwrap().contains(&"vip".to_string()));

    // egress: denied by default and logged; budget beats approval; model-context always needs approval
    let url = "https://cloud.example/x";
    assert_eq!(denied(ax.egress.send(url, &json!({"a": 1}), "other")), "blocked other egress to https://cloud.example/x: not approved");
    ax.egress.approve = Some(Box::new(|_, _, _, _| "y".into()));
    ax.egress.send(url, &json!({"a": 1}), "other").unwrap();
    assert_eq!(*sent.lock().unwrap(), [url]);
    assert!(denied(ax.egress.send(url, &json!({"ctx": "secret"}), "model-context")).contains("budget"));
    ax.egress.set_setting("budget_model_context", "10000").unwrap();
    ax.egress.approve = None;
    assert!(denied(ax.egress.send(url, &json!({"ctx": "secret"}), "model-context")).contains("not approved"));
    let log: Vec<String> = ax.egress.log().unwrap().into_iter().map(|r| r.decision).collect();
    assert_eq!(log, ["deny", "allow", "deny", "deny"]);
    assert_eq!(*sent.lock().unwrap(), [url], "denied payloads must never reach the transport");

    // NRM: realtime beats queued background; with aging, an old background job wins
    for (aging, expect) in [(30.0, ["rt", "bg"]), (0.01, ["bg", "rt"])] {
        let nrm = Nrm::new(Arc::new(LocalBackend::offline()), aging);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let order = Arc::new(Mutex::new(vec![]));
        let hold = nrm.submit(REALTIME, move || gate_rx.recv().ok());
        std::thread::sleep(Duration::from_millis(50));
        let push = |tag: &'static str| { let o = order.clone(); move || o.lock().unwrap().push(tag) };
        let bg = nrm.submit(BACKGROUND, push("bg"));
        std::thread::sleep(Duration::from_millis(200));
        let rt = nrm.submit(REALTIME, push("rt"));
        gate_tx.send(()).unwrap();
        hold.recv().unwrap();
        (bg.recv().unwrap(), rt.recv().unwrap());
        assert_eq!(*order.lock().unwrap(), expect, "aging={aging}");
    }
}
