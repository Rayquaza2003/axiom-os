# P6 — App Runtime: Containers, Capability Grants, SDK

**Plane:** P6 (unprivileged)  
**Feasibility:** 🟢 BUILDABLE — containers, capability systems, WASM runtimes exist  
**Dependencies:** P0 (caps), P1 (drivers), P2 (data), P3 (AI), P4 (mesh), P5 (session)  
**Dependents:** None (top plane)

---

## Design Goal

The developer surface. Apps run in **capability-bounded containers** — they receive only the authority they're granted, request more via user consent, and cannot escape their sandbox. The SDK makes this feel like superpowers, not restrictions.

---

## Container Model

### Isolation Layers

| Layer | Mechanism | What It Isolates |
|-------|-----------|------------------|
| **Process** | Separate address space, separate capability table | Memory, caps, threads |
| **Filesystem** | P2 namespace view (DirectoryView per app) | File access — only granted objects visible |
| **Network** | Virtual NIC per container; mesh tunnels capability-gated | Raw sockets, local ports, mesh access |
| **Devices** | No direct /dev access; all via capability servers | GPU, NPU, camera, mic, sensors |
| **IPC** | Only via granted `Cap::Endpoint` | Cross-app communication |
| **Time** | Virtualized timers (optional) | Timing side-channels |

**Not a VM.** Lightweight process isolation (like gVisor / Kata / Firecracker microVM but native to capability kernel). Boot: < 50ms cold, < 5ms warm (pre-warmed pool).

---

### Container Manifest

```toml
# app.axiom.toml
[identity]
name = "com.example.superapp"
version = "2.1.0"
publisher = "Example Corp"
signature = "..."  # signed by publisher + Axiom store

[capabilities]
# Required at install (user grants once)
required = [
  "object_read:project:work",      # Read work project files
  "object_write:project:work",     # Write work project files
  "npu_slice:interactive",         # On-device AI inference
  "semantic_index:query:work",     # Search work context
  "window_manage",                 # Create windows
  "input_target",                  # Receive input
]

# Optional (user prompted at first use)
optional = [
  "camera:photo",                  # Take photos
  "microphone:voice",              # Voice input
  "mesh_peer:tablet",              # Offload to tablet NPU
  "clipboard:read",                # Read universal clipboard
  "audio_route:output",            # Play audio
]

[runtime]
type = "wasm-gc"                   # or "native", "wasm", "android", "linux"
entry = "main.wasm"
memory_limit = "512MB"
npu_memory_limit = "256MB"

[compatibility]
# For legacy runtimes
linux_abi = "x86_64-linux-gnu"     # if type = "linux"
android_api = 34                   # if type = "android"
```

---

## Capability Grant Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ CAPABILITY GRANT LIFECYCLE                                                  │
│                                                                             │
│  INSTALL                                                                    │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐  │
│  │ App Store   │───►│ Package     │───►│ User Review │───►│ Grant       │  │
│  │ / Developer │    │ Verifier    │    │ (required)  │    │ Required    │  │
│  └─────────────┘    └─────────────┘    └─────────────┘    └──────┬──────┘  │
│                                                                     │       │
│  RUNTIME (optional capabilities)                                    │       │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌──────▼──────┐  │
│  │ App Calls   │───►│ Session     │───►│ User Prompt │───►│ Grant       │  │
│  │ Cap::Camera │    │ Mediates    │    │ (one-time)  │    │ Optional    │  │
│  └─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘  │
│                                                                     │       │
│  REVOCATION                                                           │       │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌──────▼──────┐  │
│  │ User Opens  │───►│ Settings    │───►│ Revoke      │───►│ Kernel      │  │
│  │ Privacy     │    │ Lists Grant │    │ Capability  │    │ Revokes     │  │
│  └─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Key properties:**
- **No "allow all" default** — every capability is explicit
- **Attenuation** — grant `object_read:project:work` not `object_read:*`
- **Time-bounded** — optional grants can have TTL (e.g., "camera for 1 hour")
- **Auditable** — every grant/revoke logged to user's private audit log

---

## SDK Surface (What Developers Actually Use)

### Capability Client (Rust/Swift/TS/Kotlin)

```rust
// Request a capability at runtime
let camera = Capabilities::request("camera:photo")
    .reason("Scan receipt for expense report")
    .ttl(Duration::hours(1))
    .await?;

// Use it — type-safe, capability-guarded
let photo = camera.take_photo(CaptureSettings::document()).await?;

// Filesystem via P2 graph (not paths!)
let work_docs = DataStore::open(Project::named("work"))?;
let report = work_docs.query()
    .tagged("expense")
    .modified_since(Duration::days(7))
    .collect::<Vec<_>>()
    .await?;

// AI via NRM (not loading models yourself!)
let summary = AI::summarize(&report)
    .model_hint(ModelHint::SmallClassifier)
    .priority(Priority::Interactive)
    .await?;

// Mesh offload (async, brokered)
let render_job = Mesh::offload(RenderTask::blender_scene(&scene))
    .target(DeviceClass::TabletNPU)
    .await?;
let result = render_job.wait().await?;
```

### What Developers *Don't* Do

| Old Way | Axiom Way |
|---------|-----------|
| `open("/home/user/Documents/xyz")` | `DataStore::open(Project::named("work"))` |
| Load own `model.onnx` | `AI::classify(...).model_hint(...)` |
| `tcp.connect("api.example.com")` | `Mesh::call_service("api.example.com", caps)` |
| `notify.send("Hi")` | `Session::notify(policy).send("Hi")` |
| `clipboard.write(text)` | `Mesh::clipboard().write(text)` |
| Manage window position | `Workspace::current().arrange(layout)` |

---

## Compatibility Runtimes

| Runtime | Use Case | Isolation |
|---------|----------|-----------|
| **WASM-GC** (preferred) | New apps, cross-platform | Native capability sandbox |
| **Native (ELF/Mach-O)** | Performance-critical, system tools | Process + capability table |
| **Linux ABI** (gVisor-like) | Unmodified Linux binaries | Syscall interception + caps |
| **Android** (ART on Axiom) | Android apps | Container + capability mapping |
| **Windows** (Wine/Proton-like) | Windows apps | Translation layer + caps |

**All runtimes map to the same capability model.** A Linux `open()` becomes a capability check; Android `CAMERA` permission becomes `Cap::Camera` grant.

---

## App Distribution

| Channel | Verification | Capability Review |
|---------|--------------|-------------------|
| **Axiom Store** | Publisher sig + Axiom sig + reproducibility build | Automated + human for sensitive caps |
| **Developer direct** | Publisher sig only | User sees full manifest |
| **Enterprise** | Org sig + policy compliance | MDM = capability policy (see P0) |

**No unsigned code runs with capabilities.** Unsigned = `Cap::WindowManage` only (no data, no AI, no mesh).

---

## Capability Surface (P6 → P0/P1/P2/P3/P4/P5)

| Capability | Granted By | Plane |
|------------|------------|-------|
| `Cap::ObjectRead(scope)` | User / Policy | P2 |
| `Cap::ObjectWrite(scope)` | User / Policy | P2 |
| `Cap::GraphQuery(scope)` | User / Policy | P2 |
| `Cap::NPUSlice(priority)` | NRM (P3) | P3 |
| `Cap::ModelHandle(model_id)` | NRM (P3) | P3 |
| `Cap::SemanticIndex(scope)` | NRM (P3) | P3 |
| `Cap::MeshPeer(peer_id)` | Mesh (P4) | P4 |
| `Cap::StateRoaming` | Mesh (P4) | P4 |
| `Cap::Clipboard` | Mesh (P4) | P4 |
| `Cap::AudioRoute` | Mesh (P4) | P4 |
| `Cap::WorkspaceControl` | Session (P5) | P5 |
| `Cap::InfoLayerPublish(id)` | Session (P5) | P5 |
| `Cap::WindowManage` | Session (P5) | P5 |
| `Cap::InputTarget(window)` | Session (P5) | P5 |

---

## Open Questions

See [`open/open-questions.md#p6-runtime`](../open/open-questions.md#p6-runtime).

---

## References

- gVisor: https://gvisor.dev/ — syscall interception sandbox
- Firecracker: https://firecracker-microvm.github.io/ — microVMs
- WebAssembly Component Model: https://github.com/WebAssembly/component-model
- Capsicum: Capability-mode UNIX (FreeBSD)
- CHERI: Capability Hardware Enhanced RISC Instructions