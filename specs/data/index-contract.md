# Data — Semantic Index Contract

**Feasibility:** 🟢 BUILDABLE — vector search + graph-backed metadata proven  
**Dependencies:** P2 object model, graph schema, P3 NRM (embedder)

---

## Index Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ SEMANTIC INDEX (P3 service, data in P2)                                     │
│                                                                             │
│  ┌─────────────┐    ┌─────────────┐    ┌────────────────────┐              │
│  │  INGEST     │───►│  EMBEDDING  │───►│  VECTOR STORE      │              │
│  │  PIPELINE   │    │  WORKER     │    │  (HNSW on P2)      │              │
│  └─────────────┘    └─────────────┘    └────────────────────┘              │
│         │                   │                    │                          │
│         ▼                   ▼                    ▼                          │
│  ┌──────────────────────────────────────────────────────────────────┐       │
│  │              GRAPH-BACKED METADATA (P2)                          │       │
│  │  ObjectID ↔ VectorID ↔ Tags, Projects, Provenance, Time, Session │       │
│  └──────────────────────────────────────────────────────────────────┘       │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Vector Store (HNSW on P2)

```rust
struct VectorIndex {
    // HNSW parameters
    m: usize,                    // 16–32 (connectivity)
    ef_construction: usize,      // 100–200
    ef_search: usize,            // 50–100 (query-time)
    
    // Storage
    vectors: Vec<VectorRecord>,  // P2 objects, type=IndexShard
    graph: HNSWGraph,            // In-memory during search; persisted to P2
}

struct VectorRecord {
    vector_id: VectorID,         // = ObjectID of source object
    embedding: Vec<f32>,         // 384–1024 dims (model-dependent)
    model_id: ModelID,           // Which embedder produced this
    updated_at: Timestamp,
}
```

**Sharding:** By `ProjectID` + time buckets (daily). Each shard = separate HNSW index + P2 object.

---

## Ingest Pipeline

### Sources & Capabilities

| Source | Capability | Ingest Method |
|--------|------------|---------------|
| **Files (P2)** | `Cap::GraphQuery(scope)` | Extract text + structured metadata → embed |
| **App exports** | `Cap::AppDataExport(app_id)` | App pushes entities (title, body, entities, timestamps) |
| **Screen (opt-in)** | `Cap::ScreenRead(region)` | OCR + layout → structured → embed |
| **Audio (opt-in)** | `Cap::AudioStream(session)` | ASR → text → embed |
| **Session history** | `Cap::SessionRead` | Viewed/Edited/Selected edges → contextual embed |

### Ingest Flow

```
Source Event
      │
      ▼
┌─────────────────────┐
│  CONTENT EXTRACTOR  │  (per source type)
│  • Text extraction  │
│  • Entity recognition│
│  • Structure parse  │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│  CHUNKER            │  (semantic chunking: paragraphs, sections, functions)
│  • Size: 256–512 tok│
│  • Overlap: 50 tok  │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│  EMBEDDER (NRM)     │  Cap::ModelHandle(embedder)
│  • Batch: 32–64     │
│  • Priority: Bg     │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│  INDEX WRITER       │  P2 transaction: VectorRecord + Graph edges
│  • Upsert vector    │
│  • Link: VectorID ↔ ObjectID + metadata edges
└─────────────────────┘
```

### Ingest Scheduling

- **Foreground:** User explicitly indexes → Interactive priority
- **Background:** Periodic scan of changed objects → Background priority, batched
- **Incremental:** Only re-embed objects with `updated_at > last_indexed_at`
- **Budget:** Max 5% CPU, 10% NPU background; pauses on thermal throttle

---

## Hybrid Query Engine

```rust
struct SemanticQuery {
    natural_language: String,
    parsed: ParsedQuery,
    scope: CapabilityScope,
    strategy: HybridStrategy,
    limit: u32,
}

struct ParsedQuery {
    intent: QueryIntent,          // Find, Summarize, Compare, Navigate
    entities: Vec<Entity>,        // Extracted: "Q3", "revenue", "chart"
    time_range: Option<TimeRange>,
    filters: Vec<MetadataFilter>, // tag, project, mime, provenance
}

enum HybridStrategy {
    VectorFirst { k: 50, rerank: GraphRerank },
    GraphFirst { traversal: GraphTraversal, embed: bool },
    Fused { vector_weight: f32, graph_weight: f32 },
}

struct GraphRerank {
    boost_tags: Vec<TagID>,
    boost_projects: Vec<ProjectID>,
    boost_recent: Duration,
    boost_viewed: bool,           // User viewed recently
}
```

### Query Execution

```
Natural Language Query
         │
         ▼
┌─────────────────────┐
│  QUERY PLANNER (P3) │  Local LLM (small) → ParsedQuery + Strategy
└─────────┬───────────┘
          │
          ├──────────────────────┐
          ▼                      ▼
┌─────────────────┐    ┌─────────────────┐
│  VECTOR SEARCH  │    │  GRAPH TRAVERSAL│
│  (HNSW)         │    │  (P2)           │
│  k=50–100       │    │  Project/Tag/   │
│  filter: scope  │    │  Time/Provenance│
└────────┬────────┘    └────────┬────────┘
         │                      │
         └──────────┬───────────┘
                    ▼
         ┌─────────────────────┐
         │   FUSION & RERANK   │
         │  • Score fusion     │
         │  • Graph boost      │
         │  • Dedupe           │
         │  • Capability filter│
         └─────────┬───────────┘
                   │
                   ▼
         ┌─────────────────────┐
         │   RESULTS           │
         │  ObjectID + snippet │
         │  + Cap::ObjectRead  │
         └─────────────────────┘
```

---

## Capability-Gated Results

**Every result includes a capability grant:**

```rust
struct SearchResult {
    object_id: ObjectID,
    score: f32,
    snippet: String,
    highlights: Vec<Highlight>,
    provenance: ProvenanceSummary,
    read_cap: Cap::ObjectRead,      // Scoped to this object
}
```

User can only see results they have `Cap::ObjectRead` for — enforced at query time.

---

## Privacy & Encryption

- **Index shards encrypted** under user's master key (per-project keys optional)
- **Embeddings never leave device** — no cloud index
- **Query planner runs locally** — natural language never sent out
- **Opt-in sources only** — screen/audio require explicit per-source consent

---

## Model Management

| Model Role | Model | Dims | Quantization | Update |
|------------|-------|------|--------------|--------|
| **Primary embedder** | BGE-small / E5-small | 384 | INT8 | OS update |
| **Multilingual** | mE5-small | 384 | INT8 | OS update |
| **Code-specialized** | CodeBERT / UniXcoder | 768 | Q4_K_M | App layer |
| **Fallback** | MiniLM-L6 | 384 | INT8 | Always available |

**Re-indexing on model update:** Background job; old vectors retained until new complete; atomic swap per shard.

---

## Performance Targets

| Metric | Target |
|--------|--------|
| Ingest (1000 docs) | < 30s background |
| Query latency (p99) | < 200ms |
| Index size (1M docs) | ~2–4 GB (INT8 vectors + graph) |
| Memory (search) | < 500 MB |
| Re-index (model update) | < 2h background |

---

## Open Questions

See [`open/open-questions.md#index-contract`](../open/open-questions.md#index-contract).