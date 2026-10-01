# AI — Neural Resource Manager Specification

**Feasibility:** 🟢 BUILDABLE (on-device scheduling, model management) → 🔴 RESEARCH (frontier on-device)  
**Dependencies:** P0 kernel (NPU slices), P1 NPU driver, P3 model registry

---

## NRM Responsibilities (Exhaustive)

| Responsibility | Description |
|----------------|-------------|
| **Accelerator ownership** | Sole holder of `Cap::NPUSlice`; all NPU time allocated by NRM |
| **Model lifecycle** | Load, compile, warm, evict, quantize, version, rollback |
| **Scheduling** | Quota-based, priority-aware, preemptive, power/thermal accounted |
| **Memory management** | Unified weight cache; shared across clients; LRU + pinned |
| **Inference serving** | `Cap::ModelHandle` → `infer(input, output)` |
| **Telemetry** | Latency, throughput, power, accuracy, drift → capability-gated |

---

## NRM API (Capability-Gated)

### Capability: `Cap::NPUSlice`

```rust
// Granted at app install (required) or runtime (optional)
struct NPUSliceCap {
    priority: NPUPriority,        // Max priority this app can request
    max_concurrent_slices: u32,   // Concurrency limit
    daily_power_budget_mwh: u32,  // Daily power quota
}

// App requests a slice for inference
fn request_slice(cap: &NPUSliceCap, req: SliceRequest) -> Result<SliceGrant, Error>

struct SliceRequest {
    task: InferenceTask,          // What model + input shape
    priority: Priority,           // Must ≤ cap.priority
    deadline: Option<Deadline>,   // For RT/Interactive
    power_budget_mw: Option<u32>, // Override (≤ cap remaining)
}

struct SliceGrant {
    slice_id: SliceID,
    model_handle: Cap::ModelHandle,
    duration: Duration,           // Granted time quantum
    memory_mb: u32,               // Allocated weight + activation mem
    preemptible: bool,
}
```

### Capability: `Cap::ModelHandle`

```rust
// Returned with SliceGrant; used for inference
fn infer(handle: &ModelHandle, input: TensorRef, output: TensorRef) -> Result<InferenceResult, Error>

struct InferenceResult {
    latency_ms: u32,
    power_mw: u32,
    tokens_generated: Option<u32>,
    checkpoint: Option<Checkpoint>, // For preemption resume
}
```

### Capability: `Cap::ModelRegistryRead`

```rust
fn list_models(filter: ModelFilter) -> Vec<ModelInfo>
fn get_manifest(model_id: ModelID) -> ModelManifest
fn get_telemetry(model_id: ModelID) -> ModelTelemetry
```

---

## Scheduling Algorithm

### Priority-Inheritance + Weighted Fair Queuing

```
Per-priority queues:
  RT (0–31)      → FIFO + deadline (EDF), non-preemptible slices
  Interactive    → WFQ (weight = priority), preemptible by RT
  Background     → WFQ (weight = 1), preemptible by all
```

### Slice Grant Algorithm

```
request_slice(req):
  1. Check cap.priority ≥ req.priority
  2. Check cap.daily_power_budget not exhausted
  3. Check NPU memory available (weight cache + activation)
  4. Select model:
     - If req.model_hint specified → use that if loaded
     - Else → auto-select smallest capable model (NRM policy)
  5. If model not loaded → load (compile if needed) → may evict others
  6. Allocate slice:
     - RT → reserved slice pool (guaranteed)
     - Interactive → from shared pool, preempt Background if needed
     - Background → from shared pool, only if RT/Interactive idle
  7. Return SliceGrant with Cap::ModelHandle
```

### Preemption Protocol

```
NRM decides to preempt slice S (Background → Interactive needs it):
  1. NRM sends Preempt(S) to kernel
  2. Kernel: pauses NPU context for S, saves state to secure memory
  3. Kernel: signals client thread (via Cap::NPUSlice notification)
  4. Client: checkpoints inference (if supported) or aborts
  5. NRM: grants slice to higher-priority requester
  6. When slice available again → client re-requests
```

**Checkpointing:** Required for Interactive; optional for Background. Models without checkpoint support cannot run at Interactive priority.

---

## Model Management

### Loading Pipeline

```
ModelPackage (P2 object)
       │
       ▼
┌─────────────────────┐
│ VERIFIER            │  Signature, manifest, hardware compatibility
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│ COMPILER            │  Per-target: NPU (QNN/CoreML), GPU (Vulkan/Metal), CPU (WASM)
│  • Quantize         │
│  • Graph optimize   │
│  • Memory plan      │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│ WEIGHT CACHE (P2)   │  Content-addressed; shared across models (common layers)
│  • Refcounted       │
│  • LRU eviction     │
│  • Pinned: system   │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│ RUNTIME REGISTRY    │  ModelID → LoadedModel { runtime, weights, compiled_artifacts }
└─────────────────────┘
```

### Memory Budget

| Pool | Size | Policy |
|------|------|--------|
| **System pinned** | 512 MB – 2 GB | OS models (embedder, classifier, ASR) — never evicted |
| **App shared** | 1–4 GB | LRU; apps can pin via capability |
| **Activation workspace** | Per-slice | Allocated per grant; freed on preemption |

---

## Power-Thermal Integration

```rust
struct NRMPowerAccounting {
    // Per-slice
    slice_power_mw: u32,
    slice_energy_mwh: u64,
    
    // Per-app (via capability)
    app_daily_mwh: u64,
    app_budget_mwh: u32,
    
    // Global
    npu_current_mw: u32,
    npu_sustained_mw: u32,
    npu_burst_mw: u32,
    thermal_state: ThermalState,  // Normal, Warm, Throttle, Critical
}

fn can_grant_slice(req: &SliceRequest) -> bool {
    // Check thermal state
    if thermal_state == Throttle && req.priority < Interactive { return false; }
    if thermal_state == Critical { return false; }
    
    // Check power budget
    if npu_current_mw + req.power_budget_mw > npu_sustained_mw { return false; }
    
    // Check app budget
    if app_daily_mwh + estimated_energy(req) > app_budget_mwh { return false; }
    
    true
}
```

---

## Telemetry Export (Capability-Gated)

```rust
struct NRMTelemetry {
    // Per-model
    model_latency_p50: Duration,
    model_latency_p99: Duration,
    model_throughput_tps: f32,
    model_accuracy_drift: f32,      // vs baseline eval set
    
    // Per-app
    app_inference_count: u64,
    app_total_latency_ms: u64,
    app_total_power_mwh: u64,
    app_preemption_count: u32,
    
    // System
    npu_utilization: f32,
    weight_cache_hit_rate: f32,
    compilation_queue_depth: u32,
}
```

---

## Open Questions

See [`open/open-questions.md#nrm-spec`](../open/open-questions.md#nrm-spec).