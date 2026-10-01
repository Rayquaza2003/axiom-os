# Data — Object Model (Content-Addressed Storage)

**Feasibility:** 🟢 BUILDABLE — Git, IPFS, CAS, content-defined chunking proven  
**Dependencies:** P1 block storage, P0 capabilities

---

## Object Identity

```
ObjectID = Blake3(content || type_tag || metadata_hash)
```

- **Blake3** — fast, parallelizable, 256-bit output
- **type_tag** — domain separation: `file`, `dir_view`, `model`, `index_shard`, `snapshot`, `blob`
- **metadata_hash** — hash of immutable metadata (provenance, creation); mutable tags excluded

**Collision resistance:** 256-bit = practically zero for any realistic dataset.

---

## Object Structure

```rust
struct Object {
    id: ObjectID,
    obj_type: ObjectType,
    payload: PayloadRef,        // inline (≤4KB) or blob reference
    metadata: ObjectMetadata,
    refs: Vec<ObjectID>,        // child objects (composite objects)
}

enum ObjectType {
    File,                       // Regular file
    DirectoryView,              // Saved graph query (folder illusion)
    ModelPackage,               // P3 model (manifest + weights ref)
    IndexShard,                 // Semantic index partition
    Snapshot,                   // Point-in-time system state
    BlobChunk,                  // Large payload chunk (>4KB)
}

struct PayloadRef {
    kind: PayloadKind,
    // Inline
    inline_data: Option<Vec<u8>>,      // ≤4KB, stored in object record
    // Blob (content-defined chunking)
    blob_chunks: Option<Vec<ObjectID>>, // BlobChunk objects
}

enum PayloadKind {
    Inline,
    Blob { chunk_count: u32, total_size: u64 },
}
```

---

## Object Metadata

```rust
struct ObjectMetadata {
    // Immutable provenance (set at creation, never changes)
    created_at: Timestamp,
    created_by: PrincipalID,           // User, App, System, Peer, AI
    source: ProvenanceSource,
    derivation_chain: Vec<ObjectID>,   // For AI outputs: prompt → model → result

    // Mutable user/app tags
    tags: TagSet,                      // Free-form + schema tags
    projects: Vec<ProjectID>,          // Workspace membership
    collections: Vec<CollectionID>,    // User-defined groups

    // System-managed
    size_bytes: u64,
    mime_type: MimeType,
    encryption: EncryptionInfo,
    access_policy: CapabilityPolicy,   // Capability grants required to access
}

enum ProvenanceSource {
    LocalCreate,        // User created
    Import { from: SourceID },        // Imported from external
    MeshSync { from: PeerID },        // Synced from mesh peer
    AIGenerated { model: ModelID, prompt: PromptID },
    Derived { transform: TransformID, inputs: Vec<ObjectID> },
}
```

---

## Encryption

**Every object payload encrypted independently.**

```
DEK (Data Encryption Key) = HKDF(master_key, object_id || "dek")
payload_ciphertext = AES-256-GCM(DEK, payload, aad=object_id)
```

- **Master key** — derived from user's master passphrase + TPM-sealed secret
- **Per-project keys** — optional; `HKDF(master_key, project_id || "project")`
- **P2 never sees plaintext** — encryption/decryption in P6 app container (via capability)

---

## Large Payloads: Content-Defined Chunking

For payloads > 4KB:

1. **Chunk** using FastCDC (content-defined, variable-size, avg 64KB)
2. Each chunk → `BlobChunk` object (content-addressed)
3. Parent object stores `blob_chunks: [ObjectID]`
4. **Deduplication automatic** — identical chunks = same ObjectID

```rust
struct BlobChunk {
    id: ObjectID,              // Blake3(chunk_data)
    data: Vec<u8>,             // Encrypted chunk
    sequence: u32,             // Order in parent
}
```

---

## Object Lifecycle

```
CREATE (transaction)
    │
    ├─► Compute ObjectID
    ├─► Encrypt payload → DEK wrapped by user key
    ├─► Write to object store (LSM-tree)
    ├─► Add graph edges (Contains, Tagged, InProject, etc.)
    └─► Commit transaction → new Snapshot

READ
    ├─► Capability check (Cap::ObjectRead(scope))
    ├─► Fetch object record
    ├─► If blob: fetch chunks in parallel
    ├─► Decrypt (DEK unwrapped via capability)
    └─► Return plaintext to caller

UPDATE (not in-place — new object version)
    ├─► Create new object with new ObjectID
    ├─► Add VersionOf edge (new → old)
    ├─► Update DirectoryView Contains edges (if in views)
    └─► Commit → new Snapshot

DELETE
    ├─► Capability check (Cap::ObjectWrite(scope))
    ├─► Remove graph edges (Contains, Tagged, etc.)
    ├─► Mark object tombstoned (GC later)
    └─► Commit → new Snapshot
```

---

## Garbage Collection

- **Reference counting** via graph edges (Contains, Tagged, InProject, DerivedFrom, VersionOf)
- **Root set:** User's active projects, pinned objects, recent snapshots
- **Cycle detection** — periodic mark-sweep on graph
- **Blob chunks** — refcounted by parent objects
- **Policy:** Unreferenced objects retained for 30 days (recoverable via snapshot)

---

## Performance Targets

| Operation | Target |
|-----------|--------|
| Object create (small, inline) | < 1 ms |
| Object read (inline, cached) | < 100 μs |
| Object read (blob, 100MB) | < 50 ms (parallel chunk fetch) |
| Graph query (10k edges) | < 5 ms |
| Transaction commit | < 2 ms |
| Snapshot create | < 1 ms (copy-on-write) |

---

## Open Questions

See [`open/open-questions.md#object-model`](../open/open-questions.md#object-model).