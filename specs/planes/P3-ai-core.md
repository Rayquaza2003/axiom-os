# P3 — AI Core: Neural Resource Manager, Semantic Index, Egress Boundary

**Plane:** P3 (unprivileged, but *privileged resource owner*)  
**Feasibility:** 🟢 BUILDABLE (on-device small models, semantic search, egress filtering) → 🔴 RESEARCH (frontier on-device)  
**Dependencies:** P0 (capabilities, NPU slices), P1 (NPU driver), P2 (object store, graph)  
**Dependents:** P4 (mesh offload), P5 (session context), P6 (app runtime)

---

## Design Goal

**AI is a system resource, not an app.** The Neural Resource Manager (NRM) owns all accelerator time and model memory — allocating, scheduling, and preempting like the CPU scheduler. A hard **egress boundary** ensures local context never leaves the device without explicit, per-request user approval.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ P3 AI CORE (NO-EGRESS ZONE)                                                 │
│                                                                             │
│  ┌──────────────────┐   ┌──────────────────┐   ┌────────────────────────┐  │
│  │ NEURAL RESOURCE  │   │  MODEL REGISTRY  │   │  UNIFIED SEMANTIC      │  │
│  │ MANAGER (NRM)    │   │                  │   │  INDEX                 │  │
│  │                  │   │  • Signed models │   │                        │  │
│  │  • NPU scheduler │◄──┤  • Layered store │   │  • Vector store (HNSW) │  │
│  │  • Model loader  │   │  • Rollback      │   │  • Graph-backed meta   │  │
│  │  • Power/thermal │   │  • Versioning    │   │  • Structured ingest   │  │
│  │  • Quota/preempt │   │                  │   │  • Hybrid query engine │  │
│  └────────┬─────────┘   └──────────────────┘   └───────────┬────────────┘  │
│           │                                                │               │
│           ▼                                                ▼               │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    EGRESS CLASSIFIER (FIREWALL)                     │   │
│  │  Inspects EVERY outbound network call from any P3 component         │   │
│  │  • Model context detected? → BLOCK unless user-approved             │   │
│  │  • Approved? → Encrypt under user key → Allow via mesh relay        │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Neural Resource Manager (NRM)

### NRM Responsibilities

| Responsibility | Description |
|----------------|-------------|
| **Accelerator ownership** | All NPU/GPU compute time allocated by NRM; no direct client access |
| **Model lifecycle** | Load, warm, evict, quantize, compile models for target hardware |
| **Scheduling** | Quota-based, priority-aware, preemptive, power/thermal accounted |
| **Memory management** | Unified model weight cache; shared across clients; LRU + pinned |
| **Telemetry** | Latency, throughput, power, accuracy drift — exported via capability |

### Scheduling Contract

```rust
// Client request
struct InferenceRequest {
    capability: Cap::NPUSlice,           // granted by NRM at startup
    task: InferenceTask,                 // what to run
    priority: Priority,                  // RealTime > Interactive > Background
    deadline: Option<Deadline>,          // for RT/interactive
    power_budget: Option<PowerBudget>,   // max mW for this request
    model_hint: Option<ModelHint>,       // "small classifier", "embedder", etc.
    input: TensorRef,                    // shared-memory window (Cap::Memory)
    output: TensorRef,                   // shared-memory window
}

// NRM response
struct InferenceGrant {
    model_handle: Cap::ModelHandle,      // unforgeable, revocable
    slice_id: SliceID,                   // for preemption tracking
    estimated_latency: Duration,
    actual_latency: Duration,            // filled on completion
    power_consumed: Power,               // filled on completion
}
```

### Priority & Preemption

| Priority | Use Case | Preemption Behavior |
|----------|----------|---------------------|
| **RealTime** | Voice activation, gaze tracking, safety | Non-preemptible; reserved slice |
| **Interactive** | On-device assistant, live translation | Preempts Background; yields to RealTime |
| **Background** | Indexing, summarization, nightly training | Preemptible at any time; batched |

**Preemption protocol:** NRM signals `Preempt(slice_id)` → client checkpoints → NRM reclaims slice → client resumes when re-granted.

### Power-Thermal Accounting

- Every slice grant includes a **power budget** (mW × ms)
- NRM maintains a **thermal model** of the SoC (per-accelerator)
- Background work throttled when `T_junction > T_throttle`
- User-visible: "AI background paused — device warm"

---

## Model Registry

### Model Package Format

```rust
struct ModelPackage {
    manifest: ModelManifest,
    weights: WeightBlob,           // content-addressed (P2 object)
    compiler_artifacts: Artifacts, // per-target (NPU, GPU, CPU fallback)
    signature: Signature,          // signed by Axiom model authority
}

struct ModelManifest {
    id: ModelID,
    name: String,
    version: SemVer,
    architecture: Architecture,    // e.g., "Llama-3.2-1B", "BERT-base", "Whisper-tiny"
    capabilities: ModelCapability[], // Classify, Embed, Generate, Transcribe, ...
    languages: Language[],
    context_window: u32,
    quantization: QuantizationSpec, // e.g., "Q4_K_M", "INT8", "FP16"
    hardware_targets: HardwareTarget[], // NPU:v1, GPU:Vulkan, CPU:WASM
    min_npu_tops: u32,
    min_memory_mb: u32,
    license: License,
    provenance: Provenance,        // trained_by, dataset_hash, distillation_parent
}
```

### Layering & Rollback

- **System layer:** OS-blessed models (privacy-critical: embedder, classifier, transcriber) — immutable, updated via OS update
- **App layer:** Per-app models (granted via capability) — user can add/remove
- **User layer:** Personal fine-tunes, LoRAs — encrypted under user key
- **Rollback:** Every layer versioned; NRM can atomically swap to previous version on accuracy regression or security issue

---

## Unified Semantic Index

### Index Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│ SEMANTIC INDEX SERVICE                                          │
│                                                                 │
│  ┌─────────────┐    ┌─────────────┐    ┌────────────────────┐  │
│  │  INGEST     │───►│  EMBEDDING  │───►│  VECTOR STORE      │  │
│  │  PIPELINE   │    │  WORKER     │    │  (HNSW on P2)      │  │
│  └─────────────┘    └─────────────┘    └────────────────────┘  │
│         │                   │                    │               │
│         ▼                   ▼                    ▼               │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │              GRAPH-BACKED METADATA (P2)                  │   │
│  │  ObjectID ↔ VectorID ↔ Tags, Projects, Provenance, Time  │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

### Ingest Sources (All Capability-Gated)

| Source | Capability | Ingest Method |
|--------|------------|---------------|
| **Files (P2)** | `Cap::GraphQuery(scope)` | Structured metadata + content extraction |
| **App data** | `Cap::AppDataExport(app_id)` | App pushes structured entities (not screenshots) |
| **Screen context** | `Cap::ScreenRead(region)` | *Opt-in only*; OCR + layout analysis → structured |
| **Audio** | `Cap::AudioStream(session)` | *Opt-in only*; ASR → text → embeddings |
| **Session history** | `Cap::SessionRead` | P5 exports `Viewed`, `Edited`, `Selected` edges |

**Key principle:** Apps **export structured content** (titles, entities, timestamps, relationships) rather than the OS photographing everything. This is the privacy + power budget.

### Query Engine

```rust
struct SemanticQuery {
    natural_language: String,           // "Q3 revenue chart from Tuesday"
    parsed: ParsedQuery,                // intent + entities + time range
    scope: CapabilityScope,             // what the user can see
    hybrid: HybridStrategy,             // vector + graph + keyword
}

enum HybridStrategy {
    VectorFirst { k: 50, rerank: GraphRerank },
    GraphFirst { traversal: GraphTraversal, embed: bool },
    Fused { vector_weight: f32, graph_weight: f32 },
}
```

**Result:** Ranked `ObjectID` list with snippets, provenance, and `Cap::ObjectRead` grants.

---

## Egress Boundary (The Privacy Firewall)

See [`security/egress-firewall.md`](../security/egress-firewall.md) for full spec.

### Classifier Design

Every outbound network packet from **any P3 component** passes through a single classifier:

```
Outbound Call
      │
      ▼
┌────────────────────────────────────────┐
│  EGRESS CLASSIFIER (kernel-mediated)   │
│                                        │
│  1. Extract: destination, payload size │
│  2. Heuristic: model context markers?  │
│     • Embedding vectors in payload     │
│     • Model weights / LoRA deltas      │
│     • Prompt / context window content  │
│     • Semantic index shards            │
│  3. Decision:                          │
│     • No model context → ALLOW         │
│     • Model context + approval → ALLOW │
│     • Model context, no approval → DROP│
└────────────────────────────────────────┘
      │
      ▼ (if allowed)
┌────────────────────────────────────────┐
│  ENCRYPT under user key → MESH RELAY   │
└────────────────────────────────────────┘
```

### Approval Flow

1. App/NRM requests remote model (e.g., "frontier coding assistant")
2. System presents **explicit consent dialog**: *"Send 12KB of code context to remote model X?"*
3. User approves → **one-time capability** `Cap::EgressGrant(model_id, bytes, ttl)` issued
4. Classifier allows *only* that specific call; all others dropped
5. Audit log entry: timestamp, model, bytes, user decision

**No persistent "always allow."** Every remote call is a fresh decision.

---

## Capability Surface (P3 → P6)

| Capability | Operations |
|------------|------------|
| `Cap::NPUSlice` | `request_slice(priority, budget) → SliceGrant` |
| `Cap::ModelHandle` | `infer(input, output) → Result`, `get_info() → ModelInfo` |
| `Cap::SemanticIndex(scope)` | `query(q) → Results`, `subscribe(filter) → Stream` |
| `Cap::ModelRegistryRead` | `list_models() → ModelInfo[]`, `get_manifest(id)` |
| `Cap::ModelRegistryWrite` | `install_package(pkg) → ModelID` (user-only) |

---

## Local Inference Runtime

See [`ai/local-inference.md`](../ai/local-inference.md).

**Supported backends (pluggable):**
- **ONNX Runtime** — CPU, GPU (Vulkan/Metal/DirectML), NPU (QNN, CoreML, DirectML)
- **MLC LLM** — TVM-compiled LLMs on GPU/NPU
- **ExecuTorch** — PyTorch Edge on mobile NPUs
- **llama.cpp / GGUF** — CPU fallback, quantization zoo
- **Custom** — Vendor NPU SDKs behind stable ABI

**Quantization policy:** NRM prefers Q4_K_M / INT8 for background; FP16 for interactive; user can override.

---

## Open Questions

See [`open/open-questions.md#p3-ai-core`](../open/open-questions.md#p3-ai-core).

---

## References

- Apple Neural Engine / CoreML: On-device model orchestration
- Qualcomm QNN / Snapdragon Neural Processing SDK
- MLC LLM: https://github.com/mlc-ai/mlc-llm
- ExecuTorch: https://github.com/pytorch/executorch
- FAISS/HNSW: Vector similarity search
- Private Information Retrieval / Secure Enclaves: For future hardware-backed egress