// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Application
import android.os.StrictMode

/** Process start: the crash note for Diagnostics, and StrictMode in debug builds to catch main-thread IO early. */
class GeekBridgeApp : Application() {
    override fun onCreate() {
        super.onCreate()
        // First, so the worker reads the preference files while the main thread does the work below.
        WidgetUpdater.attach(this)
        warmPrivateStorage()
        AppStrings.init(this)
        // Before anything reads a preferences file: bring saved state to this build's format (or note a downgrade).
        StateMigrations.run(this)
        WhatsNewRuntime.init(this) // decides whether the one-time "What's new" card is due (a fresh install is recorded, not shown)
        CrashNote.install(this, BuildConfig.VERSION_NAME)
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
            for (name in PrefsWarmup.FILES) getSharedPreferences(name, MODE_PRIVATE)
            WidgetUpdater.onProcessStart(this)
        }, "geekbridge-warm").apply { isDaemon = true; start() }
    }
}
