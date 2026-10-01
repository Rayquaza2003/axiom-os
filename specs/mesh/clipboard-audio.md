# Mesh — Universal Clipboard & Audio Routing

**Feasibility:** 🟢 BUILDABLE — clipboard sync proven (Universal Control, KDE Connect); audio routing via PipeWire/PulseAudio  
**Dependencies:** P1 input/display servers, P4 mesh tunnels, P5 compositor

---

## Universal Clipboard

### Data Model

```rust
struct ClipboardItem {
    // Multiple representations (negotiated at paste)
    representations: Vec<ClipboardRepresentation>,
    
    // Metadata
    source_device: DeviceID,
    source_app: AppID,
    timestamp: Timestamp,
    capabilities: ClipboardCaps,  // What the source allows
}

struct ClipboardRepresentation {
    mime_type: String,        // "text/plain", "image/png", "application/x.axiom.richtext"
    data: ObjectID,           // P2 content-addressed object
    size: u64,
}

struct ClipboardCaps {
    allow_remote_paste: bool,    // Can other devices paste?
    allow_history: bool,         // Enter clipboard history?
    expire_after: Option<Duration>,
    allowed_devices: Vec<DeviceID>,  // Empty = all mesh peers
}
```

### Sync Protocol

```
Device A (copy)                          Device B (paste)
     │                                       │
     │  1. App writes to clipboard           │
     │     (via P1 Input Server)             │
     │                                       │
     │  2. P5: Serialize ClipboardItem       │
     │     → P2 object (content-addressed)   │
     │                                       │
     │  3. Mesh: Broadcast to peers          │
     │     with Cap::ClipboardRead           │
     │─────────────────────────────────────►│
     │                                       │
     │                                       │  4. P5: Receive, validate cap
     │                                       │  5. P5: Register in local clipboard ring
     │                                       │
     │                                       │  6. User pastes on Device B
     │                                       │     → P5 resolves best representation
     │                                       │     → App receives data
```

**Latency target:** < 100ms LAN, < 500ms WAN (relay).

### Representation Negotiation

| Source provides | Target requests | Resolution |
|-----------------|-----------------|------------|
| text/plain, image/png | text/plain | text/plain |
| text/html, text/plain | text/html (if browser) | text/html |
| application/x.axiom.richtext | text/plain | Rich → plain conversion |
| video/mp4 | image/png (thumbnail) | First frame extract |

Apps declare accepted MIME types; P5 picks best match.

---

## Gaze-Following Input (Optional Hardware)

### Requirements

- Device with eye-tracking (Vision Pro, PSVR2, Tobii-enabled laptop)
- Calibrated gaze vector in screen coordinates

### Protocol

```
Eye-Tracker Device                    Target Device
     │                                       │
     │  1. Gaze stream (30 Hz)               │
     │     { x, y, confidence, timestamp }   │
     │─────────────────────────────────────►│
     │                                       │
     │                                       │  2. P5: Map to target screen coords
     │                                       │  3. P5: Inject as synthetic pointer
     │                                       │     (if Cap::GazeInput granted)
     │                                       │
     │  4. Click/Select gesture              │
     │     (dwell, blink, pinch)             │
     │─────────────────────────────────────►│
     │                                       │
     │                                       │  5. P5: Inject as click event
```

**Capability:** `Cap::GazeInput` — granted per device pair. Default: denied.

---

## Audio Routing

### Use Cases

| Scenario | Source | Target | Latency Budget |
|----------|--------|--------|----------------|
| **Headphone handoff** | Laptop audio | Phone headphones | < 50 ms |
| **Speaker group** | Phone music | Laptop + Tablet speakers | < 100 ms (sync) |
| **Mic sharing** | Phone mic | Laptop meeting | < 30 ms |
| **Call transfer** | Phone call | Laptop audio | Seamless |

### Stream Model

```rust
struct AudioRoute {
    stream_id: StreamID,
    source: AudioEndpoint,      // Device + Cap::AudioCapture
    sink: AudioEndpoint,        // Device + Cap::AudioPlayback
    format: AudioFormat,        // 48kHz / 24-bit / stereo (default)
    latency_mode: LatencyMode,  // Interactive / Playback / Voice
    encryption: NoiseSession,   // Per-stream keys
}

enum LatencyMode {
    Interactive { target_ms: 20 },   // Voice, instruments
    Playback { target_ms: 100 },     // Music, video
    Voice { target_ms: 30 },         // Calls, push-to-talk
}
```

### Pipeline

```
Source Device                          Target Device
     │                                       │
     │  1. App requests Cap::AudioRoute      │
     │     (to target device)                │
     │                                       │
     │  2. P1 Audio Server: negotiate format │
     │     (via mesh tunnel)                 │
     │─────────────────────────────────────►│
     │                                       │
     │  3. Establish Noise transport         │
     │     (low-latency, prioritized)        │
     │                                       │
     │  4. Stream Opus/PCM frames            │
     │     (mesh prioritizes AudioRoute)     │
     │─────────────────────────────────────►│
     │                                       │
     │  5. Target: decode → audio server     │
     │     → output device                   │
```

**Quality:** Opus @ 128 kbps (voice), 256 kbps (music), or raw PCM (pro audio).

### Multi-Sink Sync

```
Phone (source)
     │
     ├──► Laptop (sink 1)  ──► Buffer 50ms ──► Speaker
     │
     └──► Tablet (sink 2)  ──► Buffer 50ms ──► Speaker
           │
           └─ NTP/PTP sync → aligned playback
```

**Sync protocol:** PTP (IEEE 1588) over mesh for sub-ms alignment. Fallback: NTP + buffer.

---

## Capability Grants

| Capability | What It Allows | Default |
|------------|----------------|---------|
| `Cap::ClipboardRead` | Receive clipboard broadcasts | Granted to mesh peers |
| `Cap::ClipboardWrite` | Push to clipboard (rare) | Self only |
| `Cap::AudioRoute` | Send/receive audio streams | Ask per device pair |
| `Cap::GazeInput` | Receive gaze pointer events | Denied |
| `Cap::AudioCapture` | Use device's microphone | Ask per app |

---

## Privacy

- **Clipboard:** User sees "Device X copied Y" notification; can dismiss/block
- **Audio:** Visual indicator (orange dot) when mic routed; hardware mute respected
- **Gaze:** Never recorded; stream only lives during active session

---

## Open Questions

See [`open/open-questions.md#clipboard-audio`](../open/open-questions.md#clipboard-audio).