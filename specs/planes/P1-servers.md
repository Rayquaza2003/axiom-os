# P1 — System Servers (Drivers, Filesystem, Network)

**Plane:** P1 (unprivileged, crash-isolated)  
**Feasibility:** 🟢 BUILDABLE — Fuchsia, QNX, Genode prove this pattern  
**Dependencies:** P0 (kernel capabilities, IPC)  
**Dependents:** P2 (data), P3 (AI), P4 (mesh), P5 (session), P6 (runtime)

---

## Design Goal

Every driver, filesystem, and network protocol runs as a *user-space server* that holds capabilities to hardware resources granted by P0. A bug in any server crashes only that server — the kernel and all other servers continue.

---

## Server Catalog

| Server | Capabilities Held | Restart Policy |
|--------|-------------------|----------------|
| **GPU Driver** | `Cap::Interrupt` (GPU IRQ), `Cap::Memory` (VRAM windows), `Cap::NPUSlice` (for compute) | Auto-restart; clients reconnect via capability re-grant |
| **Display/Compositor Bridge** | `Cap::Memory` (scanout buffers), `Cap::Interrupt` (vblank) | Auto-restart; P5 compositor re-acquires |
| **Storage Driver** (NVMe, eMMC, UFS) | `Cap::Interrupt`, `Cap::Memory` (DMA windows) | Auto-restart; P2 FS server re-issues commands |
| **Filesystem Server** | `Cap::Endpoint` (storage driver), `Cap::Memory` (cache) | Auto-restart; P2 re-mounts; in-flight ops replayed |
| **Network Stack** (TCP/IP, Wi-Fi, BT, Cellular) | `Cap::Interrupt` (NIC IRQ), `Cap::Memory` (packet buffers) | Auto-restart; P4 mesh re-establishes tunnels |
| **Audio Server** | `Cap::Interrupt` (codec IRQ), `Cap::Memory` (DMA rings) | Auto-restart; P5 session re-routes streams |
| **Input Server** (HID, touch, pen, gaze) | `Cap::Interrupt`, `Cap::Memory` (event rings) | Auto-restart; P5 input bus re-subscribes |
| **Sensor Hub** (accel, gyro, proximity, ambient) | `Cap::Interrupt`, `Cap::Memory` | Auto-restart; P5 re-subscribes |
| **Power Management** | `Cap::Interrupt` (PMIC), `Cap::Memory` (registers) | Auto-restart; kernel re-queries policy |
| **USB/Thunderbolt Host** | `Cap::Interrupt`, `Cap::Memory` | Auto-restart; device enumeration replayed |

---

## Driver Server Pattern (Template)

Every driver server follows this structure:

```
┌─────────────────────────────────────────────────────────┐
│ DRIVER SERVER PROCESS                                    │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐  │
│  │  IRQ Thread │  │  Control    │  │  Data Path      │  │
│  │  (high pri) │  │  Endpoint   │  │  (shared mem)   │  │
│  └──────┬──────┘  └──────┬──────┘  └────────┬────────┘  │
│         │                │                   │           │
│         ▼                ▼                   ▼           │
│  ┌─────────────────────────────────────────────────┐    │
│  │           DEVICE STATE MACHINE                  │    │
│  │  (init → operational → error → recovery)        │    │
│  └─────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────┘
```

**Key invariants:**
- **No global state in kernel** — all device state lives in the server
- **Capability-attenuated access** — clients get only the ops they need (e.g., "submit command buffer" not "map all VRAM")
- **Graceful degradation** — server signals degraded mode (e.g., GPU compute lost, display only) via capability revocation + re-grant

---

## Filesystem Server (P1 → P2 Bridge)

The FS server is the *only* component that touches raw block storage. It presents a **block capability** interface to P2's object store.

**Interface:**
```
Cap::BlockRead(offset, length) → data
Cap::BlockWrite(offset, data) → result
Cap::BlockFlush() → result
Cap::BlockTrim(range) → result
```

**Crash recovery:**
- FS server maintains a write-ahead log in a reserved partition
- On restart, replays log → P2 sees atomic transactions
- P2's snapshot ledger (see `data/snapshot-ledger.md`) is the source of truth

---

## Network Stack Server

Runs the full TCP/IP stack + link-layer protocols in user space.

**Capabilities held:**
- NIC IRQ + DMA rings (via `Cap::Interrupt` + `Cap::Memory`)
- `Cap::Endpoint` to P4 mesh for tunnel encapsulation
- `Cap::Timer` for retransmission timers

**Interface to P4 mesh:**
```
Cap::TcpConnect(addr, port) → Cap::Stream
Cap::TcpListen(port) → Cap::Listener
Cap::UdpBind(port) → Cap::Datagram
Cap::TunnelCreate(peer_id, key) → Cap::Tunnel
```

**Cellular modem:** Separate server (or same server, different IRQ) with `Cap::ModemControl` for SMS, USSD, network selection.

---

## Restart & Recovery Protocol

When a P1 server crashes:

1. **Kernel detects** thread fault / endpoint death
2. **Kernel revokes** all capabilities the server held (atomic)
3. **Restart manager** (P6 system service) spawns fresh server instance
4. **Fresh server** requests capabilities from P0 (via bootstrap capability)
5. **Clients** (P2, P3, P4, P5) detect endpoint death → re-request capabilities
6. **State reconciliation** — server replays WAL, clients re-sync

**Target restart latency:** < 100ms for display/audio/input; < 500ms for storage/network.

---

## Hardware Abstraction (The "Windows Breadth" Promise)

| Hardware Class | Abstraction Strategy |
|----------------|---------------------|
| GPU | Vulkan-like command buffer submission; NPU slice via NRM |
| Display | Atomic modeset + explicit sync (dma-fence equivalent) |
| Storage | NVMe / UFS / eMMC behind block capability interface |
| Camera | V4L2-like buffer queue; metadata via capability |
| Audio | Ring buffers + timeline; low-latency path < 5ms |
| Sensors | Event stream + batching; privacy gating at server |
| Radios (Wi-Fi/BT/Cell) | Network stack owns link layer; P4 owns mesh layer |

**No vendor blobs in kernel.** All proprietary firmware loads into driver server address space.

---

## Open Questions

See [`open/open-questions.md#p1-servers`](../open/open-questions.md#p1-servers).

---

## References

- Fuchsia driver framework: https://fuchsia.dev/reference/hardware/driver_framework
- QNX resource managers: User-space filesystem/driver pattern
- Genode OS framework: Capability-based driver architecture
- Linux DRM/KMS: Atomic modeset, explicit sync (adapted for user-space)