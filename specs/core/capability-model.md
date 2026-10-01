# Core — Capability Model

**Feasibility:** 🟢 BUILDABLE — Capsicum, seL4, Fuchsia, CHERI prove this  
**Dependencies:** P0 kernel

---

## Capability Theory (Applied)

A **capability** is an unforgeable token of authority: `(object, rights)`. Possession = authority. No ambient authority exists.

### Capability Properties

| Property | Mechanism |
|----------|-----------|
| **Unforgeable** | Kernel stores caps in per-process capability table; user-space holds only indices (CNode slots) |
| **Transferable** | `Cap::Endpoint` invocation moves caps between processes |
| **Attenuatable** | `derive(parent, attenuation) → child` — kernel enforces child ⊆ parent |
| **Revocable** | Kernel tracks grant graph; `revoke(cap)` cascades to all derived caps |
| **Persistent** | Sealed capability tables survive reboot (see boot chain) |

---

## Primitive Capability Types

| Type | Rights | Use Case |
|------|--------|----------|
| `Endpoint` | `Send`, `Recv`, `SendRecv` | IPC to a server |
| `Memory` | `Read`, `Write`, `Execute`, `Map` | Shared-memory windows |
| `Thread` | `Start`, `Stop`, `SetPriority`, `GetRegs` | Thread control |
| `Interrupt` | `Ack`, `Mask`, `Unmask` | Driver IRQ handling |
| `Timer` | `Set`, `Read`, `Cancel` | High-res timers |
| `NPUSlice` | `Request`, `Preempt`, `GetStatus` | Accelerator time (P3) |
| `ModelHandle` | `Infer`, `GetInfo`, `Checkpoint` | Loaded model instance (P3) |
| `EgressGrant` | `SendOnce`, `GetAuditLog` | One-time approved egress (P3) |

---

## Capability Attenuation (Derivation)

```
Parent Capability                    Attenuation                    Child Capability
──────────────────────────────────────────────────────────────────────────────────
Endpoint(SendRecv)                   {Send only}                    Endpoint(Send)
Memory(Read, Write, Map)             {Read only, offset=100, len=4K} Memory(Read)
NPUSlice(Interactive)                {priority=Background, budget=50mW} NPUSlice(Background)
ObjectRead(project:work)             {tag:finance, created_after:t} ObjectRead(scope')
ModelHandle(model:llama-3.2-1b)      {max_tokens:512, temp:0.0}     ModelHandle(restricted)
```

**Attenuation is a kernel operation:** `cap_derive(parent_slot, attenuation_spec) → child_slot`. The kernel verifies `child_rights ⊆ parent_rights`.

---

## Grant Graph & Revocation

```
User (root capability)
    │
    ├─► App A: ObjectRead(project:work)
    │         │
    │         └─► Library L: ObjectRead(project:work, tag:finance)
    │
    ├─► App B: NPUSlice(Interactive)
    │         │
    │         └─► Plugin P: NPUSlice(Background, budget=10mW)
    │
    └─► System: Cap::All (kernel only)
```

**Revocation:** `revoke(user_cap)` → kernel walks graph → invalidates all descendant slots atomically. O(depth) but depth is bounded (max 8).

**Revocation triggers:**
- User explicit revoke (Settings → Privacy)
- App uninstall
- Security policy change (MDM push)
- Device removal from mesh
- Boot measurement mismatch

---

## Capability Naming & Discovery

**No global namespace.** Caps are discovered by:
1. **Bootstrap caps** — granted at process spawn (by parent or runtime)
2. **Service discovery** — `Cap::Endpoint` to a directory service (capability-gated)
3. **Explicit grant** — user consent dialog returns a new cap

**Directory service (P6 system service):**
```
Cap::Directory → query(service_name, required_caps) → Cap::Endpoint(service)
```

---

## Capability Serialization (for Persistence & Transfer)

```rust
struct SerializedCapability {
    type: CapType,
    rights: RightsSet,
    object_ref: ObjectRef,      // kernel-internal, opaque to user-space
    attenuation_chain: AttenuationSpec[],
    seal: Seal,                 // kernel signature (prevents forgery)
    expiry: Option<Timestamp>,  // for time-bounded grants
}
```

**Transfer via IPC:** `send(endpoint, message, caps: &[CapSlot])` — kernel validates sender holds each cap, moves slots to receiver's CNode.

---

## Capability Policy (Enterprise / MDM)

Policies are **capability sets** — not separate ACLs.

```rust
struct CapabilityPolicy {
    name: String,
    grants: Vec<CapGrant>,
    denies: Vec<CapDeny>,       // negative constraints (rare)
    conditions: PolicyCondition[], // time, location, network, device state
}

struct CapGrant {
    capability: CapSpec,        // e.g., "object_read:project:*"
    attenuation: AttenuationSpec,
    expiry: Option<Timestamp>,
}
```

**MDM = capability policy.** Fleet admin pushes policy → runtime merges with user grants → effective caps = intersection.

---

## Implementation Notes (seL4-style)

| Component | seL4 Equivalent |
|-----------|-----------------|
| CNode (capability node) | CSpace |
| CapSlot | Capability slot |
| CapDerive | `seL4_CNode_Mutate` / `seL4_CNode_Copy` with badge |
| CapRevoke | `seL4_CNode_Revoke` |
| CapTransfer | `seL4_MessageInfo` + caps in IPC buffer |

**Axiom additions:** `NPUSlice`, `ModelHandle`, `EgressGrant` as first-class kernel objects.

---

## Open Questions

See [`open/open-questions.md#capability-model`](../open/open-questions.md#capability-model).