use anyhow::{anyhow, Result};
use axiom_shell::{apps, mode_name, tui, Axiom};
use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};
use core_ai::egress::Approve;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "axiom", about = "AI-native personal OS layer")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index a folder of text files
    Import { dir: PathBuf, #[arg(long)] project: Option<String> },
    /// Ask a question, e.g. "summarize Q3 docs"
    Ask {
        query: Vec<String>,
        /// Send context to this URL as model-context egress (needs budget + approval)
        #[arg(long)] cloud: Option<String>,
        /// Save the answer as an object derived from its sources
        #[arg(long)] save: bool,
        /// Read the answer aloud
        #[arg(long)] speak: bool,
    },
    /// Tag an object (path or id prefix)
    Tag { obj: String, tag: String },
    /// Remove a tag (the deletion syncs to paired devices)
    Untag { obj: String, tag: String },
    /// Workspaces are saved queries; `ws open` with no name reopens the active one (it roams with the mesh)
    Ws { action: WsAction, name: Option<String>, query: Vec<String> },
    /// Universal clipboard: push the host clipboard into the mesh, pull the newest one back
    Clip { action: ClipAction },
    /// Daily briefing
    Brief { #[arg(long)] speak: bool },
    Snapshot { #[arg(default_value = "manual")] label: String },
    /// Roll back to a snapshot file (default: latest)
    Rollback { file: Option<PathBuf> },
    /// Egress firewall: log | allow DEST | budget BYTES
    Egress { action: EgressAction, value: Option<String> },
    /// Interactive terminal UI
    Tui,
    /// Device mesh: key | join HEX | serve ADDR|relay://HOST:PORT [--jobs] | sync ADDR[,ADDR..] | relay ADDR
    Mesh {
        action: MeshAction,
        value: Option<String>,
        /// serve: also run embed/generate/speak jobs for paired devices
        #[arg(long)] jobs: bool,
    },
    /// Sandboxed apps: install DIR | run NAME [ARGS..] | list | revoke NAME [CAP] | sign DIR | trust KEY
    App { action: AppAction, name: Option<String>, rest: Vec<String> },
}

#[derive(clap::ValueEnum, Clone)]
enum WsAction { Save, Open, List }

#[derive(clap::ValueEnum, Clone)]
enum MeshAction { Key, Join, Serve, Sync, Relay }

#[derive(clap::ValueEnum, Clone)]
enum ClipAction { Push, Pull, Show }

#[derive(clap::ValueEnum, Clone, PartialEq)]
enum AppAction { Install, Run, List, Revoke, Sign, Trust }

#[derive(clap::ValueEnum, Clone)]
enum EgressAction { Log, Allow, Budget }

fn cli_approve() -> Option<Approve> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    Some(Box::new(|dest, kind, n, preview| {
        eprint!("Allow {kind} egress of {n}B to {dest}?\n  {preview}\n[y/N/always] ");
        let _ = std::io::stderr().flush();
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
        match s.trim().to_lowercase().as_str() { "" => "n".into(), x => x.to_string() }
    }))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let ax = Axiom::open_default(cli.cmd.needs_approval().then(cli_approve).flatten())?;
    let now = platform::now();
    match cli.cmd {
        Cmd::Import { dir, project } => println!("imported {} files", ax.import(&dir, project.as_deref())?),
        Cmd::Ask { query, cloud, save, speak } => {
            let (r, text) = ax.ask(&query.join(" "), now, cloud.as_deref(), save)?;
            println!("[{}, confidence {:.2}, tags {:?}]\n{text}", mode_name(&r.mode), r.plan.confidence, r.plan.tags);
            if speak {
                ax.speak(&text)?;
            }
        }
        Cmd::Tag { obj, tag } => ax.store.tag(&ax.store.resolve(&obj)?, &tag, 1.0, "user")?,
        Cmd::Untag { obj, tag } => {
            if !ax.store.untag(&ax.store.resolve(&obj)?, &tag)? {
                return Err(anyhow!("{obj} is not tagged {tag:?}"));
            }
        }
        Cmd::Ws { action, name, query } => match action {
            WsAction::Save => ax.save_workspace(&name.ok_or_else(|| anyhow!("workspace name required"))?, &query.join(" "))?,
            WsAction::Open => {
                let (name, text) = ax.open_workspace(name.as_deref(), now)?;
                println!("[{name}]\n{text}");
            }
            WsAction::List => {
                let active = ax.active_workspace();
                for (n, q) in ax.workspaces()? {
                    println!("{}{n}: {q}", if active.as_ref() == Some(&n) { "* " } else { "  " });
                }
            }
        },
        Cmd::Brief { speak } => {
            let text = ax.brief(now)?;
            println!("{text}");
            if speak {
                ax.speak(&text)?;
            }
        }
        Cmd::Clip { action } => match action {
            ClipAction::Push => println!("pushed {}", &ax.clip_push(&platform::clipboard_get()?, now)?[..12]),
            ClipAction::Pull => {
                let text = ax.clip_current()?.ok_or_else(|| anyhow!("mesh clipboard is empty"))?;
                platform::clipboard_set(&text)?;
                println!("pulled {} bytes", text.len());
            }
            ClipAction::Show => println!("{}", ax.clip_current()?.unwrap_or_default()),
        },
        Cmd::Snapshot { label } => println!("{}", ax.store.snapshot(&label)?.display()),
        Cmd::Rollback { file } => {
            let mut ax = ax;
            let snap = match file {
                Some(f) => f,
                None => ax.store.snapshots()?.into_iter().last().ok_or_else(|| anyhow!("no snapshots"))?,
            };
            ax.store.rollback(&snap)?;
            println!("rolled back to {}", snap.display());
        }
        Cmd::Egress { action, value } => match action {
            EgressAction::Log => for r in ax.egress.log()? {
                let t = Local.timestamp_opt(r.ts as i64, 0).single().map(|t| t.format("%F %T").to_string()).unwrap_or_default();
                println!("{t} {:5} {} {}B {} {}", r.decision, r.kind, r.bytes, r.dest, r.reason);
            },
            EgressAction::Allow => ax.egress.allow(&value.ok_or_else(|| anyhow!("destination required"))?)?,
            EgressAction::Budget => {
                let v: u64 = value.ok_or_else(|| anyhow!("bytes required"))?.parse()?;
                ax.egress.set_setting("budget_model_context", &v.to_string())?;
            }
        },
        Cmd::Tui => tui::run(&ax)?,
        Cmd::Mesh { action, value, jobs } => {
            let need = || value.clone().ok_or_else(|| anyhow!("argument required"));
            let log = |peer: &str, r: Result<mesh::Stats>| -> Result<()> {
                match r {
                    Ok(st) => {
                        ax.egress.record(peer, "mesh", 0, "allow", &format!("{st:?}"))?;
                        println!("synced: {st:?}");
                    }
                    Err(e) => {
                        ax.egress.record(peer, "mesh", 0, "deny", &e.to_string())?;
                        eprintln!("rejected: {e}");
                    }
                }
                Ok(())
            };
            match action {
                MeshAction::Key => {
                    let hex = mesh::key_to_hex(&mesh::load_or_create_key(&ax.home)?);
                    if std::io::stdout().is_terminal() {
                        let qr = qrcode::QrCode::new(hex.as_bytes())?;
                        println!("{}", qr.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(true).build());
                    }
                    println!("{hex}\n(secret: enter it on the other device with `axiom mesh join`, or scan the code)");
                }
                MeshAction::Join => mesh::join(&ax.home, &need()?)?,
                MeshAction::Sync => {
                    let (addr, key) = (need()?, mesh::load_or_create_key(&ax.home)?);
                    let st = mesh::sync(&addr, &ax.store, &key)?;
                    ax.egress.record(&addr, "mesh", 0, "allow", &format!("{st:?}"))?;
                    println!("synced with {addr}: {st:?}");
                }
                MeshAction::Serve => {
                    let (addr, key) = (need()?, mesh::load_or_create_key(&ax.home)?);
                    let nrm = jobs.then_some(&ax.nrm);
                    if addr.starts_with("relay://") {
                        eprintln!("waiting for peers at {addr}");
                        loop {
                            match mesh::serve_relay(&addr, &ax.store, &key, nrm, std::time::Duration::from_secs(25)) {
                                Ok(None) => {}
                                Ok(Some(st)) => log(&addr, Ok(st))?,
                                Err(e) => {
                                    log(&addr, Err(e))?;
                                    std::thread::sleep(std::time::Duration::from_secs(5));
                                }
                            }
                        }
                    }
                    let l = platform::listen(&addr)?;
                    eprintln!("listening on {}", l.local_addr()?);
                    loop {
                        log("mesh-peer", mesh::serve_one(&l, &ax.store, &key, nrm))?;
                    }
                }
                MeshAction::Relay => {
                    let l = platform::listen(&need()?)?;
                    eprintln!("relaying on {} (sees only ciphertext)", l.local_addr()?);
                    mesh::relay(&l)?;
                }
            }
        }
        Cmd::App { action, name, rest } => {
            let name_or = || name.clone().ok_or_else(|| anyhow!("app name or directory required"));
            match action {
                AppAction::Install => {
                    let ask = |app: &str, cap: &str| {
                        if !std::io::stdin().is_terminal() {
                            return false;
                        }
                        eprint!("Grant app {app:?} the capability {cap:?}? [y/N] ");
                        let _ = std::io::stderr().flush();
                        let mut s = String::new();
                        let _ = std::io::stdin().read_line(&mut s);
                        s.trim().eq_ignore_ascii_case("y")
                    };
                    let (n, granted) = apps::install(&ax, &PathBuf::from(name_or()?), &ask)?;
                    println!("installed {n}; granted: {}", if granted.is_empty() { "nothing".into() } else { granted.join(", ") });
                }
                AppAction::Run => {
                    let r = apps::run(&ax, &name_or()?, &rest, now, std::time::Duration::from_secs(300))?;
                    r.log.iter().for_each(|l| println!("{l}"));
                    for (c, ok) in &r.calls {
                        eprintln!("[{}] {c}", if *ok { "allowed" } else { "denied" });
                    }
                    std::process::exit(r.exit.unwrap_or(1));
                }
                AppAction::List => {
                    for a in apps::list(&ax.home)? {
                        println!("{}: granted [{}] requires [{}] publisher {}", a.name, a.granted.join(", "), a.required.join(", "),
                                 a.publisher.as_deref().unwrap_or("(unsigned)"));
                    }
                }
                AppAction::Sign => println!("signed by {}", apps::sign(&ax.home, &PathBuf::from(name_or()?))?),
                AppAction::Trust => apps::trust(&ax.home, &name_or()?)?,
                AppAction::Revoke => {
                    let n = apps::revoke(&ax.home, &name_or()?, rest.first().map(String::as_str))?;
                    println!("revoked {n} grant(s)");
                }
            }
        }
    }
    Ok(())
}

impl Cmd {
    fn needs_approval(&self) -> bool {
        matches!(self, Cmd::Ask { cloud: Some(_), .. } | Cmd::App { action: AppAction::Run, .. })
    }
}
