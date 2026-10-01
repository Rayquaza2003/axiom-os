# Session — Unified Input Bus

**Feasibility:** 🟢 BUILDABLE — input abstraction, arbitration, multimodal fusion proven (Fuchsia Input, Windows Input, Android InputReader)  
**Dependencies:** P0 caps, P1 device drivers, P5 compositor, P3 AI (for intent)

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│ INPUT BUS (P5 System Service)                                           │
│                                                                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐   │
│  │  Keyboard   │  │  Pointer    │  │   Touch     │  │   Pen       │   │
│  │  (HID/USB)  │  │  (Mouse/    │  │   (Multi-   │  │   (Pressure,│   │
│  │             │  │   Trackpad) │  │   touch)    │  │   Tilt)     │   │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘   │
│         │                │                │                │           │
│         ▼                ▼                ▼                ▼           │
│  ┌─────────────────────────────────────────────────────────────────┐  │
│  │                    INPUT NORMALIZER                              │  │
│  │  • Coordinate space unification (logical dp)                   │  │
│  │  • Timestamp synchronization (monotonic, µs)                   │  │
│  │  • Device capability query (pressure? tilt? haptics?)          │  │
│  └──────────────────────────┬──────────────────────────────────────┘  │
│                             │                                          │
│                             ▼                                          │
│  ┌─────────────────────────────────────────────────────────────────┐  │
│  │                    ARBITRATION ENGINE                            │  │
│  │  • Focus tracking (window + element)                           │  │
│  │  • Modality gating (typing vs pointing vs voice)               │  │
│  │  • Gesture recognition (system vs app)                         │  │
│  │  • Input capture (drag, scroll, text selection)                │  │
│  └──────────────────────────┬──────────────────────────────────────┘  │
│                             │                                          │
│              ┌──────────────┼──────────────┐                          │
│              ▼              ▼              ▼                          │
│    ┌───────────────┐ ┌─────────────┐ ┌─────────────┐                 │
│    │  TEXT INPUT   │ │  POINTER    │ │  GESTURE    │                 │
│    │  (IMF)        │ │  EVENTS     │ │  EVENTS     │                 │
│    └───────────────┘ └─────────────┘ └─────────────┘                 │
│              │              │              │                          │
│              ▼              ▼              ▼                          │
│    ┌─────────────────────────────────────────────────────────────┐  │
│    │              CAPABILITY-GATED DELIVERY                       │  │
│    │  • App must hold Cap::InputKeyboard / Pointer / Touch       │  │
│    │  • Focus = capability grant (revoked on focus loss)         │  │
│    └─────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Input Types & Normalization

### Coordinate System

```rust
struct LogicalPoint {
    x: f32,      // Density-independent pixels (dp)
    y: f32,
    // Origin: top-left of primary display
    // Multi-display: global coordinate space
}

struct PhysicalPoint {
    x: f32,      // Raw device coordinates
    y: f32,
    device_id: DeviceID,
    transform: Transform,  // Device → Logical
}
```

**All input normalized to logical dp at bus entry.** Apps never see raw coordinates.

### Event Types

```rust
enum InputEvent {
    // Keyboard
    KeyDown { key: KeyCode, modifiers: Modifiers, repeat: bool },
    KeyUp { key: KeyCode, modifiers: Modifiers },
    CompositionStart,
    CompositionUpdate { text: String },
    CompositionEnd { text: String },
    
    // Pointer (mouse, trackpad, pen)
    PointerDown { pos: LogicalPoint, button: PointerButton, pressure: f32 },
    PointerMove { pos: LogicalPoint, pressure: f32, tilt: Option<Tilt> },
    PointerUp { pos: LogicalPoint, button: PointerButton },
    PointerCancel,
    Wheel { delta: LogicalVector, phase: ScrollPhase },
    
    // Touch
    TouchDown { touch_id: u32, pos: LogicalPoint, pressure: f32 },
    TouchMove { touch_id: u32, pos: LogicalPoint, pressure: f32 },
    TouchUp { touch_id: u32, pos: LogicalPoint },
    TouchCancel { touch_id: u32 },
    
    // Pen (stylus)
    PenDown { pos: LogicalPoint, pressure: f32, tilt: Tilt, rotation: f32 },
    PenMove { pos: LogicalPoint, pressure: f32, tilt: Tilt, rotation: f32 },
    PenUp { pos: LogicalPoint },
    PenButton { button: PenButton, pressed: bool },
    
    // Voice (via P3)
    VoiceCommand { intent: ParsedIntent, confidence: f32 },
    VoiceDictation { text: String, is_final: bool },
    
    // Gaze (via mesh)
    GazePoint { pos: LogicalPoint, confidence: f32 },
    GazeDwell { pos: LogicalPoint, duration_ms: u32 },
    GazeGesture { gesture: GazeGestureType },
    
    // System
    DeviceAdded { device: InputDeviceInfo },
    DeviceRemoved { device_id: DeviceID },
    FocusChanged { old: Option<WindowID>, new: Option<WindowID> },
}
```

---

## Focus & Capability Model

### Focus = Capability Grant

```
Window A focused                    Window B focused
     │                                    │
     │ Cap::InputKeyboard ─────────────►  │  (grant)
     │ Cap::InputPointer  ─────────────►  │  (grant)
     │                                    │
     │  ◄──────────────────────────────   │  (revoke from A)
     │                                    │
```

**Rule:** Focus change → atomic capability swap. No race window.

### Input Method Framework (IMF)

```rust
trait InputMethod {
    // Text input
    fn set_composition(&mut self, text: String, cursor: usize);
    fn confirm_composition(&mut self);
    fn delete_surrounding(&mut self, before: usize, after: usize);
    
    // UI
    fn show_ui(&mut self);
    fn hide_ui(&mut self);
    
    // Capability
    fn required_caps(&self) -> CapabilitySet;  // Typically: Cap::InputKeyboard
}
```

**System IMF:** Virtual keyboard, handwriting, voice dictation. Apps can provide custom IMF.

---

## Gesture Recognition

### System Gestures (Intercepted by Bus)

| Gesture | Action | Configurable |
|---------|--------|--------------|
| 3-finger swipe up | App switcher | Yes |
| 3-finger swipe down | Home / workspace view | Yes |
| 4-finger pinch | Close app | Yes |
| Edge swipe (left) | Back / previous workspace | Yes |
| Edge swipe (right) | Glance layer | Yes |
| Long press (trackpad) | Right-click / context menu | Yes |
| Double-tap (pen) | Screenshot / annotate | Yes |

### App Gestures (Delivered to App)

| Gesture | Delivered As |
|---------|--------------|
| Pinch (2-finger) | `PointerEvent::Pinch { scale, center }` |
| Rotate (2-finger) | `PointerEvent::Rotate { angle, center }` |
| Swipe (1-finger) | `PointerEvent::Swipe { direction, velocity }` |
| Drag | `PointerEvent::Drag { start, current, delta }` |

**Arbitration:** System gestures have priority. App receives `PointerCancel` if system intercepts.

---

## Multimodal Fusion (Intent Recognition)

```
┌─────────────────────────────────────────────────────────────────┐
│ INTENT FUSION (P3 AI-assisted, optional)                        │
│                                                                 │
│  Inputs:                                                        │
│  • Pointer: "dragging file.png"                                 │
│  • Voice: "move this to the Q3 folder"                          │
│  • Gaze:  looking at Q3 folder in sidebar                       │
│                                                                 │
│  Fusion:                                                        │
│  1. Temporal alignment (±200ms window)                          │
│  2. Semantic alignment (file.png ↔ "this", Q3 folder ↔ "Q3")    │
│  3. Confidence scoring                                          │
│                                                                 │
│  Output:                                                        │
│  Intent::Move { source: file.png, target: Q3 folder, confidence: 0.94 } │
│                                                                 │
│  Delivered to: Target app (Files) + Input Bus (for animation)  │
└─────────────────────────────────────────────────────────────────┘
```

**Privacy:** Fusion runs entirely on-device (P3 NRM). Raw voice/gaze never leaves input bus.

---

## Haptics & Feedback

```rust
struct HapticEvent {
    device_id: DeviceID,          // Target device
    pattern: HapticPattern,
    trigger: HapticTrigger,
}

enum HapticPattern {
    Click,           // Button press
    Light,           // UI transition
    Medium,          // Warning / attention
    Heavy,           // Error / destructive
    Custom {        // App-defined
        waveform: Vec<HapticPoint>,
        duration_ms: u32,
    },
}

enum HapticTrigger {
    SystemGesture,   // 3-finger swipe etc.
    AppGesture,      // App requests via Cap::Haptics
    KeyboardKey,     // Per-key (virtual keyboard)
    ScrollBoundary,  // End of list
    Error,           // Validation failure
}
```

**Capability:** `Cap::Haptics` — granted to focused app + system IMF.

---

## Accessibility

| Feature | Implementation |
|---------|----------------|
| **Switch Control** | Input bus maps switch events → pointer/keyboard |
| **Voice Control** | P3 voice → parsed intent → input events |
| **Eye Tracking** | Gaze → pointer (dwell = click) |
| **Keyboard Navigation** | Focus traversal via Tab/Arrow (bus-managed) |
| **Reduced Motion** | Bus suppresses gesture animations |
| **High Contrast** | Compositor hint (not bus) |

---

## Latency Targets

| Path | Target |
|------|--------|
| Hardware → Bus | < 1 ms |
| Bus → Focused App | < 0.5 ms |
| Gesture Recognition | < 5 ms |
| Intent Fusion | < 20 ms (P3) |
| Haptic Feedback | < 10 ms |

---

## Open Questions

See [`open/open-questions.md#input-bus`](../open/open-questions.md#input-bus).