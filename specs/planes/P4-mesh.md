# P4 — Device Mesh: Trusted Peer Network

**Plane:** P4 (unprivileged)  
**Feasibility:** 🟢 BUILDABLE (state roaming, E2E sync, async offload) → 🔴 RESEARCH (real-time cross-device compute)  
**Dependencies:** P0 (caps), P1 (radios), P2 (data sync), P3 (NRM offload), P5 (session state)  
**Dependents:** P5 (session handoff), P6 (distributed apps)

---

## Design Goal

**All your devices are one computer.** A trusted peer mesh that roams exact state, brokers async resources, and routes clipboard/audio to wherever you're looking — without a cloud middle-man that can read your data.

---

## Mesh Topology

```
┌────────────────────────────────────────────────────────────────────────────┐
│                        MESH TOPOLOGY                                        │
│                                                                            │
│   ┌─────────┐         ┌─────────┐         ┌─────────┐                     │
│   │ LAPTOP  │◄───────►│ TABLET  │◄───────►│  PHONE  │                     │
│   │ (primary)│  Wi-Fi │ (NPU)  │  Wi-Fi │ (cell)  │                     │
│   └────┬────┘         └────┬────┘         └────┬────┘                     │
│        │                   │                   │                          │
│        │      ┌────────────┴────────────┐      │                          │
│        │      │   ENCRYPTED MESH TUNNELS  │      │                          │
│        │      │   (WireGuard/Noise +     │      │                          │
│        │      │    capability-gated)     │      │                          │
│        │      └────────────┬────────────┘      │                          │
│        │                   │                   │                          │
│        ▼                   ▼                   ▼                          │
│   ┌─────────────────────────────────────────────────────────────────┐    │
│   │                    OPTIONAL RELAY (SCRUTABLE)                    │    │
│   │  • Finds peers via DHT/introducer                               │    │
│   │  • Carries ONLY ciphertext — CANNOT DECRYPT                    │    │
│   │  • Audited, user-run or community-run                           │    │
│   └─────────────────────────────────────────────────────────────────┘    │
└────────────────────────────────────────────────────────────────────────────┘
```

### Device Attestation & Identity

| Component | Mechanism |
|-----------|-----------|
| **Device identity** | Ed25519 keypair generated at first boot; private key in TPM/SE |
| **Mesh membership** | User signs device cert with master key → `MeshCert(device_pub, user_sig)` |
| **Trust** | Unconditional per-device; revocable via user-signed revocation cert |
| **Attestation** | Remote attestation via TPM quote + kernel measurement log |

**No Bluetooth pairing menus.** Devices join by scanning a QR code (containing `MeshCert`) or via proximity NFC.

---

## Encrypted Tunnels

**Protocol:** Noise_NK (or WireGuard) with capability-gated endpoints.

```rust
struct MeshTunnel {
    local_device: DeviceID,
    remote_device: DeviceID,
    session_key: SessionKey,          // ephemeral, rotated per session
    capabilities: CapabilitySet,      // what this tunnel grants access to
    state: TunnelState,               // Connecting, Established, Degraded, Closed
}
```

**Capability-gated:** A tunnel to your tablet grants `Cap::NPUSlice(tablet)` and `Cap::ObjectSync(scope)`, but *not* `Cap::ScreenRead` or `Cap::AudioStream` unless explicitly granted.

---

## State Roaming (True State, Not Just Data)

See [`mesh/state-roaming.md`](mesh/state-roaming.md) for full protocol.

**What roams:**
- App memory snapshot (via P5 session manager)
- Cursor position, selection, scroll offset
- Undo/redo history
- Window layout, focus, z-order
- Input method state (IME composition)
- Clipboard contents (universal)

**What does NOT roam:**
- GPU/NPU device handles (re-acquired on target)
- Raw sensor streams (re-subscribed)
- Unencrypted secrets (never leave device)

**Handoff latency target:** < 500ms LAN, < 2s over relay.

---

## Resource Brokerage (Async-First)

See [`mesh/resource-broker.md`](mesh/resource-broker.md).

**Brokerable resources:**
| Resource | Local | Remote (brokered) | Latency Class |
|----------|-------|-------------------|---------------|
| NPU compute | ✅ | ✅ (async batch) | Batch (minutes–hours) |
| GPU render | ✅ | ✅ (async) | Batch |
| Cellular modem | ✅ | ✅ (failover) | Real-time (failover) |
| Storage | ✅ | ✅ (sync) | Background |
| Sensors | ✅ | ❌ | N/A |

**Key insight:** Real-time cross-device compute (e.g., "render this frame on my tablet's NPU *now*") is **not feasible** over Wi-Fi — round-trip exceeds local inference. The broker *only* offers async batch offload.

---

## Universal Clipboard & Audio Routing

See [`mesh/clipboard-audio.md`](mesh/clipboard-audio.md).

**Clipboard:**
- Copy on any device → encrypted blob synced to all mesh peers via P2
- Paste on any device → decrypts locally
- **Gaze-following:** If eye-tracking available, clipboard auto-surfaces on device you're looking at

**Audio:**
- System audio output follows active session device
- Phone call → routes to laptop headphones if laptop is active
- Music playback → seamless handoff (like AirPlay but mesh-native, E2E encrypted)

---

## Network Failover

See [`mesh/network-failover.md`](mesh/network-failover.md).

**Cellular fallback:**
- Phone's modem exposed as `Cap::CellularModem` to laptop via mesh tunnel
- Laptop network stack treats it as a virtual NIC
- Seamless TCP migration via MPTCP or connection migration (QUIC)
- User sees: "Wi-Fi lost — using phone's 5G" (no interruption)

---

## Capability Surface (P4 → P6)

| Capability | Operations |
|------------|------------|
| `Cap::MeshPeer(peer_id)` | `send(msg)`, `recv() → msg`, `get_caps() → CapabilitySet` |
| `Cap::StateRoaming` | `snapshot() → StateBlob`, `restore(blob)`, `handoff(target)` |
| `Cap::ResourceBroker` | `request_offload(task) → JobID`, `poll(job) → Result` |
| `Cap::Clipboard` | `read() → Data`, `write(data)`, `subscribe() → Stream` |
| `Cap::AudioRoute` | `get_sinks() → Sink[]`, `route_to(sink_id)` |
| `Cap::NetworkFailover` | `enable_fallback(modem_cap)`, `get_status() → Status` |

---

## Open Questions

See [`open/open-questions.md#p4-mesh`](../open/open-questions.md#p4-mesh).

---

## References

- WireGuard / Noise protocol: Modern encrypted tunnels
- CRDTs for state sync: Yjs, Automerge
- MPTCP / QUIC connection migration: Seamless network handoff
- Apple Continuity / Microsoft Phone Link: Prior art (but cloud-dependent)
- Zero-knowledge relays: OHTTP, Oblivious DNS, mixnets