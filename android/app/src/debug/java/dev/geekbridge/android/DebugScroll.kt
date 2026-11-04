// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.app.Application
import android.os.Bundle
import android.view.View
import android.view.ViewGroup
import android.widget.ScrollView

/**
 * Debug builds only, for scripts/golden-android.sh: `--ei gb_scroll N` on any activity's start intent scrolls its main
 * ScrollView to page N (page 0 is the top; a page is 85 % of the visible height, so every page overlaps the one before
 * by a few lines), so a long screen can be captured in several golden pictures.
 */
object DebugScroll : Application.ActivityLifecycleCallbacks {
    fun install(app: Application) = app.registerActivityLifecycleCallbacks(this)

    override fun onActivityResumed(activity: Activity) {
        val page = activity.intent?.getIntExtra("gb_scroll", 0) ?: 0
        if (page <= 0) return
        val decor = activity.window.decorView
        // Layout (and the pages the screens fill in asynchronously) is not final at once: scroll a few times.
        for (delay in longArrayOf(400, 1200, 2200)) {
            decor.postDelayed({
                val sv = find(decor) ?: return@postDelayed
                sv.scrollTo(0, (sv.height * 85 / 100) * page)
            }, delay)
        }
    }

    private fun find(v: View): ScrollView? {
        if (v is ScrollView) return v
        if (v is ViewGroup) for (i in 0 until v.childCount) find(v.getChildAt(i))?.let { return it }
        return null
    }

    override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) {}
    override fun onActivityStarted(activity: Activity) {}
    override fun onActivityPaused(activity: Activity) {}
    override fun onActivityStopped(activity: Activity) {}
    override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) {}
    override fun onActivityDestroyed(activity: Activity) {}
}
