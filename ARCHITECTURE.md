# Architecture for newcomers

A one-page map. The long form, including the research and design reasoning, is [docs/architecture.md](docs/architecture.md);
protocol and platform details live in the topic documents linked below.

GeekBridge connects an **Android phone** to a **desktop** (Linux first; Windows and macOS are type-checked and partly
implemented) over the local network, using the KDE Connect LAN protocol (version 8) implemented independently in Rust. The
phone is a Kotlin app with a small Rust core; the desktop is a headless **service** plus several small **egui apps**.

## Crate map (`crates/`)

| Crate | What it is |
|---|---|
| `bridge-core` | Shared domain types and rules (pairing help, clipboard, share, calls, messages, agenda, phone data), the local IPC/discovery types and state-file helpers. No networking. |
| `bridge-kde` | KDE Connect protocol 8: UDP identity, TCP/TLS handshake in either role, certificate-pinned pairing (including QR pairing), packets, payload transfers, SMS and share packets. |
| `bridge-service` | `geekbridge-service` (the daemon) and `geekbridge-ctl` (CLI). Discovery, links to paired phones, the loopback control/read APIs, the per-feature consent gates, clipboard, notifications, tray, autostart, Hyprland/Waybar hotkey integration, `diagnose`. |
| `bridge-apps` | The desktop windows as separate executables sharing one egui shell: `geekbridge` (home), `-calls`, `-messages`, `-contacts`, `-agenda`, `-clipboard`, `-share`. They talk to the service over loopback HTTP; no protocol code. |
| `bridge-calls` | Call control over Bluetooth HFP: Linux via PipeWire's `org.pipewire.Telephony` on D-Bus; other OSes build an explicit "unavailable" backend. |
| `bridge-calls-windows` / `bridge-calls-macos` | Native call backends for Windows 11 and macOS (host-OS only; type-checked from Linux). |
| `bridge-android` | The phone's native library (`libgeekbridge_android.so`): a bounded JSON request dispatcher over `bridge-core` rules plus the phone-side transport, exported through JNI. |
| `bridge-update` | Update check against the latest GitHub Release and SHA-256-verified download. Never installs anything. |
| `bridge-phone-sim` | Test-only simulated phone (fake `org.pipewire.Telephony` on D-Bus, Android emulator modem) for end-to-end call tests without Bluetooth. |
| `bridge-chaos`, `bridge-fuzz` | Test-only: TCP fault-injection proxy; fuzz targets and a deterministic mutation fuzzer for network-facing parsers (`fuzz/` is the cargo-fuzz harness). |

Outside `crates/`: `android/` (Gradle project, package `dev.geekbridge.android`, single `:app` module, plain Android views),
`scripts/` (build, packaging, e2e, perf and release helpers), `packaging/` (Linux, Windows, macOS, Arch), `docs/`.

## Data flow, phone <-> desktop

```text
 Android app (Kotlin)                                   Desktop
 ┌──────────────────────────┐                           ┌─────────────────────────────────────────┐
 │ Activities, LinkService  │      UDP 1716 identity    │ geekbridge-service  (bridge-service)     │
 │ *Protocol.kt / *Engine.kt│ <───────────────────────> │   discovery ── bridge-kde ── TLS links   │
 │   │ JNI                  │      TCP/TLS 1716-1764    │   consent gates, clipboard, calls, data  │
 │ libgeekbridge_android.so │ <═══════════════════════> │   │ loopback control (17381) + read (17380)
 │ (bridge-android +        │   certificate-pinned,     │   ▼                                      │
 │  bridge-kde transport)   │   per-feature opt-in      │ geekbridge-ctl, tray, egui apps          │
 └──────────────────────────┘                           │ (bridge-apps)    Bluetooth HFP: bridge-calls
                                                        └─────────────────────────────────────────┘
```

1. **Discovery** only announces who is on the network; it never grants trust.
2. **Pairing** is explicit and approved on both sides; both TLS certificates are pinned. Only the exact pinned
   certificate gets a trusted link ([pairing.md](docs/pairing.md)).
3. **Features** (clipboard, files, calls, contacts, calendar, SMS) are each off until allowed for a specific phone on
   both ends. The service enforces its side (`bridge-service/src/gate.rs`); the phone enforces its own.
4. **Desktop UI** never speaks the protocol: apps call the service's loopback API and render state. Calls audio is
   Bluetooth between phone and computer, outside this link; the link carries call signalling and control.

## Where to start

| I want to... | Look at |
|---|---|
| Understand or change the wire format | `crates/bridge-kde/src/packet.rs`, `link.rs`, `tls.rs`; [kdeconnect-interop.md](docs/kdeconnect-interop.md) |
| Change pairing / QR pairing | `crates/bridge-kde/src/pairing.rs`, `qrpair.rs`; `bridge-service/src/pairing_flow.rs`; [pairing.md](docs/pairing.md); phone: `PairPanel.kt`, `QrPairing.kt` |
| Add or change a synced feature | domain rules in `bridge-core`; transport in `bridge-kde`; service side in `bridge-service` (`phone_data.rs`, `clipboard.rs`, `share.rs`, `gate.rs`); phone side in the matching `*Protocol.kt` / `*Engine.kt`; UI in `bridge-apps/src/apps` |
| Change a desktop window | `crates/bridge-apps/src` (`shell.rs`, `theme.rs`, `nothing.rs` for the visual language; [design-desktop.md](docs/design-desktop.md)) |
| Change the phone UI | `android/app/src/main/java/dev/geekbridge/android` (`HomeActivity.kt`, `SetupActivity.kt`, `UiKit.kt`, `Dot*.kt`) |
| Change what the phone's Rust core validates | `crates/bridge-android/src/dispatch.rs`; contract in [android-contract.md](docs/android-contract.md) |
| Work on calls | `crates/bridge-calls`, `bridge-service/src/call_control.rs`; [call-control.md](docs/call-control.md); try without a phone: `scripts/emulate-call.sh fake` |
| Hyprland / Omarchy hotkeys and Waybar | `bridge-service/src/hypr_config.rs`, `hotkey_*.rs`, `waybar.rs`; [omarchy.md](docs/omarchy.md) |
| Updates and releases | `crates/bridge-update`, [updates.md](docs/updates.md), [builds.md](docs/builds.md), [release-checklist.md](docs/release-checklist.md) |
| Run the whole thing on one machine | `scripts/run-desktop.sh`, [real-device-quickstart.md](docs/real-device-quickstart.md), emulator scripts in [e2e-findings.md](docs/reports/e2e-findings.md) |

## Build, test, contribute

`make setup` checks your toolchain, `make check` is the Rust gate, `make android` the phone app. The details and the
contribution rules (DCO sign-off, privacy, licensing) are in [CONTRIBUTING.md](CONTRIBUTING.md); getting set up from a
fresh clone is walked through in [docs/contributor-onboarding.md](docs/contributor-onboarding.md).
