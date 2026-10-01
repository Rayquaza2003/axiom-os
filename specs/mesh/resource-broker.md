# Mesh — Resource Brokerage (Async NPU Offload)

**Feasibility:** 🟡 HARD BUT KNOWN — batch offload to peer NPU proven; real-time not feasible  
**Dependencies:** P3 NRM, P4 mesh tunnels, capability model

---

## What This Is (And Isn't)

| Supported | Not Supported |
|-----------|---------------|
| ✅ Batch inference (indexing, training, rendering) | ❌ Real-time inference (voice, autocomplete) |
| ✅ Background summarization | ❌ Interactive LLM chat |
| ✅ Video transcoding / ML preprocessing | ❌ < 100ms latency tasks |
| ✅ Overnight fine-tuning | ❌ Live collaboration on model weights |

**Rule:** If user waits for result → local only. If result can arrive later → mesh offload eligible.

---

## Broker Architecture

```
Client (requester)                          Peer (provider)
     │                                           │
     │  1. NRM: request_slice(task, batch)       │
     │     ───────────────────────────────────►  │
     │                                           │  2. Peer NRM: accepts?
     │                                           │     (has capacity + cap)
     │  3. SliceGrant { peer_id, task_id }       │
     │     ◄───────────────────────────────────  │
     │                                           │
     │  4. Upload inputs (P2 objects)            │
     │     ───────────────────────────────────►  │
     │                                           │
     │  5. Peer runs inference                   │
     │     (NRM schedules on peer NPU)           │
     │                                           │
     │  6. Results as P2 objects                 │
     │     ◄───────────────────────────────────  │
     │                                           │
     │  7. Client notified (completion event)    │
```

---

## Capability Gating

```rust
struct OffloadCapability {
    // Granted by peer device owner
    max_batch_tokens: u32,          // e.g., 1M tokens/day
    allowed_models: Vec<ModelID>,   // e.g., [llama-3.2-1B, whisper-tiny]
    allowed_tasks: TaskType[],      // Embed, Summarize, Transcribe, Classify
    priority: OffloadPriority,      // Background only
    expiry: Timestamp,
}

// Default: No offload caps granted. User explicitly enables per peer.
```

**Peer-side policy:** User sets "allow offload when: charging + Wi-Fi + >50% battery + idle > 5 min".

---

## NRM Integration

### Request Side (Client)

```rust
impl NRM {
    fn request_offload(&self, req: OffloadRequest) -> Result<OffloadGrant, Error> {
        // 1. Check local NRM queue first (prefer local)
        if self.can_run_locally(&req) && !req.force_offload {
            return self.request_slice_local(req);
        }
        
        // 2. Query mesh for eligible peers
        let peers = self.mesh.query_peers(PeerFilter {
            caps: req.required_caps,
            min_npu_tops: req.min_npu_tops,
            max_latency_ms: req.max_latency_ms,  // For batch, can be high
        })?;
        
        // 3. Select best peer (latency × capacity × trust)
        let peer = self.select_peer(peers)?;
        
        // 4. Negotiate slice on peer
        let grant = peer.nrm.negotiate_slice(req)?;
        
        // 5. Return grant with peer info
        Ok(OffloadGrant {
            peer_id: peer.id,
            slice_id: grant.slice_id,
            estimated_completion: grant.eta,
        })
    }
}
```

### Provider Side (Peer)

```rust
impl PeerNRM {
    fn handle_offload_request(&self, req: OffloadRequest, from: DeviceID) -> Result<SliceGrant, Error> {
        // 1. Verify capability grant exists
        self.caps.verify(Cap::OffloadReceive { from, task: req.task_type })?;
        
        // 2. Check local policy (charging, Wi-Fi, battery, idle)
        if !self.policy.allows_offload_now() {
            return Err(Error::PolicyDenied);
        }
        
        // 3. Reserve NPU slice (background priority)
        let slice = self.schedule_slice(SliceRequest {
            model: req.model,
            priority: Priority::Background,
            estimated_tokens: req.estimated_tokens,
            deadline: req.deadline,  // Can be hours
        })?;
        
        // 4. Return grant
        Ok(slice)
    }
}
```

---

## Data Transfer

### Input/Output as P2 Objects

```
Client P2                          Peer P2
   │                                  │
   │  Objects: input_chunks[]         │
   │  (content-addressed, deduped)    │
   │────────────────────────────────►│
   │                                  │
   │  Objects: results[]              │
   │◄────────────────────────────────│
```

**Deduplication:** If peer already has input objects (synced via mesh), zero transfer.

### Progress & Cancellation

- Client receives `ProgressEvent { completed_chunks, total_chunks }` via mesh
- Client can `cancel(offload_id)` → peer NRM preempts slice
- Peer can `yield()` if policy changes (unplugged, user needs NPU)

---

## Scheduling Policy

| Factor | Weight |
|--------|--------|
| Local NPU available | 100 (prefer local) |
| Peer NPU idle > 30 min | 50 |
| Peer on charger + Wi-Fi | 30 |
| Peer has model cached | 20 |
| Latency (LAN vs WAN) | -10 per hop |

**Preemption:** Local Realtime/Interactive always preempts peer Background.

---

## Economic Model (Optional)

```rust
struct OffloadCredit {
    // Earned by providing offload compute
    // Spent when requesting offload
    balance: i64,           // Token-seconds
    earned_today: u64,
    spent_today: u64,
}
```

**Not required for v1.** Simple reciprocal: "I let you use mine, you let me use yours."

---

## Open Questions

See [`open/open-questions.md#resource-broker`](../open/open-questions.md#resource-broker).