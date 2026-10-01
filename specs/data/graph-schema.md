# Data — Relationship Graph Schema

**Feasibility:** 🟢 BUILDABLE — property graphs, adjacency lists, LMDB/Redb proven  
**Dependencies:** P2 object model

---

## Graph Model

**Directed, labeled, typed property graph.** Nodes = Objects (ObjectID). Edges = Typed relationships with properties.

```rust
struct Edge {
    id: EdgeID,                 // Blake3(src || type || dst || props_hash)
    src: ObjectID,
    dst: ObjectID,
    edge_type: EdgeType,
    properties: EdgeProperties, // Type-specific
    created_at: Timestamp,
    created_by: PrincipalID,
}
```

---

## Core Edge Types

### 1. Containment (Replaces Folder Hierarchy)

```rust
EdgeType::Contains {
    parent: ObjectID,           // DirectoryView or Project
    child: ObjectID,            // File, DirectoryView, etc.
    order: u32,                 // Manual sort order
    view_specific: bool,        // true = only in this view; false = global
}
```

- A file can have **multiple Contains parents** (in multiple views)
- `DirectoryView` is just an object with `Contains` edges to its children
- `view_specific=true` edges only appear in that view's query

### 2. Tags

```rust
EdgeType::Tagged {
    object: ObjectID,
    tag: TagID,                 // Normalized tag (lowercase, deduped)
    confidence: f32,            // 1.0 = user explicit; 0.0–0.9 = AI inferred
    source: TagSource,          // User, AI, Import, Heuristic
}
```

**Tag hierarchy:** Tags can have `SubtagOf` edges forming a DAG (e.g., `finance/tax` → `finance`).

### 3. Project Membership

```rust
EdgeType::InProject {
    object: ObjectID,
    project: ProjectID,
    role: ProjectRole,          // Owner, Member, Viewer, Archive
}
```

- Projects are first-class objects (have metadata, members, settings)
- Workspace = saved query `InProject:X`

### 4. Provenance / Derivation

```rust
EdgeType::DerivedFrom {
    child: ObjectID,
    parent: ObjectID,
    transform: TransformID,     // e.g., "pdf_to_text", "summarize", "translate"
    transform_version: String,
}

EdgeType::GeneratedBy {
    object: ObjectID,
    model: ModelID,
    prompt: PromptID,
    parameters: GenerationParams,
}
```

### 5. Temporal / Versioning

```rust
EdgeType::VersionOf {
    current: ObjectID,
    previous: ObjectID,
    change_type: ChangeType,    // Edit, Move, Tag, Metadata
}

EdgeType::SnapshotOf {
    snapshot: ObjectID,         // Snapshot object
    target: ObjectID,           // What was snapshotted
    at: Timestamp,
}
```

### 6. Cross-Device Sync

```rust
EdgeType::SyncedFrom {
    local: ObjectID,
    peer: PeerID,
    remote: ObjectID,           // ObjectID on peer (same content = same ID)
    at: Timestamp,
    sync_status: SyncStatus,    // Synced, Pending, Conflict
}
```

### 7. Session / Interaction History (P5)

```rust
EdgeType::Viewed {
    object: ObjectID,
    session: SessionID,
    at: Timestamp,
    duration: Duration,
    region: Option<Region>,     // Scroll position, selection
}

EdgeType::Edited {
    object: ObjectID,
    session: SessionID,
    at: Timestamp,
    diff: DiffRef,              // Reference to diff object
}

EdgeType::Selected {
    object: ObjectID,
    session: SessionID,
    at: Timestamp,
    selection: SelectionRef,    // Text range, file subset, etc.
}
```

### 8. Custom / Extensible

```rust
EdgeType::Custom {
    namespace: String,          // Reverse-DNS: "com.example.app"
    type: String,               // e.g., "layer_link", "annotation"
    payload: Vec<u8>,           // CBOR/JSON encoded
}
```

---

## Graph Indexes (For Query Performance)

| Index | Key | Use Case |
|-------|-----|----------|
| `out_edges[src][type]` | `(src, type) → [dst]` | "What tags does this file have?" |
| `in_edges[dst][type]` | `(dst, type) → [src]` | "What views contain this file?" |
| `tag_index[tag]` | `tag → [object]` | "All files tagged finance" |
| `project_index[project]` | `project → [object]` | Workspace contents |
| `time_index[edge_type]` | `(type, created_at) → [edge]` | Recent activity |
| `fulltext_index` | `token → [object]` | Keyword search (supplements semantic) |

---

## Query Language (GraphQuery)

```rust
struct GraphQuery {
    // Starting points
    roots: Vec<QueryRoot>,      // Project, Tag, ObjectID, All

    // Traversal
    traversals: Vec<Traversal>, // Chain of edge types

    // Filters
    filters: Vec<Filter>,       // On object metadata or edge properties

    // Output
    projection: Projection,     // What to return
    sort: SortSpec,
    limit: Option<u32>,
    offset: Option<u32>,
}

enum QueryRoot {
    Project(ProjectID),
    Tag(TagID),
    Object(ObjectID),
    All,
}

enum Traversal {
    Out(EdgeType),              // Follow outgoing edges
    In(EdgeType),               // Follow incoming edges
    Both(EdgeType),             // Both directions
    Recursive { max_depth: u32 }, // Transitive closure
}

enum Filter {
    Metadata { key: String, op: Op, value: Value },
    EdgeProp { edge_type: EdgeType, key: String, op: Op, value: Value },
    TimeRange { edge_type: EdgeType, since: Timestamp, until: Timestamp },
    HasEdge { edge_type: EdgeType, target: Option<ObjectID> },
}
```

---

## Example Queries

| Natural Language | GraphQuery |
|------------------|------------|
| "Files in Q3 project tagged finance" | `roots=[Project("q3")], traversals=[Out(InProject), Out(Tagged)], filters=[Tag="finance"]` |
| "Everything derived from meeting recording X" | `roots=[Object(X)], traversals=[Recursive(Out(DerivedFrom))]` |
| "What I viewed Tuesday 2–4pm" | `roots=[All], traversals=[In(Viewed)], filters=[TimeRange(Viewed, Tue 14:00, Tue 16:00)]` |
| "Charts from last week" | `roots=[All], traversals=[Out(Tagged)], filters=[Tag="chart", TimeRange(Created, -7d)]` |

---

## Graph Storage

- **Adjacency lists** in LMDB/Redb (ordered by edge type + target)
- **Edge properties** stored inline for small; separate table for large
- **Write path:** Transaction adds object + edges atomically
- **Read path:** Iterator fusion for multi-hop traversals

---

## Open Questions

See [`open/open-questions.md#graph-schema`](../open/open-questions.md#graph-schema).