# AI — Semantic Index (Detailed Implementation)

**Feasibility:** 🟢 BUILDABLE — vector search + graph metadata proven  
**Dependencies:** P2 object model, graph schema, index contract, P3 NRM

---

## Index Structure (Recap)

```
ObjectID ↔ VectorID (1:1 for indexed objects)
       │
       ├── Vector Store (HNSW on P2)
       │     └── Sharded by Project + Time
       │
       └── Graph Metadata (P2)
             ├── Tags, Projects, Provenance
             ├── Session edges (Viewed, Edited, Selected)
             └── Derivation edges (DerivedFrom, GeneratedBy)
```

---

## Embedding Models

| Model | Dims | Quant | Use Case | Latency (NPU) |
|-------|------|-------|----------|---------------|
| **BGE-small-en** | 384 | INT8 | Primary (English) | ~3 ms |
| **mE5-small** | 384 | INT8 | Multilingual | ~4 ms |
| **CodeBERT** | 768 | Q4_K_M | Code search | ~8 ms |
| **MiniLM-L6** | 384 | INT8 | Fallback / low-power | ~2 ms |

**Model selection:** NRM picks based on query language + content type (code vs prose).

---

## Ingest Pipeline (Detailed)

### Per-Source Extractors

| Source | Extractor | Output |
|--------|-----------|--------|
| **Text files** | Tree-sitter + regex | Chunks + symbols + imports |
| **Code** | Tree-sitter (per language) | Functions, classes, types, docs |
| **PDF/Office** | pdf-rs / apache-poi | Text + structure (headings, tables) |
| **Images** | CLIP / SigLIP (opt-in) | Visual embeddings |
| **Audio** | Whisper-tiny (opt-in) | Transcript + timestamps |
| **App exports** | App-defined schema | Entities + relationships |
| **Screen (opt-in)** | OCR + layout analysis | Text blocks + bounding boxes |

### Chunking Strategy

```rust
struct ChunkingPolicy {
    // Semantic chunking (preferred)
    semantic: SemanticChunker {   // By paragraph, section, function
        max_tokens: 512,
        overlap_tokens: 50,
        respect_boundaries: true, // Don't split mid-sentence/function
    },
    // Fallback: fixed-size
    fixed: FixedChunker {
        size_tokens: 256,
        overlap_tokens: 32,
    },
}
```

### Ingest Transaction

```rust
struct IngestTransaction {
    source_object: ObjectID,
    chunks: Vec<ChunkRecord>,
    vector_records: Vec<VectorRecord>,
    graph_edges: Vec<Edge>,       // Tagged, InProject, DerivedFrom, etc.
}

struct ChunkRecord {
    chunk_id: ChunkID,            // = ObjectID of chunk object
    source_object: ObjectID,
    text: String,                 // Plaintext (for snippet)
    token_range: (u32, u32),      // Position in source
    metadata: ChunkMetadata,      // Section title, function name, etc.
}
```

**Atomic:** All chunks + vectors + edges committed in single P2 transaction.

---

## Vector Store (HNSW on P2)

### Shard Design

```
Shard Key: (ProjectID, TimeBucket)  // TimeBucket = day

Shard_ProjectWork_2026-01-15  →  HNSW index + VectorRecord objects
Shard_ProjectWork_2026-01-16  →  HNSW index + VectorRecord objects
Shard_ProjectPersonal_...     →  ...
Shard_All_...                 →  Global fallback
```

### HNSW Parameters

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `M` | 16 | Balance recall / memory |
| `ef_construction` | 200 | High recall at build |
| `ef_search` | 50 | Fast query; user can increase |
| `max_elements_per_shard` | 100,000 | Auto-split when exceeded |

### VectorRecord (P2 Object)

```rust
struct VectorRecord {
    vector_id: ObjectID,           // = source ObjectID
    embedding: Vec<u8>,            // INT8 quantized (384 bytes)
    model_id: ModelID,             // Which embedder
    quantization: QuantizationSpec,
    updated_at: Timestamp,
    // Graph linkage via P2 edges (not stored here)
}
```

---

## Query Execution (Detailed)

### Planner → Execution Plan

```rust
struct ExecutionPlan {
    vector_search: Option<VectorSearchPlan>,
    graph_traversal: Option<GraphTraversalPlan>,
    fusion: FusionStrategy,
    capability_filter: CapabilityScope,
}

struct VectorSearchPlan {
    shards: Vec<ShardID>,
    query_vector: Vec<f32>,        // Dequantized at query time
    k: usize,                      // 50–100
    filter: Option<VectorFilter>,  // e.g., "only Project X"
}

struct GraphTraversalPlan {
    roots: Vec<ObjectID>,
    traversals: Vec<Traversal>,
    filters: Vec<Filter>,
}
```

### Fusion Strategies

| Strategy | When Used | How |
|----------|-----------|-----|
| **VectorFirst** | General search | Vector k=100 → Graph rerank (boost tags/projects/recency) |
| **GraphFirst** | Known project/tag | Graph traversal → embed results → vector rerank |
| **Fused** | Complex queries | Parallel vector + graph → reciprocal rank fusion |

---

## Real-Time Updates

| Event | Index Action |
|-------|--------------|
| Object created | Ingest transaction (async, background) |
| Object modified | Re-ingest changed chunks only |
| Object deleted | Mark vectors tombstoned; GC later |
| Tag added | Add `Tagged` edge; boost in rerank |
| Project added | Add `InProject` edge; new shard membership |
| Viewed/Edited | Add session edges; boost in rerank |

**Latency target:** User action → searchable in < 2s (interactive), < 30s (background).

---

## Privacy Controls

| Control | Mechanism |
|---------|-----------|
| **Per-source opt-in** | Screen/audio require explicit `Cap::ScreenRead` / `Cap::AudioStream` grant |
| **Per-project exclude** | `project:secret` → no indexing (graph filter) |
| **Tag-based exclude** | `tag:private` → filtered at query time |
| **Temporal exclude** | "Don't index last hour" → time filter |
| **Full pause** | `axiom index pause` → NRM stops ingest worker |

---

## Open Questions

See [`open/open-questions.md#semantic-index`](../open/open-questions.md#semantic-index).