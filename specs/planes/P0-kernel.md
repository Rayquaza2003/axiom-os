# P0 — Microkernel & Capability System

**Plane:** P0 (trusted)  
**Feasibility:** 🟢 BUILDABLE — seL4, QNX, Fuchsia prove this in production  
**Dependencies:** None (base)  
**Dependents:** All planes (P1–P6)

---

## Design Goal

The *only* trusted code in the system. Everything else — drivers, filesystems, network stack, AI core, UI, apps — runs as unprivileged, crash-isolated servers that hold authority only through unforgeable capabilities.

---

## Kernel Responsibilities (Exhaustive)

| Responsibility | Description |
|----------------|-------------|
| **Capability space management** | Create, copy, attenuate, revoke, delete capabilities |
| **IPC & message passing** | Capability invocation, endpoint messages, notifications |
| **Thread scheduling** | Priority, deadline, CPU affinity, NPU co-scheduling |
| **Virtual memory** | Address space management, page faults, shared-memory windows |
| **Interrupt handling** | Mask, route, acknowledge; deliver as messages to driver servers |
| **Timer services** | High-resolution timers, deadline enforcement |
| **Verified boot anchor** | Measure and attest P0..P1 at boot |

---

## What the Kernel Explicitly Does NOT Do

| Excluded | Reason |
|----------|--------|
| Filesystem logic | Runs in P1 server; crash-isolated |
| Device drivers | Runs in P1 servers; crash-isolated |
| Network stack | Runs in P1 server; crash-isolated |
| Graphics composition | Runs in P5 compositor; unprivileged |
| AI model loading | Runs in P3 NRM; scheduled resource |
| App lifecycle | Runs in P6 runtime; capability-granted |

---

## Capability Model (Summary)

See [`core/capability-model.md`](core/capability-model.md) for full detail.

**Primitive capability types:**
- `Cap::Endpoint` — send a message to a server
- `Cap::Memory` — map a region of physical memory (shared-memory window)
- `Cap::Thread` — control a thread (start, stop, set priority)
- `Cap::Interrupt` — acknowledge and handle an IRQ
- `Cap::Timer` — set/read high-resolution timers
- `Cap::NPUSlice` — *new:* request NPU compute time (see P3)
- `Cap::ModelHandle` — *new:* invoke a loaded model instance (see P3)

**Capability properties:**
- Unforgeable (kernel-enforced)
- Transferable via `Cap::Endpoint` invocation
- Attenuatable (derive a weaker cap from a stronger one)
- Revocable (kernel tracks grant graph; revoke cascades)
- Persistent across reboots via sealed capability table (see boot chain)

---

## IPC Design

See [`core/ipc-protocol.md`](core/ipc-protocol.md) for wire format.

**Two paths:**

1. **Control path (capability invocation)** — small messages, copy-based, latency-critical
   - Grant a capability, ask NRM for a model slice, revoke access
   - ~500–2000 cycles on modern ARM/x86 (seL4-class)

2. **Data path (shared-memory windows)** — bulk data, zero-copy
   - Frame buffers, model weights, audio/video streams, index shards
   - Kernel creates `Cap::Memory` windows; servers map them
   - Synchronization via kernel notifications + seqlocks

**Batching:** Multiple invocations can be coalesced into a single kernel entry for hot paths (e.g., NRM scheduling N inference requests).

---

## Thread & Scheduling Model

| Scheduler | Policy |
|-----------|--------|
| **CPU threads** | Fixed-priority preemptive + deadline-aware (EDF for real-time) |
| **NPU slices** | *Co-scheduled with CPU* — NRM requests slice; kernel grants time on accelerator |
| **Power/thermal** | Global budget enforced by kernel; NRM and CPU scheduler both account |

**Thread priorities (example):**
```
0–31    : Real-time (audio, input, display vsync)
32–63   : Interactive (foreground app, NRM foreground inference)
64–95   : Background (indexing, sync, NRM background inference)
96–127  : Idle (telemetry, maintenance)
128–255 : System servers (P1 drivers, FS, network)
```

---

## Verified Boot & Sealed Core

See [`core/boot-chain.md`](core/boot-chain.md).

**Boot sequence:**
1. ROM → immutable bootloader (measures itself)
2. Bootloader → measures P0 kernel image → extends PCR
3. Kernel → measures P1 server images → extends PCR
4. Kernel → measures P3 NRM + model registry manifests → extends PCR
5. Attestation report signed by TPM/SE; available to user & remote verifier

**Sealed core:** P0 + P1 + P3 binaries + model registry manifests are read-only after boot. Any modification requires re-signing and re-measuring.

---

## Trust Boundary (The Red Line)

```
┌─────────────────────────────────────────────────────┐
│ P0 KERNEL (TRUSTED)                                 │
│  • Capabilities  • IPC  • Scheduling  • Memory      │
│  • Verified boot  • Interrupt routing               │
└─────────────────────────────────────────────────────┘
                          │ CAPABILITY IPC ONLY
                          ▼
┌─────────────────────────────────────────────────────┐
│ P1–P6 (UNTRUSTED, CRASH-ISOLATED)                   │
│  Drivers  FS  Network  NRM  Mesh  Session  Apps     │
└─────────────────────────────────────────────────────┘
```

**No exceptions.** A GPU driver crash restarts the GPU server. A filesystem bug restarts the FS server. The kernel and every other server continue running.

---

## Hardening Checklist

| Mechanism | Status |
|-----------|--------|
| W^X enforcement | Required |
| KASLR + KPTI | Required |
| Capability table hardening | seL4-style guarded capabilities |
| Spectre/Meltdown mitigations | Required (per-CPU) |
| Deterministic builds | Required (reproducible kernel image) |
| Formal verification target | seL4-class for P0; optional for P1+ |

---

## Open Questions

See [`open/open-questions.md#p0-kernel`](../open/open-questions.md#p0-kernel).

---

## References

- seL4 verification: Klein et al., "seL4: Formal Verification of an OS Kernel" (SOSP 2009)
- Fuchsia Zircon kernel: Capability-based, user-space drivers
- QNX Neutrino: Microkernel in safety-critical production for 30+ years
- CHERI capability hardware: Fine-grained memory protection (future hardware target)