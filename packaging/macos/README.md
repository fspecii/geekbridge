# macOS packaging for the calling helper

Templates and scripts for wrapping `geekbridge-macos-calls` (crate `bridge-calls-macos`) in an app bundle so macOS has a bundle identity with real Bluetooth and microphone purpose strings.

**Status: nothing here has been signed, notarized, launched or accepted by macOS.** It was written without a Mac. `verify-templates.sh` checks structure only.

| File | Purpose |
| --- | --- |
| `Info.plist.template` | `NSBluetoothAlwaysUsageDescription`, `NSMicrophoneUsageDescription`, `LSUIElement` (no Dock icon), `LSMinimumSystemVersion` 11.0. Placeholders `@BUNDLE_ID@ @VERSION@ @BUILD@` |
| `entitlements.developer-id.plist` | Developer ID + Hardened Runtime, no sandbox. Only `com.apple.security.device.audio-input` |
| `entitlements.sandboxed.plist` | **Unproven** App Sandbox variant (`app-sandbox`, `device.bluetooth`, `device.audio-input`) |
| `assemble-app.sh` | builds `GeekBridge Calls.app/Contents/{Info.plist,MacOS/geekbridge-macos-calls}` from a built Mach-O; refuses non-Mach-O input; `--adhoc-sign` (macOS only) applies a development signature |
| `verify-templates.sh` | runs anywhere: parses the plists and asserts required keys, real purpose strings, no hardening-weakening entitlements |

## Why a bundle

Apple documents that an app using Bluetooth must carry `NSBluetoothAlwaysUsageDescription` and one using the microphone `NSMicrophoneUsageDescription`; a missing string can terminate the process. Developer reports (not Apple statements) say a bare command-line tool has its prompt attributed to the parent ("responsible") process. So the helper is shipped inside a bundle that carries both strings and launched from the daemon. **Which process macOS actually names in the prompt for a helper spawned by the daemon has not been tested.** That is the first thing to check on hardware.

The helper itself never opens an audio stream, so it does not exercise microphone permission; the microphone string and `audio-input` entitlement are for whichever bundle captures the call audio.

## Build and assemble (on a Mac)

```
cargo build --release -p bridge-calls-macos --bin geekbridge-macos-calls --target aarch64-apple-darwin
packaging/macos/assemble-app.sh \
    --binary target/aarch64-apple-darwin/release/geekbridge-macos-calls \
    --out dist/macos --bundle-id dev.geekbridge.calls.macos --version 0.1.0 --build 1
```

For distribution, sign with a Developer ID Application certificate, Hardened Runtime and `entitlements.developer-id.plist`, then notarize and staple. Those steps are not scripted here and not claimed. Do not add `get-task-allow` or `disable-library-validation`.

## Verify the templates anywhere

```
packaging/macos/verify-templates.sh
```
