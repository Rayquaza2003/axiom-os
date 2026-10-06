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
    /// Workspaces are saved queries
    Ws { action: WsAction, name: Option<String>, query: Vec<String> },
    /// Daily briefing
    Brief { #[arg(long)] speak: bool },
    Snapshot { #[arg(default_value = "manual")] label: String },
    /// Roll back to a snapshot file (default: latest)
    Rollback { file: Option<PathBuf> },
    /// Egress firewall: log | allow DEST | budget BYTES
    Egress { action: EgressAction, value: Option<String> },
    /// Interactive terminal UI
    Tui,
    /// Device mesh: key | join HEX | serve ADDR | sync ADDR
    Mesh { action: MeshAction, value: Option<String> },
    /// Sandboxed apps: install DIR | run NAME [ARGS..] | list | revoke NAME [CAP]
    App { action: AppAction, name: Option<String>, rest: Vec<String> },
}

#[derive(clap::ValueEnum, Clone)]
enum WsAction { Save, Open, List }

#[derive(clap::ValueEnum, Clone)]
enum MeshAction { Key, Join, Serve, Sync }

#[derive(clap::ValueEnum, Clone, PartialEq)]
enum AppAction { Install, Run, List, Revoke }

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
                platform::speak(&text);
            }
        }
        Cmd::Tag { obj, tag } => ax.store.tag(&ax.store.resolve(&obj)?, &tag, 1.0, "user")?,
        Cmd::Ws { action, name, query } => match action {
            WsAction::Save => {
                let name = name.ok_or_else(|| anyhow!("workspace name required"))?;
                ax.store.db.execute("insert or replace into workspaces values(?,?)", [name, query.join(" ")])?;
            }
            WsAction::Open => {
                let name = name.ok_or_else(|| anyhow!("workspace name required"))?;
                let (_, q) = ax.workspaces()?.into_iter().find(|(n, _)| *n == name).ok_or_else(|| anyhow!("no such workspace"))?;
                println!("{}", ax.ask(&q, now, None, false)?.1);
            }
            WsAction::List => ax.workspaces()?.iter().for_each(|(n, q)| println!("{n}: {q}")),
        },
        Cmd::Brief { speak } => {
            let text = ax.brief(now)?;
            println!("{text}");
            if speak {
                platform::speak(&text);
            }
        }
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
        Cmd::Mesh { action, value } => {
            let need = || value.clone().ok_or_else(|| anyhow!("argument required"));
            match action {
                MeshAction::Key => {
                    let key = mesh::load_or_create_key(&ax.home)?;
                    println!("{}\n(secret: enter it on the other device with `axiom mesh join`)", mesh::key_to_hex(&key));
                }
                MeshAction::Join => mesh::join(&ax.home, &need()?)?,
                MeshAction::Sync => {
                    let (addr, key) = (need()?, mesh::load_or_create_key(&ax.home)?);
                    let st = mesh::sync(&addr, &ax.store, &key)?;
                    ax.egress.record(&addr, "mesh", 0, "allow", &format!("{st:?}"))?;
                    println!("synced with {addr}: {st:?}");
                }
                MeshAction::Serve => {
                    let (l, key) = (platform::listen(&need()?)?, mesh::load_or_create_key(&ax.home)?);
                    eprintln!("listening on {}", l.local_addr()?);
                    loop {
                        match mesh::serve_one(&l, &ax.store, &key) {
                            Ok(st) => {
                                ax.egress.record("mesh-peer", "mesh", 0, "allow", &format!("{st:?}"))?;
                                println!("synced: {st:?}");
                            }
                            Err(e) => {
                                ax.egress.record("mesh-peer", "mesh", 0, "deny", &e.to_string())?;
                                eprintln!("rejected: {e}");
                            }
                        }
                    }
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
                    for (n, granted, required) in apps::list(&ax.home)? {
                        println!("{n}: granted [{}] requires [{}]", granted.join(", "), required.join(", "));
                    }
                }
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
