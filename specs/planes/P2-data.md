# P2 — Data Plane: Object Store + Relationship Graph

**Plane:** P2 (unprivileged)  
**Feasibility:** 🟢 BUILDABLE — content-addressed stores, graph DBs, CRDTs are mature  
**Dependencies:** P0 (capabilities), P1 (block storage)  
**Dependents:** P3 (AI index), P4 (mesh sync), P5 (session), P6 (apps)

---

## Design Goal

Replace the hierarchical folder tree (C:\Users\Name\Documents) with a **flat, content-addressed object store** overlaid with a **rich relationship graph**. Files are organized by *what they are and how they relate*, not *where they sit*.

---

## Core Abstractions

### 1. Content-Addressed Objects (The "What")

```
ObjectID = Blake3(content || type_tag || metadata_hash)
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | `ObjectID` | Global content address (64 bytes Blake3) |
| `type` | `ObjectType` | `file`, `directory_view`, `model`, `index_shard`, `snapshot` |
| `payload` | `bytes` | Immutable content (deduplicated by hash) |
| `metadata` | `ObjectMetadata` | See below |
| `refs` | `ObjectID[]` | Child objects (for composite objects) |

**ObjectMetadata:**
```rust
struct ObjectMetadata {
    // Provenance (immutable, set at creation)
    created_at: Timestamp,
    created_by: PrincipalID,      // app, user, system, peer
    source: ProvenanceSource,     // local, import, mesh_sync, ai_generated
    source_chain: ObjectID[],     // derivation chain (for AI outputs)

    // Mutable tags (user/app editable)
    tags: TagSet,                 // free-form + schema tags
    projects: ProjectID[],        // workspace membership
    collections: CollectionID[],  // user-defined groups

    // System-managed
    size_bytes: u64,
    mime_type: MimeType,
    encryption: EncryptionInfo,   // algorithm, key_id, nonce
    access_policy: CapabilityPolicy, // who can read/write via caps
}
```

**Deduplication is free:** Two identical files = one payload, two metadata records pointing to it.

---

### 2. Relationship Graph (The "How")

Edges are first-class, typed, and queryable.

```rust
enum EdgeType {
    // Containment (replaces folder hierarchy)
    Contains { parent: ObjectID, child: ObjectID, order: u32 },

    // Semantic tags
    Tagged { object: ObjectID, tag: TagID, confidence: f32 }, // 1.0 = user, <1.0 = AI inferred

    // Project/workspace membership
    InProject { object: ObjectID, project: ProjectID, role: ProjectRole },

    // Provenance / derivation
    DerivedFrom { child: ObjectID, parent: ObjectID, transform: TransformID },
    GeneratedBy { object: ObjectID, model: ModelID, prompt: PromptID },

    // Temporal
    VersionOf { current: ObjectID, previous: ObjectID },
    SnapshotOf { snapshot: ObjectID, target: ObjectID, at: Timestamp },

    // Cross-device
    SyncedFrom { local: ObjectID, peer: PeerID, remote: ObjectID, at: Timestamp },

    // App-specific (extensible)
    Custom { namespace: String, type: String, payload: bytes },
}
```

**Graph queries power everything:**
- "All files in project Q3-planning" → `InProject` edges
- "Everything tagged finance from last week" → `Tagged` + timestamp filter
- "The chart I looked at Tuesday" → P5 session history + `Viewed` edge + `Tagged: chart`
- "Files derived from that meeting recording" → `DerivedFrom` transitive closure

---

### 3. Directory Views (The "Folder Illusion")

A **directory view** is a *saved graph query* that presents a tree.

```rust
struct DirectoryView {
    id: ObjectID,
    query: GraphQuery,           // e.g., "InProject:Q3-planning AND Tagged:finance"
    sort: SortSpec,              // by name, date, tag, relevance
    presentation: ViewPresentation, // tree, list, grid, timeline
    pinned: ObjectID[],          // manual ordering overrides
}
```

**Key property:** The same object appears in multiple views simultaneously. No "move" operation — just add/remove edges.

---

## Storage Layer

| Layer | Technology | Role |
|-------|------------|------|
| **Block device** | P1 storage server | Raw encrypted blocks |
| **Object store** | LSM-tree (RocksDB-style) orsled | `ObjectID → Object` mapping |
| **Graph index** | Adjacency lists + property indexes (LMDB/Redb) | Edge lookups, traversals |
| **Vector index** | HNSW (faiss/hnswlib) | Semantic search (P3) |
| **Blob cache** | Tiered: NVMe → peer → cloud (encrypted) | Large payloads not in hot set |

**Encryption:** Every object payload encrypted with per-object DEK; DEKs wrapped by user's master key (or per-project keys). P2 never sees plaintext.

---

## Mutation & Transactions

**All mutations are transactions** on the object store + graph index.

```rust
struct Transaction {
    operations: Vec<Op>,
    // Op = CreateObject | DeleteObject | AddEdge | RemoveEdge | UpdateMetadata
    precondition: Precondition,  // e.g., "object exists", "edge count < N"
    postcondition: Postcondition, // verified after commit
}
```

**Atomicity:** Single transaction = single WAL entry. Crash → replay WAL → consistent state.

**Snapshots:** Every committed transaction produces a new `Snapshot` object (immutable). The **snapshot ledger** (see `data/snapshot-ledger.md`) chains these for rollback.

---

## Sync & Replication (P2 ↔ P4)

See [`data/sync-protocol.md`](data/sync-protocol.md) and [`mesh/mesh-topology.md`](../mesh/mesh-topology.md).

**Sync unit:** Object + its incident edges (the "object closure").
**Conflict resolution:** CRDTs for tags/sets; last-writer-wins for metadata (with vector clocks); manual resolution for structural conflicts.
**Transport:** Encrypted, capability-gated, over P4 mesh tunnels.

---

## API Surface (Capability-Granted)

| Capability | Operations |
|------------|------------|
| `Cap::ObjectRead(id)` | `read(id) → Object`, `exists(id) → bool` |
| `Cap::ObjectWrite(id)` | `create(obj) → id`, `delete(id)`, `update_meta(id, patch)` |
| `Cap::GraphQuery(scope)` | `query(q) → EdgeIterator`, `traverse(from, edge_type) → Iterator` |
| `Cap::GraphMutate(scope)` | `add_edge(e)`, `remove_edge(e)` |
| `Cap::SnapshotRead` | `list_snapshots() → Snapshot[]`, `read_snapshot(id) → State` |
| `Cap::SnapshotRollback` | `rollback_to(snapshot_id)` (system-only) |

**Scope** limits which objects/edges a capability covers (e.g., `project:Q3-planning`, `tag:personal`, `all`).

---

## Migration from Hierarchical FS

| Legacy Concept | Axiom Equivalent |
|----------------|------------------|
| Folder | `DirectoryView` (saved query) |
| File path | `ObjectID` + `Contains` edges for one view |
| `mv` | Remove `Contains` edge from view A, add to view B |
| `cp` | New metadata record pointing to same payload (free) |
| Symlink | `DerivedFrom` edge or `Custom` edge |
| Permissions | `CapabilityPolicy` on object metadata |

**Compatibility layer:** P6 provides a FUSE/VFS shim that exposes a POSIX-like tree for legacy apps — backed by a `DirectoryView` the app "sees."

---

## Open Questions

See [`open/open-questions.md#p2-data`](../open/open-questions.md#p2-data).

---

## References

- Content-addressed storage: Git, IPFS, CAS (content-addressable storage)
- Graph databases: Neo4j, Dgraph, TerminusDB
- CRDTs: Yjs, Automerge, Riak
- Vector search: FAISS, HNSWlib, Milvus
- Snapshot/rollback: ZFS, Btrfs, ostree, NixOS