// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/**
 * Every SharedPreferences file the app opens on the main thread, in the order the first screen needs them.
 * `GeekBridgeApp` loads them on a worker at process start, so the first `getSharedPreferences(..)` on the main
 * thread returns a cached object instead of reading and parsing the XML (StrictMode reported 140-335 ms for
 * `DeviceName.current` in `HomeActivity.onStart` on the emulator). `PrefsWarmupTest` fails when a file opened in
 * the sources is missing here.
 */
object PrefsWarmup {
    val FILES: List<String> = listOf(
        "geekbridge_state", "geekbridge_device_name", "geekbridge_calls", "geekbridge_grants", "geekbridge_phonedata",
        "geekbridge_clipboard", "share_policy", WhatsNewStore.FILE, "updates",
    )
}
