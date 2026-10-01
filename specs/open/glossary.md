# Glossary — Axiom OS

**Purpose:** Canonical terms used across all specs. Link to owning spec for details.

---

## A–C

| Term | Definition | Spec |
|------|------------|------|
| **App Layer (models)** | Per-app model namespace; user manages install/update | [model-registry.md](../ai/model-registry.md#app-layer) |
| **Atomic Snapshot** | COW snapshot of entire P2 state; instant rollback | [snapshot-ledger.md](../data/snapshot-ledger.md) |
| **Attestation** | Device proves identity + software state via TPM + signed cert | [attestation.md](../security/attestation.md) |
| **BGE-small** | Default embedding model (384-dim, INT8) for semantic index | [semantic-index.md](../ai/semantic-index.md#embedding-models) |
| **Boot Chain** | Measured boot: ROM → SPL → TEE → P0 kernel → P1 servers | [boot-chain.md](../core/boot-chain.md) |
| **Capability (Cap)** | Unforgeable token granting specific authority; passed by reference | [capability-model.md](../core/capability-model.md) |
| **Capability Scope** | Set of capabilities a process/thread holds; checked at IPC boundary | [capability-model.md](../core/capability-model.md#scope) |
| **Checkpoint** | Model inference state saved to resume after preemption | [local-inference.md](../ai/local-inference.md#inference-patterns) |
| **Compiler Artifact** | Per-target compiled model (QNN, CoreML, MLIR, WASM) | [model-registry.md](../ai/model-registry.md#compiler-artifacts) |
| **Content-Addressed** | Object ID = Blake3 hash of content; immutable, dedupable | [object-model.md](../data/object-model.md) |

## D–G

| Term | Definition | Spec |
|------|------------|------|
| **Derivation Edge** | Graph edge: `DerivedFrom`, `GeneratedBy`, `TransformedFrom` | [graph-schema.md](../data/graph-schema.md#derivation-edges) |
| **Device Mesh** | User's devices connected via Noise tunnels + relay | [mesh-topology.md](../mesh/mesh-topology.md) |
| **DHT** | Distributed Hash Table (libp2p Kademlia) for peer discovery | [mesh-topology.md](../mesh/mesh-topology.md#peer-discovery) |
| **Egress Firewall** | Kernel-level classifier blocking model-context exfiltration | [egress-firewall.md](../security/egress-firewall.md) |
| **Entity** | Extracted concept: Project, Tag, Person, Date, FileType, Model | [planner.md](../ai/planner.md#parsedquery-schema) |
| **Execution Plan** | Compiled query: vector search + graph traversal + fusion strategy | [planner.md](../ai/planner.md#execution-plan-generation) |
| **Fusion Strategy** | How vector + graph results combine: VectorFirst, GraphFirst, Fused | [semantic-index.md](../ai/semantic-index.md#fusion-strategies) |
| **Graph Store** | Edge database (RocksDB/Sled) separate from object store | [graph-schema.md](../data/graph-schema.md) |

## H–M

| Term | Definition | Spec |
|------|------------|------|
| **HNSW** | Hierarchical Navigable Small World — vector index algorithm | [semantic-index.md](../ai/semantic-index.md#vector-store-hnsw-on-p2) |
| **Intent Workspace** | Saved graph query grouping apps/assets/services by project | [P5-session.md](../planes/P5-session.md) |
| **IPC Protocol** | Capability-passing message format over P0 endpoints | [ipc-protocol.md](../core/ipc-protocol.md) |
| **LoRA** | Low-Rank Adaptation — small delta weights on base model | [model-registry.md](../ai/model-registry.md#layering--composition) |
| **Mesh Certificate** | Signed `{device_pub, device_id, user_sig, caps, expiry}` | [mesh-topology.md](../mesh/mesh-topology.md#device-identity--attestation) |
| **Model Handle** | `Cap::ModelHandle` — grant to run inference on a loaded model | [nrm-spec.md](../ai/nrm-spec.md) |
| **Model Manifest** | Metadata: arch, caps, quantization, hardware targets, license | [model-registry.md](../ai/model-registry.md#model-package-format) |
| **Model Registry** | Three-layer store: System / App / User models | [model-registry.md](../ai/model-registry.md#registry-structure) |
| **Noise_NK** | Handshake pattern: initiator static + ephemeral, responder static | [mesh-topology.md](../mesh/mesh-topology.md#tunnel-establishment) |
| **NPU Slice** | `Cap::NPUSlice` — time-bounded NPU compute grant from NRM | [nrm-spec.md](../ai/nrm-spec.md#slice-grant) |

## N–R

| Term | Definition | Spec |
|------|------------|------|
| **NRM** | Neural Resource Manager — P3 service owning NPU/GPU time | [nrm-spec.md](../ai/nrm-spec.md) |
| **NRM Priority** | Realtime > Interactive > Background; preemption down only | [nrm-spec.md](../ai/nrm-spec.md#scheduling-policy) |
| **Object** | Content-addressed blob + metadata (tags, provenance, version) | [object-model.md](../data/object-model.md) |
| **Object ID** | Blake3(content) — 32-byte globally unique identifier | [object-model.md](../data/object-model.md#object-id) |
| **ParsedQuery** | Structured output of planner: intent, entities, filters, strategy | [planner.md](../ai/planner.md#parsedquery-schema) |
| **P0–P6** | Plane 0 (kernel) through Plane 6 (runtime) — architecture layers | [MASTER.md](../MASTER.md#8-plane-architecture) |
| **Provenance** | Object metadata: source, author, tool, derivation chain | [object-model.md](../data/object-model.md#metadata) |
| **Relay** | Mesh node forwarding Noise frames; sees only next-hop | [mesh-topology.md](../mesh/mesh-topology.md#mesh-topology) |

## S–Z

| Term | Definition | Spec |
|------|------------|------|
| **Semantic Index** | Vector store (HNSW) + graph metadata + query planner | [semantic-index.md](../ai/semantic-index.md) |
| **Session Edge** | Graph edge: `Viewed`, `Edited`, `Selected`, `Referenced` | [graph-schema.md](../data/graph-schema.md#session-edges) |
| **Slice Grant** | NRM response: `{model_handle, slice_id, deadline, priority}` | [nrm-spec.md](../ai/nrm-spec.md#slice-grant) |
| **State Roaming** | Moving running session (cursor, undo, history) to another device | [P5-session.md](../planes/P5-session.md#state-roaming) |
| **Sync Scope** | What a tunnel syncs: `project:work`, `tag:personal`, `all` | [mesh-topology.md](../mesh/mesh-topology.md#capability-gated-tunnels) |
| **System Layer (models)** | OS-blessed immutable models (embedder, classifier, ASR, planner) | [model-registry.md](../ai/model-registry.md#system-layer-os-models) |
| **TensorRef** | Shared-memory window for zero-copy model I/O | [local-inference.md](../ai/local-inference.md#tensor-representation) |
| **Time Bucket** | Daily shard key for vector index: `(ProjectID, YYYY-MM-DD)` | [semantic-index.md](../ai/semantic-index.md#shard-design) |
| **User Layer (models)** | Personal fine-tunes/LoRAs, encrypted under user key | [model-registry.md](../ai/model-registry.md#user-layer-personal) |
| **User Master Key** | Ed25519 from passphrase + TPM; signs device certs | [mesh-topology.md](../mesh/mesh-topology.md#device-identity--attestation) |
| **VectorRecord** | P2 object: `vector_id`, `embedding` (INT8), `model_id`, `quantization` | [semantic-index.md](../ai/semantic-index.md#vectorrecord-p2-object) |
| **Workspace** | = Intent Workspace — saved query + materialized view | [P5-session.md](../planes/P5-session.md) |
| **Zero-Knowledge** | User keys never leave device; relay/mesh carries only ciphertext | [privacy-contract.md](../security/privacy-contract.md) |

---

## Plane Reference

| Plane | Name | Responsibility |
|-------|------|----------------|
| **P0** | Kernel | Capabilities, IPC, scheduling, memory, attestation |
| **P1** | System Servers | FS, network, display, input, radios, logging, update |
| **P2** | Data | Object store, graph store, semantic index, snapshots, sync |
| **P3** | AI Core | NRM, model registry, local inference, planner, classifier |
| **P4** | Mesh | Peer tunnels, relay, sync protocol, device attestation |
| **P5** | Session | Workspaces, state roaming, intent UI, clipboard/audio mesh |
| **P6** | Runtime | App sandbox, capability manifest, native + web runtimes |

---

## Feasibility Tags (used in all specs)

| Tag | Meaning |
|-----|---------|
| 🟢 **BUILDABLE** | Proven tech, clear implementation path |
| 🟡 **HARD BUT KNOWN** | Research-grade components exist; integration is the challenge |
| 🔴 **RESEARCH** | No production prior art; needs R&D before commit |

---

**See also:** [`open-questions.md`](open-questions.md) for unresolved forks, [`trade-offs.md`](trade-offs.md) for decision rationale.