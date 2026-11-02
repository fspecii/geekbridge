// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/**
 * JNI boundary to the Rust core (`crates/bridge-android`, library `geekbridge_android`).
 *
 * The declaration matches docs/android-contract.md exactly; the only addition is that a failed
 * load is recorded instead of crashing the process, so the UI can say the core is unavailable.
 * Callers should go through [CoreClient], which also guards the call itself.
 */
object NativeBridge {
    @Volatile
    var loadFailure: String? = null
        private set

    val isLoaded: Boolean get() = loadFailure == null

    init {
        try {
            System.loadLibrary("geekbridge_android")
        } catch (e: UnsatisfiedLinkError) {
            loadFailure = "library could not be loaded (${e.javaClass.simpleName})"
        } catch (e: SecurityException) {
            loadFailure = "library load was denied (${e.javaClass.simpleName})"
        }
    }

    external fun nativeRequest(request: String): String
}
