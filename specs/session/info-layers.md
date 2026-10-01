# Session — Info Layers (Glanceable UI)

**Feasibility:** 🟢 BUILDABLE — widget layer, notification center, glanceable UI patterns proven (iOS widgets, Android Glance, Wear OS tiles)  
**Dependencies:** P5 compositor, P3 semantic index, P2 graph

---

## Concept

> **Info layers are transient, glanceable surfaces that answer "What's happening?" without opening an app.**
> 
> Not notifications. Not widgets. A *layer* you peek at, then dismiss.

```
┌─────────────────────────────────────────────────────────────────┐
│ BASE LAYER (Workspace / App)                                    │
│   └─ Full app content, interactive                              │
├─────────────────────────────────────────────────────────────────┤
│ INFO LAYER 1: STATUS BAR (always)                               │
│   Time, battery, connectivity, privacy indicators              │
├─────────────────────────────────────────────────────────────────┤
│ INFO LAYER 2: GLANCE (swipe from edge / hover)                  │
│   Next meeting, unread count, active timer, Now Playing        │
├─────────────────────────────────────────────────────────────────┤
│ INFO LAYER 3: BRIEFING (intentional gesture)                    │
│   Morning: weather + calendar + tasks + news summary           │
│   Evening: screen time, tomorrow's prep, charging status       │
├─────────────────────────────────────────────────────────────────┤
│ INFO LAYER 4: CONTEXTUAL (auto on condition)                    │
│   Arriving at office → badge reader, Wi-Fi status              │
│   Headphones connected → audio controls, transcript            │
│   Low battery → power mode options                             │
└─────────────────────────────────────────────────────────────────┘
```

---

## Layer Definitions

### Layer 0: Status Bar (Always Visible)

```rust
struct StatusBar {
    // Fixed positions
    leading: StatusItem[],      // Privacy dots (mic/cam/location/net)
    center: StatusItem[],       // Time, focus mode
    trailing: StatusItem[],     // Battery, connectivity, mesh status
    
    // Dynamic
    privacy_dots: PrivacyDots,  // Orange=mic, Green=cam, Blue=location, Purple=net
}

struct PrivacyDots {
    microphone: bool,
    camera: bool,
    location: bool,
    network_egress: bool,       // Model context leaving device
}
```

**Interaction:** Click/hover any dot → detail popover (what app, since when, revoke).

---

### Layer 1: Glance (Edge Swipe / Hover)

```rust
struct GlanceLayer {
    // Configurable cards (user picks 3-5)
    cards: Vec<GlanceCard>,
    
    // Trigger
    trigger: GlanceTrigger,     // EdgeSwipe / HoverCorner / Shortcut
    
    // Dismiss
    dismiss_on: DismissTrigger, // ClickOutside / Timeout(3s) / Gesture
}

enum GlanceCard {
    NextEvent { calendar: CalendarID, lookahead_min: 30 },
    UnreadCount { sources: Vec<MessageSource> },
    ActiveTimer { timer_id: TimerID },
    NowPlaying { show_controls: bool },
    DeviceMesh { show_peers: bool },
    NPUStatus { show_active_models: bool },
    SyncStatus { scope: SyncScope },
    QuickNote { workspace: WorkspaceID },
}
```

**Design:** Semi-transparent overlay, blur background. Cards = 120×80 dp. Dismiss = swipe back or click base layer.

---

### Layer 2: Briefing (Intentional)

```rust
struct BriefingLayer {
    // Time-aware
    schedule: BriefingSchedule,  // Morning / Evening / Custom
    
    // Content modules (composed per schedule)
    modules: Vec<BriefingModule>,
    
    // Presentation
    format: BriefingFormat,      // Cards / Scroll / Voice
    duration_target_sec: u32,    // 30s / 60s / 120s
}

enum BriefingModule {
    Weather { location: LocationSource },
    Calendar { range: TimeRange, calendars: Vec<CalendarID> },
    Tasks { workspace: WorkspaceID, filter: TaskFilter },
    News { sources: Vec<NewsSource>, summary_model: ModelID },
    Commute { from: Location, to: Location, mode: TransportMode },
    ScreenTime { period: Period },
    DeviceHealth { battery, storage, updates },
    MeshStatus { peers: Vec<DeviceID> },
    ModelUpdates { check: bool },
}

enum BriefingFormat {
    VisualCards,    // Swipeable cards
    ScrollFeed,     // Single scroll
    VoiceFirst,     // TTS + minimal visual
}
```

**Trigger:** Explicit gesture (three-finger tap, voice "brief me", or scheduled).

---

### Layer 3: Contextual (Automatic)

```rust
struct ContextualLayer {
    triggers: Vec<ContextTrigger>,
    
    // Suppression
    suppress_during: SuppressCondition[],  // Fullscreen, DND, Game
    max_per_hour: u8,                      // Prevent spam
}

enum ContextTrigger {
    // Location
    ArriveAt { place: PlaceID, radius_m: u32 },
    LeavePlace { place: PlaceID },
    
    // Device
    HeadphonesConnected { type: AudioDeviceType },
    HeadphonesDisconnected,
    ChargerConnected,
    ChargerDisconnected,
    BatteryBelow { percent: u8 },
    
    // Mesh
    PeerJoined { device: DeviceID },
    PeerLeft { device: DeviceID },
    OffloadComplete { task: OffloadTaskID },
    
    // App
    AppInstalled { app: AppID },
    UpdateAvailable { app: AppID },
    
    // System
    StorageBelow { percent: u8 },
    NetworkChange { from: NetType, to: NetType },
}
```

**Example:** Arrive at office → layer shows "Badge reader ready · Wi-Fi: Axiom-Corp · 3 unread in #team"

---

## Data Sources (Privacy-First)

| Layer | Data Source | Privacy |
|-------|-------------|---------|
| Status | P1 system sensors | Never leaves device |
| Glance | P2 graph (local) + P3 index (local) | Local only |
| Briefing | P3 planner (local LLM) + P2 graph | Local; news = opt-in egress |
| Contextual | P1 sensors + P4 mesh events | Local; peer presence = mesh |

**No cloud.** All layer computation on-device. News/weather require explicit egress approval.

---

## Compositor Integration

```rust
impl Compositor {
    // Layer stack (bottom to top)
    fn layer_stack(&self) -> Vec<Layer> {
        vec![
            Layer::Base,              // Workspace / App
            Layer::StatusBar,         // Always
            Layer::Glance,            // On trigger
            Layer::Briefing,          // On trigger
            Layer::Contextual,        // On trigger
            Layer::Modal,             // Dialogs, menus
        ]
    }
    
    // Hit testing: topmost layer gets input
    // Exception: StatusBar never captures clicks (click-through to base)
    // Exception: Glance dismisses on base click
}
```

**Z-order:** StatusBar (100) < Base (0) < Glance (200) < Briefing (300) < Contextual (400) < Modal (1000)

---

## User Customization

```rust
struct LayerPreferences {
    // Per-layer enable/disable
    enabled: HashMap<LayerType, bool>,
    
    // Glance cards
    glance_cards: Vec<GlanceCard>,
    
    // Briefing schedule
    morning_briefing: Option<BriefingLayer>,
    evening_briefing: Option<BriefingLayer>,
    
    // Contextual triggers
    contextual_triggers: Vec<ContextTrigger>,
    
    // Visual
    density: Density,          // Compact / Comfortable / Spacious
    theme: ThemePreference,    // System / Light / Dark / Auto
    reduce_motion: bool,
}
```

**Settings UI:** Settings → Info Layers → drag-reorder cards, toggle triggers, preview.

---

## Open Questions

See [`open/open-questions.md#info-layers`](../open/open-questions.md#info-layers).