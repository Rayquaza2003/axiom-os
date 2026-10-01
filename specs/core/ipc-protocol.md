# Core — IPC Protocol

**Feasibility:** 🟢 BUILDABLE — seL4, Fuchsia, QNX IPC proven  
**Dependencies:** P0 kernel, capability model

---

## Two-Path IPC Design

| Path | Use Case | Mechanism | Latency |
|------|----------|-----------|---------|
| **Control** | Capability invocation, small messages, sync | Copy-based, kernel-mediated | ~500–2000 cycles |
| **Data** | Bulk transfer (frames, tensors, audio, index shards) | Shared-memory windows + notifications | Zero-copy |

---

## Control Path: Capability Invocation

### Message Format

```
┌─────────────────────────────────────────────────────────────────┐
│ IPC MESSAGE (Control Path)                                      │
├─────────────────────────────────────────────────────────────────┤
│ Header (32 bytes)                                               │
│  ┌─────────┬─────────┬─────────┬─────────┬─────────────────┐   │
│  │ msg_id  │  type   │  flags  │ cap_cnt │  reserved       │   │
│  │  (u64)  │  (u16)  │  (u16)  │  (u8)   │  (22 bytes)     │   │
│  └─────────┴─────────┴─────────┴─────────┴─────────────────┘   │
├─────────────────────────────────────────────────────────────────┤
│ Payload (inline, max 256 bytes)                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  method: u32  │  args: [u64; N]  │  ...                 │   │
│  └─────────────────────────────────────────────────────────┘   │
├─────────────────────────────────────────────────────────────────┤
│ Capability Slots (variable, max 8)                              │
│  [cap_slot_0, cap_slot_1, ...]  — kernel validates & transfers  │
└─────────────────────────────────────────────────────────────────┘
```

### Invocation Semantics

```rust
// Client side
fn invoke(endpoint: Cap::Endpoint, msg: Message, caps: &[Cap]) -> Result<Reply, Error> {
    // Kernel: validate caps, copy message, transfer caps, schedule server thread
    // Server: receive, process, reply (with optional caps)
    // Kernel: validate reply caps, copy reply, transfer caps, unblock client
}

// Server side
fn serve(endpoint: Cap::Endpoint) -> ! {
    loop {
        let (msg, caps) = recv(endpoint)?;
        let reply = match msg.method {
            OPEN => handle_open(msg, caps),
            READ => handle_read(msg, caps),
            INFER => handle_infer(msg, caps),
            _ => Error::UnknownMethod,
        };
        send_reply(msg.msg_id, reply)?;
    }
}
```

### Batching (Hot Path Optimization)

Multiple invocations coalesced into single kernel entry:

```rust
struct BatchedInvocation {
    endpoint: Cap::Endpoint,
    messages: Vec<Message>,    // max 16
    caps: Vec<Cap>,            // shared across batch
}
```

NRM uses this for scheduling N inference requests in one kernel crossing.

---

## Data Path: Shared-Memory Windows

### Window Creation

```rust
// Server creates window, grants Cap::Memory to client
fn create_window(size: usize, flags: MemoryFlags) -> (Cap::Memory, VirtualAddress) {
    // Kernel: allocates physical pages, maps into server + client (COW if needed)
    // Returns: client capability + server virtual address
}

// Client maps window
fn map_window(cap: Cap::Memory, offset: usize, len: usize) -> VirtualAddress {
    // Kernel: validates cap, maps into client address space
}
```

### Synchronization Primitives

| Primitive | Use Case | Mechanism |
|-----------|----------|-----------|
| **Seqlock** | High-frequency producer/consumer (frame buffers, sensor rings) | Writer increments seq; reader retries on odd/changed |
| **Notification** | Low-frequency events (frame ready, inference complete) | Kernel `notify(thread)` — wakes waiter |
| **Fence** | GPU/NPU completion (cross-device) | `Cap::Fence` — kernel waits on hardware signal |
| **Ring Buffer** | Audio, streaming sensor data | Seqlock + power-of-2 buffer; lock-free |

### Window Lifecycle

```
Server: create_window(4MB, READ_WRITE) → (cap, vaddr)
         │
         ├─► map into server address space
         │
         └─► send(cap) via control path to client
                    │
Client:   receive(cap) → map_window(cap, 0, 4MB) → vaddr
                    │
                    ├─► write data → seqlock.write_begin() ... write_end()
                    │
                    └─► notify(server_thread) when ready
                         │
Server:   receive notification → seqlock.read_retry() → process
```

---

## Endpoint Types

| Type | Direction | Use Case |
|------|-----------|----------|
| `Call` | Sync request/reply | Capability invocation, NRM scheduling |
| `Stream` | Async bidirectional | Mesh tunnels, audio pipes |
| `Event` | Async one-way (server→client) | Notifications, interrupts, sensor data |
| `Broadcast` | One-to-many | System events (workspace switch, power change) |

---

## Error Handling

| Error | Meaning | Recovery |
|-------|---------|----------|
| `BadCap` | Sender doesn't hold capability | Bug — fix client |
| `BadMessage` | Malformed message | Bug — fix client/server |
| `NoMemory` | Kernel OOM | Back off, retry |
| `ServerDead` | Endpoint closed | Reconnect via directory |
| `Preempted` | NRM slice reclaimed | Checkpoint, re-request |
| `Revoked` | Capability revoked | Request new grant |

---

## Performance Targets

| Metric | Target |
|--------|--------|
| Control path round-trip (same core) | < 1.5 μs |
| Control path round-trip (cross-core) | < 3 μs |
| Shared-memory map (4 KB) | < 500 ns |
| Notification delivery | < 500 ns |
| Batch invocation (16 calls) | < 2× single call |

---

## Open Questions

See [`open/open-questions.md#ipc-protocol`](../open/open-questions.md#ipc-protocol).