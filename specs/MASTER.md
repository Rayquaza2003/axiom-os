# Axiom OS — Master Specification Index

**Version:** v0.1-conceptual  
**Last updated:** 2026-08-28  
**Status:** Architecture blueprint

---

## Quick Navigation

| Plane | Spec | Role |
|-------|------|------|
| **P0** | [`planes/P0-kernel.md`](planes/P0-kernel.md) | Microkernel, capabilities, verified boot |
| **P1** | [`planes/P1-servers.md`](planes/P1-servers.md) | User-space drivers, FS, network servers |
| **P2** | [`planes/P2-data.md`](planes/P2-data.md) | Object store + relationship graph |
| **P3** | [`planes/P3-ai-core.md`](planes/P3-ai-core.md) | NRM, semantic index, egress boundary |
| **P4** | [`planes/P4-mesh.md`](planes/P4-mesh.md) | Device mesh, state roaming, brokerage |
| **P5** | [`planes/P5-session.md`](planes/P5-session.md) | Workspaces, info layers, input pipeline |
| **P6** | [`planes/P6-runtime.md`](planes/P6-runtime.md) | App containers, capability grants, SDK |

---

## Core Subsystems

| Domain | Spec | Purpose |
|--------|------|---------|
| **Capability System** | [`core/capability-model.md`](core/capability-model.md) | Types, delegation, revocation, attenuation |
| **IPC Protocol** | [`core/ipc-protocol.md`](core/ipc-protocol.md) | Invocation, shared memory, batching |
| **Boot Chain** | [`core/boot-chain.md`](core/boot-chain.md) | Measured boot, sealed core, rollback |
| **Scheduling** | [`core/scheduling.md`](core/scheduling.md) | CPU/NPU co-scheduling, power-thermal |
| **Data Model** | [`data/object-model.md`](data/object-model.md) | Content-addressed objects, metadata |
| **Graph Schema** | [`data/graph-schema.md`](data/graph-schema.md) | Tags, projects, edges, provenance |
| **Index Contract** | [`data/index-contract.md`](data/index-contract.md) | Vectors, structured content, queries |
| **Sync Protocol** | [`data/sync-protocol.md`](data/sync-protocol.md) | P2P replication, CRDTs, conflicts |
| **Snapshot Ledger** | [`data/snapshot-ledger.md`](data/snapshot-ledger.md) | Atomic updates, rollback |
| **Threat Model** | [`security/threat-model.md`](security/threat-model.md) | STRIDE, trust boundaries |
| **Egress Firewall** | [`security/egress-firewall.md`](security/egress-firewall.md) | Classifier, approval, audit |
| **Attestation** | [`security/attestation.md`](security/attestation.md) | Device ID, model signing, behavioral |
| **Privacy Contract** | [`security/privacy-contract.md`](security/privacy-contract.md) | ZK guarantees, key management |
| **NRM Spec** | [`ai/nrm-spec.md`](ai/nrm-spec.md) | NRM API, scheduling contract |
| **Model Registry** | [`ai/model-registry.md`](ai/model-registry.md) | Packaging, signing, layering |
| **Local Inference** | [`ai/local-inference.md`](ai/local-inference.md) | Runtime, quantization, backends |
| **Semantic Index** | [`ai/semantic-index.md`](ai/semantic-index.md) | Embedding pipeline, hybrid search |
| **Planner** | [`ai/planner.md`](ai/planner.md) | NL → graph query + vector retrieval |
| **Mesh Topology** | [`mesh/mesh-topology.md`](mesh/mesh-topology.md) | Device attestation, formation, relay |
| **State Roaming** | [`mesh/state-roaming.md`](mesh/state-roaming.md) | Session snapshot, handoff |
| **Resource Broker** | [`mesh/resource-broker.md`](mesh/resource-broker.md) | Async offload, NPU brokerage |
| **Clipboard/Audio** | [`mesh/clipboard-audio.md`](mesh/clipboard-audio.md) | Universal routing, gaze following |
| **Network Failover** | [`mesh/network-failover.md`](mesh/network-failover.md) | Cellular fallback, migration |
| **Workspace Model** | [`session/workspace-model.md`](session/workspace-model.md) | Intent workspaces as graph queries |
| **Info Layers** | [`session/info-layers.md`](session/info-layers.md) | Glanceable layers, compositor |
| **Input Bus** | [`session/input-bus.md`](session/input-bus.md) | Unified input, arbitration |
| **Compositor** | [`session/compositor.md`](session/compositor.md) | Scene graph, composition, theming |
| **Build Sequence** | [`build/sequence.md`](build/sequence.md) | Phase-gated order, milestones |
| **Tooling** | [`build/tooling.md`](build/tooling.md) | Build, test, CI/CD |
| **Hardware Support** | [`build/hardware-support.md`](build/hardware-support.md) | BSP, NPU drivers, sensor HAL |
| **Compatibility** | [`build/compatibility.md`](build/compatibility.md) | Linux/Android/macOS/Windows runtimes |

---

## Open Items (Your Input Needed)

| File | What's Open |
|------|-------------|
| [`open/open-questions.md`](open/open-questions.md) | 50+ genuine unknowns (23 core + 27 new from detailed specs) |
| [`open/trade-offs.md`](open/trade-offs.md) | 14 documented decisions with alternatives |
| [`open/glossary.md`](open/glossary.md) | Terminology reference |

---

## Cross-Plane Dependencies

```
P0 (kernel) 
  └─► P1 (servers) ──► P2 (data plane)
  │                     └─► P3 (AI core) ◄──┐
  │                     └─► P4 (mesh) ─────┤
  └─► P5 (session) ────────────────────────┤
  └─► P6 (runtime) ◄───────────────────────┘
```

**Key insight:** P0 and P3 are the only planes that change *what an OS is*. The rest are consequences.

---

## Feasibility Tags

Every spec uses these tags:

| Tag | Meaning |
|-----|---------|
| `🟢 BUILDABLE` | Proven in production (seL4, QNX, Fuchsia, CRDTs, graph DBs, on-device small models) |
| `🟡 HARD BUT KNOWN` | Difficult engineering, no research risk (driver coverage, NPU scheduler, compositor) |
| `🔴 RESEARCH` | Genuine unknowns (frontier on-device AI, real-time cross-device, gaze without sensors) |
| `⏳ DECADES` | Societal/infrastructure scale (billion-user OS replacement) |

---

## How to Give Input

For each open question in `open/open-questions.md`:
1. **State your preference** — pick an option or propose a third
2. **Note your constraint** — hardware target, team size, timeline, regulatory
3. **Flag what's non-negotiable** — e.g., "no cloud relay ever" changes mesh topology

The blueprint is designed to be reshaped by your constraints. That's the point.