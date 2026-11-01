# Emulated calls (development)

GeekBridge can exercise its real Linux call stack end to end without a phone or a Bluetooth controller. An
Android emulator provides a simulated cellular modem; a small simulator turns that modem's call state into the
D-Bus API that PipeWire would expose for a paired phone; and the unmodified `bridge-calls` desktop code drives
it. This is a development and regression tool. **It does not place or receive real calls, and no
Bluetooth, HFP or audio path is involved.**

## Architecture

```
 Android emulator (AVD gbcall, serial emulator-5584)
 +--------------------------------------+
 | simulated modem (call state machine) |   <- remote party: `adb emu gsm call|accept|cancel`
 +------------------+-------------------+      user side:    `adb shell input keyevent CALL|ENDCALL`
                    |  adb (server port 16037)
                    v
 +--------------------------------------+
 | geekbridge-phone-sim                 |  AdbModem: polls `dumpsys telephony.registry`, issues dial/answer/hang up
 |  fake org.pipewire.Telephony service |  claims the bus name on a PRIVATE bus (dbus-run-session)
 |  (AudioGateway1, AudioGatewayTransport1, Call1)
 +------------------+-------------------+
                    |  D-Bus (private session bus, never the real one)
                    v
 +--------------------------------------+
 | bridge-calls: PipeWireBackend        |  the real desktop code, unchanged
 |             -> Session (state machine, guard rails, events)
 +------------------+-------------------+
                    v
        service / Calls app (consumers of Session)

 geekbridge-call-e2e is the scripted consumer of Session (on the same private bus) and, through adb, the
 remote party that rings, accepts and hangs up.
```

In `fake` mode the emulator, adb and the process boundary disappear: `geekbridge-call-e2e --fake` starts its own
private `dbus-daemon`, an in-process simulator and an in-memory `FakeModem`. Everything from the D-Bus service
upward is the same code.

## What is and is not verified

| Verified (signalling and state machine) | Not verified (not emulated) |
| --- | --- |
| `bridge-calls` discovers the telephony service, reads calls and tracks their state changes | Bluetooth pairing, bonding, trust, discovery |
| Incoming call: ringing, answered, rejected, missed | HFP service-level connection and AT command exchange (`RING`, `CLCC`, `ATD`, `ATA`, `CHUP`, ...) |
| Outgoing call: dialling, connected, cancelled by the user | SCO / eSCO audio links, codec negotiation (CVSD, mSBC, LC3) |
| Guard rails refuse bad commands (invalid numbers, wrong call, no active call) without touching the modem | Real phones, real carriers, real cellular networks |
| Loss of the telephony service during an active call is surfaced (S7: fake mode, or emulator with `--spawn-sim`) | The PipeWire/WirePlumber/BlueZ telephony implementation itself |
| Number validation (digits and `+ * #`, bounded length) and redaction in logs | Latency, audio quality, echo, or anything a person would hear |
| The Android emulator's modem state machine accepts the scripted transitions | The Android companion app's (future) transport to the desktop |

A pass means: "given a modem that behaves like the emulator's, the Linux call stack produces the right
sequence of states and refuses misuse". It does not mean that a phone paired over Bluetooth will work. No real
phone call has been placed with GeekBridge and nothing in this repository is hardware-verified. The manual
hardware checklist in [call-audio-options.md](call-audio-options.md) (pairing, audio
in both directions, codec, latency) remains entirely open.

The simulator's `org.pipewire.Telephony` objects follow the *declared* PipeWire interface (see the calling
implementation notes). Whether a real PipeWire release exposes exactly the same shapes has not been checked on
hardware.

## Scenarios

The harness picks its behaviour from the mode, not by probing. The in-memory fake lets the "remote party"
do everything; the Android emulator's modem cannot, so S4 and S5 mean different things there (measured
behaviour: see below). In emulator mode the modem **auto-answers
a call the desktop places after about 2 s** and cannot be accepted or hung up from the console. Every
scenario first forces the modem idle and reports any call it did not start as `ENVIRONMENT`.

| Id | Fake mode | Emulator mode |
| --- | --- | --- |
| S1 | Incoming call rings, desktop answers, session shows Active with the (declared) audio link, event stream replays to the live state, desktop hangs up: `Completed` | Same. A lost Answer key press is detected and re-sent. |
| S2 | Incoming call rejected by the desktop: modem Idle, `NotAnswered` | Same. A reject the modem ignores while the call is still starting to ring is re-sent. |
| S3 | Missed: the remote party gives up while it rings: `NotAnswered`, no command sent, never Active | Same (the remote hangup of an *incoming* call works through the console). |
| S4 | Desktop dials, the remote party accepts, the remote party hangs up: `Completed` | Desktop dials, session shows Dialing, the modem auto-answers (about 2 s) to Active, the **desktop** hangs up: `Completed`. No remote accept or hangup is possible. |
| S5 | Desktop dials, hangs up before the remote answers: `OutgoingNotConnected` | Desktop hangs up inside the roughly 2 s dialing window; if the auto-answer wins the race the attempt is reported as `RACE lost` and retried (up to 3 attempts). |
| S6 | Guard rails: Answer on an active call, unknown call id, letters/empty/over-long number, SwapCalls with nothing held, MuteRinger and DisconnectAudio (unsupported), reject on an active call are all refused locally, and the call stays Active | Same |
| S7 | Simulator service stopped during an active call: Link Down, call ends `LinkLost`, later commands refused | Only with `scripts/emulate-call.sh emulator --spawn-sim` (the harness must own the simulator). The default emulator mode **skips S7**. |
| S8 | The **service call hub** (`bridge_service::call_control::CallHub` with the Linux HFP host, the exact view the Calls window renders) on its own `Session`: ring, the hub offers Answer and Decline over Bluetooth, Decline reaches the modem (Idle); ring again, Answer makes the modem Active, the view shows Hang up and the audio route, Hang up ends it; a decision on an ended call is refused and sends nothing; the harness's own session sends no commands | Same (input re-send as in S1/S2) |

Where a step says "audio link connected (codec Cvsd)", that is the simulator's *declared*
`AudioGatewayTransport1` state, set when a call becomes active. No SCO link exists and no audio flows.

Default fictional numbers: `5551234` in fake mode, `5557701` against the emulator (another tool driving the
same emulator also used `5551234`, and two parties on one number corrupt each other's runs).

## Running

Prerequisites: a Rust toolchain, `dbus-run-session` and `dbus-daemon` (the `dbus` package). Nothing is
installed or changed globally; the script only starts and stops processes it created itself.

### Fake mode (no emulator, CI-safe)

```sh
scripts/emulate-call.sh fake                # all scenarios S1-S8
scripts/emulate-call.sh fake --only S1,S4   # a subset
scripts/emulate-call.sh fake --json         # machine-readable result
```

It builds with `cargo build --locked`, runs the harness under `dbus-run-session` and exits with the harness
status: `0` pass, `1` a scenario step failed, `2` usage or environment error (missing dbus tools, build
failure). The same command runs in CI on Linux with `GEEKBRIDGE_REQUIRE_DBUS=1`
(`.github/workflows/check.yml`). `CARGO_TARGET_DIR` is honoured.

### Emulator mode

```sh
scripts/emulator-call.sh start
scripts/emulator-call.sh wait               # until .tools/gbcall/STATUS says ready
scripts/emulator-call.sh status

scripts/emulate-call.sh emulator            # S1-S6 (S7 is skipped)
scripts/emulate-call.sh emulator --spawn-sim  # harness runs the simulator in-process: S1-S8
scripts/emulate-call.sh emulator --only S1  # a subset
```

The script requires `scripts/emulator-call.sh status` to succeed, then, under `dbus-run-session`, starts
`geekbridge-phone-sim` against the emulator in the background, waits for it to own `org.pipewire.Telephony`,
and runs `geekbridge-call-e2e`. Logs are written to `dist/android/logs/emulate-call-<timestamp>/`
(`simulator.log`, `e2e.log`) and kept. In fake mode logs go to a temp directory that is deleted after a pass
and kept after a failure (`--keep-logs` keeps it always, `--log-dir DIR` chooses it).

Environment overrides: `ADB` (adb binary; default `.tools/android-sdk/platform-tools/adb`, then `adb` on
`PATH`), `GB_EMU_SERIAL` (default `emulator-5584`), `GB_EMU_ADB_PORT` (default `16037`).

To drive the pieces by hand, see `geekbridge-phone-sim --help` (`--fake` reads `ring N`, `accept N`,
`cancel N`, `state` on stdin) and `geekbridge-call-e2e --help` (`--spawn-sim` runs the simulator in-process
over the emulator, which also enables S7 against it; `emulate-call.sh emulator --spawn-sim` does this). How the emulator's modem behaves, and the quirks of
driving it through `adb`, is documented in this file. The Android
companion's call-state display is described in [Android call state](android-calls.md).

## Status and known limitations (2026-11-01)

- **Fake mode** passes S1-S8 consistently (about 3 s in total) and is the CI gate.
- **Emulator mode** reached S1-S6 PASS three times in a row from the harness author's runs, with per-scenario
  timings varying from about 1.4 s to 11.7 s. It is **not deterministic**. Our own runs the same day were
  mixed: one default run failed S1-S3, one `--spawn-sim` run passed S1-S7, and one default run passed S1-S5
  and failed S6 (`Session::reject` on an active call returned `NoSuchCall` instead of the expected
  refusal). Treat a single emulator failure as something to read in the logs and re-run, not as proof of a bug.
- A later run on the merged tree with the call-control work (`scripts/emulate-call.sh emulator --only S2,S8`) passed both
  (S8: Decline 0.9 s, Answer 3.6 s, Hang up 0.8 s on the emulator modem). One run is not evidence of stability.
- Known causes of emulator flakiness:
  - **Lost early Answer/reject key presses.** The emulator's Telecom can ignore a key sent in the first
    seconds after the modem registers a ring. The harness re-sends after 2.5 s, and `AdbModem` is being
    hardened to verify and retry; until then an `Answer` that never reached the modem shows up as "the modem
    is Idle after Answer".
  - **Dropped console rings.** A `gsm call` can be lost and needs a re-ring; the harness re-rings up to twice
    when no incoming call appears.
  - **Ghost calls.** Calls left behind by an earlier run or started by another tool are cleared at the start of
    each scenario and reported with an `ENVIRONMENT:` prefix rather than counted as a product failure.
  - **Shared numbers.** Several tools drive this one emulator; two parties using the same number cancel each
    other's calls. Use a distinct `--number` per tool, and do not run two harnesses at once.
- The telephony registry cannot distinguish dialling from active (`mCallState` is "offhook" for both), so
  the simulator infers it.

Fake mode is the CI gate; emulator mode is a developer check. Neither is hardware verification.

## Troubleshooting

- **`emulator is not ready`**: run `scripts/emulator-call.sh status`. `booting` means wait (cold boot takes
  minutes without KVM); `failed: ...` carries the reason; see `.tools/gbcall/emulator.log`.
- **adb cannot see the device / `no devices/emulators found`**: the dedicated emulator uses its own adb server
  on port **16037** so it does not disturb another adb server on 5037. Every adb call must use that port:
  `ANDROID_ADB_SERVER_PORT=16037 adb devices` (the script passes `--adb-port` for you; override with
  `GB_EMU_ADB_PORT`). The emulator console is port 5584, hence serial `emulator-5584`. If you changed either
  value, set `GB_EMU_SERIAL` and `GB_EMU_ADB_PORT` to match.
- **Disk or `/tmp` quota errors** (emulator will not start, `No space left on device`, cargo fails with
  `Disk quota exceeded`): `/tmp` may be a small tmpfs. Free space there, or point `TMPDIR` and
  `CARGO_TARGET_DIR` at a larger directory. The AVD and logs live under `.tools/`, not `/tmp`.
- **`note: org.pipewire.Telephony is owned on the real session bus`**: real PipeWire/WirePlumber with
  Bluetooth telephony is serving that name on your desktop. This is only a note: the script never connects to
  that bus. The simulator and harness use a private bus from `dbus-run-session`, and the script refuses to start
  the simulator if the name is already owned *on that private bus*. If you run `geekbridge-phone-sim` by hand,
  always pass `--bus` with a private address; without `--bus` it would use your real session bus (it exits
  with code 3 if the name is taken, and must not be run to compete with real PipeWire).
- **`dbus-daemon not found` / `dbus-run-session not found`**: install the distribution's `dbus` package.
- **Simulator exits with code 3 (`NameTaken`)**: another service owns the name on the bus it was pointed at.
  Only possible if you run the simulator by hand.
- **Steps time out in emulator mode**: raise the per-step deadline by running the harness directly with
  `--timeout SECS`, and check `simulator.log` for adb errors (device offline, wrong serial).
- **Scenario S7 shows `skip`** in emulator mode: expected; run `scripts/emulate-call.sh emulator --spawn-sim` so the harness owns the simulator.

## Security notes

- Development only. The simulator impersonates a PipeWire service. Never run it on a bus that real PipeWire
  or a real desktop session owns; the wrapper script runs everything on a private bus created by
  `dbus-run-session`, and refuses to start the simulator if `org.pipewire.Telephony` is already owned on
  that private bus. It never connects to your real session or system bus.
- Phone numbers are validated (digits and `+ * #`, 1-32 characters) before they reach an `adb` command line, and
  numbers are redacted (`<number>`) in error text and logs. The harness uses fictional numbers (default
  `5551234`) and prints only their last two digits.
- Nothing listens on the network. The simulator talks to a private D-Bus socket and to adb on loopback; adb's
  server port 16037 is localhost-only. The emulator is not given any real SIM, account or personal data.
- The script uses no `pkill`; it only signals the child processes it started, and cleans them up on exit,
  interrupt or termination.
- The shipping bridge-calls code does not link the simulator crate; `bridge-phone-sim` is only used by
  tests and these scripts.
