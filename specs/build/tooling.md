# Build — Tooling & CI/CD

**Feasibility:** 🟢 BUILDABLE — standard Rust/C++/Bazel toolchain  
**Dependencies:** Build sequence phases

---

## Language & Runtime Choices

| Layer | Language | Rationale |
|-------|----------|-----------|
| **P0 Kernel** | Rust (no_std) + minimal C/asm | Memory safety, verification-friendly |
| **P1 Servers** | Rust | Safety, async, ecosystem |
| **P2 Data** | Rust (sled/RocksDB bindings) | Performance, embeddable |
| **P3 AI** | Rust + Python (model tools) | NRM in Rust; model ops in Python |
| **P4 Mesh** | Rust (libp2p, snow for Noise) | Crypto, async, network |
| **P5 Session** | Rust + Swift/Kotlin (UI) | Compositor in Rust; shell per-platform |
| **P6 Runtime** | Rust (sandbox) + WASM (apps) | Host in Rust; apps in any WASM lang |

---

## Build System: Bazel

### Why Bazel

- Hermetic, reproducible builds
- Multi-language (Rust, C++, Swift, Kotlin, Go, Python)
- Remote execution + caching (critical for kernel + ML models)
- Fine-grained dependency graph (matches capability model)

### Workspace Structure

```
axiom/
├── MODULE.bazel              # Bazel module file
├── BUILD.bazel               # Root targets
├── tools/                    # Build tools, scripts
│   ├── bzl/                  # Starlark rules
│   └── ci/                   # CI pipeline definitions
├── kernel/                   # P0
│   ├── src/
│   └── BUILD.bazel
├── servers/                  # P1
│   ├── display/
│   ├── input/
│   ├── network/
│   ├── storage/
│   ├── audio/
│   └── update/
├── data/                     # P2
│   ├── object-store/
│   ├── graph-store/
│   ├── snapshot/
│   ├── sync/
│   └── index/
├── ai/                       # P3
│   ├── nrm/
│   ├── model-registry/
│   ├── inference/
│   ├── semantic-index/
│   └── planner/
├── mesh/                     # P4
│   ├── topology/
│   ├── state-roaming/
│   ├── resource-broker/
│   ├── clipboard-audio/
│   └── network-failover/
├── session/                  # P5
│   ├── workspace/
│   ├── info-layers/
│   ├── input-bus/
│   └── compositor/
├── runtime/                  # P6
│   ├── sandbox/
│   ├── native-sdk/
│   └── web-runtime/
├── sdk/                      # Public SDK (generated)
└── tests/
    ├── integration/
    ├── e2e/
    └── security/
```

---

## Key Bazel Rules

```python
# tools/bzl/rust_rules.bzl
axiom_rust_binary = rule(
    implementation = _rust_binary_impl,
    attrs = {
        "crate_root": attr.label(allow_single_file=True),
        "deps": attr.label_list(),
        "features": attr.string_list(),
        "capabilities": attr.string_list(),  # Declared caps for sandbox
    },
)

axiom_rust_test = rule(...)

# Capability-aware build
axiom_capability_binary = rule(
    implementation = _capability_binary_impl,
    attrs = {
        "binary": attr.label(),
        "manifest": attr.label(allow_single_file=True),  # capability.manifest
    },
)
```

---

## Capability Manifest (Build-Time)

```json
{
  "binary": "compositor",
  "capabilities": {
    "required": [
      "Cap::BufferAllocator",
      "Cap::DisplayConfig",
      "Cap::InputFocus"
    ],
    "optional": [
      "Cap::Haptics",
      "Cap::WirelessDisplay"
    ],
    "never": [
      "Cap::NPUSlice",
      "Cap::Camera"
    ]
  },
  "sandbox_profile": "compositor"
}
```

**Build verifies:** Binary only imports capability FFI for declared caps. Link-time check.

---

## CI Pipeline

### Stages

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ CI PIPELINE (per PR + nightly + release)                                    │
│                                                                             │
│ 1. FORMAT & LINT (5 min)                                                    │
│    ├── cargo fmt --check                                                    │
│    ├── clang-format --dry-run                                               │
│    ├── bazel build //... --check_format                                     │
│    └── markdownlint docs/                                                   │
│                                                                             │
│ 2. UNIT TESTS (15 min, parallel)                                           │
│    ├── Kernel unit tests (P0)                                               │
│    ├── Server unit tests (P1)                                               │
│    ├── Data plane tests (P2)                                                │
│    ├── AI core tests (P3)                                                   │
│    ├── Mesh tests (P4)                                                      │
│    ├── Session tests (P5)                                                   │
│    └── Runtime tests (P6)                                                   │
│                                                                             │
│ 3. INTEGRATION TESTS (30 min)                                               │
│    ├── Kernel + servers (QEMU)                                              │
│    ├── Data plane + AI (local)                                              │
│    ├── Mesh (2+ VMs)                                                        │
│    └── Session + runtime (headless)                                         │
│                                                                             │
│ 4. BUILD ARTIFACTS (20 min)                                                 │
│    ├── Kernel image (signed)                                                │
│    ├── Server packages                                                      │
│    ├── Model packages (compiled artifacts)                                  │
│    ├── SDK tarballs                                                         │
│    └── OTA update package                                                   │
│                                                                             │
│ 5. HARDWARE TESTS (nightly, 2h)                                             │
│    ├── Boot on reference boards (RK3588, M3, Snapdragon 8 Gen 3, x86)      │
│    ├── Display + input + audio                                              │
│    ├── NPU inference benchmarks                                             │
│    ├── Mesh between 3 devices                                               │
│    └── State roam stress test                                               │
│                                                                             │
│ 6. SECURITY (nightly + release)                                             │
│    ├── Capability sandbox escape attempts                                   │
│    ├── Egress firewall penetration                                          │
│    ├── Mesh tunnel MITM                                                     │
│    ├── Model supply chain verification                                      │
│    └── Fuzzing (libFuzzer, AFL++)                                           │
│                                                                             │
│ 7. RELEASE (tagged)                                                         │
│    ├── Sign all artifacts                                                   │
│    ├── Publish to artifact repository                                       │
│    ├── Generate SBOM (CycloneDX)                                            │
│    ├── Update docs site                                                     │
│    └── Notify channels                                                      │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Remote Execution (RBE)

```yaml
# .bazelrc.remote
build --remote_executor=grpc://rbe.axiom.dev:443
build --remote_cache=grpc://rbe.axiom.dev:443
build --remote_instance_name=axiom-ci
build --remote_timeout=3600
test --remote_test_executor=grpc://rbe.axiom.dev:443
```

**Workers:** Self-hosted (Mac mini cluster for ARM, Threadripper for x86). GPU workers for ML compilation.

---

## Model Compilation Pipeline (CI)

```
Model Package (weights + manifest)
         │
         ▼
┌─────────────────────────────────────────┐
│ BAZEL TARGET: //ai/model-registry:compile_all │
│                                          │
│ For each target in manifest.hardware_targets: │
│   ├── NPU: QNN / CoreML / DirectML       │
│   ├── GPU: Vulkan (SPIRV) / Metal (MLIR) │
│   └── CPU: WASM-GC / LLVM                │
│                                          │
│ Output: CompiledArtifact per target      │
│   (cached by compilation_hash)           │
└─────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────┐
│ VERIFICATION                            │
│   ├── Bit-identical reproducibility    │
│   ├── Accuracy eval (100 samples)      │
│   ├── Latency benchmark                │
│   └── Memory footprint                 │
└─────────────────────────────────────────┘
```

---

## Developer Workflow

### Local Development

```bash
# One-time setup
bazel run //tools:setup_dev_env

# Build everything
bazel build //...

# Build specific plane
bazel build //kernel/... //servers/... //data/...

# Run tests for changed files
bazel test //... --test_tag_filters=changed

# Run on device (via USB/IP)
bazel run //tools:flash -- --device=rk3588 --partition=boot

# Start dev shell (with toolchain)
bazel run //tools:dev_shell
```

### Capability-Aware Development

```bash
# Check capability manifest matches binary
bazel run //tools:cap_check -- //session/compositor

# Generate sandbox profile from manifest
bazel run //tools:gen_sandbox_profile -- //session/compositor

# Run in sandbox locally
bazel run //tools:sandbox_run -- //session/compositor
```

---

## Artifact Repository

| Artifact | Location | Retention |
|----------|----------|-----------|
| Kernel images | `oci://artifacts.axiom.dev/kernel` | 2 years |
| Server packages | `oci://artifacts.axiom.dev/servers` | 2 years |
| Model packages | `oci://artifacts.axiom.dev/models` | 5 years |
| SDK releases | `https://sdk.axiom.dev/releases/` | Forever |
| OTA updates | `https://ota.axiom.dev/` | 3 years |
| CI logs | `https://ci.axiom.dev/logs/` | 90 days |

---

## Release Process

```
Release Branch (release/vX.Y)
       │
       ▼
┌─────────────────────────────────────┐
│ 1. Freeze: Only bug fixes           │
│ 2. Soak: 2 weeks nightly on devices │
│ 3. Security audit                   │
│ 4. Performance regression check     │
│ 5. Tag: vX.Y.0                      │
│ 6. Sign + publish artifacts         │
│ 7. OTA rollout (staged: 1% → 100%)  │
└─────────────────────────────────────┘
```

---

## Open Questions

See [`open/open-questions.md#tooling`](../open/open-questions.md#tooling).