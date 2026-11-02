// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Application
import android.os.StrictMode

/** Process start: the crash note for Diagnostics, and StrictMode in debug builds to catch main-thread IO early. */
class GeekBridgeApp : Application() {
    override fun onCreate() {
        super.onCreate()
        // Before anything reads a preferences file: bring saved state to this build's format (or note a downgrade).
        StateMigrations.run(this)
        CrashNote.install(this, BuildConfig.VERSION_NAME)
        warmPrivateStorage()
        WidgetUpdater.attach(this)
        if (BuildConfig.DEBUG) {
            StrictMode.setThreadPolicy(StrictMode.ThreadPolicy.Builder().detectAll().penaltyLog().build())
            StrictMode.setVmPolicy(StrictMode.VmPolicy.Builder().detectAll().penaltyLog().build())
        }
    }

    /**
     * The first touch of the app's private directories and preference files does disk IO (creating the
     * directory, loading the XML). Do it once here on a worker so the first activity never pays for it on
     * the main thread; later calls return the cached objects.
     */
    private fun warmPrivateStorage() {
        Thread({
            filesDir
            cacheDir
            for (name in PREFS) getSharedPreferences(name, MODE_PRIVATE)
            WidgetUpdater.onProcessStart(this)
        }, "geekbridge-warm").apply { isDaemon = true; start() }
    }

    private companion object {
        val PREFS = listOf("geekbridge_state", "geekbridge_clipboard", "geekbridge_calls", "geekbridge_grants", "geekbridge_phonedata", "share_policy", "updates")
    }
}
