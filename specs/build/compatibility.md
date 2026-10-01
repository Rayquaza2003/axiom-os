# Build — Compatibility (Linux/Android/macOS/Windows Runtimes)

**Feasibility:** 🟡 HARD BUT KNOWN — gVisor, WSL, Rosetta, Wine, Crostini proven; not for v1 core  
**Dependencies:** P0 caps, P1 servers, P6 sandbox

---

## Philosophy

> **Native + Web first. Compatibility as opt-in servers, not kernel features.**
> 
> Running legacy binaries expands attack surface. Isolate in P1 servers with capability gates.

---

## Compatibility Layers (Post-v1)

### Linux ABI (Crostini-style)

```
┌─────────────────────────────────────────────────────────────────┐
│ LINUX COMPAT SERVER (P1, opt-in)                                │
│                                                                 │
│  App (Linux ELF)                                                │
│       │                                                         │
│       ▼                                                         │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ gVisor / Kata Containers (syscall interception)         │   │
│  │  • ptrace / KVM / seccomp                                │   │
│  │  • Translates Linux syscalls → Axiom caps               │   │
│  └─────────────────────────────────────────────────────────┘   │
│       │                                                         │
│       ▼                                                         │
│  Axiom P0 (capability checks)                                   │
└─────────────────────────────────────────────────────────────────┘
```

**Capability mapping:**

| Linux Syscall | Axiom Capability |
|---------------|------------------|
| `open` | `Cap::FileRead` / `Cap::FileWrite` (path resolved) |
| `socket` | `Cap::NetworkConnect` / `Cap::NetworkListen` |
| `mmap` | `Cap::Memory` (with limits) |
| `clone` | `Cap::ProcessSpawn` |
| `ioctl` (drm) | `Cap::BufferAllocator` |
| `ioctl` (v4l2) | `Cap::Camera` / `Cap::VideoEncode` |

**Not supported:** Raw `/dev/mem`, `kexec`, `ptrace` (except inside compat), kernel modules.

---

### Android (ARCVM-style)

```
┌─────────────────────────────────────────────────────────────────┐
│ ANDROID COMPAT SERVER (P1, opt-in)                              │
│                                                                 │
│  Android App (APK)                                              │
│       │                                                         │
│       ▼                                                         │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ ART Runtime (on Axiom)                                   │   │
│  │  • DEX → AOT (art_compile)                               │   │
│  │  • Binder IPC → Axiom IPC                                │   │
│  │  • Hardware abstraction → P1 servers                     │   │
│  └─────────────────────────────────────────────────────────┘   │
│       │                                                         │
│       ▼                                                         │
│  Axiom P0 + P1                                                  │
└─────────────────────────────────────────────────────────────────┘
```

**Key mappings:**

| Android API | Axiom Equivalent |
|-------------|------------------|
| `Context` | Capability scope |
| `Binder` | Cap::IpcEndpoint |
| `ContentProvider` | P2 graph queries |
| `WorkManager` | NRM background slices |
| `Camera2` | Cap::Camera + P1 camera server |
| `NNAPI` | Cap::NPUSlice (NRM) |
| `SensorManager` | P1 Sensor HAL events |

**Play Services:** Not included. MicroG or user-installed.

---

### macOS (Rosetta-style Translation)

```
┌─────────────────────────────────────────────────────────────────┐
│ MACOS COMPAT SERVER (P1, opt-in, ARM only)                      │
│                                                                 │
│  macOS Binary (x86_64 / arm64)                                  │
│       │                                                         │
│       ▼                                                         │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Translation Layer                                        │   │
│  │  • x86_64 → ARM64: Rosetta 2 style (ahead-of-time)      │   │
│  │  • arm64 → arm64: Direct (dyld shim)                     │   │
│  │  • syscall shim: macOS → Axiom caps                      │   │
│  │  • Framework shims: AppKit → Axiom UI, etc.              │   │
│  └─────────────────────────────────────────────────────────┘   │
│       │                                                         │
│       ▼                                                         │
│  Axiom P0 + P1                                                  │
└─────────────────────────────────────────────────────────────────┘
```

**Challenges:** AppKit/UIKit reimplementation is massive. Practical path: Win32-style subset for CLI tools, not GUI apps.

---

### Windows (WSL2/Proton-style)

```
┌─────────────────────────────────────────────────────────────────┐
│ WINDOWS COMPAT SERVER (P1, opt-in)                              │
│                                                                 │
│  Windows Binary (PE/COFF)                                       │
│       │                                                         │
│       ▼                                                         │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Wine / Proton Stack                                      │   │
│  │  • NT syscall → Axiom caps                               │   │
│  │  • PE loader + DLL mapping                               │   │
│  │  • DXVK / VKD3D → Vulkan → P1 display                    │   │
│  │  • Win32k → Axiom compositor protocol                    │   │
│  └─────────────────────────────────────────────────────────┘   │
│       │                                                         │
│       ▼                                                         │
│  Axiom P0 + P1                                                  │
└─────────────────────────────────────────────────────────────────┘
```

**Gaming focus:** Proton/DXVK already mature. Axiom adds capability sandbox.

---

## Capability Sandbox for Compat

```
┌─────────────────────────────────────────────────────────────────┐
│ COMPAT SANDBOX PROFILE                                          │
│                                                                 │
│  Each compat server runs as P1 process with:                    │
│                                                                 │
│  Capabilities GRANTED:                                          │
│  ├── Cap::Memory (bounded)                                      │
│  ├── Cap::ProcessSpawn (child compat processes only)            │
│  ├── Cap::IpcEndpoint (to P1 servers)                           │
│  ├── Cap::FileRead/Write (mapped paths only)                    │
│  ├── Cap::NetworkConnect (if app declares)                      │
│  ├── Cap::DisplaySurface (via compositor protocol)              │
│  ├── Cap::AudioPlayback/Capture                                 │
│  └── Cap::NPUSlice (if app declares ML)                         │
│                                                                 │
│  Capabilities DENIED:                                           │
│  ├── Cap::KernelDebug                                           │
│  ├── Cap::CapabilityMint                                        │
│  ├── Cap::ModelRegistryWrite                                    │
│  ├── Cap::MeshTunnelCreate                                      │
│  └── Cap::AttestationSign                                       │
└─────────────────────────────────────────────────────────────────┘
```

**User sees:** "Linux Compat wants access to: Files (~/LinuxFiles), Network, Display. Allow?"

---

## Web Runtime (v1 Native)

```
┌─────────────────────────────────────────────────────────────────┐
│ WEB RUNTIME (P6, built-in)                                      │
│                                                                 │
│  Web App (WASM + HTML/CSS/JS)                                   │
│       │                                                         │
│       ▼                                                         │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Wasmtime / V8 (isolates)                                 │   │
│  │  • Capability bridge: JS ↔ Axiom caps                    │   │
│  │  • WebGPU → P1 Vulkan/Metal                              │   │
│  │  • WebNN → NRM (Cap::NPUSlice)                           │   │
│  │  • File System Access API → P2 graph (scoped)            │   │
│  │  • Clipboard API → P5 universal clipboard                │   │
│  │  • Screen Capture API → Cap::ScreenRead                  │   │
│  └─────────────────────────────────────────────────────────┘   │
│       │                                                         │
│       ▼                                                         │
│  Axiom P0 + P1 + P3                                             │
└─────────────────────────────────────────────────────────────────┘
```

**WebNN mapping:**

| WebNN | Axiom |
|-------|-------|
| `MLContext` | NRM session |
| `MLModel` | ModelHandle (Cap::ModelHandle) |
| `MLCommandEncoder` | SliceGrant |
| `compute()` | `infer()` |

---

## Capability Bridge (WASM ↔ Native)

```rust
// Host function injected into WASM instance
#[wasm_bindgen]
extern "C" {
    // Capability request
    fn axiom_cap_request(cap_name: &str, params: &[u8]) -> CapResult;
    
    // Capability invoke
    fn axiom_cap_invoke(handle: u64, method: &str, args: &[u8]) -> InvokeResult;
    
    // Memory grant
    fn axiom_mem_grant(size: usize, flags: MemFlags) -> MemHandle;
    fn axiom_mem_read(handle: u64, offset: usize, buf: &mut [u8]);
    fn axiom_mem_write(handle: u64, offset: usize, buf: &[u8]);
}

// Example: WebNN MLContext.create()
#[wasm_bindgen]
pub fn ml_context_create() -> u64 {
    // 1. Request Cap::NPUSlice from NRM
    // 2. Return handle to WASM
    // 3. WASM calls ml_model_create(handle, model_id) → Cap::ModelHandle
}
```

---

## File System Projection

| Axiom | Linux Compat | Android | Web |
|-------|--------------|---------|-----|
| **P2 Objects** | `~/LinuxFiles/` (9P/FUSE) | `/sdcard/` (FUSE) | File System Access API (scoped) |
| **Graph queries** | Not exposed | ContentProvider shim | Custom JS API |
| **Sync** | Native | Native | Background Sync API |

---

## Timeline

| Compat Layer | Phase | Effort |
|--------------|-------|--------|
| Web Runtime | P6 (v1) | 4 months |
| Linux CLI | Post-v1 | 6 months |
| Linux GUI | Post-v1 | 12 months |
| Android | Post-v1 | 12 months |
| Windows (Proton) | Post-v1 | 18 months |
| macOS | Post-v1 | Research |

---

## Open Questions

See [`open/open-questions.md#compatibility`](../open/open-questions.md#compatibility).