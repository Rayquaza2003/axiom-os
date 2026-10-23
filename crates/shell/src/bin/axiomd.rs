//! axiomd: the system AI service (see `axiom_shell::daemon`). Configured by the same AXIOM_* variables as
//! the CLI; `axiom daemon install` copies them into its LaunchAgent.
use axiom_shell::{daemon, default_backend};

fn main() -> anyhow::Result<()> {
    let home = platform::home()?;
    let backend = default_backend(&home)?;
    let (dim, llm) = (backend.dim(), backend.has_llm());
    let http = daemon::start(&home, backend, Some(&daemon::http_addr()))?;
    eprintln!("axiomd: serving {} (embedding dim {dim}, LLM {}), API {}", home.join(daemon::SOCK).display(),
              if llm { "on" } else { "off" }, http.map_or("off".into(), |a| format!("http://{a}/v1")));
    loop {
        std::thread::park();
    }
}
