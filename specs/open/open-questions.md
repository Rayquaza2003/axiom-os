# Open Questions — Axiom OS

**Status:** 🟡 PENDING USER INPUT  
**Purpose:** Every plane raised unresolved design forks. This file aggregates them so you can decide in one pass. Each question notes the impact and the default I'd pick if none is given.

---

## Decisions (2026-10-01)

Chosen for best result, not spec defaults. Questions not listed keep their stated default; mesh, runtime and
kernel questions are deferred until the prototype proves out.

| Question | Decision |
|----------|----------|
| P0.1 kernel base | Linux now. Later seL4 with Linux-in-VM for drivers. Not Zircon. |
| P0.2 drivers | Userspace, except interrupt routing and timer. |
| P0.3 capability TTL | Short default TTL for camera, mic and model-context caps; explicit revoke for the rest. |
| P2.2 graph store | Objects, edges, metadata and vectors in one transactional embedded DB (SQLite). Vector search is a linear scan (~280ms at 400k passages on M-series); add sqlite-vec or HNSW past ~1M passages. |
| P3.1 NRM policy | Strict priority tiers plus aging so Background cannot starve. (`crates/core-ai/src/nrm.rs`) |
| P3.3 planner | <0.7 keyword fallback; >=0.7 execute read-only; any write needs an explicit flag (`--save`). |
| S.1 egress | Deny by default; per-destination allowlist; model-context always asks and has a byte budget (default 0). (`crates/core-ai/src/egress.rs`) |
| BS.2 hardware | Apple Silicon Mac for the prototype; RK3588 for the first bare-metal target. |
| BT.1 build | Cargo workspace for the prototype; Bazel only when a multi-language kernel build exists. |

Prototype status: Rust workspace (`platform`, `core-data`, `core-ai`, `mesh`, `shell`, `app-sdk`). Built: P2 (Blake3 + FastCDC objects,
graph with replicated deletions, copy-on-write snapshots), P3 (NRM with chunk-level preemption, rule planner refined by an LLM when one is
configured, egress firewall), P4 (Noise NNpsk0 pairing with a shared key shown as hex + QR; objects union, edges LWW, tombstones;
relay for NAT, address-list failover; session roaming of workspaces, active workspace and clipboard; resource broker for
embed/generate/speak offload), P5 subset (CLI, TUI, briefing), P6 (capability broker with tag-scoped and expiring grants; native apps
under sandbox-exec or bwrap; wasm apps on wasmi; Rust SDK; ed25519-signed bundles with trusted publishers and same-publisher updates).
Not built: kernel and drivers (P0/P1, by decision P0.1), UDP hole punching, MPTCP / cellular failover, credit limits (jobs are counted,
not capped), Swift/Kotlin SDKs, WebGPU/WebNN for wasm apps, a hosted app catalog, gaze-based audio routing.
Known gaps: offline embeddings are lexical (semantic needs an OpenAI-compatible server); an LLM `generate` job cannot be preempted;
snapshots are only O(1) on copy-on-write filesystems; the Linux sandbox path is untested; wasm apps get a fixed fuel budget (~2s CPU).
---

## P0 — Kernel

### P0.1 Microkernel base choice
**Question:** seL4 (verified, but limited drivers/hw support), Fuchsia Zircon (modern, Google-backed, capability-native), or a custom from scratch?
**Impact:** Determines driver ecosystem, verification story, porting cost.
**Default:** Zircon-derived core + seL4-inspired verification harness for P0 syscalls. Best balance of capability-native design and real hardware support.

### P0.2 Driver model
**Question:** In-kernel drivers (faster, less safe) vs userspace driver processes (slower IPC, safer)?
**Impact:** Latency for display/network/storage; blast radius of a bad driver.
**Default:** Userspace drivers for everything except interrupt routing + timer. Matches capability model.

### P0.3 Time source for capabilities
**Question:** Should capability grants have TTL/expiry, or be revoked only by explicit action?
**Impact:** UX friction vs attack surface.
**Default:** No TTL by default; explicit revoke. Optional TTL for sensitive caps (camera, mic) via user policy.

---

## P1 — System Servers

### P1.1 Resource accounting granularity
**Question:** Per-app, per-process, or per-capability accounting for CPU/mem/IO?
**Impact:** Overhead vs observability for users.
**Default:** Per-app namespace, sampled at 100ms, attributed to capability holder.

### P1.2 Crash recovery policy
**Question:** Auto-restart failed server (risk: loop), or fail-closed (risk: unavailable feature)?
**Impact:** Availability vs safety.
**Default:** Restart with exponential backoff (max 5); if still failing, isolate and notify user. Never auto-restart P0.

---

## P2 — Data Plane

### P2.1 Object storage backend
**Question:** Copy-on-write B-tree (ostree-style), log-structured (F2FS/ZNS), or content-defined chunking (restic-style)?
**Impact:** Dedupe ratio, snapshot cost, garbage collection complexity.
**Default:** Content-defined chunking + B-tree index. Max dedupe across devices; cheap snapshots.

### P2.2 Graph store co-location
**Question:** Graph edges as separate store, or embedded in object metadata?
**Impact:** Query performance for "related items"; update atomicity.
**Default:** Separate graph store (RocksDB / Sled) with edge records pointing to object IDs. Enables graph-only queries without loading objects.

---

## P3 — AI Core

### P3.1 NRM scheduling policy
**Question:** Strict priority (real-time AI beats background), fair-share, or deadline-based?
**Impact:** Responsiveness of voice/planner vs throughput of indexing.
**Default:** Strict priority tiers: Realtime (voice, autocomplete) > Interactive (planner, summarize) > Background (index, train). Preemption allowed down tiers only.

### P3.2 Model store signing authority
**Question:** Who signs model packages — Axiom root only, or community/third-party keys user trusts?
**Impact:** Ecosystem openness vs supply-chain risk.
**Default:** Axiom root for system models; user-added keys for community models (explicit trust prompt).

### P3.3 Local planner confidence threshold
**Question:** Below what parse confidence do we fall back to keyword search vs ask user to rephrase?
**Impact:** Search quality vs friction.
**Default:** < 0.7 → keyword fallback (silent); 0.7–0.9 → execute + log for review; > 0.9 → execute.

---

## P4 — Mesh

### P4.1 Relay trust model
**Question:** Only user-run relays, or vetted community relays, or untrusted public relays?
**Impact:** Metadata leakage surface (next-hop + timing).
**Default:** User-run + community relays with published keys; untrusted relays allowed but flagged (only carry ciphertext, no identity).

### P4.2 NAT traversal strategy
**Question:** STUN/TURN-style (centralized), libp2p AutoNAT, or hole-punch relay always-on?
**Impact:** Connectivity rate vs infrastructure cost.
**Default:** libp2p AutoNAT + relay fallback. No always-on central server.

### P4.3 Offline-first conflict resolution
**Question:** CRDT (always-merge) vs OT (server-order) vs last-write-wins with vector clocks?
**Impact:** Multi-device edit conflicts for notes/docs.
**Default:** CRDT for text/structured data; vector-clock LWW for binary blobs. Per-object type policy.

---

## P5 — Session

### P5.1 Workspace persistence model
**Question:** Workspaces as saved queries (stateless, recomputed) or materialized collections (stateful)?
**Impact:** Staleness vs storage cost.
**Default:** Saved graph queries + cached materialization. Recompute on open; refresh in background.

### P5.2 State roaming granularity
**Question:** Full session snapshot (heavy) vs incremental deltas (complex) vs app-declared state?
**Impact:** Roam latency vs fidelity.
**Default:** App-declared state + system snapshot of window/input state. Apps opt into fine-grained roaming.

---

## P6 — Runtime

### P6.1 App sandbox escape policy
**Question:** If app needs cap it lacks, prompt user every time, or remember per-app grant?
**Impact:** Friction vs privilege creep.
**Default:** Prompt once per app + cap combo; remember until user revokes. Show what data the cap touches.

### P6.2 Legacy/non-Axiom app support
**Question:** Linux binary compat layer (slow, large), web-only runtime, or native-only?
**Impact:** App availability vs attack surface.
**Default:** Native + web runtime only for v1. Linux compat as opt-in server (P1) later.

---

## Security

### S.1 Egress firewall default mode
**Question:** Ask-every-time (safe, annoying), allowlist (balanced), or automatic for signed system apps (convenient, risky)?
**Impact:** Privacy vs UX.
**Default:** Allowlist for system-signed apps; ask for third-party; always ask for model-context egress.

### S.2 Attestation revocation
**Question:** How to revoke a compromised device mid-session without central server?
**Impact:** Blast radius of stolen device.
**Default:** User master key can sign device-revoke cert; pushed via mesh gossip; 10-min max propagation.

### S.3 Privacy budget
**Question:** Do we offer a "privacy budget" (max bytes/user identity exposed per day) with hard cutoff?
**Impact:** User control vs feature breakage.
**Default:** Yes — user-set daily cap; hard block + notify at limit. Default 0 for model-context.

---

## Open Questions Index (by plane)

| Plane | Questions |
|-------|-----------|
| P0 | P0.1, P0.2, P0.3 |
| P1 | P1.1, P1.2 |
| P2 | P2.1, P2.2 |
| P3 | P3.1, P3.2, P3.3 |
| P4 | P4.1, P4.2, P4.3 |
| P5 | P5.1, P5.2 |
| P6 | P6.1, P6.2 |
| Security | S.1, S.2, S.3 |

---

## Mesh — State Roaming

### SR.1 Snapshot vs delta default
**Question:** Full COW snapshot (correct, ~50-200MB) vs incremental deltas (fast, complex) as v1 default?
**Impact:** Roam latency vs implementation complexity.
**Default:** Full COW snapshot for v1. Deltas as opt-in API for apps.

### SR.2 App roam participation
**Question:** Require all apps to implement `Roamable` trait, or make it optional with graceful degradation?
**Impact:** Ecosystem adoption vs data continuity guarantee.
**Default:** Optional. Non-participating apps get window position + focus only.

### SR.3 Multi-device simultaneous edit
**Question:** Allow simultaneous editing on multiple devices (CRDT merge) or single-active handoff only?
**Impact:** Collaboration UX vs conflict complexity.
**Default:** Per-workspace policy: `simultaneous` (CRDT) or `handoff` (single active).

---

## Mesh — Resource Brokerage

### RB.1 Offload eligibility
**Question:** What task types qualify for mesh offload — only batch, or also interactive with SLA?
**Impact:** User-perceived latency vs NPU utilization.
**Default:** Batch only (indexing, training, rendering). Interactive = local only.

### RB.2 Credit accounting
**Question:** Implement reciprocal credit system (earn by providing, spend by requesting) or trust-based?
**Impact:** Fairness vs complexity.
**Default:** Trust-based for v1 (explicit user grant per peer). Credits post-v1.

### RB.3 Model availability
**Question:** Require peer to have model cached, or compile on-demand (slower first run)?
**Impact:** First-offload latency vs storage on peer.
**Default:** Compile on-demand with artifact cache. Peer policy: "cache models used > 3×".

---

## Mesh — Clipboard & Audio

### CA.1 Clipboard history retention
**Question:** How long to keep clipboard history across devices — 1 hour, 24 hours, 7 days?
**Impact:** Privacy vs utility.
**Default:** 1 hour default, user configurable (1h–30d). Encrypted in P2.

### CA.2 Audio routing priority
**Question:** Mesh audio streams preempt other mesh traffic, or share bandwidth fairly?
**Impact:** Voice quality vs sync/offload throughput.
**Default:** Audio gets dedicated QoS class (priority 80/100). Preempts batch sync.

### CA.3 Gaze input without eye-tracker
**Question:** Support gaze-following via webcam (lower accuracy) or require dedicated hardware?
**Impact:** Accessibility reach vs accuracy bar.
**Default:** Dedicated hardware only for v1. Webcam fallback post-v1.

---

## Mesh — Network Failover

### NF.1 Cellular data limits
**Question:** Per-peer daily/monthly caps enforced by phone owner, or per-app on laptop?
**Impact:** Bill shock prevention vs granularity.
**Default:** Per-peer caps (phone owner sets). Per-app breakdown for visibility.

### NF.2 MPTCP vs custom
**Question:** Use kernel MPTCP (standard, limited control) or userspace multi-path (control, complexity)?
**Impact:** Handoff seamlessness vs maintenance burden.
**Default:** Kernel MPTCP v1. Userspace fallback if kernel support missing.

### NF.3 Offline queue persistence
**Question:** Persist offline queue to P2 (survives reboot) or memory-only (simpler)?
**Impact:** Data loss on crash vs write amplification.
**Default:** Persist to P2. Max 500 MB, 7 days TTL.

---

## Session — Workspace Model

### WM.1 Query language expressiveness
**Question:** Full graph query language (Cypher-like) or simplified DSL (current spec)?
**Impact:** Power user flexibility vs implementation/maintenance.
**Default:** Simplified DSL for v1. Full language as opt-in advanced mode.

### WM.2 Materialization freshness
**Question:** Background refresh interval — 30s, 5min, 15min, or event-driven only?
**Impact:** Staleness vs battery/CPU.
**Default:** Event-driven (graph change) + periodic 5min fallback. User configurable.

### WM.3 Cross-workspace object sharing
**Question:** Same object in multiple workspaces — shared materialization or per-workspace copy?
**Impact:** Storage vs consistency.
**Default:** Shared object, per-workspace view (score, position, pins differ).

---

## Session — Info Layers

### IL.1 Briefing content sources
**Question:** Allow third-party briefing modules (news, stocks, etc.) or system-only?
**Impact:** Extensibility vs privacy surface.
**Default:** System-only for v1. Module SDK post-v1 with capability gate.

### IL.2 Contextual trigger sensitivity
**Question:** How aggressive with auto-layers — every trigger, or deduplicated (max 1/hour)?
**Impact:** Helpfulness vs annoyance.
**Default:** Max 1 contextual layer per hour per trigger type. User can increase.

### IL.3 Voice-first briefing
**Question:** TTS briefing as primary format (accessibility) or visual-first?
**Impact:** Inclusive design vs development effort.
**Default:** Visual-first. Voice format as accessibility option (uses local TTS).

---

## Session — Input Bus

### IB.1 Intent fusion default
**Question:** Enable multimodal fusion (pointer+voice+gaze) by default, or opt-in?
**Impact:** Magic UX vs privacy surprise.
**Default:** Opt-in per app. System gestures always work.

### IB.2 Gesture customization
**Question:** Full user remapping of system gestures, or fixed set with enable/disable?
**Impact:** Power user control vs consistency.
**Default:** Enable/disable per gesture. Remapping post-v1.

### IB.3 Haptics on non-haptic devices
**Question:** Synthesize haptics via audio (speaker click) on devices without Taptic Engine?
**Impact:** Consistency vs cheesiness.
**Default:** No synthesis. Haptics only on hardware that supports it.

---

## Session — Compositor

### C.1 3D scene graph vs 2D
**Question:** Full 3D scene graph (perspective, depth) or 2.5D (layers + Z)?
**Impact:** Immersive UI potential vs GPU complexity.
**Default:** 2.5D for v1. 3D as opt-in for AR/VR workspace mode.

### C.2 Theme token compatibility
**Question:** Adopt existing token spec (Design Tokens W3C, Figma, Material) or custom?
**Impact:** Tooling interop vs design freedom.
**Default:** Custom (matches capability model). Export to W3C format.

### C.3 Variable refresh rate default
**Question:** VRR on by default (power saving) or off (compatibility)?
**Impact:** Battery vs flicker on some panels.
**Default:** On for internal displays. Off for external (user enables).

---

## Build — Sequence

### BS.1 Phase overlap
**Question:** Start P3/P4/P5 in parallel after M2.3, or strict sequential?
**Impact:** Calendar time vs integration risk.
**Default:** Parallel where deps allow (see dependency graph). Integration gate at M7.

### BS.2 Hardware enablement order
**Question:** Which SoC first — RK3588 (open), M3 (laptop), Snapdragon X (Windows ARM)?
**Impact:** Dev velocity vs market relevance.
**Default:** RK3588 first (kernel bring-up), M3 + Snapdragon X in parallel (P1+).

### BS.3 Security audit timing
**Question:** Audit after each phase, or single audit at M7.5?
**Impact:** Early catch vs audit fatigue.
**Default:** Light audit per phase. Full pen test at M7.2.

---

## Build — Tooling

### BT.1 Bazel vs Cargo + Just
**Question:** Bazel (hermetic, multi-lang) or Cargo workspaces + Just (Rust-native, simpler)?
**Impact:** Build correctness vs contributor friction.
**Default:** Bazel. Rust-only components can use Cargo internally.

### BT.2 Remote execution required
**Question:** Mandate RBE for CI, or allow local-only for contributors?
**Impact:** Reproducibility vs barrier to entry.
**Default:** RBE for CI (required). Local build works without RBE (slower).

### BT.3 Model compilation in CI
**Question:** Compile all model variants in CI (hours) or on-demand + cache?
**Impact:** CI latency vs artifact completeness.
**Default:** On-demand + cache. Nightly compiles all for release artifacts.

---

## Build — Hardware Support

### HW.1 Apple NPU strategy
**Question:** MPS/CoreML userspace only (no kernel driver) — accept for v1?
**Impact:** NPU access on Mac vs kernel purity.
**Default:** Yes. MPS backend for NRM. No kernel driver needed.

### HW.2 Qualcomm NPU upstreaming
**Question:** Wait for qcom_qnp upstream, or carry out-of-tree patch?
**Impact:** Maintainability vs schedule.
**Default:** Carry patch for v1. Upstream contribution parallel.

### HW.3 Firmware blob policy
**Question:** Ship optional blobs in OS image, or download on first use?
**Impact:** Offline install vs image size / licensing.
**Default:** Download on first use (with user consent). Offline install package available.

---

## Build — Compatibility

### COMP.1 Linux compat priority
**Question:** CLI tools first (easy) or GUI apps (hard but visible)?
**Impact:** Developer adoption vs user visibility.
**Default:** CLI first. GUI via Wayland proxy post-v1.

### COMP.2 Android Play Services
**Question:** Bundle MicroG, require user install, or not support?
**Impact:** App compatibility vs legal/privacy.
**Default:** Not supported. Document MicroG install. No Google binaries.

### COMP.3 Windows gaming focus
**Question:** Proton/DXVK integration depth — full Steam Deck parity or subset?
**Impact:** Gaming credibility vs maintenance.
**Default:** Subset (DXVK + VKD3D + Win32k shim). Full parity community-driven.

---

**To respond:** Annotate each question with your choice, or write "default" to accept my recommendation. I'll fold answers back into the plane specs.
