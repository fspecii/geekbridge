// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.app.Application
import android.os.Bundle

/**
 * Counts started activities of this app so moving between its screens (Home -> Setup -> Diagnostics) is not
 * mistaken for leaving the app, which would otherwise stop the link between two screens.
 */
object ActivityTracker : Application.ActivityLifecycleCallbacks {
    @Volatile private var installed = false

    @Volatile var startedCount = 0
        private set

    fun install(activity: Activity) {
        if (installed) return
        synchronized(this) {
            if (installed) return
            activity.application.registerActivityLifecycleCallbacks(this)
            installed = true
            // Called from onCreate, before this activity's onStart, so the count starts at zero correctly.
        }
    }

    /**
     * Call from an activity's onStop *before* super.onStop: true if the app is really leaving the foreground
     * (no other of its screens started, not a rotation, no background service owning the link).
     */
    fun leavingApp(activity: Activity): Boolean =
        !activity.isChangingConfigurations && startedCount <= 1 && !LinkService.isRunning

    override fun onActivityStarted(activity: Activity) { startedCount++ }
    override fun onActivityStopped(activity: Activity) { if (startedCount > 0) startedCount-- }
    override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) {}
    override fun onActivityResumed(activity: Activity) {}
    override fun onActivityPaused(activity: Activity) {}
    override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) {}
    override fun onActivityDestroyed(activity: Activity) {}
}
