# Security — Attestation (Device, Model, Behavioral)

**Feasibility:** 🟢 BUILDABLE — TPM 2.0, remote attestation, binary transparency proven  
**Dependencies:** P0 kernel, boot chain, P3 model registry

---

## Attestation Types

| Type | What It Proves | Who Verifies |
|------|----------------|--------------|
| **Device Identity** | "This device is mine, running Axiom" | User, Peers, Services |
| **Boot Integrity** | "P0–P3 match known-good measurements" | User, Remote services |
| **Model Authenticity** | "This model is signed by Axiom / publisher" | NRM, User |
| **Behavioral** | "NRM hasn't been coerced to exfiltrate" | User (continuous) |

---

## Device Identity Attestation

### Key Hierarchy

```
Axiom Root CA (offline, hardware-secured)
       │
       ▼
Device Attestation CA (per-device, in TPM)
       │
       ├── Device Identity Key (Ed25519, in TPM)  →  Mesh certs
       ├── Attestation Identity Key (AIK, RSA/ECC, in TPM)  →  Remote attestation
       └── Sealing Key (in TPM)  →  Disk encryption, capability sealing
```

### Mesh Certificate

```rust
struct MeshCert {
    device_pubkey: Ed25519PublicKey,
    device_id: DeviceID,
    user_signature: Signature,      // User's master key signs device cert
    issued_at: Timestamp,
    expires_at: Timestamp,          // 1 year, renewable
    capabilities: CapabilitySet,    // What this device can do in mesh
}
```

**Verification:** Peer verifies `user_signature` over `device_pubkey + device_id` using user's known master public key.

---

## Boot Integrity Attestation (Measured Boot)

### Event Log (TCG Format)

```
PCR[0] = Hash(IBL)
PCR[1] = Hash(VBL)
PCR[2] = Hash(P0_kernel)
PCR[3] = Hash(kernel_cmdline)
PCR[4] = Hash(P1_manifest)
PCR[5] = Hash(NRM_binary)
PCR[6] = Hash(model_registry_manifest)
PCR[7] = Hash(user_policy)
```

### Attestation Report

```rust
struct AttestationReport {
    device_id: DeviceID,
    pcr_values: [Hash; 8],
    event_log: Vec<EventLogEntry>,    // What extended each PCR
    tpm_quote: TPMQuote,              // TPM signature over PCRs + nonce
    aik_cert: AIKCertificate,         // Chains to Axiom Root CA
    nonce: Nonce,                     // Verifier-provided
    timestamp: Timestamp,
}
```

### Verification Flow (Remote)

1. Verifier sends `nonce + policy` (e.g., "PCR[2] must equal hash X")
2. Device produces `AttestationReport`
3. Verifier:
   - Validates AIK cert chain → Axiom Root
   - Validates TPM quote signature
   - Checks `pcr_values` against `policy`
   - Checks `event_log` for unexpected entries
4. If all pass → issue short-lived credential

---

## Model Attestation

### Model Signing

```
Axiom Model Authority (offline)
       │
       ▼
Model Signing Key (per-model-family, in HSM)
       │
       ▼
ModelPackage.manifest.signature = Sign(manifest_hash)
```

### NRM Verification (At Load Time)

```rust
fn verify_model(package: ModelPackage) -> Result<ModelID, Error> {
    // 1. Verify signature chains to Axiom Model Authority
    // 2. Verify manifest.hash == content_hash(weights + artifacts)
    // 3. Verify manifest.hardware_targets compatible with this device
    // 4. Verify manifest.license permits this use
    // 5. Check revocation list (pushed via OS update)
    // 6. Record in model registry (P2 object)
    // 7. Return ModelID ( = manifest_hash )
}
```

### Behavioral Attestation (Continuous)

**Goal:** Detect if NRM has been coerced (e.g., malicious update, memory injection).

```rust
struct BehavioralAttestation {
    // Periodic (every 5 min) NRM self-report
    nrm_measurement: NRMMeasurement,
    // Kernel-enforced: NRM memory region hash
    nrm_memory_hash: Hash,
    // NRM's own view of its state
    nrm_state_hash: Hash,
    // Signed by NRM's attestation key (derived from device key)
    signature: Signature,
}

struct NRMMeasurement {
    loaded_models: Vec<ModelID>,      // What's currently loaded
    active_slices: Vec<SliceID>,      // What's running
    egress_grants_active: u32,        // How many approvals in flight
    total_inferences: u64,            // Counter
    total_power_mwh: u64,             // Power accounting
}
```

**Verification:** User (or auditor) can request `BehavioralAttestation` at any time. Kernel measures NRM memory region independently → compares to `nrm_memory_hash`. Mismatch = NRM compromised.

---

## Binary Transparency

**All released binaries logged to public transparency log (like Certificate Transparency).**

```
Build → Reproducible Artifact → Submit to Transparency Log → Signed Tree Head
                                                     │
                                                     ▼
                                            Monitor checks inclusion
```

**User verification:** `axiom verify --binary-transparency` checks all sealed binaries against log.

---

## Open Questions

See [`open/open-questions.md#attestation`](../open/open-questions.md#attestation).