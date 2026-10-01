# Data — Snapshot Ledger (Atomic Updates & Rollback)

**Feasibility:** 🟢 BUILDABLE — ZFS, Btrfs, ostree, NixOS, CRDTs proven  
**Dependencies:** P2 object model, graph schema, P0 verified boot

---

## Design Goal

**Every system state change is a transaction producing an immutable snapshot.** Rollback is instant: boot the previous snapshot. No "update failed, system broken" — the previous working state is always one reboot away.

---

## Snapshot Model

```rust
struct Snapshot {
    id: SnapshotID,              // Blake3(prev_snapshot || tx_root || timestamp)
    prev: Option<SnapshotID>,    // Parent snapshot (chain)
    timestamp: Timestamp,
    tx_root: TxRootHash,         // Merkle root of all transactions in this snapshot
    description: String,         // "OS update 2.1.0", "User installed app", "AI model updated"
    tags: Vec<SnapshotTag>,      // System, User, App, AI, Recovery
    bootable: bool,              // Can this snapshot boot?
}

struct TxRootHash {
    // Merkle tree of all transactions since prev snapshot
    // Enables efficient verification of state at any point
}
```

---

## Transaction Structure

```rust
struct Transaction {
    id: TxID,                    // Blake3(operations || timestamp || author)
    timestamp: Timestamp,
    author: PrincipalID,         // User, System, App, AI, Peer
    operations: Vec<Operation>,  // Atomic unit
    preconditions: Vec<Precondition>,  // Must hold before commit
    postconditions: Vec<Postcondition>, // Verified after commit
}

enum Operation {
    CreateObject { object: Object },
    DeleteObject { id: ObjectID },
    UpdateMetadata { id: ObjectID, patch: MetadataPatch },
    AddEdge { edge: Edge },
    RemoveEdge { edge_id: EdgeID },
    CreateSnapshot { description: String, tags: Vec<SnapshotTag> },
    InstallModel { model_pkg: ModelPackage },
    UpdatePolicy { policy: CapabilityPolicy },
}
```

**All mutations go through transactions.** No direct writes.

---

## Snapshot Ledger

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ SNAPSHOT CHAIN                                                              │
│                                                                             │
│  S0 (initial) ──► S1 ──► S2 ──► S3 ──► S4 (current)                        │
│       │           │       │       │        │                                │
│       │           │       │       │        └─ "OS update 2.1.0" [System]   │
│       │           │       │       └─ "Installed SuperApp" [User]           │
│       │           │       └─ "Index rebuilt" [AI]                          │
│       │           └─ "Mesh peer added" [User]                              │
│       └─ "Fresh install" [System]                                          │
│                                                                             │
│  Each snapshot = complete system state (P0+P1+P3 sealed + P2 mutable)      │
│  Rollback = set boot target to S2 → reboot                                 │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Boot-Time Rollback

**Bootloader menu:**
```
Axiom Boot Menu
────────────────────────────────────────
►  Boot current (S4) — "OS update 2.1.0"
    Boot previous (S3) — "Installed SuperApp"
    Boot previous (S2) — "Index rebuilt"
    Boot previous (S1) — "Mesh peer added"
    Boot recovery (S0) — "Fresh install"
    Enter recovery shell
```

**Selection →** Bootloader sets `boot_snapshot = S3` in kernel cmdline → Kernel loads P0+P1+P3 from S3's sealed partition → P2 mounts S3's object store root → System boots to exact S3 state.

---

## Automatic Rollback on Failure

| Failure Point | Detection | Action |
|---------------|-----------|--------|
| **P0 kernel panic** | Watchdog / double fault | Auto-reboot → boot previous snapshot |
| **P1 server crash loop** | Restart > 3x in 30s | Mark snapshot unbootable; boot previous |
| **P3 NRM init failure** | Health check timeout | Mark snapshot unbootable; boot previous |
| **P2 corruption detected** | fs-verity / merkle mismatch | Mark snapshot unbootable; boot previous |
| **Boot timeout** | > 60s to user session | Mark snapshot unbootable; boot previous |

**User always notified:** "Previous update had an issue. You're running the last known-good version. [View details] [Try update again]"

---

## Snapshot Storage

| Component | Storage |
|-----------|---------|
| **P0+P1+P3 (sealed)** | A/B partitions (active + previous) + rollback partition |
| **P2 object store** | Copy-on-write B-tree (like ZFS) — snapshots are COW roots |
| **Capability tables** | Sealed to snapshot PCR state |
| **User data** | Included in P2 COW — rollback restores user files too |

**Space efficiency:** COW means snapshots share unchanged blocks. 100 snapshots ≈ 1.2× base size (typical).

---

## User-Initiated Snapshots

| Trigger | Description |
|---------|-------------|
| **Pre-update** | Automatic before any OS/app/model update |
| **Pre-risky-op** | Before `Cap::SnapshotRollback` grant to app |
| **Manual** | User: "Save checkpoint before experiment" |
| **Periodic** | Daily automatic (configurable) |
| **Mesh sync** | After successful peer sync (optional) |

---

## Garbage Collection

- **Keep:** Last 50 snapshots, all tagged `System`, all `bootable=true`
- **Prune:** Older untagged snapshots (configurable retention)
- **Deep prune:** `axiom snapshot prune --deep` — reclaims blob chunks unreferenced by any retained snapshot

---

## Performance Targets

| Operation | Target |
|-----------|--------|
| Snapshot create (COW) | < 5 ms |
| Rollback boot (vs normal) | +2–3 s |
| Space per snapshot (typical) | 50–200 MB |
| Full rollback verify | < 1 s |

---

## Open Questions

See [`open/open-questions.md#snapshot-ledger`](../open/open-questions.md#snapshot-ledger).