# AI — Query Planner (NL → Graph + Vector)

**Feasibility:** 🟡 HARD BUT KNOWN — small local LLMs for planning proven (Phi-3, Gemma, Llama-3.2)  
**Dependencies:** P3 NRM, semantic index, P2 graph

---

## Planner Role

Translate natural language → `ParsedQuery` + `ExecutionPlan` using a **small local LLM** (Phi-3-mini / Gemma-2B) running on NRM.

---

## Planner Model

```rust
struct PlannerModel {
    model_id: ModelID,              // System layer: phi-3-mini-planner
    prompt_template: String,        // Few-shot + schema
    max_tokens: 512,
    temperature: 0.0,               // Deterministic
    quantization: "Q4_K_M",
}
```

### Prompt Structure

```
You are a query planner for the Axiom OS semantic index.
Convert the user's natural language query into a structured ParsedQuery.

SCHEMA:
- Intent: Find | Summarize | Compare | Navigate
- Entities: [{type: Project|Tag|Person|Date|FileType|Model, value: string, confidence: float}]
- TimeRange: {since: ISO8601, until: ISO8601} | null
- Filters: [{field: tag|project|mime|provenance|author, op: eq|contains|gt|lt, value: string}]
- Strategy: VectorFirst | GraphFirst | Fused

EXAMPLES:
User: "Find the Q3 revenue chart I looked at last Tuesday"
→ Intent: Find
  Entities: [{Project, "Q3", 0.9}, {Tag, "chart", 0.8}, {Tag, "revenue", 0.7}]
  TimeRange: {since: "2026-01-13", until: "2026-01-14"}
  Filters: []
  Strategy: VectorFirst

User: "Summarize all finance docs from the work project this week"
→ Intent: Summarize
  Entities: [{Project, "work", 0.95}, {Tag, "finance", 0.9}]
  TimeRange: {since: "2026-01-12", until: "2026-01-19"}
  Filters: []
  Strategy: GraphFirst

OUTPUT FORMAT: JSON matching ParsedQuery schema exactly.
```

---

## ParsedQuery Schema

```rust
struct ParsedQuery {
    intent: QueryIntent,
    entities: Vec<Entity>,
    time_range: Option<TimeRange>,
    filters: Vec<MetadataFilter>,
    strategy: HybridStrategy,
    confidence: f32,          // Planner's confidence in parse
    fallback: Option<String>, // If confidence < 0.7, use keyword fallback
}

enum QueryIntent {
    Find,          // Return ranked objects
    Summarize,     // Return summary + sources
    Compare,       // Return diff / comparison
    Navigate,      // Open specific object / workspace
}

struct Entity {
    type: EntityType,
    value: String,
    confidence: f32,
}

enum EntityType {
    Project, Tag, Person, Date, FileType, Model, App, Device,
}

struct TimeRange {
    since: Timestamp,
    until: Timestamp,
}
```

---

## Execution Plan Generation

```rust
fn plan(parsed: ParsedQuery, scope: CapabilityScope) -> ExecutionPlan {
    // 1. Resolve entities → concrete IDs
    let project_ids = resolve_projects(parsed.entities);
    let tag_ids = resolve_tags(parsed.entities);
    
    // 2. Build vector search (if needed)
    let vector_plan = if needs_vector(parsed) {
        Some(VectorSearchPlan {
            shards: select_shards(project_ids, tag_ids, parsed.time_range),
            query_vector: embed_query(parsed),  // Use planner's embedder
            k: 100,
            filter: build_vector_filter(parsed, scope),
        })
    } else { None };
    
    // 3. Build graph traversal (if needed)
    let graph_plan = if needs_graph(parsed) {
        Some(GraphTraversalPlan {
            roots: project_ids.into_iter().map(QueryRoot::Project).collect(),
            traversals: build_traversals(parsed),
            filters: build_graph_filters(parsed, scope),
        })
    } else { None };
    
    // 4. Fusion strategy
    let fusion = select_fusion(parsed, vector_plan.is_some(), graph_plan.is_some());
    
    ExecutionPlan { vector_plan, graph_plan, fusion, capability_filter: scope }
}
```

---

## Fallback Chain

```
NL Query
    │
    ▼
┌─────────────────────┐
│ PLANNER (Phi-3)     │  → ParsedQuery (confidence ≥ 0.7)
└─────────┬───────────┘
          │
          ├─ confidence ≥ 0.9 → Execute plan
          │
          ├─ 0.7–0.9 → Execute plan + log for review
          │
          └─ confidence < 0.7 → KEYWORD FALLBACK
                                    │
                                    ▼
                         ┌─────────────────────┐
                         │ KEYWORD SEARCH      │  (P2 fulltext index)
                         │  • Tokenize query   │
                         │  • BM25 on metadata │
                         │  • Graph boost      │
                         └─────────────────────┘
```

---

## Continuous Improvement

| Signal | Action |
|--------|--------|
| User clicks result #1 | Positive reward for that parse |
| User reformulates query | Negative reward; log for retraining |
| User uses "Show me..." after failed search | Capture as training pair |
| Explicit "This wasn't what I wanted" | High-weight negative example |

**Local learning:** Planner model fine-tuned nightly on-device (NRM background) using user's interaction history (encrypted, never leaves device).

---

## Open Questions

See [`open/open-questions.md#planner`](../open/open-questions.md#planner).