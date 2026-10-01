# Data — P2P Sync Protocol

**Feasibility:** 🟢 BUILDABLE — CRDTs, libp2p, WireGuard, BitTorrent proven  
**Dependencies:** P2 object model, graph schema, P4 mesh tunnels

---

## Design Goals

1. **Zero-configuration** — devices find each other via mesh
2. **End-to-end encrypted** — relay cannot read
3. **Conflict-free** — CRDTs for concurrent edits
4. **Efficient** — delta sync, content-defined chunking
5. **Selective** — sync by project, tag, or explicit selection

---

## Sync Topology

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ MESH SYNC TOPOLOGY                                                          │
│                                                                             │
│   Device A ◄─────────────────────────────────────────► Device B             │
│      │                                                    │                  │
│      │              ┌─────────────────┐                  │                  │
│      └─────────────►│   OPTIONAL      │◄─────────────────┘                  │
│                     │   RELAY         │                                       │
│                     │  (DHT/Introducer)│                                       │
│                     │  Carries ONLY   │                                       │
│                     │  CIPHERTEXT     │                                       │
│                     └─────────────────┘                                       │
│                                                                             │
│   All sync traffic: Encrypted Mesh Tunnel (Noise_NK)                       │
│   Capability-gated: Cap::ObjectSync(scope) per tunnel                      │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Sync Unit: Object Closure

**What syncs:** An object + all its incident edges (the "closure").

```rust
struct SyncObject {
    object: Object,                    // Full object (metadata + inline payload)
    edges: Vec<Edge>,                  // All edges where src or dst = object
    blob_chunks: Vec<BlobChunk>,       // If payload is blob
    vector_records: Vec<VectorRecord>, // If indexed
}
```

**Sync scope** (capability-gated):
- `project:work` — everything in work project
- `tag:personal` — everything tagged personal
- `object:<id>` — single object + closure
- `all` — everything (user-initiated backup)

---

## CRDT Design

### Tag Set (G-Counter + OR-Set)

```rust
// User adds tag → increment G-Counter(tag)
// User removes tag → add to OR-Set tombstone
// Merge: max(counters) - tombstones
```

### Project Membership (LWW-Register per object-project)

```rust
// Last-writer-wins on (object, project) pair
// Timestamp = hybrid logical clock (HLC)
```

### Graph Edges (Add-Wins OR-Set)

```rust
// Add edge → wins
// Remove edge → add tombstone
// Concurrent add+remove → add wins (safer: user sees it, can remove again)
```

### Object Metadata (LWW-Map)

```rust
// Per-field LWW (size, mime, encryption, access_policy)
// Custom fields: app-defined, last-writer-wins
```

---

## Sync Protocol

### Handshake

```
A → B: Hello { device_id, capabilities, sync_scopes, hlc_time }
B → A: Hello { device_id, capabilities, sync_scopes, hlc_time, known_heads }
```

`known_heads` = `{scope: (object_id, hlc_timestamp)}` — what B thinks is latest.

### Delta Exchange

```
A → B: SyncRequest { scope, since: HLC, limit: 1000 }
B → A: SyncResponse { objects: [SyncObject], edges: [Edge], heads: {scope: HLC}, more: bool }
```

**Pagination:** `limit` + `more` flag; client requests next batch.

### Conflict Detection

```
Local object.version_vector vs Remote object.version_vector
    │
    ├─► Local dominates → keep local
    ├─► Remote dominates → accept remote
    └─► Concurrent → CRDT merge (per field)
```

**User-visible conflicts:** Only for structural conflicts (e.g., two DirectoryViews claim same child with different order). Presented as "Sync conflict — choose layout."

---

## Large Blob Sync

1. **Manifest first** — SyncObject lists `blob_chunks: [ObjectID]`
2. **Chunk availability** — Bloom filter of available chunks exchanged
3. **Parallel fetch** — Request missing chunks from any peer that has them
4. **Verify** — Blake3 hash of reassembled blob matches parent object

---

## Mesh Relay (For NAT Traversal)

- **Relay cannot decrypt** — only sees Noise handshake + encrypted frames
- **Relay cannot correlate** — no metadata about what's syncing
- **User can run own relay** — VPS, home server, community relay
- **Relay discovery** — DHT (libp2p) or static config

---

## Offline & Intermittent Sync

- **HLC (Hybrid Logical Clocks)** — handles clock drift, offline edits
- **Local-first** — all edits apply immediately; sync when online
- **Queue** — Outbound sync queued per peer; retries with exponential backoff
- **Conflict resolution** — Deferred until both online; CRDT auto-merges most

---

## Selective Sync UI

| Scope | Use Case |
|-------|----------|
| `project:work` | Laptop ↔ Tablet (work context) |
| `tag:personal` | Phone ↔ Laptop (personal photos, notes) |
| `object:<id>` | "Send this file to tablet" |
| `all` | New device setup; full backup |

---

## Performance Targets

| Metric | Target |
|--------|--------|
| LAN sync (1 GB) | < 2 min |
| Relay sync (1 GB) | < 10 min (bandwidth-limited) |
| Conflict resolution | < 100 ms |
| Memory during sync | < 200 MB |
| Battery impact (background) | < 2%/hour |

---

## Open Questions

See [`open/open-questions.md#sync-protocol`](../open/open-questions.md#sync-protocol).