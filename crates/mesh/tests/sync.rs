use anyhow::Result;
use core_data::{EdgeRow, Filter, Store};
use mesh::{serve_one, sync, Key, Stats};
use std::path::Path;

fn embed(texts: &[String]) -> Result<Vec<Vec<f32>>> {
    Ok(texts.iter().map(|t| vec![t.len() as f32, 1.0, 0.0, 0.0]).collect())
}

fn store(dir: &Path, dim: usize, files: &[(&str, &str)]) -> Store {
    let docs = dir.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    for (n, t) in files {
        std::fs::write(docs.join(n), t).unwrap();
    }
    let s = Store::open(&dir.join("axiom.db"), dim).unwrap();
    s.import_dir(&docs, &embed, None).unwrap();
    s
}

fn edge(ts: f64, conf: f64) -> EdgeRow {
    EdgeRow { src: "o1".into(), ty: "tagged".into(), dst: "tag:x".into(), conf, source: "user".into(), ts }
}

/// Run one sync round: server thread owns `b`, client uses `a`.
fn round(a: &Store, b: Store, ka: &Key, kb: &Key) -> (Result<Stats>, Result<Stats>, Store) {
    let l = platform::listen("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let kb = *kb;
    let h = std::thread::spawn(move || {
        let r = serve_one(&l, &b, &kb, None);
        (r, b)
    });
    let client = sync(&addr, a, ka);
    let (server, b) = h.join().unwrap();
    (client, server, b)
}

#[test]
fn replicates_and_merges() {
    let (ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let big = "lorem ipsum ".repeat(20_000); // > one 64 KiB Noise message: exercises fragmentation
    let a = store(ta.path(), 4, &[("a1.md", "alpha zebra notes"), ("shared.md", "shared document"), ("big.md", &big)]);
    let b = store(tb.path(), 4, &[("b1.md", "beta quokka notes"), ("shared.md", "shared document")]);
    a.merge_edge(&edge(100.0, 0.5)).unwrap();
    b.merge_edge(&edge(200.0, 0.9)).unwrap(); // newer: must win on both sides
    let key = [7u8; 32];

    let (c, s, b) = round(&a, b, &key, &key);
    let (c, s) = (c.unwrap(), s.unwrap());
    assert_eq!((c.objects_in, c.objects_out), (1, 2));
    assert_eq!((s.objects_in, s.objects_out), (2, 1));
    assert_eq!(a.object_ids().unwrap(), b.object_ids().unwrap());
    assert_eq!(a.object_ids().unwrap().len(), 4);

    // bytes and search index crossed the wire
    let id = b.resolve(tb.path().join("docs/b1.md").to_str().unwrap()).unwrap();
    assert_eq!(a.get(&id).unwrap(), b"beta quokka notes");
    assert_eq!(a.keyword("quokka", &Filter::default(), 5).unwrap().len(), 1);
    assert_eq!(b.keyword("zebra", &Filter::default(), 5).unwrap().len(), 1);
    assert_eq!(a.title(&id), "b1.md", "file names replicate");
    let big_id = a.resolve(ta.path().join("docs/big.md").to_str().unwrap()).unwrap();
    assert_eq!(b.get(&big_id).unwrap(), big.as_bytes());

    // last-writer-wins on edges, both directions
    for s in [&a, &b] {
        let e = s.edge_rows().unwrap().into_iter().find(|e| e.src == "o1").unwrap();
        assert_eq!((e.ts, e.conf), (200.0, 0.9));
    }

    // idempotent
    let (c, s, b) = round(&a, b, &key, &key);
    let (c, s) = (c.unwrap(), s.unwrap());
    assert_eq!((c.objects_in, c.objects_out, s.objects_in, s.objects_out), (0, 0, 0, 0));
    assert_eq!(a.edge_rows().unwrap().len(), b.edge_rows().unwrap().len());

    // a deletion replicates as a tombstone; a later re-link beats the tombstone
    let has_o1 = |s: &Store| s.edge_rows().unwrap().iter().any(|e| e.src == "o1");
    assert!(a.untag("o1", "x").unwrap());
    let (c, s, b) = round(&a, b, &key, &key);
    (c.unwrap(), s.unwrap());
    assert!(!has_o1(&a) && !has_o1(&b));
    b.tag("o1", "x", 1.0, "user").unwrap();
    let (c, s, b) = round(&a, b, &key, &key);
    (c.unwrap(), s.unwrap());
    assert!(has_o1(&a) && has_o1(&b));

    // wrong key: both sides fail, nothing changes
    let before = (a.object_ids().unwrap(), b.edge_rows().unwrap().len());
    let (c, s, b) = round(&a, b, &key, &[9u8; 32]);
    assert!(c.is_err() && s.is_err());
    assert_eq!((a.object_ids().unwrap(), b.edge_rows().unwrap().len()), before);
}

#[test]
fn rejects_dimension_mismatch_and_tampered_objects() {
    let (ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = store(ta.path(), 4, &[("a.md", "alpha")]);
    let b = Store::open(&tb.path().join("axiom.db"), 8).unwrap();
    let key = [1u8; 32];
    let (c, s, b) = round(&a, b, &key, &key);
    assert!(c.is_err() && s.is_err());
    assert!(b.object_ids().unwrap().is_empty());

    let id = a.object_ids().unwrap().remove(0);
    let mut bundle = a.export_object(&id).unwrap();
    bundle.chunks[0].1[0] ^= 1; // flip a bit
    assert!(b.import_object(&bundle).is_err());
    let mut bundle = a.export_object(&id).unwrap();
    bundle.id = "0".repeat(64);
    let tb2 = tempfile::tempdir().unwrap();
    let b4 = Store::open(&tb2.path().join("axiom.db"), 4).unwrap();
    assert!(b4.import_object(&bundle).is_err(), "id must match content hash");
}

#[test]
fn edges_and_tombstones_merge_in_any_order() {
    let tomb = || core_data::Tombstone { src: "o1".into(), ty: "tagged".into(), dst: "tag:x".into(), ts: 100.0 };
    for edge_first in [true, false] {
        let t = tempfile::tempdir().unwrap();
        let s = Store::open(&t.path().join("a.db"), 4).unwrap();
        if edge_first {
            s.merge_edge(&edge(100.0, 1.0)).unwrap();
            s.merge_tombstone(&tomb()).unwrap();
        } else {
            s.merge_tombstone(&tomb()).unwrap();
            s.merge_edge(&edge(100.0, 1.0)).unwrap();
        }
        assert!(s.edge_rows().unwrap().is_empty(), "delete wins a tie (edge_first={edge_first})");
        s.merge_edge(&edge(101.0, 1.0)).unwrap();
        s.merge_tombstone(&tomb()).unwrap(); // stale tombstone arrives late
        assert_eq!(s.edge_rows().unwrap().len(), 1, "newer edge beats older tombstone");
        assert!(s.tombstone_rows().unwrap().is_empty());
    }
}

#[test]
fn relay_failover_and_offload() {
    let (ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = store(ta.path(), 4, &[("a1.md", "alpha via relay")]);
    let key = [3u8; 32];
    let rl = platform::listen("127.0.0.1:0").unwrap();
    let relay = format!("relay://{}", rl.local_addr().unwrap());
    std::thread::spawn(move || mesh::relay(&rl));

    // nobody parked yet: the dialer is told at once instead of hanging
    let err = sync(&relay, &a, &key).unwrap_err().to_string();
    assert!(err.contains("no peer waiting"), "{err}");
    let idle = Store::open(&tb.path().join("idle.db"), 4).unwrap();
    assert!(mesh::serve_relay(&relay, &idle, &key, None, std::time::Duration::from_millis(200)).unwrap().is_none());

    // B parks at the relay; A fails over from a dead address to the relay
    let (r2, tbp) = (relay.clone(), tb.path().to_path_buf());
    let h = std::thread::spawn(move || {
        let b = Store::open(&tbp.join("axiom.db"), 4).unwrap();
        let st = mesh::serve_relay(&r2, &b, &key, None, std::time::Duration::from_secs(10)).unwrap();
        (st, b.object_ids().unwrap().len())
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    let st = sync(&format!("127.0.0.1:1,{relay}"), &a, &key).unwrap();
    assert_eq!(st.objects_out, 1);
    let (served, n) = h.join().unwrap();
    assert_eq!((served.unwrap().objects_in, n), (1, 1));

    // offload: B embeds for A through its own NRM, logged as one job
    let nrm = core_ai::Nrm::new(std::sync::Arc::new(core_ai::LocalBackend::offline()), 30.0);
    let l = platform::listen("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let audited = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let h = std::thread::spawn(move || {
        let b = Store::open(&tempfile::tempdir().unwrap().path().join("b.db"), 4).unwrap();
        (0..2).map(|_| serve_one(&l, &b, &key, Some(&nrm)).unwrap().jobs).sum::<usize>()
    });
    let au = audited.clone();
    let remote = mesh::RemoteBackend::connect(&addr, key, Box::new(move |n| *au.lock().unwrap() += n)).unwrap();
    use core_ai::NpuBackend;
    let local = core_ai::LocalBackend::offline();
    assert_eq!(remote.dim(), local.dim());
    let texts = vec!["quarterly revenue".to_string()];
    assert_eq!(remote.embed(&texts).unwrap(), local.embed(&texts).unwrap());
    assert_eq!((h.join().unwrap(), *audited.lock().unwrap()), (2, 17));

    // a device without a broker refuses jobs
    let l = platform::listen("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let h = std::thread::spawn(move || serve_one(&l, &Store::open(&tempfile::tempdir().unwrap().path().join("c.db"), 4).unwrap(), &key, None).is_err());
    assert!(mesh::RemoteBackend::connect(&addr, key, Box::new(|_| {})).is_err());
    assert!(h.join().unwrap());
}
