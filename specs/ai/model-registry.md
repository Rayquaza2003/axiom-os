# AI — Model Registry

**Feasibility:** 🟢 BUILDABLE — model versioning, signing, layering proven  
**Dependencies:** P2 object store, P3 NRM, P0 boot chain

---

## Registry Structure

```
Model Registry (P2 objects)
│
├── System Layer (OS-blessed, immutable, updated via OS update)
│   ├── embedder:bge-small:v1.0.0
│   ├── classifier:bert-tiny:v1.2.0
│   ├── asr:whisper-tiny:v1.1.0
│   └── planner:phi-3-mini:v1.0.0
│
├── App Layer (Per-app, user-managed)
│   ├── com.example.coder:code-llama-7b:v0.5.0
│   └── com.example.designer:sd-xl-base:v1.0.0
│
└── User Layer (Personal fine-tunes, LoRAs, encrypted)
    ├── user:style-lora:v1
    └── user:code-assistant-lora:v3
```

---

## Model Package Format

```rust
struct ModelPackage {
    manifest: ModelManifest,
    weights: WeightBlob,                    // P2 object (content-addressed)
    compiler_artifacts: ArtifactMap,        // Per-target compiled artifacts
    signature: Signature,                   // Signed by publisher + Axiom authority
}

struct ModelManifest {
    id: ModelID,                            // = Blake3(manifest_canonical)
    name: String,
    version: SemVer,
    architecture: Architecture,             // "Llama-3.2-1B", "BERT-base", "Whisper-tiny"
    family: ModelFamily,                    // Llama, BERT, Whisper, Phi, Custom
    capabilities: ModelCapability[],        // Classify, Embed, Generate, Transcribe, ...
    languages: Language[],
    context_window: u32,
    quantization: QuantizationSpec,         // "Q4_K_M", "INT8", "FP16", "BF16"
    hardware_targets: HardwareTarget[],     // NPU:v1, GPU:Vulkan:1.3, CPU:WASM:GC
    min_npu_tops: u32,
    min_memory_mb: u32,
    license: License,
    provenance: Provenance,                 // trained_by, dataset_hash, distillation_parent
    // Layering
    base_model: Option<ModelID>,            // For LoRAs / fine-tunes
    adapter_type: Option<AdapterType>,      // LoRA, Prefix, Adapter, Full
}
```

---

## Layering & Composition

### Base + Adapter

```
Base Model (System Layer)
       │
       ▼
┌─────────────────────┐
│ WEIGHT MERGE        │  At load time: base_weights + adapter_delta → merged
│  • LoRA: W + A*B    │
│  • Prefix: concat   │
│  • Adapter: residual│
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│ COMPILER            │  Compile merged weights for target
└─────────┬───────────┘
          │
          ▼
Loaded Model (single ModelHandle)
```

**Key property:** Adapters are separate packages; NRM merges at load time. No duplicated base weights.

### Versioning & Rollback

- **Every layer versioned independently** — `base:v1.2.0 + lora:v3` = distinct combination
- **Rollback:** NRM maintains `active_combination` per capability; user can `rollback(model_id)` → NRM swaps to previous combination atomically
- **Health check:** After rollback, NRM runs quick eval (10 samples) → if accuracy OK, commits; else rolls forward

---

## Model Signing & Trust

### Signing Chain

```
Axiom Root Authority (offline, air-gapped)
       │
       ▼
Model Publisher Key (per org, in HSM)
       │
       ▼
ModelPackage.signature = Sign(manifest_hash, publisher_key)
```

### Verification (At Install)

```rust
fn verify_and_install(pkg: ModelPackage, cap: Cap::ModelRegistryWrite) -> Result<ModelID, Error> {
    // 1. Verify signature chains to Axiom Root
    // 2. Verify manifest.hash == content_hash(weights + artifacts)
    // 3. Verify hardware_targets compatible
    // 4. Verify license permits installation
    // 5. Check revocation list (pushed via OS update channel)
    // 6. Write to P2: ModelPackage object + ModelManifest object
    // 7. Update registry index (Cap::ModelRegistryRead sees it)
    // 8. Return ModelID
}
```

### Revocation

- **Publisher revocation:** Publisher signs revocation cert → pushed via OS update
- **Axiom emergency revocation:** Axiom Root signs → immediate push (security critical)
- **Effect:** NRM refuses to load; running instances get `Revoked` error on next inference

---

## System Layer (OS Models)

| Model | Role | Update Cadence |
|-------|------|----------------|
| **Embedder** (BGE-small/E5-small) | Semantic index, search | OS minor release |
| **Classifier** (BERT-tiny) | Intent, spam, safety | OS minor release |
| **ASR** (Whisper-tiny) | Voice input, captions | OS minor release |
| **Planner** (Phi-3-mini) | NL → query planning | OS minor release |
| **Summarizer** (Phi-3-mini) | Background summaries | OS minor release |

**Immutable:** System layer models cannot be removed/modified by user. Updated only via signed OS update (measured boot).

---

## App Layer

- **Per-app namespace:** `com.example.app:model-name:version`
- **User manages:** Install, update, remove via Settings → AI → Models
- **Capability:** `Cap::ModelRegistryWrite` required (granted at app install for its namespace)

---

## User Layer (Personal)

- **Encrypted under user key** — NRM never sees plaintext weights
- **Fine-tunes / LoRAs** — small deltas on base models
- **Capability:** `Cap::ModelRegistryWrite` (user namespace only)

---

## Compiler Artifacts

```rust
struct ArtifactMap {
    // Per hardware target
    npu: Option<CompiledArtifact>,      // QNN DLC, CoreML model, etc.
    gpu: Option<CompiledArtifact>,      // Vulkan/Metal/MLIR module
    cpu: Option<CompiledArtifact>,      // WASM-GC / llvm bitcode
}

struct CompiledArtifact {
    target: HardwareTarget,
    format: ArtifactFormat,             // QNN, CoreML, MLIR, WASM, GGUF
    blob: ObjectID,                     // P2 content-addressed
    compilation_hash: Hash,             // Reproducibility
    compiled_at: Timestamp,
    compiler_version: String,
}
```

**On-demand compilation:** If artifact missing for device's hardware, NRM compiles (background) → caches → uses.

---

## Open Questions

See [`open/open-questions.md#model-registry`](../open/open-questions.md#model-registry).