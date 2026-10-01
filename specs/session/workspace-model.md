# Session — Intent Workspace Model

**Feasibility:** 🟢 BUILDABLE — saved queries + materialized views pattern proven  
**Dependencies:** P2 graph, P3 semantic index, P5 compositor

---

## Core Concept

> **A workspace is a saved graph query + materialized view.**
> 
> Not a folder. Not a virtual desktop. A *live question* about your data.

```
User: "Show me the Q3 project"
        │
        ▼
┌─────────────────────────────────────────────────────────────┐
│ Workspace: "Q3 Project"                                     │
│                                                             │
│ Query:                                                      │
│   Project = "Q3"                                            │
│   OR Tag ∈ ["Q3", "quarterly", "revenue"]                  │
│   AND (Modified > 30 days ago)                              │
│                                                             │
│ Materialized View (cached):                                 │
│   ├─ Objects: 247 (docs, sheets, slides, code, notes)      │
│   ├─ Apps: [Sheets, Slides, Code, Notes, Browser]          │
│   ├─ People: [Sarah, Mike, Priya]                          │
│   ├─ Devices: [Laptop, Tablet]                             │
│   └─ Last sync: 2 min ago                                   │
└─────────────────────────────────────────────────────────────┘
```

---

## Workspace Definition

```rust
struct Workspace {
    id: WorkspaceID,              // = Blake3(name + owner)
    name: String,                 // User-visible
    description: Option<String>,
    
    // The query (immutable once saved)
    query: GraphQuery,            // See graph-schema.md
    
    // Materialization policy
    materialize: MaterializationPolicy,
    
    // UI hints
    layout: WorkspaceLayout,      // Grid, List, Canvas, Timeline
    pinned_objects: Vec<ObjectID>, // Always shown
    hidden_tags: Vec<TagID>,      // Filtered out
    
    // Sync / sharing
    sync_scope: SyncScope,        // Which devices get this workspace
    shared_with: Vec<UserID>,     // Collaborators (read-only or read-write)
    
    // Metadata
    created_at: Timestamp,
    updated_at: Timestamp,
    accessed_at: Timestamp,
}

enum MaterializationPolicy {
    Eager,        // Background refresh continuously
    OnOpen,       // Recompute when opened
    Manual,       // User clicks "Refresh"
    Scheduled,    // Cron: "every morning at 6am"
}

enum WorkspaceLayout {
    Grid { columns: u8 },
    List { group_by: GroupBy },
    Canvas { positions: HashMap<ObjectID, Pos> },
    Timeline { time_field: TimeField },
}
```

---

## Graph Query Language (Simplified)

```rust
struct GraphQuery {
    // Root constraints
    projects: Vec<ProjectID>,
    tags: TagQuery,
    time_range: Option<TimeRange>,
    
    // Traversal
    traversals: Vec<Traversal>,
    
    // Filters
    mime_types: Vec<MimeType>,
    provenance: Vec<ProvenanceFilter>,
    authors: Vec<UserID>,
    
    // Ranking
    rank_by: RankStrategy,
    limit: usize,
}

enum TagQuery {
    All(Vec<TagID>),           // AND
    Any(Vec<TagID>),           // OR
    None(Vec<TagID>),          // NOT
    Nested(Box<TagQuery>, Box<TagQuery>), // AND/OR/NOT combo
}

enum Traversal {
    DerivedFrom { depth: u8 },      // Follow derivation edges
    References { depth: u8 },       // Follow reference edges
    Session { types: Vec<SessionEdgeType> },  // Viewed/Edited/Selected
    Collaborators { hops: u8 },     // People connected via objects
}

enum RankStrategy {
    Recency,           // Modified time
    Relevance,         // Vector similarity to query embedding
    Frequency,         // How often accessed
    Hybrid { weights: HashMap<RankSignal, f32> },
}
```

---

## Materialization Engine

```
┌─────────────────────────────────────────────────────────────────┐
│ WORKSPACE MATERIALIZER (P5 background service)                  │
│                                                                 │
│  Input: Workspace.query                                         │
│                                                                 │
│  1. EXECUTE QUERY                                               │
│     └─► P2 Graph: traverse from roots                          │
│     └─► P3 Semantic Index: vector search if needed             │
│     └─► Fuse results                                            │
│                                                                 │
│  2. ENRICH                                                      │
│     └─► Resolve app associations (which app opens what)        │
│     └─► Fetch thumbnails/previews (P2 objects)                 │
│     └─► Compute collaborator presence                          │
│     └─► Check device availability (mesh)                       │
│                                                                 │
│  3. MATERIALIZE                                                 │
│     └─► Write WorkspaceView object to P2                       │
│     └─► Update workspace.accessed_at                           │
│     └─► Notify compositor (P5)                                 │
│                                                                 │
│  4. INCREMENTAL UPDATE (on graph change)                       │
│     └─► Subscribe to relevant graph edges                      │
│     └─► On edge add/remove: recompute affected subset          │
│     └─► Update WorkspaceView                                   │
└─────────────────────────────────────────────────────────────────┘
```

---

## WorkspaceView (Materialized Output)

```rust
struct WorkspaceView {
    workspace_id: WorkspaceID,
    version: u64,                 // Incremented on each materialization
    objects: Vec<WorkspaceObject>,
    apps: Vec<WorkspaceApp>,
    people: Vec<WorkspacePerson>,
    devices: Vec<WorkspaceDevice>,
    generated_at: Timestamp,
    query_hash: Hash,             // Detect query changes
}

struct WorkspaceObject {
    object_id: ObjectID,
    score: f32,                   // Rank score
    thumbnail: Option<ObjectID>,  // Preview image
    open_with: Vec<AppID>,        // Apps that can open
    last_accessed: Timestamp,
    tags: Vec<TagID>,
    project: Option<ProjectID>,
}

struct WorkspaceApp {
    app_id: AppID,
    object_count: usize,          // How many objects it can open
    last_used: Timestamp,
}

struct WorkspacePerson {
    user_id: UserID,
    object_count: usize,
    last_interaction: Timestamp,
    presence: PresenceStatus,     // Online/Active/Idle/Offline (mesh)
}

struct WorkspaceDevice {
    device_id: DeviceID,
    object_count: usize,          // Objects cached locally
    sync_status: SyncStatus,      // Synced / Syncing / Offline
}
```

---

## Workspace Lifecycle

```
CREATE
   │
   ▼
MATERIALIZE (initial)
   │
   ▼
OPEN ──────► User interacts ──────► Session edges added (Viewed, Edited)
   │                                    │
   │                                    ▼
   │                          INCREMENTAL UPDATE
   │                                    │
   ▼                                    ▼
REFRESH (manual/scheduled) ◄────── GRAPH CHANGE NOTIFICATION
   │
   ▼
SHARE (optional) ──► Collaborator receives workspace + query
   │
   ▼
ARCHIVE ──► Query preserved, materialization paused
```

---

## Cross-Device Workspace Sync

| Aspect | Mechanism |
|--------|-----------|
| **Query sync** | Workspace object synced via mesh (P2 sync protocol) |
| **Materialization** | Each device materializes locally (uses local P2/P3) |
| **View sync** | WorkspaceView NOT synced — recomputed per device |
| **Pin/hide** | User preferences synced as separate objects |
| **Collaboration** | Shared workspace = same query + shared graph scope |

---

## Default Workspaces (System-Provided)

| Workspace | Query |
|-----------|-------|
| **Today** | `TimeRange = today` + `Session.Edited` |
| **This Week** | `TimeRange = 7 days` |
| **Inbox** | `Tag = inbox` OR `Provenance = import` |
| **Shared With Me** | `Session.Collaborators` depth=1 |
| **Projects** | Group by `Project` edge, show all |
| **Code** | `MimeType ∈ [code/*]` + `Project = work` |
| **Design** | `MimeType ∈ [image/*, application/figma]` |
| **Research** | `Tag = research` OR `Provenance = web-clip` |

---

## Open Questions

See [`open/open-questions.md#workspace-model`](../open/open-questions.md#workspace-model).