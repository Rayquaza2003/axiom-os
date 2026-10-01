# Mesh — Topology, Attestation, Relay

**Feasibility:** 🟢 BUILDABLE — WireGuard, libp2p, DHT, device attestation proven  
**Dependencies:** P0 caps, P1 radios, P4 mesh service

---

## Device Identity & Attestation

### Key Hierarchy (Recap)

```
User Master Key (Ed25519, derived from passphrase + TPM)
       │
       ▼ signs
Device Certificate (per device)
       │
       ├── Device Identity Key (Ed25519, in TPM)
       └── Mesh Certificate = { device_pub, device_id, user_sig, caps, expiry }
```

### Mesh Join Flow

```
New Device                          Existing Mesh
     │                                   │
     │  1. Generate DeviceID + Keypair   │
     │  2. Create MeshCert (unsigned)    │
     │  3. Display QR (MeshCert)         │
     │◄──────────────────────────────────│
     │                                   │  4. User scans QR on trusted device
     │                                   │  5. Trusted device verifies user_sig
     │                                   │  6. Trusted device signs MeshCert
     │                                   │  7. Broadcasts signed cert to mesh
     │──────────────────────────────────►│
     │  8. Receives signed MeshCert      │
     │  9. Establishes tunnels to peers  │
     │                                   │
```

**No central authority.** Trust = user's master key signs device cert.

---

## Tunnel Establishment

### Noise_NK Handshake (Per Peer Pair)

```
Initiator (A)                    Responder (B)
    │                                 │
    │  1. Ephemeral keypair           │
    │  2. Static keypair (DeviceID)   │
    │                                 │
    │────── e, s, payload ──────────►│
    │  (payload = MeshCert_A)         │
    │                                 │
    │◄────── e, ee, se, payload ──────│
    │  (payload = MeshCert_B)         │
    │                                 │
    │  3. Verify MeshCert_B           │
    │  4. Derive session keys         │
    │                                 │
    │  Session established            │
    │  (transport keys, handshake done)│
```

**Capability-gated:** Handshake payload includes `CapabilitySet` — what this tunnel grants. Peer validates against its policy.

---

## Mesh Topology

### Full Mesh (≤ 8 devices)

```
All peers connect directly (Wi-Fi LAN)
Tunnel count: n*(n-1)/2
Latency: < 2ms LAN
```

### Relay-Assisted (≥ 8 devices or NAT)

```
┌─────────────────────────────────────────────────────────────────┐
│ RELAY TOPOLOGY                                                  │
│                                                                 │
│  Device A ◄── Tunnel ──► RELAY ◄── Tunnel ──► Device B         │
│       │                      │                      │           │
│       │                      │                      │           │
│       └──────────────────────┴──────────────────────┘           │
│                          │                                      │
│                   Relay sees:                                   │
│                   • Encrypted Noise frames                      │
│                   • Next-hop address                            │
│                   • NO payload, NO user identity                │
└─────────────────────────────────────────────────────────────────┘
```

**Relay selection:** Lowest latency, user-preferred, community-run or self-hosted.

---

## Capability-Gated Tunnels

```rust
struct TunnelCapabilities {
    // Data sync
    object_sync: Vec<SyncScope>,       // project:work, tag:personal, all
    
    // Compute offload
    npu_offload: bool,                 // Can request NRM slice on peer
    gpu_offload: bool,
    
    // State roaming
    state_roaming: bool,               // Can send/receive session snapshots
    
    // Clipboard / audio
    clipboard: bool,
    audio_route: bool,
    
    // Network
    cellular_failover: bool,           // Can use peer's modem
}
```

**Default:** New peer gets `object_sync: [all], clipboard, audio_route`. Others require explicit grant.

---

## Peer Discovery

### LAN (mDNS / Bonjour)

- `_axiom-mesh._tcp.local` → advertises device ID + Noise public key
- Automatic tunnel establishment on same network

### Internet (DHT / Introducer)

- libp2p Kademlia DHT for peer discovery
- **Introducer** (user-run or community) — helps initial connection
- Once connected, direct tunnels preferred; relay only for NAT traversal

---

## Mesh Health & Monitoring

| Metric | Target |
|--------|--------|
| Tunnel establishment (LAN) | < 500 ms |
| Tunnel establishment (relay) | < 2 s |
| Heartbeat interval | 10 s |
| Failure detection | 3 missed heartbeats (30 s) |
| Reconnect (LAN) | < 1 s |
| Reconnect (relay) | < 5 s |

---

## Open Questions

See [`open/open-questions.md#mesh-topology`](../open/open-questions.md#mesh-topology).