# Mesh — Network Failover & Cellular Fallback

**Feasibility:** 🟢 BUILDABLE — MPTCP, Wi-Fi/cellular handoff, eSIM management proven  
**Dependencies:** P1 network server, P4 mesh, capability model

---

## Problem

User moves: Home Wi-Fi → Café Wi-Fi → Cellular → Train (no signal) → Office Wi-Fi.
Mesh tunnels must survive. Apps must not notice.

---

## Multi-Path Transport (MPTCP)

### Default: MPTCP for All Mesh Tunnels

```
Device A                                    Device B
     │                                          │
     ├──── Wi-Fi ──────────────────────────────┤  (Subflow 1)
     │                                          │
     ├──── Cellular ───────────────────────────┤  (Subflow 2, backup)
     │                                          │
     └──── Ethernet ───────────────────────────┤  (Subflow 3, if docked)
```

**Kernel support:** MPTCP v1 (Linux 5.6+, FreeBSD, Windows 11). P0 enables `CONFIG_MPTCP`.

### Subflow Priority

| Subflow | Priority | Use When |
|---------|----------|----------|
| Wi-Fi (LAN) | 100 | Available, < 50ms RTT |
| Wi-Fi (WAN) | 80 | Available, < 200ms RTT |
| Ethernet | 90 | Docked, preferred for bulk |
| Cellular | 50 | Fallback only |
| Relay | 30 | Last resort |

**Policy:** MPTCP scheduler prefers highest-priority working subflow. Seamless handoff.

---

## Cellular Failover Capability

```rust
struct CellularFailoverCap {
    // Granted by peer device owner (phone → laptop)
    allowed_networks: Vec<NetworkType>,  // [LTE, 5G, NR]
    max_data_mb_per_day: u32,            // User-set limit
    allowed_apps: Vec<AppID>,            // Or empty = all
    priority: FailoverPriority,          // MeshOnly / Critical / All
}

enum FailoverPriority {
    MeshOnly,    // Only mesh tunnels use cellular
    Critical,    // Mesh + user-marked critical apps
    All,         // Any traffic (user explicit)
}
```

**Default:** Phone grants `MeshOnly` to laptop/tablet. User can upgrade per-app.

---

## eSIM Management (Phone as Modem)

### Architecture

```
Phone (eSIM + modem)                    Laptop (no modem)
     │                                        │
     │  P1 Modem Server exposes:              │
     │  • Cap::ModemControl                   │
     │  • Cap::NetworkMetrics                 │
     │  • Cap::CellularData                   │
     │                                        │
     │  Laptop requests Cap::CellularData     │
     │  via mesh tunnel (Cap::CellularFailover)│
     │                                        │
     ▼                                        ▼
```

### Modem Server API (P1)

```rust
trait ModemServer {
    // Query
    fn get_signal_quality(&self) -> SignalQuality;
    fn get_data_usage(&self, period: Period) -> DataUsage;
    fn get_carrier_info(&self) -> CarrierInfo;
    
    // Control (requires Cap::ModemControl)
    fn set_preferred_network(&self, net: NetworkType) -> Result<(), Error>;
    fn enable_data(&self, enable: bool) -> Result<(), Error>;
    
    // Data path (requires Cap::CellularData)
    fn create_pdp_context(&self, apn: APN) -> Result<PDPHandle, Error>;
    fn get_mptcp_endpoint(&self) -> SocketAddr;  // For MPTCP subflow
}
```

---

## Failover State Machine

```
┌─────────────────────────────────────────────────────────────────────┐
│ NETWORK STATE MACHINE (per mesh tunnel)                             │
│                                                                     │
│    CONNECTED (Wi-Fi)                                                │
│         │                                                           │
│         │ Wi-Fi degrades (RTT > 200ms, loss > 5%)                  │
│         ▼                                                           │
│    DEGRADED ──► Probe cellular (if Cap::CellularFailover)          │
│         │                                                           │
│         │ Cellular ready + MPTCP subflow established               │
│         ▼                                                           │
│    FAILOVER (Wi-Fi + Cellular active)                               │
│         │                                                           │
│         │ Wi-Fi recovers                                           │
│         ▼                                                           │
│    CONNECTED (Wi-Fi primary, Cellular standby)                     │
│                                                                     │
│    ┌──────────────────────────────────────────────────────────┐    │
│    │ OFFLINE (no path)                                        │    │
│    │   → Queue outbound                                        │    │
│    │   → Local-first operation                                 │    │
│    │   → Sync on reconnect                                     │    │
│    └──────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Offline-First Operation

### Local Queue

```rust
struct OfflineQueue {
    // Persisted to P2 (survives reboot)
    operations: Vec<QueuedOp>,
    max_size_mb: u32,      // Default 500 MB
    max_age_days: u32,     // Default 7
}

struct QueuedOp {
    id: OpID,
    type: OpType,          // Sync, Offload, Notification, Update
    payload: ObjectID,     // P2 object
    priority: Priority,
    created_at: Timestamp,
    retry_count: u32,
}
```

### Reconnect Sync

```
Device comes online
       │
       ▼
┌─────────────────────────────────────┐
│ 1. Re-establish mesh tunnels        │  (MPTCP fast)
│ 2. Fetch peer vector clocks         │  (what changed)
│ 3. Compute delta (CRDT merge)       │
│ 4. Pull missing objects (prioritized)│
│ 5. Replay offline queue             │
│ 6. Notify apps: "synced"            │
└─────────────────────────────────────┘
```

**Target:** Interactive apps responsive < 2s; full sync background.

---

## Data Usage Accounting

```rust
struct DataAccounting {
    // Per capability grant
    grant_id: GrantID,
    used_mb_today: u32,
    used_mb_month: u32,
    limit_mb_day: u32,
    limit_mb_month: u32,
    
    // Per-app breakdown (if grant allows)
    by_app: HashMap<AppID, AppUsage>,
}

struct AppUsage {
    foreground_mb: u32,
    background_mb: u32,
    mesh_mb: u32,
}
```

**User sees:** "Laptop used 240 MB of your phone's data today (Mesh: 180 MB, Browser: 60 MB)"

---

## Power Management

| Network | Power Cost | Policy |
|---------|------------|--------|
| Wi-Fi | Baseline | Always on if available |
| Cellular (LTE) | ~2x Wi-Fi | On-demand, release after 5 min idle |
| Cellular (5G) | ~3x Wi-Fi | On-demand, release after 2 min idle |
| Modem (phone) | Phone battery | Phone owner sets "share when charging" |

**Phone policy default:** Share cellular only when charging + > 50% battery + Wi-Fi unavailable.

---

## Open Questions

See [`open/open-questions.md#network-failover`](../open/open-questions.md#network-failover).