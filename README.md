# Axiom

**An AI-native operating system layer.** AI is a system resource with one scheduler. Your files form a graph instead of a folder tree. Apps hold only the capabilities you grant. Nothing leaves the device without your consent. All your devices work as one computer.

This repository holds two things:

- **The blueprint** ([`specs/`](specs/)): the architecture of a complete operating system in seven planes, from a capability microkernel up to the app runtime. Every spec is tagged by feasibility, from "buildable today" to "research".
- **A working prototype** ([`crates/`](crates/)): a Rust workspace that runs today as a layer on top of macOS. It implements the data plane, the AI core, the device mesh, the app runtime, a system AI daemon (`axiomd`) and an AI activity monitor.

> **Status:** personal prototype, v0.1. macOS on Apple Silicon is the tested platform. Most of it also runs on Linux, but parts of the Linux path are untested. Windows is not supported yet.

## Contents

- [The idea](#the-idea)
- [Architecture](#architecture)
- [Getting started](#getting-started)
- [Command reference](#command-reference)
- [axiomd: AI as a system service](#axiomd-ai-as-a-system-service)
- [AI activity monitor](#ai-activity-monitor)
- [Device mesh](#device-mesh)
- [Apps](#apps)
- [Egress firewall](#egress-firewall)
- [Configuration](#configuration)
- [Data layout](#data-layout)
- [Security model](#security-model)
- [Design rules](#design-rules)
- [Testing](#testing)
- [Status and roadmap](#status-and-roadmap)
- [Repository layout](#repository-layout)

## The idea

Today's operating systems were designed before AI. Every AI app brings its own model and loads its own copy into memory. They all compete for the same GPU, and each one can read whatever the user can read. Files sit in folders that record where they are, not what they are. Each device is an island.

Axiom rethinks six things:

| Principle | What it means |
|---|---|
| **AI is a system resource** | A Neural Resource Manager (NRM) owns accelerator time and model memory, the way a CPU scheduler owns cores. There is one queue with priority tiers and preemption, and one loaded copy of each model. |
| **Data is a graph** | Content-addressed objects carry tags, projects, names and provenance as graph edges. You find things by what they are and how they relate, with semantic search over everything. |
| **Capabilities, not ambient authority** | An app gets only what you grant: search, a tag scope, model access, one network host. Grants are checked on every call, and revoking one takes effect immediately. |
| **Nothing leaves without consent** | Every outbound call passes an egress firewall. It denies by default and keeps a per-destination allowlist. Model context needs per-request approval and has a byte budget. The audit log survives a rollback. |
| **All devices are one computer** | An encrypted mesh syncs data and roams session state between your devices, and one device can run inference for another. |
| **Intent first** | A workspace is a saved query over the graph ("Q3 planning"), not a folder or a set of app windows. |

The blueprint's key observation: P0 (the capability kernel) and P3 (the AI core) are the only planes that change what an OS *is*. The rest follow from them.

## Architecture

### The seven planes (target system)

```
 P6  Runtime    apps in capability-bounded containers, SDK, signed bundles
 P5  Session    intent workspaces, info layers, unified input, compositor
 P4  Mesh       trusted device mesh: sync, state roaming, resource brokerage
 P3  AI core    Neural Resource Manager, model registry, semantic index, egress firewall
 P2  Data       content-addressed object store, relationship graph, snapshots
 P1  Servers    user-space drivers, filesystem, network stack (crash-isolated)
 P0  Kernel     microkernel: capabilities, IPC, scheduling, verified boot
```

P0 is the only trusted code. Everything above it runs as unprivileged, crash-isolated servers that hold authority only through unforgeable capabilities. Each spec carries one of four feasibility tags:
- 🟢 buildable
- 🟡 hard but known
- 🔴 research
- ⏳ decades

The plan for the kernel (decision P0.1): Linux first, later seL4 with Linux in a VM for drivers. The first bare-metal target is the RK3588. Phase-gated milestones are in [`specs/build/sequence.md`](specs/build/sequence.md).

### What the prototype implements

The prototype has no kernel of its own. It runs on a host OS, and the `platform` crate stands in for P0 and P1: it is the only code that touches the host. Moving Axiom to another kernel means rewriting that one crate.

```
  axiom CLI / TUI          sandboxed apps              other AI tools
         │               (native or wasm)           (OpenAI-compatible)
         │                      │ JSON calls,                │
         │                      │ checked per grant          │ HTTP + API key
         ▼                      ▼                            │
  ┌───────────────────────────────────────────────┐          │
  │ shell:   Axiom facade, app broker, monitor    │          │
  │ core-ai: planner, summarizer, egress firewall │          │
  └─────────┬─────────────────────────┬───────────┘          │
            │ objects, graph,         │ embed / generate     │
            │ vectors                 │ with priority tier   │
            ▼                         ▼ (unix socket)        ▼
  ┌──────────────────┐   ┌────────────────────────────────────────────┐
  │ core-data        │   │ axiomd: one NRM queue, one model backend   │
  │ (one SQLite db)  │   └─────────────────────┬──────────────────────┘
  └─────────┬────────┘                         ▼
            │ mesh: Noise-encrypted     Ollama, llama-server or any
            ▼ sync, roaming, jobs       OpenAI-compatible server, or a
      your other devices                paired device (AXIOM_OFFLOAD)
```

When `axiomd` is not running, each Axiom process runs its own NRM in-process with the same backend.

| Crate | Plane | Responsibility |
|---|---|---|
| [`platform`](crates/platform) | P0/P1 stand-in | All host access: files, clock, HTTP client and server, TCP and unix sockets, randomness, sandboxed processes, launchd services, clipboard, text-to-speech, per-process accounting |
| [`core-data`](crates/core-data) | P2 | Blake3 + FastCDC content-addressed objects, graph edges (tags, names, projects, provenance), passage vectors, FTS5 keyword index, replicated deletions (tombstones), copy-on-write snapshots; all in one SQLite file |
| [`core-ai`](crates/core-ai) | P3 | `NpuBackend` trait and `LocalBackend`; the NRM (priority tiers, aging, chunk-level preemption); planner; summarizer; egress firewall |
| [`mesh`](crates/mesh) | P4 | Noise NNpsk0 pairing, two-device sync, relay, address failover, resource broker, `RemoteBackend` for offloading inference |
| [`shell`](crates/shell) | P5/P6 | The `Axiom` facade, the `axiom` CLI and TUI, the app broker (OS sandbox, wasm runtime, signing), `axiomd` and the AI activity monitor |
| [`app-sdk`](crates/app-sdk) | P6 | The `axiom-app` crate for app authors, native and wasm32 |

### How the pieces work

**Import and ask.** `axiom import DIR` takes each text file and:
- stores it as a content-addressed object;
- records its file name;
- tags it with its folder name;
- links it to a project, if one is given;
- embeds its passages at background priority.

`axiom ask` then answers in four steps:
1. **Plan.** The planner pulls dates ("Q3", "last week", "yesterday"), known tags and search terms out of the question. When an LLM is configured, it refines the terms and tags.
2. **Search.** Below 0.7 confidence, the query falls back to keyword search. Otherwise it runs hybrid search: cosine similarity over passage vectors, filtered by tags, project and time span, with matching tags boosted.
3. **Summarize.** Summaries come from the LLM, or are extractive when none is configured.
4. **Save (optional).** Writes always need an explicit flag. `--save` stores the answer as a new object linked `derived_from` its sources.

**Scheduling.** The NRM has three tiers: realtime, interactive and background.
- **Aging:** a waiting job gains one tier every 30 seconds, so background work never starves.
- **Preemption:** embeddings run in 16-text chunks, so a higher-priority job waits for at most one chunk.
- **One queue:** with `axiomd` running, every process's jobs share the daemon's queue.

**Session roaming.** Workspaces, the active workspace and the clipboard are stored as graph edges, so they travel with every mesh sync. `axiom ws open` with no name reopens whatever workspace was last active, possibly on another device.

## Getting started

### Requirements

- **macOS on Apple Silicon** is the tested platform. Linux runs the core, but its app sandbox (bubblewrap) is untested. `axiom daemon install` and `axiom monitor` are macOS-only.
- **A recent stable Rust.** Edition 2024 needs 1.85 or newer; development uses 1.98.
- **Optional: Ollama**, or any OpenAI-compatible server, for real models. Without one, Axiom runs fully offline with lexical embeddings and extractive summaries.

### Build and test

```sh
cargo build --release      # builds target/release/axiom and target/release/axiomd
cargo test                 # gate tests
```

Put `target/release` on your `PATH`, or prefix commands with `cargo run --release --`. Data lives in `~/.axiom` unless `AXIOM_HOME` says otherwise.

### First run

```sh
axiom import ~/notes --project q3      # index a folder of text files
axiom ask "summarize Q3 docs"
axiom ask "what did we decide about pricing" --save
axiom ws save planning "q3 finance"    # a workspace is a saved query
axiom brief                            # daily briefing
axiom tui                              # terminal UI (Esc quits, Ctrl-S speaks)
```

### Use real models

```sh
ollama pull qwen2.5:3b-instruct
ollama pull nomic-embed-text
export AXIOM_LLM_MODEL=qwen2.5:3b-instruct   # LLM summaries and query planning
export AXIOM_EMBED_MODEL=nomic-embed-text    # semantic vectors
```

A store is tied to one embedding dimension. To change the embedding model, use a fresh `AXIOM_HOME` and import again.

## Command reference

| Command | What it does |
|---|---|
| `axiom import DIR [--project P]` | Index a folder of text files |
| `axiom ask QUERY [--cloud URL] [--save] [--speak]` | Answer a question from your data. `--cloud` sends context through the egress firewall, `--save` stores the answer, `--speak` reads it aloud |
| `axiom tag OBJ TAG` / `axiom untag OBJ TAG` | Tag or untag an object (a path or an id prefix). Removals sync as tombstones |
| `axiom ws save NAME QUERY` / `ws open [NAME]` / `ws list` | Workspaces as saved queries. `open` without a name reopens the active one |
| `axiom clip push` / `pull` / `show` | Universal clipboard across paired devices |
| `axiom brief [--speak]` | Daily briefing: new objects, top tags, each workspace's top hits |
| `axiom snapshot [LABEL]` / `axiom rollback [FILE]` | Snapshot the store, or roll back (default: latest snapshot) |
| `axiom egress log` / `allow DEST` / `budget BYTES` | Inspect and configure the egress firewall |
| `axiom tui` | Terminal UI |
| `axiom mesh key` / `join HEX` / `serve ADDR [--jobs]` / `sync ADDR[,ADDR]` / `relay ADDR` | Device mesh |
| `axiom app install DIR` / `run NAME [ARGS]` / `list` / `revoke NAME [CAP]` / `sign DIR` / `trust KEY` | Sandboxed apps |
| `axiom daemon status` / `install` / `uninstall` / `key NAME` / `revoke NAME` | The system AI service |
| `axiom monitor [SECS] [--all] [--watch]` | AI activity monitor |
| `axiomd` | Run the system AI service in the foreground |

## axiomd: AI as a system service

`axiomd` holds the model and the single NRM queue for your user account. Every `axiom` process sends its AI work to it over `$AXIOM_HOME/axiomd.sock`, along with each job's priority tier. A question asked in one process therefore runs ahead of a background import running in another, and the model is loaded once. When the daemon is not running, processes fall back to in-process inference.

```sh
axiomd                    # run in the foreground
axiom daemon install      # macOS LaunchAgent: starts at login, restarts if it exits
axiom daemon status
axiom daemon uninstall
```

```
axiomd up 7s, embedding dim 256, LLM on, API http://127.0.0.1:7779/v1
queued: realtime 0, interactive 0, background 0
requests: editor 3, local 4
```

`install` copies your shell's `AXIOM_*` variables into the LaunchAgent, because launchd passes no shell environment. While the daemon runs, its model settings apply to every Axiom process.

### Connect other AI tools

Other programs reach the daemon through an OpenAI-compatible API. Each tool gets its own key:

```sh
axiom daemon key editor        # prints the key once; only its hash is stored
axiom daemon revoke editor     # cuts the tool off on its next request
```

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:7779/v1", api_key="ax-...")
reply = client.chat.completions.create(model="axiom", messages=[{"role": "user", "content": "Hi"}])
print(reply.choices[0].message.content)
```

| Endpoint | Notes |
|---|---|
| `GET /v1/models` | Lists one model, `axiom` |
| `POST /v1/chat/completions` | `stream: true` is supported but sends the whole answer as one chunk |
| `POST /v1/embeddings` | `input` is a string or a list of strings |

Requests without a valid key get `401`. A key reaches inference only, never your data. The daemon adds about 45 µs per request (100 µs in the slowest 5%) and uses about 6 MB of memory without a model.

**What it can and cannot capture.** A tool uses the daemon only if it is pointed at the daemon. Cloud AI apps (Claude, ChatGPT) run on remote servers. Apps with a built-in engine (LM Studio's chat, Draw Things) send work straight to the GPU. macOS lets no outside program take over another app's GPU work, so neither kind can be routed through `axiomd` today. The roadmap below covers how to reach further.

## AI activity monitor

`axiom monitor` shows which processes run local AI and what they use. It needs no admin rights.

```
   PID  PROCESS               GPU    CPU  CPU W   MEMORY  NEURAL  MODEL
 79487  llama-server        86.4%  75.8%   1.19   2.3 GB       -  Qwen2.5 3B Instruct (GGUF 1.8 GB)
 15544  Google Chrome        0.0%   0.3%   0.00   221 MB       -  model.tflite (TFLite 4 MB)
   738  Siri AI              0.0%   0.0%   0.00   197 MB       -  spotlight_l2 (Core ML)
AI: GPU 86.4%, CPU 76.1%, 1.19 W CPU, memory 2.7 GB. GPU busy, all processes: 94.2%. Window 3.0s.
```

```sh
axiom monitor              # one report over 2 seconds
axiom monitor 30           # average over a 30-second workload (an efficiency baseline)
axiom monitor --watch      # keep refreshing
axiom monitor --all        # also list every other process that used the GPU
```

A process counts as AI when it holds Neural Engine memory or model weights. Weights are found in three ways:
- **Command line:** a model file named on it, which is how Ollama's `llama-server` holds its blob.
- **Open files:** a model file the process has open or memory-mapped.
- **File content:** GGUF and safetensors files are recognised by their first bytes. Core ML, ONNX, TFLite and PyTorch files are recognised by name. GGUF headers also give the model's real name.

**Where the numbers come from.** GPU time comes from the IOKit registry. CPU, memory and Neural Engine figures come from `proc_pid_rusage`, which only works for your own processes.

**Limits:**
- Power covers the CPU only, because macOS does not attribute GPU energy to processes.
- Cloud AI apps do not appear.
- A report costs about 0.15 s of CPU.

## Device mesh

Pair two devices with a shared 32-byte key, then sync:

```sh
# device A
axiom mesh key                          # prints the key as hex and a QR code
axiom mesh serve 0.0.0.0:7777 --jobs    # --jobs also runs inference for peers

# device B
axiom mesh join <hex>
axiom mesh sync deviceA.local:7777
```

**What a sync does:**
- Objects are unioned.
- Edges are last-writer-wins.
- Deletions travel as tombstones.
- File names travel as `named` edges.

**Behind NAT.** Run a relay on any reachable machine. It only ever sees ciphertext. Give a comma-separated address list to fail over in order:

```sh
axiom mesh relay 0.0.0.0:7778                         # on a reachable box
axiom mesh serve relay://box.example.com:7778         # device A
axiom mesh sync 192.168.1.5:7777,relay://box.example.com:7778   # device B: LAN first
```

**Resource broker.** `AXIOM_OFFLOAD=<addr>` runs this device's inference on a peer that serves with `--jobs`, at background priority there. `AXIOM_AUDIO_SINK=<addr>` plays `--speak` on that peer. All mesh traffic is recorded in the egress audit log.

## Apps

An app is a directory with a `manifest.json`:

```json
{"name": "summarizer", "cmd": ["python3", "main.py"], "requires": ["search:tag=work", "generate"]}
```

Use `"wasm": "app.wasm"` instead of `cmd` for a WebAssembly app.

| Capability | Grants |
|---|---|
| `search` / `search:tag=<t>` | Search all objects, or only objects tagged `t` |
| `tag` / `tag:tag=<t>` | Tag any object, or only objects tagged `t` |
| `generate` | Use the LLM. Expires after one hour |
| `egress:<host>` | Fetch from one host. The egress firewall still has to approve |

**Installing and checking grants.** `axiom app install DIR` asks you about each capability. A non-interactive install grants nothing. Every call is checked against the grant table when it happens, so `axiom app revoke` takes effect immediately.

**Native apps** run under `sandbox-exec` on macOS or `bwrap` on Linux. They get no network, no writes, and no reads of `$AXIOM_HOME`, which also keeps them away from the daemon socket. On any other host, native apps refuse to run.

**Wasm apps** run in-process on wasmi, with a fuel budget (about 2 s of CPU) and a 256 MiB memory cap, on any host.

**Protocol.** Apps talk to the broker in JSON lines: `{"id":1,"call":"search","args":{"q":"revenue"}}` is answered with `{"id":1,"ok":...}` or `{"id":1,"error":"..."}`. The calls are `search`, `tag`, `generate`, `fetch` and `log`. The [`axiom-app`](crates/app-sdk) crate wraps this for Rust:

```rust
fn main() {
    match axiom_app::search("revenue") {
        Ok(hits) => axiom_app::log(&format!("{} hits", hits.as_array().map_or(0, |h| h.len()))),
        Err(e) => axiom_app::log(&format!("search refused: {e}")),
    }
}
```

**Signed apps.** `axiom app sign DIR` signs a bundle with your publisher key (ed25519), and `axiom app trust KEY` trusts a publisher. A signed bundle needs a trusted publisher, and an update must come from the same publisher. An unsigned bundle can never replace a signed one.

## Egress firewall

Every outbound call from Axiom goes through `Egress::send`:
- Calls are denied by default.
- Destinations are allowed one by one.
- Model context (your data sent to a model) always asks first, and has a byte budget that defaults to 0.
- Every decision is written to an audit log in its own database, so a data rollback cannot erase it.

```sh
axiom egress allow api.example.com
axiom egress budget 20000
axiom ask "summarize Q3 docs" --cloud https://api.example.com/v1/summarize
axiom egress log
```

A `--cloud` endpoint receives `{"prompt": ...}` and must reply with `{"response": ...}`, the shape of Ollama's `/api/generate`.

## Configuration

| Variable | Purpose | Default |
|---|---|---|
| `AXIOM_HOME` | Data directory | `~/.axiom` |
| `AXIOM_LLM_MODEL` | LLM for summaries and query planning | unset: extractive summaries |
| `AXIOM_LLM_URL` | OpenAI-compatible server for the LLM | `http://localhost:11434` |
| `AXIOM_EMBED_MODEL` | Embedding model for semantic vectors | unset: lexical hash embeddings |
| `AXIOM_EMBED_URL` | OpenAI-compatible server for embeddings | `http://localhost:11434` |
| `AXIOM_OFFLOAD` | Run inference on a paired device (`host:port`, `relay://...`, or a comma list) | unset |
| `AXIOM_AUDIO_SINK` | Play `--speak` on a paired device | unset |
| `AXIOM_HTTP` | Address of the `axiomd` API | `127.0.0.1:7779` |

macOS limits unix socket paths to 104 bytes, so keep `AXIOM_HOME` short.

## Data layout

Everything lives in `$AXIOM_HOME`:

| Path | Contents |
|---|---|
| `axiom.db` | Objects, graph edges and passage vectors (SQLite) |
| `snapshots/` | Copy-on-write snapshots of `axiom.db` |
| `egress.db` | Egress allowlist, budget and audit log (not touched by rollback) |
| `apps.db`, `apps/` | App grants, installs, trusted publishers and call audit; installed bundles |
| `daemon.db` | Hashes of `axiomd` API keys |
| `mesh.key`, `publisher.key` | Mesh key and app signing key (mode 0600) |
| `axiomd.sock`, `axiomd.log` | Daemon socket (mode 0600) and the LaunchAgent's log |

## Security model

| Boundary | Protection |
|---|---|
| Apps and your data | OS sandbox, deny-by-default capabilities checked on every call, immediate revocation, a one-hour lifetime for model access, signed bundles |
| Your data and the network | Egress firewall: allowlist, approval and byte budget for model context, audit log that survives rollback |
| Other programs and the AI service | Socket only your user can open; per-client API keys on a loopback port, which also blocks web pages from using it; keys stored as hashes and limited to inference |
| Your devices and the network | Noise NNpsk0 with a pre-shared key, so traffic is encrypted, authenticated and forward-secret; relays see only ciphertext |
| Mistakes | Snapshots and rollback; the audit log is kept separately, so a rollback cannot erase it |

The full threat model is in [`specs/security/`](specs/security/).

## Design rules

These keep the layer portable to another kernel:

1. All OS access goes through the `platform` crate. Other crates never touch `std::fs`, `std::process` or sockets.
2. Sockets, randomness and sandboxed processes also live in `platform`.
3. Inference goes through the `NpuBackend` trait. A vendor NPU backend implements it (see [`specs/build/hardware-support.md`](specs/build/hardware-support.md)).
4. Egress goes only through `Egress::send`, and its audit log lives in its own database.
5. Decisions on the open design questions are recorded in [`specs/open/open-questions.md`](specs/open/open-questions.md), in the "Decisions" section.

## Testing

`cargo test` runs these gate tests. The `shell` tests need macOS for the sandbox and the IOKit registry.

| Test | Covers |
|---|---|
| `shell/tests/gate.rs` | End to end: import, planner, hybrid search, egress firewall, NRM priority, aging and preemption |
| `shell/tests/apps.rs` | Sandbox escapes (writes, reads, network, the daemon socket), grants, scoped and expiring capabilities, revocation, wasm apps, signing |
| `shell/tests/daemon.rs` | Priority across processes, API keys, embeddings, chat, streaming, malformed requests, revocation |
| `shell/tests/monitor.rs` | A process holding a model shows up with its counters |
| `mesh/tests/sync.rs` | Merges in any order, tombstones, tampered objects, dimension mismatch, relay failover, offload |
| unit tests | Planner refinement, GGUF and model detection, IOKit parsing, SDK reply handling |

## Status and roadmap

**Built:**

| Plane | What exists |
|---|---|
| P2 data | Objects, graph, tombstones, snapshots |
| P3 AI core | NRM, planner, summarizer, egress firewall, `axiomd`, AI activity monitor |
| P4 mesh | Pairing, sync, relay, failover, session roaming, resource broker |
| P5 session | CLI, TUI, briefing |
| P6 runtime | Capability broker, native and wasm apps, Rust SDK, signing |

**Not built:**
- The kernel and drivers (P0/P1, deferred by decision)
- UDP hole punching
- MPTCP and cellular failover
- Credit limits for the broker
- Swift and Kotlin SDKs
- WebGPU and WebNN for wasm apps
- A hosted app catalog
- Gaze-based audio routing
- A compositor

**Known gaps:**
- **Offline embeddings are lexical.** Semantic search needs a model server.
- **Some jobs can't be interrupted.** A running `generate` job cannot be preempted.
- **The daemon's API is simplified.** It flattens chat roles into one prompt and streams the whole answer as one chunk.
- **Snapshots are only O(1) on copy-on-write filesystems.**
- **The Linux sandbox is untested.**
- **The daemon runs on macOS only.** There is no Linux or Windows service for `axiomd` yet.

**Next steps:**
1. **Measure.** Time each job in `axiomd` (queue wait and run time per tier and per client), and add `axiom bench` to compare answer latency under background load, through the daemon versus direct to the model server.
2. **Stream and preempt.** Token streaming over the API, and a `generate` that higher-priority work can interrupt.
3. **Bundle an engine.** Put llama.cpp with Metal inside `axiomd`, so it owns the model instead of fronting Ollama.
4. **Ship it.** A signed, notarized `.dmg` with `axiomd` registered at login.
5. **Windows.** Port the `platform` crate and ship an `.exe` installer.
6. **Reach more AI apps.**
   - Serve the Ollama and LM Studio ports, identifying each caller by its process.
   - Add a Network Extension that makes the egress firewall system-wide for cloud AI apps.
   - On Linux, use eBPF, cgroups and GPU scheduler priorities to capture every local AI workload.

## Repository layout

```
.
├── Cargo.toml              workspace
├── README.md               this file
├── CLAUDE.md               working notes for coding agents: run commands, rules, crate map
├── axiom-blueprint.html    visual overview of the blueprint (open in a browser)
├── crates/
│   ├── platform/           host OS access; the only crate that touches the OS
│   ├── core-data/          P2: object store, graph, vectors, snapshots
│   ├── core-ai/            P3: NRM, planner, summarizer, egress firewall
│   ├── mesh/               P4: device mesh and resource broker
│   ├── shell/              P5/P6: CLI, TUI, apps, axiomd, AI monitor
│   └── app-sdk/            P6: SDK for app authors
└── specs/                  the blueprint
    ├── MASTER.md           index and cross-references; start here
    ├── planes/             P0 to P6
    ├── core/ data/ ai/ mesh/ session/ security/ build/
    └── open/               open questions, decisions, trade-offs, glossary
```

## License

No license has been chosen yet.
