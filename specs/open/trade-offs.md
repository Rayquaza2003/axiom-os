# Trade-offs & Architectural Decisions — Axiom OS

**Status:** 📋 DOCUMENTED  
**Purpose:** Record why each major fork was chosen over alternatives. Future re-evaluation starts here.

---

## 1. Microkernel vs Monolithic

| Aspect | Microkernel (Chosen) | Monolithic |
|--------|---------------------|------------|
| **TCB size** | ~10K lines (P0) | ~20M lines |
| **Verification** | seL4/Zircon formally verified | Practically impossible |
| **Driver isolation** | Native (userspace) | Requires extra mechanisms |
| **IPC overhead** | ~200-500 ns/call | Near-zero (direct calls) |
| **Real-world examples** | seL4, QNX, Fuchsia | Linux, Windows NT, macOS XNU |

**Decision:** Microkernel. The security/verification gains outweigh IPC cost for an AI-first OS where model isolation is P0. Modern seL4/Zircon show IPC can be < 1μs.

**Revisit if:** Real-time audio/video paths prove IPC latency unacceptable. Mitigation: zero-copy shared memory caps for media.

---

## 2. Capability-based vs ACL

| Aspect | Capabilities (Chosen) | ACLs |
|--------|----------------------|------|
| **Delegation** | Natural (pass cap) | Complex (ACL edit) |
| **Least privilege** | Default | Requires discipline |
| **Revocation** | Explicit, surgical | Global ACL scan |
| **Confused deputy** | Prevented by design | Common |
| **Mental model** | "Keys" | "Permissions lists" |

**Decision:** Capabilities. Matches the "AI as first-class resource" model — you hand the model a capability to your calendar, not a blanket "calendar access" ACL entry.

---

## 3. Immutable Core + Separate Model Store vs Single Mutable Root

| Aspect | Immutable Core + Signed Model Store (Chosen) | Single Mutable Root |
|--------|----------------------------------------------|---------------------|
| **OS updates** | Atomic, verified, instant rollback | Complex, risky |
| **Model updates** | Independent cadence, signed, rollback | Bundled with OS |
| **Attack surface** | Core stays tiny; models verified separately | Large mutable base |
| **User customization** | Limited to model layer | Full but dangerous |

**Decision:** Immutable core (ostree/ZFS boot environments) + separate signed model registry. AI models change weekly; OS core changes quarterly. Decouple.

---

## 4. Default Sandbox (Deny) vs Default Allow

| Aspect | Default Deny (Chosen) | Default Allow |
|--------|----------------------|---------------|
| **New app risk** | Zero until granted | Full until restricted |
| **User friction** | Prompt on first need | Silent breakage later |
| **Malware containment** | Built-in | Requires detection |
| **Dev friction** | Manifest declares needs | Works until sandbox hit |

**Decision:** Default deny with capability manifest at install. Apps declare `requires: [Cap::Files, Cap::Camera, Cap::NPUSlice]` — user grants at install or first use.

---

## 5. Content-Addressed Objects + Relationship Graph vs Hierarchical FS

| Aspect | Object Graph (Chosen) | Hierarchical FS |
|--------|----------------------|-----------------|
| **Same file in multiple projects** | Native (multiple edges) | Symlinks / copies |
| **Derived-from tracking** | Native edge type | Lost |
| **Semantic search** | Vector + graph unified | External index |
| **Sync/conflict** | Object-level CRDT | File-level LWW |
| **Legacy compatibility** | Requires FUSE layer | Native |

**Decision:** Object graph. The "tag-based, relationship-graph filesystem" requirement demands it. FUSE layer for POSIX apps.

---

## 6. Local-First AI + Explicit Egress vs Cloud-First

| Aspect | Local-First (Chosen) | Cloud-First |
|--------|---------------------|-------------|
| **Privacy** | Data never leaves | Trust provider |
| **Latency** | < 50ms for small models | 100-500ms + network |
| **Cost** | User's hardware | Provider's hardware |
| **Model quality** | Limited by NPU | Frontier models |
| **Offline** | Fully functional | Broken |

**Decision:** Local-first. NPU handles embedder, classifier, ASR, planner (Phi-3-mini). Frontier models require explicit egress approval via firewall. User chooses per-task.

---

## 7. Egress Firewall at Kernel vs App Level

| Aspect | Kernel (Chosen) | App/User-space |
|--------|-----------------|----------------|
| **Bypass risk** | None (enforced on wire) | App can link around |
| **Context** | Sees full packet | Sees app intent |
| **Policy language** | Simple (allow/deny/inspect) | Rich (per-app rules) |
| **Performance** | ~50ns overhead | ~10μs + context switch |

**Decision:** Kernel-level classifier (Rust, verified) for model-context exfiltration. User-space policy daemon for complex rules. Defense-in-depth.

---

## 8. Semantic Index: Vector + Graph vs Pure Vector / Pure Graph

| Aspect | Hybrid Vector + Graph (Chosen) | Pure Vector | Pure Graph |
|--------|--------------------------------|-------------|------------|
| **"Find Q3 chart from Tuesday"** | Vector (content) + Graph (time/project) ✓ | Time filter weak | Content search weak |
| **"Summarize finance docs this week"** | Graph (project/tag) → Vector rerank ✓ | No project concept | No content similarity |
| **Implementation complexity** | Medium | Low | Medium |
| **Index size** | 2x (vectors + edges) | 1x | 1x |

**Decision:** Hybrid. Vector for content similarity, graph for structure (projects, tags, provenance, sessions). Fused at query time.

---

## 9. NRM: Single Scheduler vs Per-App NPU Allocation

| Aspect | Global NRM (Chosen) | Per-App Allocation |
|--------|---------------------|---------------------|
| **Fragmentation** | Packed efficiently | NPU idle when app idle |
| **Priority inversion** | Preempts fairly | Hard to enforce |
| **Multi-model** | Shared weight cache | Duplicate weights |
| **QoS guarantees** | Central policy | Best-effort |

**Decision:** Global NRM. NPU is scarce (1-2 per device). Central scheduler packs, preempts, shares weights. Apps request `Cap::NPUSlice` with priority.

---

## 10. Mesh: Full P2P vs Relay-Assisted

| Aspect | Full P2P | Relay-Assisted (Chosen) |
|--------|----------|------------------------|
| **NAT traversal** | Hole-punch (fragile) | Relay always works |
| **Latency (WAN)** | Direct (best) | +1 hop via relay |
| **Metadata** | Direct peer knows IP | Relay sees next-hop only |
| **Offline sync** | Requires both online | Relay stores + forwards |
| **Infrastructure** | None | User/community relays |

**Decision:** Relay-assisted with direct tunnels preferred. Relays carry only Noise ciphertext — no identity, no payload. User can self-host.

---

## 11. State Roaming: Full Snapshot vs Incremental Deltas

| Aspect | Full Snapshot (Chosen for v1) | Incremental Deltas |
|--------|-------------------------------|---------------------|
| **Complexity** | Simple (COW snapshot) | Complex (app cooperation) |
| **Fidelity** | Perfect (cursor, undo, history) | App-dependent |
| **Size** | ~100-500 MB | ~1-10 MB |
| **Roam time (LAN)** | 2-5 s | < 500 ms |
| **Roam time (WAN)** | 30-120 s | 5-15 s |

**Decision:** Full COW snapshot for v1 (instant correctness). Deltas as opt-in API for apps that need fast roam. Wi-Fi Direct / LAN = acceptable.

---

## 12. Sync Protocol: CRDT vs Vector Clocks vs Central Server

| Aspect | CRDT (Chosen) | Vector Clocks | Central Server |
|--------|---------------|---------------|----------------|
| **Conflict resolution** | Automatic merge | Manual / LWW | Server decides |
| **Offline** | Full | Full | Read-only |
| **Metadata leakage** | None (P2P) | None | Server sees all |
| **Implementation** | Complex (per-type) | Medium | Simple |
| **Scaling** | Linear | Linear | Bottleneck |

**Decision:** CRDT for text/structured data (Automerge / Yjs style). Vector-clock LWW for binary blobs. No central server — mesh gossip.

---

## 13. Privacy: Zero-Knowledge (User Keys Never Leave) vs Server-Held Keys

| Aspect | Zero-Knowledge (Chosen) | Server-Held Keys |
|--------|------------------------|------------------|
| **User data** | Only user can decrypt | Server can decrypt |
| **Legal requests** | Nothing to hand over | Must comply |
| **Breach impact** | User keys safe | All users exposed |
| **Features** | No server-side search | Easy server features |
| **Sync** | P2P + relay (ciphertext) | Server-mediated |

**Decision:** Zero-knowledge. User master key (passphrase + TPM) encrypts everything. Relay/mesh sees only ciphertext. No server ever sees plaintext.

---

## 14. App Runtime: Native + Web vs Linux Compat vs Web-Only

| Aspect | Native + Web (Chosen) | Linux Compat | Web-Only |
|--------|----------------------|--------------|----------|
| **Performance** | Full hardware access | Overhead (syscall emu) | Limited (WASM) |
| **Security** | Capability sandbox | Large attack surface | Strong sandbox |
| **App ecosystem** | New apps needed | Huge existing | Growing |
| **Dev effort** | New SDK | Port existing | Web skills |
| **NPU access** | Direct | Emulated / passthrough | WebGPU / WebNN |

**Decision:** Native (Rust/Swift/Kotlin) + Web (WASM + WebGPU/WebNN) runtimes for v1. Linux compat as opt-in server later. NPU access is key differentiator — native gets direct NRM caps.

---

## Summary Table

| # | Decision | Chosen | Key Reason |
|---|----------|--------|------------|
| 1 | Kernel architecture | Microkernel | Verification, AI isolation |
| 2 | Auth model | Capabilities | Delegation, least privilege |
| 3 | Update model | Immutable core + separate model store | Decouple AI cadence from OS |
| 4 | Default sandbox | Deny | AI-first threat model |
| 5 | Filesystem | Object graph + relationships | Semantic search, multi-project |
| 6 | AI locality | Local-first | Privacy, latency, offline |
| 7 | Egress control | Kernel firewall | Unbypassable model-context protection |
| 8 | Semantic index | Vector + Graph hybrid | Best of both for queries |
| 9 | NPU scheduling | Global NRM | Pack, share, preempt fairly |
| 10 | Mesh topology | Relay-assisted P2P | NAT traversal + metadata privacy |
| 11 | State roaming | Full COW snapshot | Correctness first, optimize later |
| 12 | Sync protocol | CRDT (text) + LWW (binary) | Offline-first, no central server |
| 13 | Privacy model | Zero-knowledge | User keys never leave device |
| 14 | App runtime | Native + Web | NPU access + security |

---

**To revise:** Note the decision number and your alternative choice with rationale. I'll update the affected plane specs.