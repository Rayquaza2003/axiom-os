# Build — Phase-Gated Sequence

**Feasibility:** 🟢 BUILDABLE — standard OS bring-up pattern  
**Purpose:** Ordered milestones so each phase proves the next's foundation.

---

## Phase 0: Kernel Bring-Up (P0)

**Goal:** Verified microkernel boots on target hardware, runs capability tests.

| Milestone | Criteria |
|-----------|----------|
| **M0.1** | ROM → SPL → TEE → P0 kernel measured boot |
| **M0.2** | Capability create/invoke/revoke syscalls pass |
| **M0.3** | IPC latency < 1μs (loopback) |
| **M0.4** | Scheduler: CPU pinning, priority inheritance |
| **M0.5** | Memory: caps, mapping, ASID, COW fork |
| **M0.6** | Attestation: TPM quote + certificate chain |
| **M0.7** | **Gate:** All M0.x pass on 2+ SoCs (ARM + x86) |

**Duration:** 3–6 months  
**Team:** 4–6 kernel engineers

---

## Phase 1: System Servers (P1)

**Goal:** Userspace drivers + core servers functional; self-hosting build.

| Milestone | Criteria |
|-----------|----------|
| **M1.1** | Display server: modeset, buffer alloc, vsync |
| **M1.2** | Input server: HID, touch, pen, keyboard |
| **M1.3** | Network server: Wi-Fi, Ethernet, Bluetooth, MPTCP |
| **M1.4** | Storage server: NVMe, UFS, eMMC, F2FS/ZNS |
| **M1.5** | Audio server: PipeWire-compatible, low-latency |
| **M1.6** | Update server: A/B partitions, rollback, verification |
| **M1.7** | **Gate:** Graphical shell (minimal) runs on target |

**Duration:** 4–8 months  
**Team:** 6–10 systems engineers

---

## Phase 2: Data Plane (P2)

**Goal:** Content-addressed object store + graph + snapshots working.

| Milestone | Criteria |
|-----------|----------|
| **M2.1** | Object store: put/get, dedupe, GC, verification |
| **M2.2** | Graph store: edges, traversals, indexes, transactions |
| **M2.3** | Snapshot ledger: COW clone, atomic commit, rollback |
| **M2.4** | Sync protocol: CRDT merge, vector clocks, peer-to-peer |
| **M2.5** | Index contract: structured content, full-text, metadata |
| **M2.6** | **Gate:** 10M objects, 100M edges, snapshot < 100ms |

**Duration:** 4–6 months  
**Team:** 4–6 storage engineers

---

## Phase 3: AI Core (P3)

**Goal:** NRM schedules NPU; local models run; egress firewall blocks.

| Milestone | Criteria |
|-----------|----------|
| **M3.1** | NRM: slice grant, preempt, weight cache, checkpoint |
| **M3.2** | Model registry: install, verify, layer, compile, rollback |
| **M3.3** | Local inference: ONNX/MLC/llama.cpp backends, INT8/FP16 |
| **M3.4** | Semantic index: embed pipeline, HNSW, hybrid query |
| **M3.5** | Planner: Phi-3-mini NL→query, fallback chain |
| **M3.6** | Egress firewall: classifier, approval UI, audit log |
| **M3.7** | **Gate:** End-to-end: "Summarize Q3 docs" works offline |

**Duration:** 6–9 months  
**Team:** 6–8 ML systems engineers

---

## Phase 4: Mesh (P4)

**Goal:** Devices pair, tunnels establish, sync works, roam works.

| Milestone | Criteria |
|-----------|----------|
| **M4.1** | Device attestation: QR join, Noise NK, capability gates |
| **M4.2** | Mesh tunnels: full mesh ≤8, relay-assisted, AutoNAT |
| **M4.3** | Sync: CRDT text, LWW binary, offline queue, reconnect |
| **M4.4** | State roam: COW snapshot transfer, app restore, <2s LAN |
| **M4.5** | Resource broker: batch offload, credit accounting |
| **M4.6** | Clipboard/audio: universal clipboard, audio routing, gaze |
| **M4.7** | Network failover: MPTCP, cellular fallback, offline queue |
| **M4.8** | **Gate:** 3 devices, 48h continuous, roam 10×, zero data loss |

**Duration:** 5–8 months  
**Team:** 5–7 distributed systems engineers

---

## Phase 5: Session (P5)

**Goal:** Workspaces, info layers, input bus, compositor integrated.

| Milestone | Criteria |
|-----------|----------|
| **M5.1** | Workspace: query + materialize, incremental update |
| **M5.2** | Info layers: status, glance, briefing, contextual |
| **M5.3** | Input bus: all modalities, arbitration, IMF, fusion |
| **M5.4** | Compositor: scene graph, theme engine, multi-display |
| **M5.5** | **Gate:** Daily-driver shell on reference device |

**Duration:** 4–6 months  
**Team:** 5–7 UI/framework engineers

---

## Phase 6: Runtime (P6)

**Goal:** App sandbox, capability manifest, native + web runtimes.

| Milestone | Criteria |
|-----------|----------|
| **M6.1** | Sandbox: deny-by-default, manifest, grant UI, revoke |
| **M6.2** | Native runtime: Rust/Swift/Kotlin SDK, cap FFI |
| **M6.3** | Web runtime: WASM + WebGPU + WebNN, cap bridge |
| **M6.4** | App store: sign, verify, update, sandbox profile |
| **M6.5** | **Gate:** 10 third-party apps run, pass security audit |

**Duration:** 4–6 months  
**Team:** 4–6 platform engineers

---

## Phase 7: Integration & Hardening

**Goal:** Whole-system stability, security audit, hardware enablement.

| Milestone | Criteria |
|-----------|----------|
| **M7.1** | Full stack integration test suite (CI) |
| **M7.2** | Security audit: threat model validation, pen test |
| **M7.3** | Hardware enablement: 5+ SoCs, 10+ device configs |
| **M7.4** | Performance: boot < 10s, app launch < 300ms, roam < 2s |
| **M7.5** | **Gate:** Release candidate |

**Duration:** 3–4 months  
**Team:** Full team

---

## Dependency Graph

```
P0 (M0.7)
    │
    ├──► P1 (M1.7)
    │         │
    │         ├──► P2 (M2.6)
    │         │         │
    │         │         ├──► P3 (M3.7)
    │         │         │         │
    │         │         │         ├──► P4 (M4.8)
    │         │         │         │         │
    │         │         │         │         ├──► P5 (M5.5)
    │         │         │         │         │         │
    │         │         │         │         │         ├──► P6 (M6.5)
    │         │         │         │         │         │         │
    │         │         │         │         │         │         ▼
    │         │         │         │         │         │    P7 (M7.5) → RC
    │         │         │         │         │         │
    │         │         │         │         │         └──► (parallel: P6 can start after M3.3)
    │         │         │         │         │
    │         │         │         │         └──► (parallel: P5 can start after M2.3)
    │         │         │         │
    │         │         │         └──► (parallel: P4 can start after M2.4 + M3.1)
    │         │         │
    │         │         └──► (parallel: P3 can start after M1.3 + M2.2)
    │         │
    │         └──► (P1 needs M0.7 only)
    │
    └──► (P0 is the root)
```

---

## Parallelization Notes

| Phase | Can Start After | Rationale |
|-------|-----------------|-----------|
| P3 (AI Core) | M1.3 (network) + M2.2 (graph) | NRM needs network for model fetch; semantic index needs graph |
| P4 (Mesh) | M2.4 (sync) + M3.1 (NRM) | Mesh sync uses P2 protocol; offload needs NRM |
| P5 (Session) | M2.3 (snapshots) | Workspace materialization needs snapshots; roam needs COW |
| P6 (Runtime) | M3.3 (local inference) | Apps need NRM access; SDK can develop against mock NRM |

---

## Total Timeline

| Scenario | Duration |
|----------|----------|
| **Aggressive** (fully staffed, low churn) | ~24 months |
| **Realistic** (hiring, iteration, hardware delays) | ~36 months |
| **Conservative** (research spikes, re-architecture) | ~48+ months |

**Critical path:** P0 → P1 → P2 → P3 → P4 → P5 → P6 → P7

---

## Open Questions

See [`open/open-questions.md#build-sequence`](../open/open-questions.md#build-sequence).