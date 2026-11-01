# Android native library (`bridge-android`)

Crate `crates/bridge-android`, Rust library name `geekbridge_android` (`libgeekbridge_android.so`).
It is the Rust side of the Kotlin/JNI boundary in `android-contract.md`: one bounded JSON request
handler over shared `bridge-core` rules, plus the JNI export Kotlin calls.

## What this is, and is not

**Implemented and tested:** stateless request validation with strict envelope rules.

- `version` — schema/limits discovery.
- `validate_sms` — runs `bridge-core::messages::OutgoingTextIntent`.
- `validate_clipboard` — runs the shared clipboard text rules (see below).

**Not implemented here:** pairing, TLS, any network or phone transport, SMS sending, clipboard
reads/writes, call handling, persistence, logging, key or contact storage. A `valid: true` response
means only "this input satisfies the shared rules". It never means paired, trusted, connected, sent
or call-ready, and no response field says otherwise. Add pairing/transport actions only when that
work exists and is reviewed; unknown actions are rejected until then.

## Crate API

```rust
geekbridge_android::handle_request(request: &str) -> String
geekbridge_android::input_error_response(InputError) -> String
geekbridge_android::{MAX_REQUEST_BYTES, REQUEST_VERSION}
geekbridge_android::mutf8::has_unpaired_surrogate(&[u8]) -> bool
```

- `handle_request` is total: always returns one JSON document, never panics outward (a panic is
  caught and becomes `internal_error`), keeps no state, performs no I/O. Host tests call it directly.
- `input_error_response` encodes failures the JNI layer detects before a request string exists.
- The JNI module is compiled only for `target_os = "android"`; `jni` is an Android-only dependency,
  so `cargo test -p bridge-android` on a desktop needs no JVM.

## JNI export

```kotlin
package dev.geekbridge.android
object NativeBridge {
    init { System.loadLibrary("geekbridge_android") }
    external fun nativeRequest(request: String): String
}
```

Rust symbol `Java_dev_geekbridge_android_NativeBridge_nativeRequest(EnvUnowned, JObject, JString) -> JString`.
The receiver is the `NativeBridge` instance (a `JObject`), because a Kotlin `object` member is not
static. The only `unsafe` in the crate is the Rust 2024 `#[unsafe(no_mangle)]` attribute on that
function (`#![deny(unsafe_code)]` elsewhere); there are no `unsafe` blocks.

Boundary behaviour:

- The Java string is borrowed through `JString::mutf8_chars` (a guard released after the response is
  built; local references are not leaked). Modified UTF-8 is decoded to UTF-8 by the `jni` crate;
  embedded NUL and supplementary characters round-trip correctly.
- A `null` string, an unreadable string, an oversized string (more than 2x `MAX_REQUEST_BYTES` of
  modified UTF-8, checked before decoding) or an unpaired UTF-16 surrogate yields a structured error
  (`invalid_request`, `input_unreadable`, `request_too_large`, `malformed_text`). A pending Java
  exception raised while reading is cleared so the structured response can be returned.
- Panics are caught twice (`handle_request` and `EnvUnowned::with_env`); none unwinds into ART.
- Only if the response `String` cannot be created (e.g. out of memory) does the call throw: an
  already-pending exception is left alone, otherwise one `RuntimeException` with the fixed message
  `geekbridge native bridge failure`. No request data, JNI error text or panic payload is included.
- Do not build this crate with `panic = "abort"`; the guards rely on unwinding. The workspace
  profiles do not set it.

## Wire format

Request: one JSON object. Response: `{"version":1,"ok":true,"result":...}` or
`{"version":1,"ok":false,"error":{"code":"...","message":"..."}}`. Error messages are static
strings; they never contain request text, field names, action names or counts of input. Key order
in responses is not significant.

| action | fields | result |
| --- | --- | --- |
| `version` | none | `core_version`, `request_versions`, `actions`, `limits`, `validation_only: true` |
| `validate_sms` | `recipient` (string), `text` (string), optional `subscription_id` (integer) | `valid`, `text_chars`, `has_subscription` |
| `validate_clipboard` | `text` (string) | `valid`, `text_bytes`, `max_text_bytes` |

`core_version` is this workspace's crate version (`bridge-core` shares it); it is not a protocol
version.

### Strictness (in order of evaluation)

1. More than `MAX_REQUEST_BYTES` UTF-8 bytes → `request_too_large`, before any parsing.
   `MAX_REQUEST_BYTES` is 394 240: the worst-case JSON escaping (6 bytes per byte) of a 64 KiB
   clipboard item plus envelope, asserted equal to `clipboard::max_wire_bytes(DEFAULT_MAX_TEXT_BYTES)`.
2. Invalid JSON, trailing data, or depth over serde_json's limit → `malformed_json`.
3. Not an object, a duplicate key (even with equal values), or more than 8 keys → `invalid_request`.
4. `version` missing → `missing_field`; anything but the integer `1` (including `1.0`, `"1"`,
   `null`) → `unsupported_version`. This is checked before the action.
5. `action` missing → `missing_field`; not a string → `invalid_field`; not in the table →
   `unknown_action` (checked before unknown fields).
6. Per action: a field missing → `missing_field`; wrong type → `invalid_field`; any field not in
   the table (including `recipients`, `sensitive`, pairing-looking names) → `unknown_field`.
   Explicit `null` is not "absent": `"subscription_id": null` is `invalid_field`, so omit the key.
7. Semantic rules from `bridge-core` (below).

### SMS rules (`bridge-core::messages`, nothing duplicated)

Exactly one recipient. Text is not trimmed or normalised. Codes: `empty_text`, `blank_text`,
`text_too_long` (more than 2000 Unicode scalar values, not bytes or UTF-16 units),
`forbidden_control_char` (control characters other than `\n \r \t`, including NUL),
`invalid_recipient` (empty, over 64 chars, contains a control character), `invalid_subscription`
(`0`, negative, `i32::MAX`, or outside `i32`). The recipient is trimmed by the shared type. No
phone-number normalisation or format check exists in the shared rules, so `+1555` is "valid".

### Clipboard rules

Limits come from `bridge-core::clipboard`: non-empty, at most 64 KiB (65 536 UTF-8 bytes) of text,
no NUL. Whitespace-only text and other control characters are valid clipboard text (unlike SMS).
The empty/NUL check is private to `bridge-core`, so the handler runs a throwaway manual-send
`ClipboardSync` reducer with default policy and a user-gesture capability and reads the decision
(`Skip(Invalid(..))` → error). It does no I/O and nothing survives the call; no clipboard is touched.
Codes: `empty_text`, `text_too_large`, `contains_nul`. If `bridge-core` ever adds a rule that
makes this reducer skip for another reason, the response is `internal_error`, never a silent pass.

### Error codes

`request_too_large`, `malformed_json`, `invalid_request`, `unsupported_version`, `missing_field`,
`invalid_field`, `unknown_action`, `unknown_field`, `empty_text`, `blank_text`, `text_too_long`,
`text_too_large`, `forbidden_control_char`, `contains_nul`, `invalid_recipient`,
`invalid_subscription`, `validation_failed` (a `bridge-core` rule this crate has no code for yet),
`input_unreadable`, `malformed_text`, `internal_error`. Treat unknown codes as generic failure.

## Verification

Run from a checkout whose workspace resolves (see the status note below):

```
cargo test -p bridge-android
cargo clippy -p bridge-android --all-targets -- -D warnings
```

Host tests (`tests/requests.rs`, 30 cases, plus unit tests) cover: valid and invalid SMS and
clipboard requests, Unicode bounds (4-byte scalars at exactly 2000, combining sequences, 3-byte
characters at the clipboard byte limit), exact request-size boundary in bytes (not chars), worst-case
JSON escaping at the clipboard limit, escapes and surrogate-pair decoding, lone-surrogate escapes,
malformed JSON and truncations/mutations of a valid request, deep nesting, duplicate and unknown
fields, version strictness, non-echo of input in every response, and panic containment including a
payload whose destructor panics.

The Android-only JNI module cannot be compiled by `cargo test` on a desktop. It was additionally
type-checked and exercised against a real JVM outside the workspace: the same `dispatch.rs`,
`mutf8.rs` and `jni_export.rs` were built into a host `cdylib` against `jni 0.22`, loaded by JDK 17
with `-Xcheck:jni` through a Java class mirroring `NativeBridge` (instance `native` method), and
called with valid, null, oversized (900 000 chars; 700 000 NULs = 2x modified-UTF-8), lone/reversed
surrogate, supplementary-character and NUL inputs, 200 sequential calls and 8 threads x 500 calls.
All returned the expected responses with no JNI warnings. The real module (not the mirror) also passes `cargo check -p bridge-android --locked --target
aarch64-linux-android` and `--target x86_64-linux-android` with the project-local toolchain under
`.tools/`; clippy was run on the JNI module only through the host mirror, since the local toolchain
has no clippy. That is evidence about the glue and type-checking, not about ART, a linked `.so`,
16 KiB alignment, or an APK: none of those were built or run by this work.

## Build requirements for the toolchain step

- Targets `aarch64-linux-android` (`arm64-v8a`) and `x86_64-linux-android`; link with NDK clang for
  API 26 or newer. Edition 2024 / rust-version 1.88 per the workspace.
- Output must be `libgeekbridge_android.so` in `jniLibs/<abi>/`. Verify the export survived with
  `llvm-nm -D libgeekbridge_android.so | grep Java_dev_geekbridge_android_NativeBridge_nativeRequest`.
- **16 KiB page alignment** (required for Android 15+ devices with 16 KiB pages and for Play
  targeting API 35+): every `PT_LOAD` segment of each `.so` must have alignment `0x4000` or more.
  Do not rely on the NDK default; NDK r27 and older default to 4 KiB. Link with
  `-C link-arg=-Wl,-z,max-page-size=16384` (e.g. `RUSTFLAGS` or a target-specific `.cargo/config`
  owned by the tooling step) or use NDK r28+, and then check each ABI with
  `llvm-readelf -lW libgeekbridge_android.so` (every `LOAD` line `Align 0x4000`). The final APK
  also needs 16 KiB zip alignment of uncompressed native libs (AGP 8.13 does this; confirm with
  `zipalign -c -P 16 4 app.apk` and `check_elf_alignment`-style verification of the packaged `.so`).
  This crate adds no linker flags itself and has no build script; the check is the tooling step's.
- Keep default unwinding (no `panic = "abort"`), and strip symbols only after the export check.

## Status note

While this was written the shared `Cargo.lock` briefly failed `--locked` for the whole workspace
(another crate's manifest changed), so tests were first run in an isolated copy containing only
`bridge-core` and `bridge-android`. They were re-run in the real workspace afterwards with
`--locked` and pass. `bridge-android`'s dependency set (`bridge-core`, `serde`, `serde_json`,
Android-only `jni = 0.22.4`) matches the existing lock entry.
