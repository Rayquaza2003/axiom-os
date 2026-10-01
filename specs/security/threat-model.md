# Security — Threat Model (STRIDE)

**Feasibility:** 🟢 BUILDABLE — threat modeling is standard practice  
**Dependencies:** All planes

---

## Assets

| Asset | Value | Classification |
|-------|-------|----------------|
| User data (files, messages, keys) | Critical | Confidential, Integrity |
| User identity & credentials | Critical | Confidential, Integrity |
| AI context (screen, audio, index) | Critical | Confidential, Integrity |
| Capability grants / policy | High | Integrity, Availability |
| System integrity (P0–P3) | Critical | Integrity, Availability |
| Mesh peer keys | High | Confidential, Integrity |
| Model weights (personal) | Medium | Confidential |

---

## Trust Boundaries

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ TRUST BOUNDARY 0: HARDWARE (TPM/SE, CPU, NPU, Memory)                       │
│  • Root of trust for boot, keys, attestation                                │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ TRUST BOUNDARY 1: P0 KERNEL (ONLY TRUSTED SOFTWARE)                         │
│  • Capability enforcement, IPC, scheduling, memory                          │
│  • Compromise = total system compromise                                     │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │ CAPABILITY IPC ONLY
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ TRUST BOUNDARY 2: P1–P3 SYSTEM SERVERS (UNPRIVILEGED, CRASH-ISOLATED)       │
│  • Drivers, FS, Network, NRM                                                │
│  • Compromise = single service; contained by capabilities                   │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ TRUST BOUNDARY 3: P4–P6 USER SPACE (APPS, SESSION, MESH)                    │
│  • Apps in capability-bounded containers                                    │
│  • Compromise = single app; no lateral movement without caps                │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ TRUST BOUNDARY 4: MESH PEERS & RELAY (OTHER DEVICES, INFRASTRUCTURE)        │
│  • Peer devices: trusted (user-attested)                                    │
│  • Relay: UNTRUSTED — carries only ciphertext                               │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## STRIDE Analysis

### Spoofing

| Threat | Target | Mitigation |
|--------|--------|------------|
| Fake device joins mesh | Mesh | Device attestation (TPM quote + user-signed cert) |
| App impersonates another | Capability system | Unforgeable caps; kernel validates on every IPC |
| Malicious relay impersonates peer | Mesh | Noise_NK handshake verifies peer identity |
| Spoofed capability grant | Kernel | Kernel-only cap creation; sealed cap tables |

### Tampering

| Threat | Target | Mitigation |
|--------|--------|------------|
| Modify P0 kernel | Boot chain | Measured boot + TPM PCR; dm-verity on sealed partitions |
| Modify P1/P3 binaries | Boot chain | Measured launch; PCR mismatch → no start |
| Modify user data | P2 storage | AES-256-GCM per object; DEK wrapped by user key |
| Modify capability grants | Kernel CNode | Kernel-enforced; revocation cascades |
| Modify mesh traffic | Relay | Noise encryption + authentication; relay cannot modify |
| Inject false index entries | P3 index | Capability-gated ingest; append-only index shards |

### Repudiation

| Threat | Target | Mitigation |
|--------|--------|------------|
| User denies grant | Audit log | Tamper-evident log (hash-chained, TPM-sealed) |
| App denies access | Audit log | Every capability use logged |
| Peer denies sync | Mesh log | Signed sync receipts |
| System denies update | Boot log | Measured boot event log |

### Information Disclosure

| Threat | Target | Mitigation |
|--------|--------|------------|
| App reads another app's data | Capability system | Default-deny; explicit `Cap::ObjectRead(scope)` required |
| NRM leaks model context | Egress firewall | Classifier blocks unapproved model-context egress |
| Screen/audio indexed without consent | P3 ingest | Opt-in per source; capability-gated |
| Mesh relay reads traffic | Relay design | Relay carries ONLY ciphertext; no metadata |
| Side-channel (timing, power) | Scheduler | Constant-time kernel paths; noise injection for secrets |
| Cold boot / DMA attack | Hardware | TPM-sealed keys; memory encryption (TME/MKTME) |

### Denial of Service

| Threat | Target | Mitigation |
|--------|--------|------------|
| App exhausts NPU | NRM scheduling | Quotas, preemption, background throttling |
| App exhausts capability table | Kernel | Per-process cap table limits |
| Mesh peer floods tunnels | Mesh | Per-tunnel rate limits; capability-gated |
| Malicious update bricks device | Boot chain | A/B partitions; auto-rollback on boot failure |
| Indexer OOMs system | P3 ingest | Memory limits; background priority; thermal throttle |

### Elevation of Privilege

| Threat | Target | Mitigation |
|--------|--------|------------|
| App escapes container | Container runtime | Capability sandbox; no ambient authority; seccomp-like syscall filter |
| P1 server exploits kernel | Kernel IPC | Minimal IPC surface; formal verification target for P0 |
| NRM grants extra slices | NRM logic | Kernel enforces slice grants; NRM cannot self-grant |
| Compromised relay attacks peer | Mesh | Relay cannot initiate; only forwards; peer validates all |
| MDM policy bypass | Capability policy | Policy = capability set; kernel enforces intersection |

---

## Assumptions

| Assumption | Rationale |
|------------|-----------|
| Hardware TPM/SE is trustworthy | Industry standard; remote attestation verifies |
| User's master passphrase is strong | PBKDF2/Argon2 with high iteration count |
| User verifies attestation occasionally | "Trust but verify" model |
| Relay is honest-but-curious | Cannot decrypt; only metadata leakage (timing, volume) |
| NPU firmware is not malicious | Vendor-signed; measured at boot |
| No hardware backdoors | Open hardware (RISC-V/CHERI) as future target |

---

## Residual Risks (Accepted)

| Risk | Likelihood | Impact | Acceptance |
|------|------------|--------|------------|
| Zero-day in P0 kernel | Low | Critical | Formal verification target; minimal TCB |
| Side-channel key extraction | Medium | High | TPM-sealed keys; memory encryption |
| Compromised relay correlates traffic | Medium | Medium | User can run own relay; mixnet future |
| User loses master key | Low | Critical | Recovery key split (Shamir) to trusted contacts |
| Supply chain attack on build | Low | Critical | Reproducible builds + binary transparency |

---

## Open Questions

See [`open/open-questions.md#threat-model`](../open/open-questions.md#threat-model).