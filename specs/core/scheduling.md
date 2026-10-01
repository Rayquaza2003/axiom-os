# Core — CPU/NPU Co-Scheduling & Power-Thermal Accounting

**Feasibility:** 🟡 HARD BUT KNOWN — heterogeneous scheduling is an active research area but production systems exist (Android, iOS, game consoles)  
**Dependencies:** P0 kernel, P1 NPU driver, P3 NRM

---

## Scheduling Goals

1. **Predictable latency** for interactive/real-time (voice, gaze, input, display)
2. **Maximum throughput** for background (indexing, sync, training)
3. **Power/thermal safety** — never exceed SoC limits
4. **Fairness** — no app starves others
5. **Preemptibility** — background yields to foreground instantly

---

## Scheduler Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ GLOBAL SCHEDULER (P0 Kernel)                                                │
│                                                                             │
│  ┌─────────────────┐    ┌─────────────────┐    ┌────────────────────────┐  │
│  │ CPU SCHEDULER   │    │ NPU SCHEDULER   │    │ POWER-THERMAL GOVERNOR │  │
│  │                 │    │                 │    │                        │  │
│  │ • Per-core runq │    │ • Slice alloc   │    │ • SoC thermal model   │  │
│  │ • Priority +    │    │ • Preemption    │    │ • Power budget (mW)   │  │
│  │   deadline (EDF)│    │ • Affinity      │    │ • Throttle signals    │  │
│  │ • Affinity      │    │ • Memory budget │    │ • DVFS requests       │  │
│  └────────┬────────┘    └────────┬────────┘    └───────────┬────────────┘  │
│           │                      │                        │               │
│           └──────────────────────┼────────────────────────┘               │
│                                  ▼                                        │
│                    ┌─────────────────────────┐                             │
│                    │   CO-SCHEDULING DECISION │                             │
│                    │   (every 1ms tick)       │                             │
│                    └─────────────────────────┘                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## CPU Scheduling

### Priority Bands (256 levels)

| Band | Range | Policy | Use Cases |
|------|-------|--------|-----------|
| **Real-Time** | 0–31 | FIFO + deadline (EDF) | Audio (1ms), Input (2ms), Display vsync (16ms), Voice wake |
| **Interactive** | 32–63 | CFS + latency-nice | Foreground app, NRM interactive inference |
| **Normal** | 64–95 | CFS | Background apps, P1 servers |
| **Background** | 96–127 | CFS + nice=10 | Indexing, sync, NRM background inference |
| **Idle** | 128–159 | CFS + nice=19 | Telemetry, maintenance |
| **System** | 160–255 | Fixed priority | P1 drivers (IRQ threads), kernel threads |

### Thread Attributes

```rust
struct ThreadAttrs {
    priority: u8,              // 0–255
    deadline: Option<Duration>, // for RT band
    cpu_affinity: CpuMask,     // preferred cores (big.LITTLE aware)
    qos_class: QoSClass,       // UserInteractive, Utility, Background, etc.
    power_class: PowerClass,   // HighPerf, Balanced, LowPower
}
```

---

## NPU Scheduling (Co-Scheduled with CPU)

### Slice Model

An **NPU slice** = (time quantum, memory budget, power budget, priority).

```rust
struct NPUSliceGrant {
    slice_id: SliceID,
    duration: Duration,           // e.g., 10ms
    memory_mb: u32,               // weight buffer + activation buffer
    power_mw: u32,                // max average power
    priority: NPUPriority,        // RealTime > Interactive > Background
    preemptible: bool,            // RT = false, others = true
    accelerator_affinity: AccelMask, // which NPU core(s)
}
```

### NRM ↔ Kernel Contract

| NRM Requests | Kernel Grants |
|--------------|---------------|
| `request_slice(attrs)` | `SliceGrant` or `Denied(Reason)` |
| `preempt(slice_id)` | Immediate reclaim; client gets `Preempted` |
| `extend(slice_id, delta)` | If budget allows |
| `get_telemetry(slice_id)` | Latency, power, utilization |

**NRM is the *only* NPU slice holder.** Apps request inference via `Cap::ModelHandle` → NRM schedules slice → returns grant.

---

## Power-Thermal Governor

### SoC Thermal Model (Per-Accelerator)

```
Thermal State Machine:
    ┌─────────┐     T > T_throttle     ┌───────────┐
    │ NORMAL  │ ─────────────────────► │ THROTTLE  │
    └─────────┘                        └─────┬─────┘
         ▲                                    │
         │ T < T_recover                      │
         └────────────────────────────────────┘

Throttle Actions (in order):
1. Reduce Background NPU slices → 0
2. Reduce Background CPU frequency
3. Reduce Interactive NPU slice duration
4. Reduce Interactive CPU frequency
5. Throttle RealTime (last resort — safety critical)
```

### Power Budget Accounting

```rust
struct PowerBudget {
    // Sustained (thermal design power)
    sustained_mw: u32,
    // Burst (short duration, e.g., 10s)
    burst_mw: u32,
    burst_duration: Duration,
    // Current consumption (telemetry)
    current_mw: u32,
}

// Every slice grant consumes budget:
//   budget.current_mw += slice.power_mw
// On completion:
//   budget.current_mw -= slice.power_mw
// If budget exceeded → deny or throttle
```

**Per-app accounting:** NRM tracks power per capability grant; reports to user ("App X used 2.3Wh on AI today").

---

## Cross-Device Scheduling (P4 Mesh)

When offloading to another device's NPU:

1. Local NRM creates `OffloadJob` with `Cap::NPUSlice(remote)` 
2. P4 broker matches job to peer with available `NPUSlice`
3. Peer NRM schedules locally → returns result via mesh tunnel
4. Local NRM accounts **local** power for data transfer only

**No cross-device thermal coupling** — each device governs its own silicon.

---

## Scheduling Scenarios

| Scenario | CPU | NPU | Power |
|----------|-----|-----|-------|
| **Voice wake** | RT thread on little core | RT slice (reserved 5ms/100ms) | < 50mW |
| **Typing + AI autocomplete** | Interactive (foreground) | Interactive slice (20ms, 500mW) | Budgeted |
| **Video call (encode/decode)** | RT (audio) + Normal (video) | Background (encode assist) | Sustained |
| **Background indexing** | Background (nice=10) | Background slices (batched, 100mW) | Opportunistic |
| **Gaming** | RT (input) + HighPerf (render) | None (GPU owns) | Burst allowed |
| **Phone 5G failover** | Normal (network stack) | None | < 100mW |

---

## Open Questions

See [`open/open-questions.md#scheduling`](../open/open-questions.md#scheduling).