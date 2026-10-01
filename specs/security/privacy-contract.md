# Security — Privacy Contract (Zero-Knowledge Guarantees)

**Feasibility:** 🟢 BUILDABLE — E2E encryption, capability isolation, local-first proven  
**Dependencies:** All planes

---

## Privacy Principles (Non-Negotiable)

| Principle | Enforcement |
|-----------|-------------|
| **Local-first** | All AI, indexing, sync runs on-device by default |
| **Keys only you hold** | Master key derived from passphrase + TPM; never leaves device |
| **No ambient authority** | Capability system = default-deny |
| **Explicit consent** | Every sensitive capability = user grant |
| **Auditability** | Every access logged; user can review |
| **Minimal relay** | Mesh relay carries only ciphertext; cannot decrypt |
| **No telemetry by default** | Opt-in only; local-only; encrypted |

---

## Key Management

### Master Key Derivation

```
User Passphrase (high entropy)
        │
        ▼
Argon2id(memory=1GB, iterations=8, parallelism=4)
        │
        ▼
Master Key (32 bytes)
        │
        ├──► HKDF(master, "disk")       → Disk Encryption Key
        ├──► HKDF(master, "mesh")       → Mesh Identity Key
        ├──► HKDF(master, "backup")     → Backup Encryption Key
        ├──► HKDF(master, "audit")      → Audit Log Signing Key
        └──► HKDF(master, "project:*")  → Per-Project Keys (optional)
```

**TPM sealing:** Each derived key sealed to PCR state (boot integrity) + user PIN.

### Key Rotation

| Key | Rotation Trigger | Mechanism |
|-----|------------------|-----------|
| Disk | User changes passphrase | Re-encrypt DEKs (not full disk) |
| Mesh | Device removed | Revoke cert; generate new |
| Backup | User request | Re-encrypt backup blobs |
| Project | User request | Re-wrap project DEKs |

---

## Data Flows & Guarantees

### On-Device AI (P3)

| Data | Where It Goes | Guarantee |
|------|---------------|-----------|
| Files | P2 object store (encrypted) | Never leaves device |
| Screen content (opt-in) | P3 index (encrypted) | Never leaves device |
| Audio (opt-in) | P3 index (encrypted) | Never leaves device |
| Model inference | NRM → NPU (local) | Never leaves device |
| Embeddings | P3 vector store (encrypted) | Never leaves device |
| Query planner | Local LLM (P3) | Never leaves device |

### Mesh Sync (P4)

| Data | Path | Guarantee |
|------|------|-----------|
| Objects | Device ↔ Device (Noise tunnel) | E2E encrypted; relay blind |
| Capabilities | Device ↔ Device | Encrypted in tunnel |
| State roaming | Device → Device | Encrypted snapshot; relay blind |
| NPU offload | Device → Device | Encrypted tensors; relay blind |

### Approved Egress (P3 Firewall)

| Data | Path | Guarantee |
|------|------|-----------|
| Model context | Device → Remote model | User-approved per-request; encrypted to model's user key |
| Model response | Remote model → Device | Encrypted to user key; relay blind |

---

## What the OS *Cannot* See (By Design)

| Data | Why |
|------|-----|
| User's master passphrase | Never stored; only used for key derivation |
| Decrypted user files | Encryption/decryption in app container (P6) |
| Decrypted mesh traffic | Keys only on endpoints |
| Remote model responses (decrypted) | Decrypted in app container |
| AI index plaintext | Encrypted at rest in P2 |
| Capability grant decisions (user) | Stored in sealed kernel CNode |

---

## What the OS *Can* See (Necessary for Function)

| Data | Purpose | Protection |
|------|---------|------------|
| Object metadata (tags, projects, size) | Graph queries, sync | Encrypted at rest; capability-gated |
| Graph edges | Workspace, search, sync | Encrypted at rest; capability-gated |
| Vector IDs + model IDs | Semantic search | Encrypted at rest |
| Network flow metadata (IP, port, volume) | Firewall, QoS | Not logged by default |
| Device identity (public keys) | Mesh attestation | Public by design |
| Boot measurements | Attestation | Public by design |

---

## Telemetry (Opt-In Only)

```rust
struct TelemetryConfig {
    enabled: bool,                    // Default: false
    local_only: bool,                 // Default: true (never leaves device)
    include: TelemetryCategory[],     // User selects
    retention_days: u32,              // Default: 30
}

enum TelemetryCategory {
    Performance,      // Latency, throughput, power
    Reliability,      // Crashes, restarts, errors
    Usage,            // Feature adoption (anonymized)
    Security,         // Blocked egress, revoked caps (no payload)
}
```

**If enabled:** Stored in P2 as encrypted objects; user can export/delete anytime.

---

## Enterprise / MDM Privacy Boundary

| MDM Can | MDM Cannot |
|---------|------------|
| Push capability policy | Read user files |
| Require device attestation | Decrypt mesh traffic |
| Enforce boot integrity | See AI context |
| Remotely wipe (cryptographic) | Access master key |
| Audit capability grants | See screen/audio index |

**MDM policy = capability set.** No separate "management agent" with god mode.

---

## Legal / Regulatory Alignment

| Regulation | Axiom Property |
|------------|----------------|
| GDPR Art. 25 (Privacy by Design) | Default-deny, local-first, minimal data |
| GDPR Art. 17 (Right to Erasure) | `axiom wipe --all` — cryptographic erasure |
| GDPR Art. 20 (Portability) | Full data export (encrypted) |
| CCPA | Opt-in telemetry; no sale; user controls |
| ePrivacy | No tracking; local processing |
| AI Act (EU) | On-device AI = minimal risk; egress = high-risk (gated) |

---

## Open Questions

See [`open/open-questions.md#privacy-contract`](../open/open-questions.md#privacy-contract).