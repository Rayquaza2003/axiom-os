//! P6 gate: deny-by-default capabilities and OS sandbox. Run: cargo test
use axiom_shell::{apps, Axiom};
use core_ai::LocalBackend;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const APP: &str = r#"#!/bin/sh
# args: $1 = open TCP port, $2 = axiom home, $3 = path of an indexed doc
call() { printf '{"id":1,"call":"%s","args":%s}\n' "$1" "$2"; read -r reply; }
log() { printf '{"id":2,"call":"log","args":{"msg":"%s"}}\n' "$1"; read -r _; }

call search '{"q":"revenue"}'
log "search=$(printf '%s' "$reply" | tr -d '"\\' | cut -c1-200)"
call tag "{\"obj\":\"$3\",\"tag\":\"from-app\"}"
log "tag=$(printf '%s' "$reply" | tr -d '"\\')"
call generate '{"prompt":"hi"}'
log "generate=$(printf '%s' "$reply" | tr -d '"\\')"
call fetch '{"url":"https://evil.example/x","payload":{}}'
log "fetch=$(printf '%s' "$reply" | tr -d '"\\')"

echo pwned > /tmp/axiom-app-probe 2>/dev/null; w=$?
cat "$2/axiom.db" >/dev/null 2>&1; r=$?
nc -z -w1 127.0.0.1 "$1" >/dev/null 2>&1; n=$?
log "probe write=$w read=$r net=$n"
"#;

fn probe(port: &str, home: &Path) -> (bool, bool, bool) {
    let sh = |c: &str| std::process::Command::new("sh").arg("-c").arg(c).status().unwrap().success();
    (
        sh("echo x > /tmp/axiom-app-probe-control"),
        sh(&format!("cat {}/axiom.db >/dev/null", home.display())),
        sh(&format!("nc -z -w1 127.0.0.1 {port}")),
    )
}

#[test]
fn apps_are_denied_by_default_and_sandboxed() {
    let tmp = tempfile::tempdir().unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("q3-revenue.md"), "Q3 revenue grew 12 percent.").unwrap();
    let home = tmp.path().join("home");
    let ax = Axiom::open(&home, Arc::new(LocalBackend::offline()), None, None).unwrap();
    ax.import(&docs, None).unwrap();
    let doc = platform::abspath(&docs.join("q3-revenue.md")).to_string_lossy().to_string();

    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.sh"), APP).unwrap();
    std::fs::write(src.join("manifest.json"),
        r#"{"name":"probe","cmd":["sh","main.sh"],"requires":["search","tag","generate","egress:evil.example"]}"#).unwrap();

    // a listener the app could reach if the sandbox let it
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port().to_string();
    let home_s = home.to_string_lossy().to_string();
    assert_eq!(probe(&port, &home), (true, true, true), "probes must succeed unsandboxed, else the test proves nothing");
    let args = [port.clone(), home_s.clone(), doc.clone()];

    // user grants only search + tag
    let (name, got) = apps::install(&ax, &src, &|_, cap| cap == "search" || cap == "tag").unwrap();
    assert_eq!((name.as_str(), got.as_slice()), ("probe", ["search".to_string(), "tag".to_string()].as_slice()));

    let r = apps::run(&ax, "probe", &args, platform::now(), Duration::from_secs(20)).unwrap();
    assert_eq!(r.exit, Some(0), "{r:?}");
    let log = r.log.join("\n");
    assert!(log.contains("search={id:1,ok:[{id:") && log.contains("title:q3-revenue.md"), "{log}");
    assert!(log.contains("tag={id:1,ok:ok}"), "{log}");
    assert!(log.contains("generate={error:capability denied: generate,"), "{log}");
    assert!(log.contains("fetch={error:capability denied: egress:evil.example,"), "{log}");
    assert!(!log.contains("write=0") && !log.contains("read=0") && !log.contains("net=0"), "sandbox leaked: {log}");
    assert!(log.contains("probe write=") && log.contains("net="), "{log}");
    assert_eq!(r.calls, [("search", true), ("tag", true), ("generate", false), ("fetch", false)].map(|(c, ok)| (c.to_string(), ok)));
    assert!(ax.store.known_tags().unwrap().contains(&"from-app".to_string()));
    let src_of_tag: String = ax.store.db.query_row("select source from edges where dst='tag:from-app'", [], |r| r.get(0)).unwrap();
    assert_eq!(src_of_tag, "app:probe", "app writes carry provenance");

    // grant egress too: capability alone is not enough, the firewall still denies (no approval)
    apps::revoke(&home, "probe", None).unwrap();
    apps::install(&ax, &src, &|_, _| true).unwrap();
    let r = apps::run(&ax, "probe", &args, platform::now(), Duration::from_secs(20)).unwrap();
    assert!(r.log.join("\n").contains("fetch={error:blocked app egress to https://evil.example/x: not approved,"), "{:?}", r.log);

    // revoke bites on the next run
    apps::revoke(&home, "probe", Some("search")).unwrap();
    let r = apps::run(&ax, "probe", &args, platform::now(), Duration::from_secs(20)).unwrap();
    assert!(r.log.join("\n").contains("search={error:capability denied: search,"), "{:?}", r.log);

    // a hung app is killed at the timeout
    std::fs::write(src.join("main.sh"), "#!/bin/sh\nsleep 30\n").unwrap();
    apps::install(&ax, &src, &|_, _| false).unwrap();
    let t = std::time::Instant::now();
    let r = apps::run(&ax, "probe", &[], platform::now(), Duration::from_millis(500)).unwrap();
    assert!(t.elapsed() < Duration::from_secs(10) && r.exit != Some(0), "{r:?}");

    // bad manifests are rejected
    std::fs::write(src.join("manifest.json"), r#"{"name":"Bad Name","cmd":["sh"],"requires":[]}"#).unwrap();
    assert!(apps::install(&ax, &src, &|_, _| true).is_err());
    std::fs::write(src.join("manifest.json"), r#"{"name":"x","cmd":["sh"],"requires":["root"]}"#).unwrap();
    assert!(apps::install(&ax, &src, &|_, _| true).is_err());
}
