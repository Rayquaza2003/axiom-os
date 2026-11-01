# Axiom

Architecture blueprint in `specs/` plus a working personal prototype: Rust workspace in `crates/`.

## Run (Rust)

- Setup: `. ~/.cargo/env`
- Gate test: `cargo test`
- CLI: `cargo run --release -- import DIR | ask "summarize Q3 docs" [--cloud URL] [--save] [--speak] | tag | untag | ws save|open|list | clip push|pull|show | brief [--speak] | snapshot | rollback | egress log|allow|budget | tui | mesh key|join HEX|serve ADDR [--jobs]|sync ADDR[,ADDR]|relay ADDR | app install DIR|run NAME [ARGS]|list|revoke NAME [CAP]|sign DIR|trust KEY | daemon status|install|uninstall|key NAME [--tier background]|revoke NAME | monitor [SECS] [--all] [--watch] | bench [embed|generate] [--secs N]`
- Data lives in `$AXIOM_HOME` (default `~/.axiom`).
- Package: `packaging/macos/build-dmg.sh` writes `dist/Axiom-<version>.dmg` (Axiom.app with `axiom`, `axion` and a `setup` launcher that runs `axiom daemon install` from inside the app and links the CLI to `~/.local/bin`). Ad-hoc signed; `AXIOM_SIGN_ID` + `AXIOM_NOTARY_PROFILE` sign and notarize. `AXIOM_QUIET=1` makes setup print instead of showing a dialog.
- Offline by default (lexical hash embeddings, extractive summaries). For real models point at any OpenAI-compatible server (llama-server with Metal, Ollama): `AXIOM_LLM_MODEL=<model>` for summaries, `AXIOM_EMBED_MODEL=<model>` for semantic vectors, `AXIOM_LLM_URL` / `AXIOM_EMBED_URL` (default `http://localhost:11434`). A store is tied to one embedding dimension.

## axion (AI as a system resource)

- `axion` (built by `cargo build --release`) owns the model and the one NRM queue for the user. `axiom` processes use it over `$AXIOM_HOME/axion.sock` (mode 0600) when it runs, passing each job's tier; otherwise they run inference in-process. While it runs, model settings (`AXIOM_*_MODEL/URL`, `AXIOM_OFFLOAD`) are the daemon's.
- `axiom daemon install` registers a macOS LaunchAgent (`dev.axiom.axion`: starts at login, restarts if it exits). It copies the shell's `AXIOM_*` variables. Log: `$AXIOM_HOME/axion.log`.
- OpenAI-compatible API on `AXIOM_HTTP` (default `127.0.0.1:7779`): `/v1/models`, `/v1/chat/completions`, `/v1/embeddings`. Every request needs a key from `axiom daemon key NAME`; it runs at the key's tier (`--tier`, default interactive). Only the key's hash is stored, in `daemon.db`. A chat with `tools` or `tool` messages goes whole through `NpuBackend::chat` (LocalBackend and RemoteBackend forward it; the engine answers 400); `Nrm::chat` slices it like a generation, and a slice that ends inside a tool call (Ollama returns a cut call empty) reruns that call to its end. A prompt the server truncated (Ollama: under 1 token per 6 chars kept) is an error. A key reaches inference only, never the store. `stream: true` streams tokens (chunked SSE); a client that hangs up cancels its generation.
- The NRM generates in 8-token slices (`SLICE_TOKENS`, `generate_stream` with `max_tokens`). Between slices, a waiting higher-tier job runs first, then the generation resumes by sending its partial answer as a trailing assistant message. Aging orders the queue but never shields a running job from a higher tier; a resumed job ages from its re-queue, not from its request (else an old background answer wins the queue back after every yield). A BACKGROUND job reads a long prompt in steps (`Prefill`: the prompt cut there, one token asked) sized to take `PREFILL_S` (2 s) at the last step's measured speed, so the server's prefix cache holds it and higher tiers run between steps.
- Built-in engine (`crates/engine`, default `engine` feature of `axiom-shell`): llama.cpp with Metal in-process, chosen by `AXIOM_ENGINE_MODEL` / `AXIOM_ENGINE_EMBED` (GGUF path or `ollama:NAME[:TAG]`, resolved from Ollama's manifests). One thread owns llama.cpp; up to 4 generations run as sequences in one batch (one token each per decode step), new requests prefill between steps and join at once, and waiting ones start in tier order. A paused sequence keeps its KV cache, so a resume does not re-read the prompt. `NpuBackend::batches()` is true for it, so the NRM passes generations straight through instead of slicing. Before loading, `engine::plan` fits the model to the machine (`platform::memory`, the GPU's free working set, `AXIOM_ENGINE_MEMORY` cap): fewer GPU layers when tight (the rest run from the mmapped file), shorter context only when its KV cache would exceed a quarter of the room; the plan shows in `axiom daemon status`. Models load on first use; idle `AXIOM_ENGINE_IDLE` s (120) frees contexts, idle `AXIOM_ENGINE_UNLOAD` s (900) or memory pressure unloads models (files stay in the OS cache). Embeddings batch up to 16 texts per decode. Generations share `AXIOM_ENGINE_CTX` tokens (default 8192); one answer may use all of it up to the model's trained length, and when it fills, paused sequences are dropped (LRU) before the longest running answer is cut off. Dropping an `Engine` frees its models at once. Model test: `AXIOM_ENGINE_MODEL=ollama:qwen2.5:3b-instruct cargo test -p engine`. An exit hook (`platform::at_exit`) frees GPU memory before ggml's Metal teardown, which otherwise asserts.
- macOS limits unix socket paths to 104 bytes, so keep `AXIOM_HOME` short.

## AI activity monitor

- `axiom monitor [SECS]` lists the processes running local AI over a window, with their GPU busy share, CPU, CPU power, memory and Neural Engine memory. `--all` also lists every other process that used the GPU; `--watch` keeps refreshing.
- A process counts as AI when it holds Neural Engine memory, or holds model weights. Weights are found three ways: a model file named on its command line (how Ollama's `llama-server` holds its blob), an open or mapped model file (scanned only for processes that use the GPU, the Neural Engine, or ≥20% CPU), or file content. GGUF and safetensors are matched by content; Core ML, ONNX, TFLite and PyTorch by name.
- Needs no root. GPU time comes from the IOKit registry (`ioreg`); the other counters from `proc_pid_rusage`, which only works for this user's processes.
- CPU power covers the CPU only, because macOS does not attribute GPU energy to processes. Cloud AI apps do not show.

## Measuring

- `axiom bench` runs each workload three ways: no load, background load sent straight to the model server, and the same load through a private axion (`$AXIOM_HOME/bench`). It reports interactive p50/p95/max, background throughput, GPU busy and AI memory. Workloads: `embed` (needs `AXIOM_EMBED_MODEL`) and `generate` (needs `AXIOM_LLM_MODEL`).
- `axiom daemon status` shows queue wait p50/p95 and run time per tier over the last 1024 jobs (`Nrm::recent`), plus requests and time per client.
- Results and their reading: README, "Measuring".

## Mesh and apps

- Mesh pairing: `axiom mesh key` on device A, `axiom mesh join <hex>` on device B (shared 32-byte key, used as the PSK of a Noise NNpsk0 handshake). `mesh serve 0.0.0.0:7777` on one, `mesh sync host:7777` on the other. Objects union, edges last-writer-wins, deletions sync as tombstones, file names sync as `named` edges. Mesh traffic is logged in the egress audit log (kind `mesh`).
- Addresses: `relay://host:port` meets the peer at a relay (`axiom mesh relay 0.0.0.0:7778` on any reachable box; it sees only ciphertext). A comma list fails over in order (LAN first, relay second).
- Session state roams: workspaces, the active workspace (`ws open` with no name) and the clipboard (`clip push` / `clip pull`) are graph edges, so they sync.
- Resource broker: `mesh serve ADDR --jobs` runs embed/generate/speak for peers at BACKGROUND priority. `AXIOM_OFFLOAD=<addr>` runs this device's inference on that peer; `AXIOM_AUDIO_SINK=<addr>` plays `--speak` there.
- Apps: a directory with `manifest.json` (`{"name","cmd":[...] or "wasm":"file.wasm","requires":["search"|"search:tag=<t>"|"tag"|"tag:tag=<t>"|"generate"|"egress:<host>"]}`). `app install` asks per capability (non-interactive = grants nothing); `generate` expires after 1h. Native apps run under macOS `sandbox-exec` or Linux `bwrap` (no network, no writes, no reads of `$AXIOM_HOME`; untested on Linux), elsewhere they refuse to run. Wasm apps run in-process (wasmi, fuel-limited) on any host. Both talk JSON to the broker in `crates/shell/src/apps.rs`; `crates/app-sdk` (`axiom-app`) wraps it for Rust.
- Signed apps: `app sign DIR` (key in `$AXIOM_HOME/publisher.key`), `app trust KEY`. A signed bundle needs a trusted publisher; an update needs the same publisher; unsigned bundles cannot replace signed ones.

## Portability rules (so the layer can move to another kernel)

1. All OS access goes through the `platform` crate. Other crates never touch `std::fs`, `std::process` or sockets.
2. Sockets, randomness and sandboxed processes also live in `platform`.
3. Inference goes through the `NpuBackend` trait (`core-ai`); a vendor NPU backend implements it (see `specs/build/hardware-support.md`).
4. Egress goes only through `Egress::send`. Its audit log lives in its own DB, so a data rollback cannot erase it.
5. Decisions on the open questions: `specs/open/open-questions.md`, "Decisions" section.

## Map

- `platform`: host access (files, clock, HTTP client and server, unix sockets, launchd, process accounting, TTS).
- `core-data` P2: Blake3 + FastCDC objects, graph edges, passage vectors, snapshots (one SQLite file).
- `core-ai` P3: `NpuBackend`/`LocalBackend`, `Nrm` (priority tiers + aging), planner, summarizer, egress firewall.
- `mesh` P4: encrypted two-device sync (`sync`, `serve_one`).
- `shell` P5/P6: `Axiom` facade, CLI (`axiom`), TUI, `apps` sandbox broker + wasm runtime + signing, `daemon` (`axion` server and its client backend), `monitor` (AI activity monitor).
- `engine` P3: llama.cpp in-process (Metal), an `NpuBackend` with resumable sequences.
- `app-sdk` P6: `axiom-app` crate for app authors (native and wasm32).
