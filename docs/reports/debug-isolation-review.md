# Debug diagnostics isolation and reconnect review

Scope: `android/app/src/debug/**`, `DebugHooks.kt`, `crates/bridge-core/src/debug_link.rs`, `phone_debug.rs`, the
debug gating in `gate.rs`/`daemon.rs`, `geekbridge-ctl phone-log|phone-state|consent`, and
`crates/bridge-service/src/reconnect.rs`.

## 1. Release build contains no debug extras (verified on a fresh artifact)

`gradle assembleRelease` was rebuilt (the older APK predated the diagnostics) and inspected with `aapt2` and `strings`:

- Manifest: 20 permissions, all from `src/main`. No `WRITE_CONTACTS`, `DUMP`, `READ_LOGS`. (`WRITE_CALENDAR` is in the main
  manifest for the opt-in calendar editing feature; the debug manifest's copy is not present.) No `debuggable`, no
  `networkSecurityConfig`, no cleartext. Components are only the 5 activities-with-launcher/share/pair, services, boot
  receiver, SMS receiver, tile, widgets and file providers of `src/main`. `DebugControlReceiver`, `ScaleSeedReceiver`,
  `DebugBootProvider`, `DesignGalleryActivity`, `DebugPinWidgetActivity` are absent.
- Dex: none of `DebugDiagnostics`, `DebugLogStreamer`, `DebugRedactor`, `logcat`, `DebugSettings`, `DebugState*` appear.
  Only residue: the inert prefix literal `geekbridge.debug.` in `PhoneDataEngine` (routes to `DebugHooks.onDataEvent`, which
  is never set in release, so the packet is dropped).
- Native lib: contains the packet-name strings (shared Rust core), but the transport declares them only when the
  `debug` flag is set, and only `DebugDiagnostics` (debug source set) sets it (`DebugHooks.streaming` stays false). Existing
  test `incoming_types(on())` asserts no `debug` kind is declared without the flag.
- New guard: `scripts/check-release-isolation.sh [APK]` fails on any of the above. Passes on the release APK, fails
  (as it should) on the debug APK.

## 2. Unpaired / non-consented peers are rejected

Chain: only `trusted_loop` (post-pairing, pinned TLS) calls `PhoneDebug::link_up`; `gate::classify_incoming` yields
`Incoming::Debug` only for a kind the peer declared and we negotiated; `on_log`/`on_state` check consent first, then
the registered link, rate limit, strictly increasing `seq`, size and shape; packets with payloads are Invalid. Files are
written 0600 in a 0700 dir, O_NOFOLLOW, device id restricted to `[A-Za-z0-9_-]`, rotated at 5 MiB x 4. `phone-state`
needs consent, a connected debug-declaring phone, one outstanding request, an 8 s timeout and matches `requestId`.
`geekbridge-ctl` validates the device id before building any path.

Added tests (`phone_debug.rs`): a peer with no registered link (unpaired, or link gone) stores nothing and is not
"offered" even when consent is true; a release phone / no consent / not connected yields the right `AskError`.

Accepted behaviour: consent for `debug` is switched on once by default for a *paired* phone that declares the packets
(only a debug build does); the user's later "off" sticks (`default_debug_on`). Bounded by the limits above.

## 3. Secret leakage

The phone redacts before buffering (`DebugRedactor`, also applied again to the state dump), logs are fed only by
`GbLog` (reasons and counts, class names only for exceptions) and the own-pid logcat. Existing tests cover names,
numbers, mail, IPs, MAC/IPv6, UUID/hex, storage paths, SMS/clipboard "body/text" keys.

Gap found and fixed: credentials were not redacted (`token=...`, `Authorization: Bearer ...`, `password`, `secret`,
`api_key`, pairing/verification codes, `pin`, JWTs, long base64-like blobs). `DebugRedactor` now removes the value after
any key containing token/secret/password/passphrase/authoriz*/cookie/api_key/private_key/verification_code/pairing_code,
after `pin`/`otp`/`auth`, after `Bearer`/`Basic`, JWTs, and any 24+ char letter+digit run. `code=peer_closed`-style
diagnostic lines stay readable. Tests: `tokensAndSecretsNeverSurviveRedaction`, `diagnosticCodesStayReadable`.

## 4. Other fixes

- `DebugControlReceiver` and `ScaleSeedReceiver` were `exported=true` with no permission, so any app on a debug phone
  could flip consents/grants (`grant`) or write contacts. They now require `android.permission.DUMP` (held by the adb
  shell, so the e2e scripts keep working; not verified on a device here).
- `sanitize_line`/`parse_state` did not neutralise bidi overrides, zero-width marks or BOM (not "control" characters),
  letting a hostile phone disguise lines shown by `phone-log -f`. Now replaced with `?`. Test:
  `bidi_overrides_and_zero_width_marks_cannot_disguise_a_line`.

## 5. PC auto-reconnect (`reconnect.rs`, `Daemon::reconnect_tick`)

No defect found. Only phones present in the pairing store are dialed; the address policy and the KDE port range are
enforced by `check_endpoint`; the pinned certificate is verified on the dial (spoofed announcements cannot authenticate);
back-off 2 s doubling to 60 s, 12 tries, address flips do not reset the budget (test), manual disconnect pauses until
the phone has been silent, unpaired phones are never dialed (lifecycle test). Residual: a spoofed announcement from a
local-network host can make the PC open at most 12 TCP connections to an address on its permitted LAN range and
KDE port range; that is bounded and inherent to LAN discovery.

## Not verified

On-device behaviour of the DUMP permission and a runtime test of the redactor against real logcat output were not run;
Android JVM unit tests (`DebugDiagnosticsTest`) pass.
