// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.Toast

/**
 * An invisible activity whose only job is to take window focus for a moment, because Android 10+ lets an
 * app read the clipboard only while it has focus. Started by a deliberate user action: the Quick Settings
 * tile, the "tap to send" notification, or the home screen's Send clipboard. It reads the clipboard once,
 * hands the result to the engine (which answers the computer's question or sends it explicitly), and closes.
 */
class ClipboardFloatingActivity : Activity() {
    private var handled = false
    private val main = Handler(Looper.getMainLooper())

    // A transparent window that never gets focus (keyguard, a Samsung overlay such as the edge panel) would sit over
    // the screen swallowing touches; give up after a moment and say so.
    private val giveUp = Runnable {
        if (handled || isFinishing) return@Runnable
        handled = true
        Toast.makeText(applicationContext, R.string.clip_no_focus, Toast.LENGTH_SHORT).show()
        finish()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ClipboardRuntime.init(this)
        main.postDelayed(giveUp, FOCUS_WAIT_MS)
    }

    override fun onDestroy() {
        main.removeCallbacks(giveUp)
        super.onDestroy()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (!hasFocus || handled) return
        handled = true
        val appCtx = applicationContext
        // Only the clip reference is taken here; every provider access happens on the worker.
        val clip = AndroidClipboardHost.grab(this)
        val engine = ClipboardRuntime.engine()
        ClipboardRuntime.async {
            val snapshot = clip?.let { AndroidClipboardHost.snapshot(appCtx, it) }
            val result = engine?.onGesture(snapshot) ?: SendResult(false, "disabled")
            Handler(Looper.getMainLooper()).post {
                Toast.makeText(appCtx, result.message(appCtx.strings), Toast.LENGTH_SHORT).show()
            }
        }
        finish()
        @Suppress("DEPRECATION")
        overridePendingTransition(0, 0)
    }

    companion object {
        private const val FOCUS_WAIT_MS = 4_000L

        fun intent(ctx: Context): Intent = Intent(ctx, ClipboardFloatingActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_ANIMATION or Intent.FLAG_ACTIVITY_NO_HISTORY)
    }
}
