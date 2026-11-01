# Android companion integration contract

The product includes an Android companion APK and separate desktop Rust apps. The Android app uses Kotlin for framework APIs and a Rust native library for shared domain rules. Real pairing/phone interoperability is still being implemented independently.

## Build pins for the initial Android project

- Application ID/package: `dev.geekbridge.android`.
- Android min SDK 26, compile/target SDK 36.
- Android Gradle Plugin 8.13.2, Gradle 8.13, Kotlin Android plugin 2.2.21, JDK 17.
- Native crate: `bridge-android`; Rust library name `geekbridge_android`.
- Embedded ABIs initially `arm64-v8a` and `x86_64` (phone and emulator).
- Tooling lives under ignored `.tools/`; generated artifacts under ignored `dist/`.
- Do not change the developer's global Java/Rust/Android setup.

## Kotlin/JNI boundary

```kotlin
package dev.geekbridge.android
object NativeBridge {
    init { System.loadLibrary("geekbridge_android") }
    external fun nativeRequest(request: String): String
}
```

JNI symbol: `Java_dev_geekbridge_android_NativeBridge_nativeRequest`; receiver is a Kotlin object instance. The function never throws a Rust panic through JNI. Input/output are bounded JSON strings. The native crate exposes the same request handler for host unit tests.

Requests carry `version: 1`, a string `action`, and action arguments. Responses use `{ "version": 1, "ok": true, "result": ... }` or `{ "version": 1, "ok": false, "error": { "code": "...", "message": "..." } }`.

Initial actions:

- `version`: no arguments; result includes Rust core version and supported request schema.
- `validate_sms`: `recipient` and `text` strings plus optional numeric `subscription_id`; validate with `bridge-core::messages::OutgoingTextIntent`. This validates only; never sends.
- `validate_clipboard`: `text` string; bounded UTF-8/non-empty/NUL checks matching shared clipboard defaults. This validates only; never reads or writes the clipboard.

Never expose keys, cached phone content, or full untrusted input in errors or diagnostics. Unknown action/fields/version and malformed/oversized input must fail predictably. Parsing or validation success never implies paired/trusted/call-ready.

## Ownership while agents work

- Android app agent: all `android/**`, plus `docs/android-app.md`.
- Rust/JNI agent: all `crates/bridge-android/**`, plus `docs/android-native.md`.
- Toolchain agent: ignored `.tools/**`, `scripts/bootstrap-android.sh`, `scripts/android-env.sh`, `scripts/build-android.sh`, `scripts/build-desktop.sh`, `docs/builds.md`. May write generated `.so`/APKs under ignored app build/jniLibs directories only; no source edits in Android/Rust crates.
- Parent: workspace root, this contract, integration and final validation.

## Share actions (file / link / text sharing)

Added for the "Send to computer" Share-sheet target and for receiving from a paired computer. They use the
KDE Connect `share` plugin (`kdeconnect.share.request`), now advertised in both directions by the transport.
All of them need a running transport (`transport_start`) and a **trusted** connection; otherwise `not_running` /
`not_trusted`. File bytes are never passed through JSON or held in memory: Kotlin hands Rust a **file descriptor**.

File descriptor rule: `fd` is a plain integer from `ParcelFileDescriptor.getFd()`. Rust **duplicates** it
(`dup`/`F_DUPFD_CLOEXEC`) before the call returns, so Kotlin keeps ownership of its own descriptor and must close
its `ParcelFileDescriptor` itself (immediately after the call is fine). An invalid descriptor fails with
`invalid_fd`. Rust never closes a descriptor it did not duplicate.

| action | fields | result |
| --- | --- | --- |
| `share_send_file` | `connection_id` (u64), `fd` (read, ≥ 0), `name` (display name, ≤ 255 chars; sanitised by the peer too), `size` (u64 ≥ 1, bytes; a provider that cannot report a size cannot be sent), optional `last_modified_ms` (u64) | `{transfer_id}` |
| `share_send_text` | `connection_id`, `kind` (`"text"` \| `"url"`), `text` (≤ 48 KiB; url ≤ 4 KiB) | `{queued:true}` |
| `share_accept` | `transfer_id`, `fd` (a descriptor **opened for writing**, e.g. a pending MediaStore row) | `{accepted:true}` |
| `share_decline` | `transfer_id` | `{declined:true}` |
| `share_cancel` | `transfer_id` (incoming or outgoing, any live state) | `{cancelled:true}` |
| `share_dismiss` | `transfer_id` (a finished/failed/received entry) | `{dismissed:true}` |

Errors (fixed messages): `not_running`, `unknown_connection`, `not_trusted`, `unknown_transfer`, `wrong_state`,
`invalid_fd`, `invalid_field`, `too_large` (over the 8 GiB cap), `empty`, `busy` (too many transfers: at most 3 active, 32 listed),
`unsupported_platform`.

`transport_status` gains `transfers` (≤ 32, newest last):

```jsonc
{ "transfer_id": 5, "connection_id": 2, "device_id": "…", "peer_name": "Desk",
  "direction": "incoming" | "outgoing",
  "kind": "file" | "text" | "url",
  "name": "report.pdf",            // file only; already reduced to a safe single component
  "size": 1048576,                  // null when the sender announced an unknown length
  "bytes": 524288,                  // transferred so far
  "state": "offered" | "waiting" | "active" | "done" | "failed" | "cancelled" | "declined",
  "code": "timeout",                // only on failed: cancelled|timeout|too_large|io_error|tls_failed|pin_mismatch|truncated|port_rejected|no_port|expired
  "text": "…"                       // text/url only, state "done" (= received); ≤ 16 KiB, untrusted
}
```

Lifecycle: an incoming file arrives as `offered` and is never touched until Kotlin calls `share_accept` with a
writable descriptor (or `share_decline`; unanswered offers expire after 5 minutes). Incoming `text`/`url` appear
directly as `done` entries and are removed with `share_dismiss`. Outgoing: `waiting` until the computer
connects to the payload socket, then `active`, then `done`. Kotlin must finish the MediaStore row
(`IS_PENDING = 0`) only after the transfer reports `done`, and delete the row for `failed`/`cancelled`.
Entries are kept for up to 10 minutes after they finish.

## Phone data relay (contacts, calendar, SMS)

Wire contract: `docs/phone-data-link.md`. Rust relays an allow-listed set of packets and interprets none of them.

* `transport_start` takes `data: {contacts?, calendar?, sms?}` (booleans, default false). It decides the declared
  capabilities; changing it needs a restart. `transport_data_config {contacts?, calendar?, sms?}` toggles the runtime
  flags (an off feature drops its packets and discards its queued requests).
* `transport_data_wait {since?, timeout_ms?}` (max 25 s) -> `{events:[{seq, connection_id, kind, body}], seq, stopped,
  contacts, calendar, sms}`. Desktop requests of enabled features; queue of 64, oldest dropped, events expire after 60 s
  and are never delivered twice (so a restarted waiter cannot replay an SMS send).
* `transport_data_send {kind, body, connection_id?}` -> `{queued}`. Errors: `kind_not_allowed`, `data_disabled`,
  `too_large` (serialized body > 1 MiB), `not_connected`, `rate_limited` (burst 20, 2/s sustained), `busy`.
  This is the only request allowed above the 16 KiB transport cap (`MAX_DATA_REQUEST_BYTES` = 1 MiB + 2 KiB, also the
  JNI input bound). While any data feature is on, the link packet-line limit is 1 MiB + 16 KiB in both directions.
