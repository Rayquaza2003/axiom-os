# Mesh — State Roaming

**Feasibility:** 🟡 HARD BUT KNOWN — live migration of app state exists (VM, containers); app-cooperative is harder  
**Dependencies:** P0 caps, P2 snapshots, P4 mesh tunnels, P5 session model

---

## Roaming Model

```
Source Device                          Target Device
     │                                    │
     │  1. User initiates roam            │
     │     (tap, gaze, or voice)          │
     │                                    │
     │  2. P5 captures session state      │
     │     (COW snapshot + app state)     │
     │                                    │
     │  3. Stream to target               │
     │     via mesh tunnel                │
     │───────────────────────────────────►│
     │                                    │
     │  4. Target restores                │
     │     (kernel resume + app resume)   │
     │                                    │
     │  5. Handoff complete               │
     │     (input/output redirected)      │
     │                                    │
```

**Goal:** < 2s LAN, < 10s WAN (relay). User-perceptible continuity.

---

## What Moves

### System State (Automatic)

| Component | Mechanism | Size |
|-----------|-----------|------|
| **Window tree** | Compositor scene graph snapshot | ~1 MB |
| **Input focus** | Capability to input bus | ~1 KB |
| **Clipboard** | P2 object (auto-synced) | ~10 KB |
| **Audio routing** | Capability grants to audio server | ~1 KB |
| **Network sockets** | TCP migration (MPTCP) or re-establish | — |

### App State (Cooperative)

```rust
trait Roamable {
    // Called on source before suspend
    fn prepare_roam(&mut self) -> Result<AppRoamState, Error>;
    
    // Called on target after resume
    fn restore_roam(&mut self, state: AppRoamState) -> Result<(), Error>;
    
    // Optional: incremental sync during session
    fn roam_delta(&mut self) -> Option<AppRoamDelta>;
}

struct AppRoamState {
    // Opaque to system; app-defined
    // Typically: scroll position, undo stack, form data, cursor
    blob: Vec<u8>,
    version: u32,
    schema_hash: Hash,  // Detect incompatible versions
}
```

**Default implementation:** Apps that don't implement `Roamable` get window position + focus only. No data continuity.

---

## Snapshot Pipeline (COW)

```
P2 Snapshot Ledger (ZFS/ostree-style)
     │
     ▼
┌─────────────────────────────────────┐
│ ROAM SNAPSHOT (Read-only clone)     │
│                                     │
│  /objects   → COW clone             │
│  /graph     → COW clone             │
│  /vectors   → COW clone (refs only) │
│                                     │
│  Size: ~50-200 MB (metadata only)   │
│  Time: < 50 ms (metadata ops)       │
└─────────────────────────────────────┘
     │
     ▼
Stream to target via mesh tunnel (Noise-encrypted)
     │
     ▼
Target mounts as new root → kernel resumes processes
```

**Key insight:** COW means snapshot is instant. Only modified pages transfer during roam.

---

## Handoff Protocol

### Phase 1: Prepare (Source)

```
1. P5: Capture compositor state → scene graph
2. P5: Request app roam state (parallel, 200ms timeout)
3. P2: Create COW snapshot of relevant projects/tags
4. P4: Establish high-priority tunnel to target
5. P5: Serialize handoff manifest
```

### Phase 2: Transfer

```
6. Stream: manifest → snapshot metadata → app states
7. Priority: system state first, then app state
8. Target: Acknowledges each chunk
9. Source: Keeps running until target confirms ready
```

### Phase 3: Activate (Target)

```
10. P0: Mount snapshot as new root
11. P1: Restore window server from scene graph
12. P6: Launch apps with roam state
13. P5: Transfer input focus capability
14. P4: Redirect mesh tunnels to new device
15. Source: Suspends (or keeps running if multi-device)
```

---

## Multi-Device Continuity

### Simultaneous Active (Default)

```
┌─────────────────────────────────────────────────────────────┐
│  Laptop: Editing doc (primary input)                        │
│  Tablet: Viewing same doc (secondary, read-only cursor)     │
│  Phone:   Notification mirror only                          │
│                                                             │
│  Edits → CRDT merge → all devices update in < 100ms         │
└─────────────────────────────────────────────────────────────┘
```

### Handoff (Single Active)

```
Laptop ──roam──► Tablet ──roam──► Phone
     │              │              │
     ▼              ▼              ▼
  Suspend        Resume         Resume
```

**Policy:** User chooses per-workspace: `simultaneous` or `handoff`.

---

## Failure Modes

| Failure | Detection | Recovery |
|---------|-----------|----------|
| Transfer interrupted | Chunk ack timeout | Resume from last ack (COW helps) |
| Target restore fails | App `restore_roam` error | Fallback: fresh launch + open same objects |
| Version mismatch | `schema_hash` check | Auto-convert or prompt user |
| Network partition | Mesh heartbeat loss | Local continue; sync on reconnect |

---

## Open Questions

See [`open/open-questions.md#state-roaming`](../open/open-questions.md#state-roaming).