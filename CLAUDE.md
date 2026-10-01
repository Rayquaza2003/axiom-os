# Axiom

Architecture blueprint in `specs/` plus a working personal prototype: Rust workspace in `crates/`.

## Run (Rust)

- Setup: `. ~/.cargo/env`
- Gate test: `cargo test`
- CLI: `cargo run --release -- import DIR | ask "summarize Q3 docs" [--cloud URL] [--save] [--speak] | tag | ws | brief [--speak] | snapshot | rollback | egress log|allow|budget | tui`
- Data lives in `$AXIOM_HOME` (default `~/.axiom`).
- Offline by default (lexical hash embeddings, extractive summaries). For real models point at any OpenAI-compatible server (llama-server with Metal, Ollama): `AXIOM_LLM_MODEL=<model>` for summaries, `AXIOM_EMBED_MODEL=<model>` for semantic vectors, `AXIOM_LLM_URL` / `AXIOM_EMBED_URL` (default `http://localhost:11434`). A store is tied to one embedding dimension.

## Portability rules (so the layer can move to another kernel)

1. All OS access goes through the `platform` crate. Other crates never touch `std::fs`, `std::process` or sockets.
2. Inference goes through the `NpuBackend` trait (`core-ai`); a vendor NPU backend implements it (see `specs/build/hardware-support.md`).
3. Egress goes only through `Egress::send`. Its audit log lives in its own DB, so a data rollback cannot erase it.
4. Decisions on the open questions: `specs/open/open-questions.md`, "Decisions" section.

## Map

- `platform`: host access (files, clock, HTTP, TTS).
- `core-data` P2: Blake3 + FastCDC objects, graph edges, passage vectors, snapshots (one SQLite file).
- `core-ai` P3: `NpuBackend`/`LocalBackend`, `Nrm` (priority tiers + aging), planner, summarizer, egress firewall.
- `shell` P5: `Axiom` facade, CLI (`axiom`), TUI.
