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
        let r = serve_one(&l, &b, &kb);
        (r, b)
    });
    let client = sync(&addr, a, ka);
    let (server, b) = h.join().unwrap();
    (client, server, b)
}

#[test]
fn replicates_and_merges() {
    let (ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = store(ta.path(), 4, &[("a1.md", "alpha zebra notes"), ("shared.md", "shared document")]);
    let b = store(tb.path(), 4, &[("b1.md", "beta quokka notes"), ("shared.md", "shared document")]);
    a.merge_edge(&edge(100.0, 0.5)).unwrap();
    b.merge_edge(&edge(200.0, 0.9)).unwrap(); // newer: must win on both sides
    let key = [7u8; 32];

    let (c, s, b) = round(&a, b, &key, &key);
    let (c, s) = (c.unwrap(), s.unwrap());
    assert_eq!((c.objects_in, c.objects_out), (1, 1));
    assert_eq!((s.objects_in, s.objects_out), (1, 1));
    assert_eq!(a.object_ids().unwrap(), b.object_ids().unwrap());
    assert_eq!(a.object_ids().unwrap().len(), 3);

    // bytes and search index crossed the wire
    let id = b.resolve(tb.path().join("docs/b1.md").to_str().unwrap()).unwrap();
    assert_eq!(a.get(&id).unwrap(), b"beta quokka notes");
    assert_eq!(a.keyword("quokka", &Filter::default(), 5).unwrap().len(), 1);
    assert_eq!(b.keyword("zebra", &Filter::default(), 5).unwrap().len(), 1);

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
