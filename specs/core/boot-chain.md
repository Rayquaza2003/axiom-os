# Core — Verified Boot Chain & Sealed Core

**Feasibility:** 🟢 BUILDABLE — TPM 2.0, measured boot, dm-verity, ostree proven  
**Dependencies:** P0 kernel, hardware root of trust (TPM/SE)

---

## Threat Model

| Threat | Mitigation |
|--------|------------|
| Bootloader replacement | ROM-anchored trust; immutable first-stage |
| Kernel replacement | Measured boot + TPM attestation |
| P1/P3 server replacement | Measured launch; hash in PCR |
| Runtime rootkit | Sealed core (read-only) + capability isolation |
| Evil maid (physical) | TPM-sealed disk encryption; user PIN at boot |
| Supply chain | Reproducible builds; binary transparency log |

---

## Boot Stages

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ BOOT CHAIN                                                                  │
│                                                                             │
│  ROM (immutable)                                                            │
│       │                                                                     │
│       ▼ measures                                                            │
│  STAGE 1: Immutable Bootloader (IBL)                                        │
│  • Verifies STAGE 2 signature                                               │
│  • Extends PCR[0] = hash(IBL)                                               │
│  • Extends PCR[1] = hash(STAGE2)                                            │
│       │                                                                     │
│       ▼ measures                                                            │
│  STAGE 2: Verified Bootloader (VBL)  ──► User PIN / Biometric               │
│  • Unlocks disk encryption key (TPM-sealed)                                │
│  • Verifies P0 kernel signature                                             │
│  • Extends PCR[2] = hash(P0_kernel)                                         │
│  • Extends PCR[3] = hash(P0_cmdline)                                        │
│       │                                                                     │
│       ▼ measures                                                            │
│  P0 KERNEL                                                                  │
│  • Measures P1 server images → extends PCR[4..N]                           │
│  • Measures P3 NRM + Model Registry manifests → extends PCR                │
│  • Seals capability tables to PCR state                                     │
│  • Starts P1 servers (capability-granted)                                  │
│       │                                                                     │
│       ▼                                                                     │
│  USER SPACE (P1–P6)                                                         │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## PCR Allocation

| PCR | Content | Purpose |
|-----|---------|---------|
| 0 | IBL hash | Hardware root |
| 1 | VBL hash | Bootloader integrity |
| 2 | P0 kernel hash | Kernel integrity |
| 3 | Kernel cmdline | Boot parameters |
| 4 | P1 server manifest hash | System server set |
| 5 | P3 NRM hash | AI core integrity |
| 6 | Model registry manifest hash | Model set integrity |
| 7 | User policy hash | Capability policy |
| 8–15 | Reserved / future | Extensibility |

---

## Sealed Core

**What's sealed (read-only after boot):**
- P0 kernel image
- P1 server binaries
- P3 NRM binary
- Model registry manifests (not weights — those are P2 objects)
- Capability policy (MDM + user)

**What's mutable (but measured/attested):**
- Model weights (P2 objects, content-addressed, signed)
- User data (P2, encrypted)
- Semantic index (P2, encrypted)
- Runtime capability grants (ephemeral, in kernel CNode)

**Sealing mechanism:** dm-verity / fs-verity on root filesystem + kernel-enforced W^X + capability table sealing to PCR state.

---

## Attestation Protocol

**Local attestation (user verifies own device):**
1. User requests attestation report via Settings
2. Kernel asks TPM for `Quote(PCR[0..7], nonce)`
3. TPM signs with Attestation Identity Key (AIK)
4. Report displayed: PCR values + event log + signature
5. User compares to known-good values (published by Axiom)

**Remote attestation (service verifies device):**
1. Service sends nonce + policy requirements
2. Device produces TPM Quote + event log
3. Service verifies: AIK cert chains to Axiom root, PCRs match policy, event log matches expected measurements
4. Service issues short-lived credential (e.g., for enterprise access)

---

## Rollback Protection

| Component | Rollback Mechanism |
|-----------|-------------------|
| P0 kernel | PCR[2] mismatch → boot fails; user must confirm downgrade |
| P1 servers | PCR[4] mismatch → servers not started; recovery mode |
| Model registry | Manifest hash in PCR[6] → NRM refuses old manifests |
| User data | Snapshot ledger (P2) → user chooses snapshot to boot |
| Capability grants | Stored in sealed capability table; replayed only if PCRs match |

**User-controlled rollback:** At boot menu, user can select "Boot previous OS snapshot" → kernel loads previous P0+P1+P3 from verified partition → PCRs reflect older but *known-good* state.

---

## Reproducible Builds

**Requirement:** Every binary in the sealed core must be bit-for-bit reproducible.

| Tool | Purpose |
|------|---------|
| `hermetic-toolchain` | Fixed compiler, libc, deps |
| `build-manifest.json` | All inputs hashed (source, deps, config) |
| `build.log` | Full build transcript for audit |
| `binary-transparency.log` | Public log of all released artifacts (like CT logs) |

---

## Disk Encryption

- **Data partition:** AES-256-XTS, key sealed to TPM + user PIN
- **Key hierarchy:** `MasterKey → (TPM_seal + User_PIN) → DiskKey`
- **Rotation:** User can rotate PIN without re-encrypting (re-seal only)
- **Mesh sync keys:** Derived from MasterKey; never leave device unencrypted

---

## Open Questions

See [`open/open-questions.md#boot-chain`](../open/open-questions.md#boot-chain).