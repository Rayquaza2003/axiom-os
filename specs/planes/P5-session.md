# P5 — Session & Interface: Intent Workspaces, Info Layers, Input Pipeline

**Plane:** P5 (unprivileged)  
**Feasibility:** 🟢 BUILDABLE (compositor, workspaces, input bus) → 🔴 RESEARCH (gaze/gesture without dedicated sensors)  
**Dependencies:** P0 (caps), P1 (display, input drivers), P2 (data graph), P3 (AI context), P4 (mesh handoff)  
**Dependents:** P6 (app runtime)

---

## Design Goal

Replace the desktop/folder/start-menu metaphor with an **intent-first session** that reshapes around what you're doing. Workspaces group apps + assets + notifications + services by *project*, not by application type. Information surfaces as glanceable layers, not widgets. Input is a unified, arbitrated bus.

---

## Intent Workspaces

### Workspace = Saved Graph Query

```rust
struct Workspace {
    id: WorkspaceID,
    name: String,                    // "Q3 Planning", "Gaming", "Code Review"
    icon: IconSpec,
    query: GraphQuery,               // P2 query: project, tags, recency, provenance
    apps: AppSet,                    // Pinned + auto-included from query
    services: ServiceSet,            // Background services to run/suspend
    notifications: NotificationPolicy, // Per-workspace rules
    layout: LayoutSpec,              // Window arrangement, splits, floats
    info_layers: InfoLayerID[],      // Glanceable layers active in this workspace
    input_profile: InputProfile,     // Voice/gaze/pen sensitivity per workspace
}
```

**Workspace switch = single session operation:**
1. Suspend services not in `new.services`
2. Start services in `new.services`
3. Apply `new.notifications` policy
4. Restore `new.layout` (window positions, focus)
5. Activate `new.info_layers`
6. Switch `new.input_profile`

**No manual "open apps, arrange windows, mute notifications."** The workspace *is* the project context.

### Workspace Discovery

- **Explicit:** User creates "Q3 Planning" → adds project, pins apps
- **Inferred:** NRM + session history → "You open Figma + Linear + Slack together at 9am weekdays" → suggests workspace
- **Ephemeral:** "Quick look at that PDF" → temporary workspace, auto-expires

---

## Dynamic Information Layers

### Layer Model

```
┌─────────────────────────────────────────────────────────────────┐
│ COMPOSITOR SCENE GRAPH                                          │
│                                                                 │
│  Layer 0: Workspace background (wallpaper, solid)               │
│  Layer 1: App windows (z-ordered, managed)                      │
│  Layer 2: FOCUS RING (active window highlight)                  │
│  ─────────────────────────────────────────────────────────────  │
│  Layer 3: INFO LAYERS (glanceable, non-interactive by default)  │
│    ├── Calendar layer (next meeting, time to leave)             │
│    ├── Build status layer (CI passing/failing)                  │
│    ├── AI summary layer ("3 new emails, 1 urgent")              │
│    ├── Device mesh layer (tablet battery 84%, phone on 5G)      │
│    └── Custom layer (app-provided, capability-gated)            │
│  ─────────────────────────────────────────────────────────────  │
│  Layer 4: SYSTEM OVERLAYS (volume, input switcher, auth)        │
│  Layer 5: CURSOR / GAZE INDICATOR                               │
└─────────────────────────────────────────────────────────────────┘
```

### Info Layer Properties

| Property | Description |
|----------|-------------|
| **Non-occluding** | Never steals focus; lives above apps, below system overlays |
| **Glanceable** | Designed for < 2s comprehension; dense info, minimal chrome |
| **Interactive on demand** | Hover/gaze → expands; click → opens source app |
| **Compositor-owned** | Consistent theming, animation, layout across all layers |
| **Capability-gated** | App must hold `Cap::InfoLayerPublish(layer_id)` to push updates |

### Layer Protocol

```rust
// App publishes to layer
struct LayerUpdate {
    layer_id: InfoLayerID,
    content: LayerContent,        // Structured: cards, metrics, text, sparklines
    ttl: Option<Duration>,        // Auto-expire
    priority: LayerPriority,      // Critical > High > Normal > Low
}

// Compositor renders
struct LayerContent {
    kind: LayerKind,              // Card, List, Metric, Chart, Text
    items: Vec<LayerItem>,
    actions: Vec<LayerAction>,    // "Open email", "Retry build", "View on tablet"
}
```

---

## Unified Input Pipeline

### Input Bus Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│ INPUT BUS (P5 Session Service)                                  │
│                                                                 │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐        │
│  │ Keyboard │  │ Pointer  │  │   Pen    │  │  Voice   │  ...   │
│  │  (HID)   │  │ (mouse/  │  │ (stylus) │  │ (ASR)    │        │
│  │          │  │ touch)   │  │          │  │          │        │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘        │
│       │             │             │             │               │
│       └─────────────┼─────────────┼─────────────┘               │
│                     ▼                                           │
│         ┌─────────────────────────┐                             │
│         │   ARBITRATION ENGINE    │                             │
│         │                         │                             │
│         │  Priority:              │                             │
│         │  1. Explicit contact    │  (touch/pen/click)          │
│         │  2. Gaze + dwell        │  (eye-tracking)             │
│         │  3. Voice activation    │  ("Hey Axiom")              │
│         │  4. Proximity           │  (device you're near)       │
│         │                         │                             │
│         │  Output: Focused target │  (window, layer, system)    │
│         └───────────┬─────────────┘                             │
│                     │                                           │
│                     ▼                                           │
│         ┌─────────────────────────┐                             │
│         │   INPUT TRANSFORMERS    │                             │
│         │  • Gesture recognition  │                             │
│         │  • Handwriting → text   │                             │
│         │  • Voice → intent + text│                             │
│         │  • Gaze → region select │                             │
│         └───────────┬─────────────┘                             │
│                     │                                           │
│                     ▼                                           │
│         ┌─────────────────────────┐                             │
│         │   DELIVERY TO TARGET    │                             │
│         │  (via Cap::InputTarget) │                             │
│         └─────────────────────────┘                             │
└─────────────────────────────────────────────────────────────────┘
```

### Arbitration Rules

| Scenario | Winner |
|----------|--------|
| Touch screen + mouse move | Touch (explicit contact) |
| Gaze on window A, voice command | Voice (explicit activation) |
| Pen on tablet, laptop trackpad | Pen (explicit + precision) |
| No explicit input, two devices near | Proximity + last active |

**Lag-free switching:** Single bus, single arbitration, single scheduler — no N half-integrated drivers fighting.

### Input Capabilities

| Capability | Operations |
|------------|------------|
| `Cap::InputTarget(window_id)` | `send_event(event)`, `set_focus()` |
| `Cap::InputSource(device_id)` | `subscribe(filter) → Stream`, `calibrate()` |
| `Cap::GestureRecognize` | `recognize(stream) → GestureEvent` |
| `Cap::VoiceIntent` | `listen() → Intent`, `cancel()` |

---

## Compositor

See [`session/compositor.md`](session/compositor.md) for full spec.

**Responsibilities:**
- Scene graph management (layers, windows, surfaces)
- Damage tracking & presentation scheduling (vsync-aligned)
- Color management (HDR, wide gamut, per-layer)
- Animation & transitions (workspace switch, layer expand)
- Theming (light/dark/high-contrast, per-workspace override)

**Not responsible for:** App rendering, input handling, AI, networking.

---

## Capability Surface (P5 → P6)

| Capability | Operations |
|------------|------------|
| `Cap::WorkspaceControl` | `switch(id)`, `create(ws)`, `delete(id)`, `list() → Workspace[]` |
| `Cap::InfoLayerPublish(layer_id)` | `update(content)`, `remove()` |
| `Cap::InfoLayerSubscribe(layer_id)` | `stream() → LayerUpdate[]` |
| `Cap::WindowManage` | `create(spec) → WindowID`, `destroy(id)`, `arrange(layout)` |
| `Cap::InputTarget(window_id)` | `send(event)`, `request_focus()` |
| `Cap::SessionState` | `snapshot() → StateBlob`, `restore(blob)` |

---

## Open Questions

See [`open/open-questions.md#p5-session`](../open/open-questions.md#p5-session).

---

## References

- Wayland / Weston: Compositor architecture
- Fuchsia Scenic: Scene graph, layers
- Windows DWM / macOS WindowServer: Production compositors
- Yjs / Automerge: CRDTs for collaborative layer content
- Eye-tracking: Tobii, Apple Vision Pro input model