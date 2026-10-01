# Session — Compositor & Scene Graph

**Feasibility:** 🟡 HARD BUT KNOWN — Wayland/Weston, Fuchsia Scenic, Flutter Embedder proven; novel theming model  
**Dependencies:** P0 caps, P1 display driver, P5 input bus, P6 app runtime

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│ COMPOSITOR (P5 System Service)                                          │
│                                                                         │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                      SCENE GRAPH                                 │   │
│  │  Root                                                           │   │
│  │   ├─ Layer: Workspace "Q3 Project"                              │   │
│  │   │   ├─ Surface: Sheets (spreadsheet)                          │   │
│  │   │   │   ├─ Subsurface: Cell editor                            │   │
│  │   │   │   └─ Subsurface: Chart overlay                          │   │
│  │   │   ├─ Surface: Code Editor                                   │   │
│  │   │   │   └─ Subsurface: Minimap                                │   │
│  │   │   └─ Surface: Browser (web runtime)                         │   │
│  │   ├─ Layer: Info Layer — Glance                                 │   │
│  │   ├─ Layer: Info Layer — Briefing                               │   │
│  │   ├─ Layer: Status Bar                                          │   │
│  │   └─ Layer: Modal / Dialogs                                     │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                                    │                                    │
│                                    ▼                                    │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                    COMPOSITION ENGINE                            │   │
│  │  • Damage tracking (dirty rects)                                │   │
│  │  • Occlusion culling                                            │   │
│  │  • Blend mode resolution                                        │   │
│  │  • HDR/SDR tone mapping                                         │   │
│  │  • Variable refresh rate (VRR) sync                             │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                                    │                                    │
│                                    ▼                                    │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                    THEME ENGINE                                  │   │
│  │  • Token resolution (semantic → color)                          │   │
│  │  • Dark/light/system + per-app override                         │   │
│  │  • Motion reduction                                             │   │
│  │  • High contrast / color blind modes                            │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                                    │                                    │
│                                    ▼                                    │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                    OUTPUT MANAGER                                │   │
│  │  • Multi-display layout                                         │   │
│  │  • Per-display scale/rotation/HDR                               │   │
│  │  • External display hotplug                                     │   │
│  │  • Wireless display (Miracast/AirPlay)                          │   │
│  └─────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Scene Graph

### Node Types

```rust
enum SceneNode {
    // Top-level containers
    Layer(LayerNode),
    Workspace(WorkspaceNode),
    
    // App surfaces
    Surface(SurfaceNode),
    Subsurface(SubsurfaceNode),
    
    // Visual elements
    Rect(RectNode),
    Text(TextNode),
    Image(ImageNode),
    Vector(VectorNode),
    Mesh(MeshNode),           // 3D / GPU-accelerated
    
    // Effects
    Blur(BlurNode),
    Shadow(ShadowNode),
    Mask(MaskNode),
    Transform(TransformNode), // 2D/3D transform + opacity
}

struct LayerNode {
    id: LayerID,
    name: String,
    z_order: i32,
    opacity: f32,
    clip: Option<Rect>,
    children: Vec<SceneNode>,
    // Special layers
    kind: LayerKind,          // Workspace / Info / Status / Modal
}

struct SurfaceNode {
    id: SurfaceID,
    app_id: AppID,
    window_id: WindowID,
    buffer: BufferHandle,     // From P1 display server (dmabuf / IOSurface)
    size: Size,
    transform: Transform2D,
    opacity: f32,
    blend_mode: BlendMode,
    // Damage tracking
    damage: Vec<Rect>,        // Dirty regions since last frame
    // Input
    input_region: Option<Region>,  // Non-rectangular hit test
}
```

### Layer Stack (Z-Order)

| Z-Range | Layer | Purpose |
|---------|-------|---------|
| -1000 | Wallpaper | Desktop background |
| 0–999 | Workspaces | App content (one active) |
| 1000–1999 | System UI | Dock, panel, app switcher |
| 2000–2999 | Info Layers | Glance, Briefing, Contextual |
| 3000–3999 | Status Bar | Always visible |
| 4000–4999 | Modals | Dialogs, menus, toasts |
| 5000+ | Debug / Overlay | FPS, layout bounds, pointer |

---

## Buffer Management

### Zero-Copy Pipeline

```
App (P6)                              Compositor (P5)                    Display (P1)
   │                                        │                                 │
   │ 1. Allocate buffer (dmabuf/IOSurface)  │                                 │
   │     via Cap::BufferAllocator           │                                 │
   │                                        │                                 │
   │ 2. Render into buffer (GPU/CPU)        │                                 │
   │                                        │                                 │
   │ 3. Submit buffer + damage              │                                 │
   │     ─────────────────────────────────► │                                 │
   │                                        │                                 │
   │                                        │ 4. Composite (GPU)              │
   │                                        │     (read-only access)          │
   │                                        │                                 │
   │                                        │ 5. Submit to display            │
   │                                        │     ────────────────────────►  │
   │                                        │                                 │
   │ 6. Buffer released (fence)             │                                 │
   │     ◄───────────────────────────────── │                                 │
   │                                        │                                 │
```

**Capability:** `Cap::BufferAllocator` — granted per app at window creation. Buffer format negotiated (RGBA8, RGBA1010102, YUV).

---

## Theme Engine

### Design Tokens (Semantic → Concrete)

```rust
struct ThemeTokens {
    // Color
    color: ColorTokens,
    // Spacing
    space: SpaceTokens,
    // Typography
    type: TypeTokens,
    // Motion
    motion: MotionTokens,
    // Shape
    shape: ShapeTokens,
}

struct ColorTokens {
    // Backgrounds
    bg_primary: TokenRef,
    bg_secondary: TokenRef,
    bg_tertiary: TokenRef,
    bg_overlay: TokenRef,
    // Content
    fg_primary: TokenRef,
    fg_secondary: TokenRef,
    fg_disabled: TokenRef,
    fg_on_primary: TokenRef,
    // Accent
    accent_primary: TokenRef,
    accent_secondary: TokenRef,
    // Semantic
    success: TokenRef,
    warning: TokenRef,
    error: TokenRef,
    // Borders
    border_subtle: TokenRef,
    border_emphasis: TokenRef,
    // Selection
    selection_bg: TokenRef,
    selection_fg: TokenRef,
}
```

### Theme Resolution

```
App Request: "accent_primary"
       │
       ▼
┌────────────────────────────────────────┐
│ THEME RESOLVER                          │
│                                         │
│ 1. Check app override                  │
│ 2. Check workspace theme               │
│ 3. Check system theme (light/dark)     │
│ 4. Check accessibility (high contrast) │
│ 5. Check color blind mode              │
│ 6. Fallback: base theme                │
│                                         │
│ Output: Concrete color (sRGB / P3)     │
└────────────────────────────────────────┘
```

### Per-App Theming

```rust
struct AppThemeOverride {
    // App can request theme adjustments
    force_dark: bool,           // "I only work in dark"
    force_light: bool,          // "I only work in light"
    custom_accent: Option<Color>, // Brand color
    respect_system: bool,       // Default: true
}

// Compositor applies: App override → Workspace → System → Base
```

---

## Animation & Motion

### Motion Tokens

```rust
struct MotionTokens {
    // Durations
    duration_instant: u32,      // 0 ms
    duration_fast: u32,         // 100 ms
    duration_normal: u32,       // 200 ms
    duration_slow: u32,         // 400 ms
    // Easings
    ease_standard: Easing,      // cubic-bezier(0.2, 0, 0, 1)
    ease_emphasized: Easing,    // cubic-bezier(0.2, 0, 0, 1) + overshoot
    ease_decelerate: Easing,    // cubic-bezier(0, 0, 0.2, 1)
    ease_accelerate: Easing,    // cubic-bezier(0.4, 0, 1, 1)
    // Spring
    spring_stiff: SpringConfig, // stiff, low bounce
    spring_gentle: SpringConfig,// gentle, slight bounce
}
```

### Reduce Motion

```rust
impl Compositor {
    fn resolve_duration(&self, token: DurationToken) -> u32 {
        if self.preferences.reduce_motion {
            return 0;  // Instant
        }
        self.theme.motion[token]
    }
}
```

---

## Multi-Display

### Layout Model

```rust
struct DisplayLayout {
    displays: Vec<DisplayConfig>,
    arrangement: Arrangement,    // Grid / Linear / Custom
}

struct DisplayConfig {
    id: DisplayID,
    logical_size: Size,          // dp
    scale: f32,                  // 1.0, 1.5, 2.0, 3.0
    rotation: Rotation,          // 0, 90, 180, 270
    hdr: bool,
    vrr: bool,                   // Variable refresh rate
    position: Point,             // In global coordinate space
    // Color
    color_space: ColorSpace,     // sRGB / P3 / Rec2020
    brightness_nits: u32,
}
```

### Hotplug

```
Display connected
       │
       ▼
┌─────────────────────────────────────┐
│ 1. P1: Detect + enumerate modes     │
│ 2. P5: Create DisplayConfig         │
│ 3. P5: Auto-arrange (right of main) │
│ 4. P5: Migrate surfaces if needed   │
│ 5. Notify apps: displayChanged      │
└─────────────────────────────────────┘
```

---

## Wireless Display

```
Source Device                          Sink Device (TV/Monitor)
     │                                       │
     │ 1. Discover (mDNS / Wi-Fi Direct)     │
     │ 2. Negotiate Cap::WirelessDisplay     │
     │ 3. Establish mesh tunnel              │
     │ 4. Encode: H.264 / HEVC / AV1         │
     │    (hardware encoder)                 │
     │ 5. Stream via RTP + FEC               │
     │    ────────────────────────────────►  │
     │                                       │
     │                                       │ 6. Decode → display
```

**Latency target:** < 50ms (local), < 100ms (mesh relay).

---

## Performance Targets

| Metric | Target |
|--------|--------|
| Frame latency (GPU) | < 4 ms (120 fps) |
| Frame latency (CPU fallback) | < 8 ms |
| Surface commit → visible | < 1 frame |
| Multi-display sync | < 1 ms skew |
| Theme switch | < 100 ms |
| App launch → first frame | < 300 ms |

---

## Open Questions

See [`open/open-questions.md#compositor`](../open/open-questions.md#compositor).