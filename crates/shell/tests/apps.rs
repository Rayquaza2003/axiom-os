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
log "search=$(printf '%s' "$reply" | tr -d '"\\' | cut -c1-600)"
call tag "{\"obj\":\"$3\",\"tag\":\"from-app\"}"
log "tag=$(printf '%s' "$reply" | tr -d '"\\')"
call generate '{"prompt":"hi"}'
log "generate=$(printf '%s' "$reply" | tr -d '"\\')"
call fetch '{"url":"https://evil.example/x","payload":{}}'
log "fetch=$(printf '%s' "$reply" | tr -d '"\\')"

echo pwned > /tmp/axiom-app-probe 2>/dev/null; w=$?
cat "$2/axiom.db" >/dev/null 2>&1; r=$?
nc -z -w1 127.0.0.1 "$1" >/dev/null 2>&1; n=$?
printf '\000\000\000\017{"op":"status"}' | nc -U "$2/axiomd.sock" >/dev/null 2>&1; u=$?
log "probe write=$w read=$r net=$n ipc=$u"
"#;

fn probe(port: &str, home: &Path) -> (bool, bool, bool, bool) {
    let sh = |c: &str| std::process::Command::new("sh").arg("-c").arg(c).status().unwrap().success();
    (
        sh("echo x > /tmp/axiom-app-probe-control"),
        sh(&format!("cat {}/axiom.db >/dev/null", home.display())),
        sh(&format!("nc -z -w1 127.0.0.1 {port}")),
        sh(&format!(r#"printf '\000\000\000\017{{"op":"status"}}' | nc -U {}/axiomd.sock >/dev/null"#, home.display())),
    )
}

#[test]
fn apps_are_denied_by_default_and_sandboxed() {
    let tmp = tempfile::tempdir().unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("q3-revenue.md"), "Q3 revenue grew 12 percent.").unwrap();
    std::fs::create_dir_all(docs.join("travel")).unwrap();
    std::fs::write(docs.join("travel/lisbon.md"), "Lisbon offsite revenue plan.").unwrap();
    let home = tmp.path().join("home");
    let ax = Axiom::open(&home, Arc::new(LocalBackend::offline()), None, None).unwrap();
    ax.import(&docs, None).unwrap();
    let doc = platform::abspath(&docs.join("q3-revenue.md")).to_string_lossy().to_string();

    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.sh"), APP).unwrap();
    std::fs::write(src.join("manifest.json"),
        r#"{"name":"probe","cmd":["sh","main.sh"],"requires":["search","tag","generate","egress:evil.example"]}"#).unwrap();

    // a listener and an axiomd socket the app could reach if the sandbox let it (apps go through the broker)
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port().to_string();
    axiom_shell::daemon::start(&home, Arc::new(LocalBackend::offline()), None).unwrap();
    let home_s = home.to_string_lossy().to_string();
    assert_eq!(probe(&port, &home), (true, true, true, true), "probes must succeed unsandboxed, else the test proves nothing");
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
    assert!(!log.contains("write=0") && !log.contains("read=0") && !log.contains("net=0") && !log.contains("ipc=0"), "sandbox leaked: {log}");
    assert!(log.contains("probe write=") && log.contains("ipc="), "{log}");
    assert_eq!(r.calls, [("search", true), ("tag", true), ("generate", false), ("fetch", false)].map(|(c, ok)| (c.to_string(), ok)));
    assert!(ax.store.known_tags().unwrap().contains(&"from-app".to_string()));
    let src_of_tag: String = ax.store.db.query_row("select source from edges where dst='tag:from-app'", [], |r| r.get(0)).unwrap();
    assert_eq!(src_of_tag, "app:probe", "app writes carry provenance");

    // grant egress too: capability alone is not enough, the firewall still denies (no approval)
    apps::revoke(&home, "probe", None).unwrap();
    apps::install(&ax, &src, &|_, _| true).unwrap();
    let r = apps::run(&ax, "probe", &args, platform::now(), Duration::from_secs(20)).unwrap();
    assert!(r.log.join("\n").contains("fetch={error:blocked app egress to https://evil.example/x: not approved,"), "{:?}", r.log);

    // model-context capability expires; search does not
    let r = apps::run(&ax, "probe", &args, platform::now() + 7200.0, Duration::from_secs(20)).unwrap();
    let log = r.log.join("\n");
    assert!(log.contains("generate={error:capability expired: generate"), "{log}");
    assert!(log.contains("search={id:1,ok:[{id:"), "{log}");

    // scoped grants: search sees only travel docs, tag may only touch them
    std::fs::write(src.join("manifest.json"),
        r#"{"name":"probe","cmd":["sh","main.sh"],"requires":["search:tag=travel","tag:tag=travel"]}"#).unwrap();
    apps::install(&ax, &src, &|_, _| true).unwrap();
    let r = apps::run(&ax, "probe", &args, platform::now(), Duration::from_secs(20)).unwrap();
    let log = r.log.join("\n");
    assert!(log.contains("title:lisbon.md") && !log.contains("q3-revenue.md"), "{log}");
    assert!(log.contains("tag={error:object outside capability scope (tag:travel)"), "{log}");
    std::fs::write(src.join("manifest.json"),
        r#"{"name":"probe","cmd":["sh","main.sh"],"requires":["search","tag","generate","egress:evil.example"]}"#).unwrap();
    apps::install(&ax, &src, &|_, _| true).unwrap();

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

/// A wasm app whose `run` sends each request in order and copies each reply to offset 60000.
fn wat_app(reqs: &[&str]) -> String {
    let (mut data, mut body, mut off) = (String::new(), String::new(), 0);
    for r in reqs {
        data += &format!("(data (i32.const {off}) \"{}\")\n", r.replace('"', "\\\""));
        body += &format!("(drop (call $call (i32.const {off}) (i32.const {})))\n(call $reply (i32.const 60000))\n", r.len());
        off += r.len() + 8;
    }
    format!(r#"(module
  (import "axiom" "call" (func $call (param i32 i32) (result i32)))
  (import "axiom" "reply" (func $reply (param i32)))
  (memory (export "memory") 1)
  {data}
  (func (export "run") {body}))"#)
}

fn install_dir(root: &Path, name: &str, manifest: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let d = root.join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("manifest.json"), manifest).unwrap();
    for (f, body) in files {
        std::fs::write(d.join(f), body).unwrap();
    }
    d
}

#[test]
fn wasm_apps_and_signed_bundles() {
    let tmp = tempfile::tempdir().unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("q3-revenue.md"), "Q3 revenue grew 12 percent.").unwrap();
    let home = tmp.path().join("home");
    let ax = Axiom::open(&home, Arc::new(LocalBackend::offline()), None, None).unwrap();
    ax.import(&docs, None).unwrap();

    // wasm: same broker, same capability checks, no OS sandbox needed
    let wat = wat_app(&[
        r#"{"id":1,"call":"search","args":{"q":"revenue"}}"#,
        r#"{"id":2,"call":"log","args":{"msg":"hello from wasm"}}"#,
        r#"{"id":3,"call":"generate","args":{"prompt":"x"}}"#,
    ]);
    let src = install_dir(tmp.path(), "w", r#"{"name":"w","wasm":"app.wat","requires":["search","generate"]}"#, &[("app.wat", &wat)]);
    apps::install(&ax, &src, &|_, cap| cap == "search").unwrap();
    let r = apps::run(&ax, "w", &[], platform::now(), Duration::from_secs(20)).unwrap();
    assert_eq!(r.exit, Some(0), "{r:?}");
    assert_eq!(r.log, ["hello from wasm"]);
    assert_eq!(r.calls, [("search".to_string(), true), ("generate".to_string(), false)]);

    // a runaway module traps on fuel, quickly
    let spin = r#"(module (memory (export "memory") 1) (func (export "run") (loop $l (br $l))))"#;
    let src = install_dir(tmp.path(), "spin", r#"{"name":"spin","wasm":"app.wat","requires":[]}"#, &[("app.wat", spin)]);
    apps::install(&ax, &src, &|_, _| false).unwrap();
    let t = std::time::Instant::now();
    let r = apps::run(&ax, "spin", &[], platform::now(), Duration::from_secs(60)).unwrap();
    assert!(r.exit == Some(1) && t.elapsed() < Duration::from_secs(30), "{r:?} {:?}", t.elapsed());
    assert!(apps::read_manifest(&install_dir(tmp.path(), "bad", r#"{"name":"bad","wasm":"../x","requires":[]}"#, &[])).is_err());

    // signing: untrusted publisher refused, trusted accepted, tampering and publisher switch refused
    let src = install_dir(tmp.path(), "s", r#"{"name":"s","wasm":"app.wat","requires":[]}"#, &[("app.wat", spin)]);
    let publisher = apps::sign(&home, &src).unwrap();
    let err = apps::install(&ax, &src, &|_, _| false).unwrap_err().to_string();
    assert!(err.contains("untrusted publisher"), "{err}");
    apps::trust(&home, &publisher).unwrap();
    apps::install(&ax, &src, &|_, _| false).unwrap();
    assert_eq!(apps::list(&home).unwrap().iter().find(|a| a.name == "s").unwrap().publisher.as_deref(), Some(publisher.as_str()));
    std::fs::write(src.join(".hidden"), "extra file").unwrap(); // dot-files are covered too
    assert!(apps::install(&ax, &src, &|_, _| false).unwrap_err().to_string().contains("does not match"));
    std::fs::remove_file(src.join("signature.json")).unwrap();
    assert!(apps::install(&ax, &src, &|_, _| false).unwrap_err().to_string().contains("same publisher"), "unsigned must not replace signed");
    assert!(!home.join("apps/.staging").exists(), "failed installs leave no staging dir");
    assert!(home.join("apps/s/signature.json").exists(), "the installed version is untouched");
}
