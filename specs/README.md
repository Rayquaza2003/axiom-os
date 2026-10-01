# Axiom OS — Complete Specification Package

**Working codename:** Axiom  
**Version:** v0.1-conceptual  
**Status:** Architecture blueprint — not an implementation

---

## Directory Structure

```
specs/
├── README.md                    ← this file
├── MASTER.md                    ← master index & cross-references
├── planes/
│   ├── P0-kernel.md             ← Microkernel & capability system
│   ├── P1-servers.md            ← User-space drivers, filesystem, network
│   ├── P2-data.md               ← Content-addressed object store + relationship graph
│   ├── P3-ai-core.md            ← Neural Resource Manager, semantic index, egress boundary
│   ├── P4-mesh.md               ← Device mesh, state roaming, resource brokerage
│   ├── P5-session.md            ← Intent workspaces, info layers, input pipeline
│   └── P6-runtime.md            ← App containers, capability grants, SDK
├── core/
│   ├── capability-model.md      ← Capability types, delegation, revocation
│   ├── ipc-protocol.md          ← Invocation, shared-memory windows, batching
│   ├── boot-chain.md            ← Verified boot, measured launch, sealed core
│   └── scheduling.md            ← CPU/NPU co-scheduling, power-thermal accounting
├── data/
│   ├── object-model.md          ← Content-addressed objects, metadata, provenance
│   ├── graph-schema.md          ← Relationship graph: tags, projects, edges
│   ├── index-contract.md        ← Semantic index: vectors, structured content, queries
│   ├── sync-protocol.md         ← P2P replication, CRDTs, conflict resolution
│   └── snapshot-ledger.md       ← Atomic snapshots, rollback, update transactions
├── security/
│   ├── threat-model.md          ← STRIDE analysis, trust boundaries, assumptions
│   ├── egress-firewall.md       ← Classifier, per-request approval, audit log
│   ├── attestation.md           ← Device identity, model signing, behavioral attestation
│   └── privacy-contract.md      ← Zero-knowledge guarantees, key management
├── ai/
│   ├── nrm-spec.md              ← Neural Resource Manager API & scheduling contract
│   ├── model-registry.md        ← Model packaging, signing, layering, rollback
│   ├── local-inference.md       ← ONNX/MLC/ExecuTorch runtime, quantization
│   ├── semantic-index.md        ← Embedding pipeline, vector store, hybrid search
│   └── planner.md               ← Natural language → graph query + vector retrieval
├── mesh/
│   ├── mesh-topology.md         ← Device attestation, mesh formation, relay
│   ├── state-roaming.md         ← Session snapshot, cursor/undo roaming, handoff
│   ├── resource-broker.md       ← Async compute offload, NPU brokerage, failover
│   ├── clipboard-audio.md       ← Universal clipboard, audio routing, gaze following
│   └── network-failover.md      ← Cellular fallback, connection migration
├── session/
│   ├── workspace-model.md       ← Intent workspaces as saved graph queries
│   ├── info-layers.md           ← Glanceable layers, compositor ownership
│   ├── input-bus.md             ← Unified input, arbitration, multi-modal
│   └── compositor.md            ← Scene graph, layer composition, theming
├── build/
│   ├── sequence.md              ← Phase-gated build order, milestones, exit criteria
│   ├── tooling.md               ← Build system, test infrastructure, CI/CD
│   ├── hardware-support.md      ← Board support, NPU drivers, sensor HAL
│   └── compatibility.md         ← Linux/Android/macOS/Windows app runtimes
└── open/
    ├── open-questions.md        ← Genuine unknowns needing your input
    ├── trade-offs.md            ← Documented decisions with alternatives
    └── glossary.md              ← Terminology reference
```

---

## How to Use This

1. **Start with `MASTER.md`** — it cross-references everything
2. **Read `planes/P0-kernel.md` first** — it's the security foundation
3. **Then `planes/P3-ai-core.md`** — it's the differentiator
4. **Use `open/open-questions.md`** — that's where your input matters most

---

## Grounding Reminder

This is an **architecture blueprint** with honest engineering framing. Every spec marks what's:
- **Buildable today** (seL4/QNX/Fuchsia foundations, on-device small-model orchestration, graph databases, E2E sync)
- **Research-grade** (universal on-device frontier AI, real-time cross-device compute, gaze/gesture without dedicated sensors)
- **Decades of engineering** (a drop-in OS replacement for billions of users)

The blueprint is honest about this. Your input should be too.