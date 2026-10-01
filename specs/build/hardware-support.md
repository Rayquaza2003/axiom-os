# Build — Hardware Support (BSP, NPU Drivers, Sensor HAL)

**Feasibility:** 🟡 HARD BUT KNOWN — vendor BSPs exist; NPU drivers are the main gap  
**Dependencies:** P0 kernel, P1 servers

---

## Reference Platforms (v1 Target)

| Platform | SoC | NPU | Form Factor | Priority |
|----------|-----|-----|-------------|----------|
| **Axiom DevKit** | RK3588 | 6 TOPS (INT8) | SBC | P0 (primary dev) |
| **Laptop** | Apple M3 | 18 TOPS | MacBook Air | P0 (primary laptop) |
| **Laptop** | Snapdragon X Elite | 45 TOPS | Windows on ARM | P0 (primary Windows) |
| **Phone** | Pixel 9 (Tensor G4) | ~20 TOPS | Phone | P1 |
| **Tablet** | iPad Pro M4 | 38 TOPS | Tablet | P1 |
| **Desktop** | AMD Ryzen AI 9 HX 370 | 50 TOPS | Mini PC | P1 |

---

## Board Support Package (BSP) Structure

```
kernel/
├── arch/
│   ├── arm64/
│   │   ├── rockchip/
│   │   │   ├── rk3588/
│   │   │   │   ├── dts/           # Device tree
│   │   │   │   ├── clock.c        # Clock tree
│   │   │   │   ├── power.c        # Power domains
│   │   │   │   ├── irq.c          # Interrupt controller
│   │   │   │   └── pinctrl.c      # Pin muxing
│   │   │   └── common/
│   │   ├── apple/
│   │   │   ├── m3/
│   │   │   └── common/
│   │   └── qualcomm/
│   │       ├── sm8650/            # Snapdragon X Elite
│   │       └── common/
│   └── x86_64/
│       ├── amd/
│       │   ├── ryzen_ai_300/
│       │   └── common/
│       └── intel/
│           ├── core_ultra_200/
│           └── common/
```

### BSP Bring-Up Checklist

| Component | Status | Notes |
|-----------|--------|-------|
| UART console | ✅ | Early boot logs |
| Timer / clocksource | ✅ | Scheduler needs it |
| Interrupt controller | ✅ | GICv3 / APIC |
| Memory map | ✅ | Reserved regions |
| CPU hotplug | 🟡 | For power management |
| PSCI / SMCCC | ✅ | ARM firmwares |
| PCIe / USB | 🟡 | For NVMe, peripherals |
| Display controller | 🟡 | DRM/KMS driver in P1 |
| NPU | 🔴 | **Main gap** — see below |

---

## NPU Driver Strategy

### The Problem

| Vendor | NPU SDK | Kernel Driver | Userspace Stack | Open? |
|--------|---------|---------------|-----------------|-------|
| **Rockchip** | RKNN | rknpu (staging) | RKNN API | Partial |
| **Apple** | CoreML / MPS | Closed | Metal / CoreML | ❌ |
| **Qualcomm** | QNN | qcom_qnp (upstreaming) | QNN SDK | Partial |
| **AMD** | Vitis AI | xdna (upstream) | Vitis AI EP | ✅ |
| **Intel** | OpenVINO | ivpu (upstream) | OpenVINO | ✅ |
| **Google** | TensorRT / LiteRT | edgetpu (upstream) | TFLite | Partial |

### Axiom Approach: Unified NRM Backend

```
┌─────────────────────────────────────────────────────────────────┐
│ NRM BACKEND INTERFACE (see local-inference.md)                  │
│                                                                 │
│  trait NpuBackend {                                             │
│      fn init(&mut self) -> Result<(), Error>;                   │
│      fn get_info(&self) -> NpuInfo;                             │
│      fn allocate_context(&self, size: usize) -> ContextHandle;  │
│      fn submit_graph(&self, ctx: ContextHandle, graph: Graph)  │
│          -> Result<SubmissionHandle, Error>;                    │
│      fn wait(&self, handle: SubmissionHandle) -> Result<()>;    │
│      fn map_buffer(&self, handle: BufferHandle) -> *mut u8;     │
│      fn unmap_buffer(&self, handle: BufferHandle);              │
│  }                                                              │
└─────────────────────────────────────────────────────────────────┘
         ▲                    ▲                    ▲
         │                    │                    │
    ┌────┴────┐         ┌────┴────┐         ┌────┴────┐
    │  RKNN   │         │  QNN    │         │  XDNA   │
    │ Backend │         │ Backend │         │ Backend │
    └────┬────┘         └────┬────┘         └────┬────┘
         │                   │                   │
    ┌────┴────┐         ┌────┴────┐         ┌────┴────┐
    │ rknpu   │         │ qcom_qnp│         │ xdna    │
    │ kernel  │         │ kernel  │         │ kernel  │
    │ driver  │         │ driver  │         │ driver  │
    └─────────┘         └─────────┘         └─────────┘
```

### Per-Vendor Status

| Vendor | Kernel Driver | Userspace | Axiom Backend | Effort |
|--------|---------------|-----------|---------------|--------|
| **Rockchip** | rknpu (staging) | RKNN | 🟢 WIP | Medium |
| **Qualcomm** | qcom_qnp (RFC) | QNN | 🟡 Planned | High |
| **AMD** | xdna (6.8+) | Vitis AI EP | 🟢 Prototype | Low |
| **Intel** | ivpu (6.10+) | OpenVINO | 🟢 Prototype | Low |
| **Google** | edgetpu | TFLite | 🟡 Planned | Medium |
| **Apple** | ❌ Closed | Metal/MPS | 🔴 **Research** | Very High |

**Apple strategy:** Use Metal Performance Shaders (MPS) + CoreML via userspace. No kernel driver needed — MPS runs in app process. NRM backend wraps MPS graph execution.

---

## Sensor HAL (P1 Input Server)

### Sensor Types

| Sensor | Interface | Typical Use |
|--------|-----------|-------------|
| Accelerometer | IIO / HID | Orientation, shake, step count |
| Gyroscope | IIO / HID | Rotation, gesture |
| Magnetometer | IIO / HID | Compass, heading |
| Ambient Light | IIO | Auto-brightness |
| Proximity | IIO | Screen off during call |
| Barometer | IIO | Altitude, floor detection |
| GPS/GNSS | NMEA / proprietary | Location |
| Camera (ToF) | V4L2 | Depth, gesture |
| Microphone | ALSA / PDM | Voice, audio |
| Touchscreen | HID / I2C | Input |
| Pen / Stylus | HID / USB | Input + pressure/tilt |
| Eye Tracker | Custom / USB | Gaze input |
| Fingerprint | SPI / USB | Auth |
| Face ID | Custom | Auth |

### HAL Abstraction

```rust
trait Sensor {
    fn id(&self) -> SensorID;
    fn kind(&self) -> SensorKind;
    fn sampling_rates(&self) -> Vec<u32>;  // Hz
    fn enable(&mut self, rate_hz: u32) -> Result<(), Error>;
    fn disable(&mut self);
    fn read(&mut self) -> Result<SensorEvent, Error>;
    fn batch(&mut self, max_latency_ms: u32) -> Result<(), Error>;  // FIFO
}

struct SensorEvent {
    sensor: SensorID,
    timestamp: Timestamp,   // Monotonic, ns
    data: SensorData,
}

enum SensorData {
    Accel { x: f32, y: f32, z: f32 },
    Gyro { x: f32, y: f32, z: f32 },
    Mag { x: f32, y: f32, z: f32 },
    Light { lux: f32 },
    Proximity { cm: f32 },
    Pressure { hpa: f32 },
    Location { lat: f64, lon: f64, accuracy_m: f32 },
    // ...
}
```

### Fusion (P1 → P5)

```
Sensor HAL (P1)          Input Bus (P5)           Compositor (P5)
    │                         │                        │
    │ SensorEvent stream ────►│                        │
    │                         │ Fusion:                │
    │                         │  • Orientation filter  │
    │                         │  • Step detection      │
    │                         │  • Gesture recognition │
    │                         │                        │
    │                         │ FusedEvent ──────────►│
    │                         │                        │
```

**Privacy:** Raw sensor data never leaves P1. Only fused events (orientation, step, gesture) delivered to apps via capability.

---

## Power Management

### CPU

| Feature | Implementation |
|---------|----------------|
| **Idle states** | PSCI / ACPI C-states |
| **Frequency scaling** | CPPC / AMD P-state / Intel HWP |
| **Core parking** | Scheduler hint + firmware |
| **Thermal** | IPMI / ACPI thermal zones → governor |

### GPU/NPU

| Feature | Implementation |
|---------|----------------|
| **DVFS** | Vendor-specific (simple sysfs/ioctl) |
| **Power gating** | Runtime PM (kernel) + NRM hints |
| **Thermal** | Vendor thermal zones → NRM throttling |

### Display

| Feature | Implementation |
|---------|----------------|
| **Panel self-refresh** | DRM PSR |
| **Variable refresh** | DRM VRR |
| **Backlight** | PWM / I2C / DPCD |

---

## Peripheral Support Matrix

| Peripheral | P0 Kernel | P1 Server | Notes |
|------------|-----------|-----------|-------|
| NVMe | ✅ nvme driver | Storage server | |
| UFS | 🟡 ufs driver | Storage server | Mobile |
| eMMC | ✅ mmc driver | Storage server | |
| SD/MMC | ✅ mmc driver | Storage server | |
| USB 2/3/4 | ✅ xhci | USB server | Host + Device |
| Thunderbolt | 🟡 nhi | TBT server | Security levels |
| PCIe | ✅ pcie | PCIe server | Root complex |
| Wi-Fi 6/6E/7 | 🟡 mac80211 | Network server | Firmware blobs |
| Bluetooth 5.x | 🟡 btusb/hci | Network server | |
| Cellular (modem) | 🟡 qmi/mbim | Modem server | QRTR / MBIM |
| Audio (HD/SoC) | ✅ snd_hda/soc | Audio server | PipeWire |
| Display (DP/HDMI) | ✅ drm/kms | Display server | Atomic modeset |
| MIPI DSI | 🟡 drm/mipi | Display server | Internal panels |
| Camera (CSI/USB) | 🟡 v4l2 | Camera server | ISP in userspace |

---

## Firmware & Blobs

| Component | Source | License | Axiom Handling |
|-----------|--------|---------|----------------|
| **ARM Trusted Firmware** | Upstream | BSD | Build from source |
| **UEFI / U-Boot** | Upstream | BSD/GPL | Build from source |
| **Wi-Fi firmware** | Vendor | Proprietary | Optional package |
| **Bluetooth firmware** | Vendor | Proprietary | Optional package |
| **GPU firmware** | Vendor | Proprietary | Optional package |
| **NPU firmware** | Vendor | Proprietary | Optional package |
| **Modem firmware** | Vendor | Proprietary | Optional package |
| **Camera ISP** | Vendor | Proprietary | Optional package |

**Policy:** Core OS boots without blobs (minimal functionality). Blobs loaded by P1 servers on demand with capability gate (`Cap::FirmwareLoad`). User chooses at setup.

---

## Open Questions

See [`open/open-questions.md#hardware-support`](../open/open-questions.md#hardware-support).