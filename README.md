# ControlAnything

Control your whole PC from a single application. Today it works like FanControl: it reads the sensors (CPU, GPU, motherboard)
and drives the fans from curves and profiles, and it drives MSI Mystic Light RGB lighting. A privileged daemon does the
hardware work, and a GPUI interface connects to it over local IPC (named pipe on Windows, Unix socket on Linux).

## Download

Windows installers are published on the releases page: https://github.com/0XLUC4/ControlAnything/releases

## Features

- **Fans**: curves per fan, hysteresis, failsafe, per-channel names.
- **Profiles**: Silent, Balanced, Performance and Custom, shipped in `configs/controlanything-profiles.toml`.
  Hotkeys **Ctrl+Shift+F1..F4** activate profiles 1 to 4 (order of the profile list; on Linux, X11 only).
- **Presets** (Silent, Balanced, Performance) in the Fans page header apply the full recipe (curve, minimum duty,
  hysteresis, step up/down, response time) to all fans at once, identical to the shipped profiles. Pumps keep
  their safe settings. A per-fan speed range can be skipped to avoid resonance noise.
- **Automations**: switch profile while an app or game runs, time schedules (e.g. Silent from 23:00 to 07:00),
  and sleep/resume handling (fans back to firmware and lighting off during sleep, everything reapplied on resume).
- **Dashboard**: live readings, history of 2 min, 10 min or 1 h, CSV export, and desktop alerts when a
  temperature goes over a limit or a fan stalls.
- **Lighting** page (RGB, MSI Mystic Light over USB HID): global brightness, sync (same effect everywhere) or
  per-zone effects: static, breathing, rainbow, wave, temperature (color follows a temperature source), load
  (color follows a sensor), plus "off" and "leave to firmware".
- **RGB drivers** (native, no OpenRGB): MSI Mystic Light (USB HID), Corsair DDR5 memory (SMBus, via PawnIO),
  and a generic **HID LampArray** driver (Windows Dynamic Lighting standard) that covers Logitech and other
  compliant mice and keyboards. Devices with a generic HID name are labeled "{vendor} {kind}", e.g.
  "Logitech Mouse".
- **Devices** page: every detected device (lighting devices, fan channels, sensor counts per component).
  A lighting device can be released from ControlAnything so its firmware or vendor software controls it again.
  Click a lighting device to open its page: color picker, effect, brightness per device, whole device or per zone.
- **Audio** page (Windows): volume, mute and default device for every output and input.
- **Network** page (Windows): adapters, IP and MAC addresses, link speed, live upload and download rates.
- **Updates**: the update button in the title bar checks the latest GitHub release and installs it (the SHA-256
  published by GitHub is verified first).
- **Single instance**: launching the UI a second time exits immediately.

### Supported hardware (validated)

| Device | Id | Notes |
|--------|----|-------|
| MSI Mystic Light | `mystic-light/1462:7e06` | 3 zones |
| 2x Corsair DDR5 | `corsair-dram/0x19`, `corsair-dram/0x1b` | 10 LEDs per module (keeps its last color when released) |
| Logitech G502 X Plus (HID LampArray) | `lamparray/046d:4099/1.0` | Mouse, 8 lamps |
| Logitech PRO X TKL (HID LampArray) | `lamparray/046d:c35b/1.0` | Keyboard, 93 lamps |
| Nuvoton NCT6687D Super I/O | | Motherboard fans and sensors |
| NVIDIA GPU | | GPU fan and sensors |

## Architecture

| Crate | Role |
|-------|------|
| `hub-core` | Sensor/fan/RGB types, config, control engine (curves, hysteresis, failsafe), IPC protocol. No OS, no UI. |
| `hub-backend-windows` | PawnIO: Intel CPU temperature (IntelMSR), Nuvoton NCT6687D Super I/O (LpcIO). |
| `hub-backend-linux` | sysfs hwmon (`/sys/class/hwmon`). |
| `hub-rgb` | Native RGB (no OpenRGB): effects engine, MSI Mystic Light and HID LampArray drivers. |
| `hub-daemon` | `controlanythingd` binary: control loop, IPC server, Windows service. |
| `hub-ui` | GPUI interface (gpui-kit): dashboard, fans, profiles, lighting, devices. |
| `hub-app` | `controlanything` binary (the UI). |

## Requirements

- Rust >= 1.98 (`rustup` reads `rust-toolchain.toml`).
- **Windows**: MSVC Build Tools (C++), and the **PawnIO** driver: https://pawnio.eu or
  `winget install namazso.PawnIO`. The modules (`IntelMSR.bin`, `LpcIO.bin`) are in
  `assets/pawnio/`; for a release, copy them into a `pawnio\` folder next to `controlanythingd.exe`.
- **Linux** (Debian/Ubuntu):
  ```sh
  sudo apt install libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libvulkan1 \
    libfontconfig-dev libglib2.0-dev libssl-dev libzstd-dev clang cmake libgtk-3-dev \
    libayatana-appindicator3-dev libxdo-dev libudev-dev
  ```
  Sensors: see [packaging/linux/README.md](packaging/linux/README.md) (`sensors-detect`,
  `nct6687d` DKMS module for MSI boards).

## Build

```sh
cargo build --release
```

Binaries: `target/release/controlanythingd` (daemon) and `target/release/controlanything` (UI).

## Run

- Daemon in an **administrator** console (root on Linux): `controlanythingd` (or `controlanythingd run`).
  Ctrl+C stops it cleanly.
- UI (normal user): `controlanything`.

## Windows service (start at boot)

The service points to the exe as is: first install the binaries and `pawnio\` in a
stable location.

```powershell
cargo build --release
# admin console:
$dst = "C:\Program Files\ControlAnything"
New-Item -ItemType Directory -Force $dst | Out-Null
Copy-Item target\release\controlanythingd.exe, target\release\controlanything.exe $dst
Copy-Item assets\pawnio "$dst\pawnio" -Recurse -Force
& "$dst\controlanythingd.exe" install   # also removes the old PCHub service
```

`install` creates the "ControlAnything" service (auto, LocalSystem, restarts 5 s after a failure)
and starts it; `controlanythingd uninstall` stops it (fans handed back to the BIOS), then deletes it.

Service logs: `%ProgramData%\ControlAnything\logs\controlanythingd.*.log` (daily rotation, 14 files).

### Migrating from PC Hub

- `install` stops and deletes the old "PCHub" service if it exists.
- The config `%ProgramData%\PC Hub\config.toml` is copied automatically to
  `%ProgramData%\ControlAnything\config.toml` on first launch (the old one stays in place).
- The old UI autostart is disabled by the new UI.
- The `C:\Program Files\PC Hub` folder can then be deleted.

On Linux: systemd unit, see [packaging/linux](packaging/linux/README.md).

## Safety

- AIO pump: never below **60 %**.
- Failsafe: CPU >= **90 °C** or GPU >= **85 °C**: fans driven at 100 % until the temperature is back 5 °C below the threshold.
- Clean shutdown (Ctrl+C, close, power off, SIGTERM, service stop) or panic: fans handed back to the BIOS.
- Hard kill (`taskkill /F`, `kill -9`): nothing can be restored, the BIOS takes over at the next reboot.
- RGB: only volatile writes are sent (the "save to flash" byte is always 0), and only to a strict allowlist of verified boards.

## Configuration

`%ProgramData%\ControlAnything\config.toml` (Windows) or `/etc/controlanything/config.toml` (Linux), created on
first launch. An invalid file is moved aside as `config.toml.bad` and replaced by the default.

## Tests and hardware validation

```sh
cargo test --workspace
```

NCT6687D validation tools (Windows, admin console):

```powershell
cargo run -p hub-backend-windows --example nct6687_probe -- assets\pawnio\LpcIO.bin       # read only
cargo run -p hub-backend-windows --example nct6687_write_test -- assets\pawnio\LpcIO.bin  # System Fan 1: 100 %, 40 %, back to BIOS
```

Mystic Light validation tools: `cargo run -p hub-rgb --example mystic_light_probe` (read only) and
`cargo run -p hub-rgb --example mystic_light_test -- --yes-write` (write test).

## Packaging

- Windows (NSIS installer, installs and starts the service): `powershell -ExecutionPolicy Bypass -File packaging\build-windows.ps1`
  -> `target\packager\controlanything_<version>_x64-setup.exe`.
- Linux (.deb with systemd unit): `sh packaging/linux/build-deb.sh`.

Details: [packaging/README.md](packaging/README.md).
