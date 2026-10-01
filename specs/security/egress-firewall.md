# Security — Egress Firewall (AI Privacy Boundary)

**Feasibility:** 🟢 BUILDABLE — network filtering, DPI, capability gating proven  
**Dependencies:** P0 kernel, P3 NRM, P4 mesh

---

## Design Goal

**No model context leaves the device without explicit, per-request user approval.** A single system-level classifier inspects every outbound network call from any P3 component and drops any carrying model context unless a one-time user grant exists.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ EGRESS PATH                                                                 │
│                                                                             │
│  Any P3 Component                                                           │
│  (NRM, Index, Model Runtime, App via Cap::ModelHandle)                     │
│         │                                                                   │
│         ▼                                                                   │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │ KERNEL NETWORK STACK (P1)                                           │    │
│  │  • Outbound packet → EGRESS CLASSIFIER (kernel module)              │    │
│  │  • Classifier decision: ALLOW / DROP / REDIRECT_TO_APPROVAL         │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│         │                                                                   │
│         ├─ ALLOW ──────────────────► Normal routing (mesh tunnel / internet)│
│         │                                                                   │
│         ├─ DROP ───────────────────► Silent drop; audit log entry          │
│         │                                                                   │
│         └─ REDIRECT ──────────────► Approval UI (P5 session)               │
│                                           │                                 │
│                                           ▼                                 │
│                                  User: Approve / Deny                        │
│                                           │                                 │
│                                           ▼                                 │
│                                  Cap::EgressGrant issued                    │
│                                           │                                 │
│                                           ▼                                 │
│                                  Retry original call (now ALLOWed)          │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Classifier Design

### What It Inspects

| Signal | Detection Method |
|--------|------------------|
| **Embedding vectors** | Float32 arrays with characteristic norms (384–1024 dims) |
| **Model weights / LoRA** | Known weight tensor shapes, quantization patterns |
| **Prompt / context window** | Text with instruction patterns, conversation structure |
| **Semantic index shards** | HNSW graph structure + vector batches |
| **NRM protocol messages** | Known NRM wire format (inference requests, model loads) |

### Classification Rules

```rust
struct ClassifierDecision {
    // High-confidence model context indicators
    has_embedding_vectors: bool,
    has_model_weights: bool,
    has_prompt_structure: bool,
    has_index_shard: bool,
    has_nrm_protocol: bool,
    
    // Context
    source_process: ProcessID,
    destination: SocketAddr,
    payload_size: usize,
    capabilities_held: CapabilitySet,
}

fn classify(decision: ClassifierDecision) -> Verdict {
    // If any model-context indicator + no approval grant → DROP
    if (decision.has_embedding_vectors 
        || decision.has_model_weights
        || decision.has_prompt_structure
        || decision.has_index_shard
        || decision.has_nrm_protocol) {
        
        if !has_valid_egress_grant(decision.source_process, decision.destination) {
            return Verdict::Drop { reason: "Model context without approval" };
        }
    }
    
    // Otherwise allow
    Verdict::Allow
}
```

**False positive tolerance:** Near zero. Classifier errs on side of dropping — user can always approve.

---

## Approval Flow

### User Experience

```
┌─────────────────────────────────────────────────────────────────┐
│ ⚠️  App "Code Assistant" wants to send 12.3 KB to              │
│     remote model "claude-3.5-sonnet"                           │
│                                                                 │
│     This includes:                                             │
│     • Current file context (2.1 KB)                            │
│     • Recent edit history (450 bytes)                          │
│     • Project structure summary (1.2 KB)                       │
│                                                                 │
│     Data will be encrypted under your keys.                    │
│                                                                 │
│     [Deny]                    [Allow Once]    [Allow Session]  │
└─────────────────────────────────────────────────────────────────┘
```

### Grant Structure

```rust
struct EgressGrant {
    grant_id: GrantID,
    source_process: ProcessID,
    destination: DestinationSpec,      // Exact host:port or model endpoint
    max_bytes: usize,                  // Exact payload size approved
    model_context: bool,               // True = this grant covers model ctx
    ttl: Duration,                     // Max 1 hour (session = 4 hours)
    issued_at: Timestamp,
    user_decision: UserDecision,       // AllowOnce | AllowSession
}

struct DestinationSpec {
    // Either exact endpoint or model registry entry
    endpoint: Option<SocketAddr>,
    model_id: Option<ModelID>,         // If approved via model registry
}
```

### Grant Enforcement

- **One-time:** Single packet matching `destination` + `max_bytes` + `model_context`
- **Session:** Multiple packets to same `model_id` within `ttl`
- **Audit log:** Every grant + use logged to user's private audit log (P2)

---

## Mesh Relay Integration

When approved egress goes to a remote model:

```
Approved Packet
       │
       ▼
┌─────────────────────────────────────────────────────────────────┐
│ ENCRYPTION LAYER (P4 Mesh)                                      │
│                                                                 │
│  1. Encrypt payload with session key (Noise)                   │
│  2. Wrap session key with USER's public key (not relay's)      │
│  3. Route via mesh tunnel to relay                             │
│                                                                 │
│  Relay sees:                                                    │
│  • Encrypted frame (Noise)                                     │
│  • Next-hop relay address                                      │
│  • NO payload, NO model identity, NO user identity             │
└─────────────────────────────────────────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────────────────────────────────┐
│ REMOTE MODEL ENDPOINT                                           │
│                                                                 │
│  • Receives ciphertext                                         │
│  • Decrypts with user's private key (held in secure enclave)   │
│  • Processes request                                           │
│  • Returns encrypted response                                  │
└─────────────────────────────────────────────────────────────────┘
```

**Key property:** The relay **cannot** decrypt. The remote model endpoint **must** have user's public key (provisioned at model registration).

---

## Audit Log

```rust
struct EgressAuditEntry {
    timestamp: Timestamp,
    source_process: ProcessID,
    destination: SocketAddr,
    payload_bytes: usize,
    classifier_verdict: Verdict,
    grant_used: Option<GrantID>,
    user_decision: Option<UserDecision>,
    // Payload hash (for verification) — NOT the payload itself
    payload_hash: Blake3Hash,
}
```

**User accessible:** Settings → Privacy → Egress Audit Log (capability-gated).

---

## Bypass Prevention

| Bypass Attempt | Prevention |
|----------------|------------|
| App uses non-P3 network stack | All network via P1 stack; classifier at kernel level |
| App encodes model context in DNS | Classifier inspects all L7 payloads |
| App uses mesh tunnel to peer then egress | Peer's classifier also enforces |
| App exfiltrates via timing/covert channel | Rate limiting; noise injection; not perfect but raises bar |
| Compromised P1 network stack | P1 measured boot; classifier in separate kernel module |

---

## Open Questions

See [`open/open-questions.md#egress-firewall`](../open/open-questions.md#egress-firewall).