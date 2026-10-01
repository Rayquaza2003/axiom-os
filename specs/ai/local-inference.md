# AI — Local Inference Runtime

**Feasibility:** 🟢 BUILDABLE — ONNX Runtime, MLC, ExecuTorch, llama.cpp proven  
**Dependencies:** P1 NPU driver, P3 NRM, model registry

---

## Runtime Abstraction

NRM uses a **pluggable backend interface** — the same `Cap::ModelHandle.infer()` works regardless of backend.

```rust
trait InferenceBackend {
    fn load(model: &ModelPackage, target: HardwareTarget) -> Result<LoadedModel, Error>;
    fn infer(&self, handle: &LoadedModel, input: &[Tensor], output: &mut [Tensor]) -> Result<InferenceResult, Error>;
    fn checkpoint(&self, handle: &LoadedModel) -> Result<Checkpoint, Error>;
    fn restore(&self, handle: &mut LoadedModel, checkpoint: Checkpoint) -> Result<(), Error>;
    fn unload(&self, handle: LoadedModel) -> Result<(), Error>;
    fn get_info(&self, handle: &LoadedModel) -> ModelInfo;
}
```

---

## Supported Backends

| Backend | Targets | Strengths | Status |
|---------|---------|-----------|--------|
| **ONNX Runtime** | CPU, GPU (Vulkan/Metal/DX12), NPU (QNN, CoreML, DirectML) | Broad hardware support, mature | 🟢 Production |
| **MLC LLM** | GPU (Vulkan/Metal/CUDA), NPU (via TVM) | LLM-optimized, speculative decoding | 🟢 Production |
| **ExecuTorch** | Mobile NPU (QNN, CoreML, XNNPACK), CPU | PyTorch-native, mobile-first | 🟢 Production |
| **llama.cpp / GGUF** | CPU (AVX2/NEON), GPU (Metal/Vulkan/CUDA/CL), NPU (partial) | Quantization zoo, runs everywhere | 🟢 Production |
| **Custom Vendor** | Vendor NPU SDK | Maximum performance on specific SoC | 🟡 Per-SoC |

**NRM selects backend at load time** based on:
1. Model's `hardware_targets` manifest
2. Device's available accelerators
3. Performance profile (latency vs throughput vs power)

---

## Quantization Policy

| Priority | Default Quantization | Rationale |
|----------|---------------------|-----------|
| **RealTime** | FP16 / BF16 | Accuracy critical; power budget allows |
| **Interactive** | Q4_K_M / INT8 | Good accuracy/latency tradeoff |
| **Background** | Q3_K_M / INT4 | Max throughput per watt |

**User override:** `Cap::NPUSlice` grant can specify `quantization_override`.

---

## Tensor Representation

```rust
struct TensorRef {
    // Shared-memory window (Cap::Memory)
    memory_cap: Cap::Memory,
    offset: usize,
    shape: Vec<usize>,
    dtype: DataType,          // FP32, FP16, BF16, INT8, INT4, BOOL
    layout: TensorLayout,     // RowMajor, ChannelsLast, Blocked
}

enum DataType {
    FP32, FP16, BF16, INT8, INT4, UINT8, BOOL,
}
```

**Zero-copy:** Input/output tensors are shared-memory windows granted via `Cap::Memory`. No serialization overhead.

---

## Inference Patterns

### 1. Single-Shot (Classification, Embedding, Transcription)

```
Client → NRM: request_slice(task, priority)
NRM → Client: SliceGrant { model_handle }
Client → NRM: infer(input, output) via model_handle
NRM → Client: InferenceResult
```

### 2. Streaming (LLM Generation, TTS)

```
Client → NRM: request_slice(streaming_task, priority)
NRM → Client: SliceGrant { model_handle, streaming: true }
loop:
  Client → NRM: infer_next(input, output)  // input = next token / chunk
  NRM → Client: InferenceResult { next_token, ... }
until done
```

### 3. Batched (Background Indexing)

```
Client → NRM: request_slice(batch_task, Background)
NRM → Client: SliceGrant { model_handle, batch_size: 32 }
Client → NRM: infer_batch(inputs[], outputs[])  // Single call, multiple inputs
NRM → Client: BatchResult { results[] }
```

---

## Performance Targets (2026 NPU Class: 20–50 TOPS)

| Task | Model | Target Latency | Target Power |
|------|-------|----------------|--------------|
| **Embedding** (384-dim) | BGE-small | < 5 ms | < 50 mW |
| **Classification** | BERT-tiny | < 3 ms | < 30 mW |
| **ASR** (10s audio) | Whisper-tiny | < 200 ms | < 200 mW |
| **LLM Generate** (1 token) | Phi-3-mini (3.8B) | < 50 ms | < 500 mW |
| **LLM Generate** (1 token) | Llama-3.2-1B | < 30 ms | < 300 mW |
| **Image Gen** (512²) | SD-XL (distilled) | < 2 s | < 2 W |

---

## Memory Management

### Weight Cache (Shared)

```
┌─────────────────────────────────────────────────────────────────┐
│ NRM WEIGHT CACHE (P2 content-addressed)                         │
│                                                                 │
│  Model A (Llama-3.2-1B)                                         │
│  ├── layer_0.q_proj.weight  ◄──┐                                │
│  ├── layer_0.k_proj.weight  ───┤                                │
│  ├── layer_0.v_proj.weight  ───┼── Shared with Model B         │
│  ├── layer_0.o_proj.weight  ───┤   (common base architecture)  │
│  └── ...                      ──┘                                │
│                                                                 │
│  Model B (Llama-3.2-3B)                                         │
│  ├── layer_0.q_proj.weight  ◄──┘  (same ObjectID = same blob)  │
│  └── ...                                                        │
│                                                                 │
│  Refcounted • LRU eviction • Pinned: System models             │
└─────────────────────────────────────────────────────────────────┘
```

### Activation Workspace (Per-Slice)

- Allocated from NRM's activation pool at slice grant
- Size = `max_batch × max_seq × hidden_dim × 2` (FP16)
- Freed on slice release / preemption

---

## Compilation Pipeline

```
ModelPackage (weights + manifest)
       │
       ▼
┌─────────────────────┐
│ GRAPH OPTIMIZER     │  Fuse ops, eliminate dead code, layout transform
│  • ONNX Graph Opt   │
│  • MLIR passes      │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│ QUANTIZER           │  Calibration (if needed) → Quantize
│  • PTQ / QAT        │
│  • Per-layer policy │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│ TARGET COMPILER     │  NPU: QNN / CoreML / DirectML
│  • NPU backend      │  GPU: Vulkan / Metal / CUDA
│  • GPU backend      │  CPU: WASM-GC / LLVM
│  • CPU backend      │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│ ARTIFACT CACHE      │  Key = (model_hash, target, quant_policy, compiler_ver)
│  • P2 content-addr  │
│  • Reproducible     │
└─────────────────────┘
```

**Reproducibility:** Same inputs → bit-identical artifact. Verified at load time.

---

## Open Questions

See [`open/open-questions.md#local-inference`](../open/open-questions.md#local-inference).