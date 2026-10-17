# Axiom

Architecture blueprint in `specs/` plus a working personal prototype: Rust workspace in `crates/`.

## Run (Rust)

- Setup: `. ~/.cargo/env`
- Gate test: `cargo test`
- CLI: `cargo run --release -- import DIR | ask "summarize Q3 docs" [--cloud URL] [--save] [--speak] | tag | untag | ws | brief [--speak] | snapshot | rollback | egress log|allow|budget | tui | mesh key|join HEX|serve ADDR|sync ADDR | app install DIR|run NAME [ARGS]|list|revoke NAME [CAP]`
- Data lives in `$AXIOM_HOME` (default `~/.axiom`).
- Offline by default (lexical hash embeddings, extractive summaries). For real models point at any OpenAI-compatible server (llama-server with Metal, Ollama): `AXIOM_LLM_MODEL=<model>` for summaries, `AXIOM_EMBED_MODEL=<model>` for semantic vectors, `AXIOM_LLM_URL` / `AXIOM_EMBED_URL` (default `http://localhost:11434`). A store is tied to one embedding dimension.

## Mesh and apps

- Mesh pairing: `axiom mesh key` on device A, `axiom mesh join <hex>` on device B (shared 32-byte key, used as the PSK of a Noise NNpsk0 handshake). `mesh serve 0.0.0.0:7777` on one, `mesh sync host:7777` on the other. Objects union, edges last-writer-wins, deletions sync as tombstones, file names sync as `named` edges. Mesh traffic is logged in the egress audit log (kind `mesh`).
- Apps: a directory with `manifest.json` (`{"name","cmd":[...],"requires":["search"|"tag"|"generate"|"egress:<host>"]}`). `app install` asks per capability (non-interactive = grants nothing). Apps run under macOS `sandbox-exec` (no network, no writes, no reads of `$AXIOM_HOME`) and talk JSON lines to the broker in `crates/shell/src/apps.rs`. The sandbox needs macOS; elsewhere apps refuse to run.

## Portability rules (so the layer can move to another kernel)

1. All OS access goes through the `platform` crate. Other crates never touch `std::fs`, `std::process` or sockets.
2. Sockets, randomness and sandboxed processes also live in `platform`.
3. Inference goes through the `NpuBackend` trait (`core-ai`); a vendor NPU backend implements it (see `specs/build/hardware-support.md`).
4. Egress goes only through `Egress::send`. Its audit log lives in its own DB, so a data rollback cannot erase it.
5. Decisions on the open questions: `specs/open/open-questions.md`, "Decisions" section.

## Map

- `platform`: host access (files, clock, HTTP, TTS).
- `core-data` P2: Blake3 + FastCDC objects, graph edges, passage vectors, snapshots (one SQLite file).
- `core-ai` P3: `NpuBackend`/`LocalBackend`, `Nrm` (priority tiers + aging), planner, summarizer, egress firewall.
- `mesh` P4: encrypted two-device sync (`sync`, `serve_one`).
- `shell` P5/P6: `Axiom` facade, CLI (`axiom`), TUI, `apps` sandbox broker.
