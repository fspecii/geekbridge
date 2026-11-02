// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.Toast

/** What the "Send to computer" confirmation shows and sends; pure so it is tested. */
object SendConfirm {
    const val PREVIEW_CHARS = 600

    /** The text to offer for sending, or null when there is nothing worth asking about. */
    fun accept(raw: CharSequence?): String? = raw?.toString()?.takeIf { it.isNotBlank() }

    /** What the dialog shows: the start of the text, with control characters made visible as spaces. */
    fun preview(text: String): String {
        val flat = text.map { if (it.isISOControl() && it != '\n') ' ' else it }.joinToString("")
        return Bounds.clip(flat, PREVIEW_CHARS)
    }
}

/**
 * "Send to computer" in the text selection menu (ACTION_PROCESS_TEXT). The activity has to be exported for the system
 * menu to reach it, so any app can start it with any text: nothing is sent until the user reads the text here and
 * taps Send (otherwise a hostile app could fill the computer's clipboard for a later paste). Read-only: nothing is
 * returned to the calling app.
 */
class SendToComputerActivity : Activity() {
    private var dialog: AlertDialog? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ClipboardRuntime.init(this)
        val text = SendConfirm.accept(intent?.getCharSequenceExtra(Intent.EXTRA_PROCESS_TEXT))
        if (text == null || ClipboardRuntime.engine() == null) return finish()
        dialog = AlertDialog.Builder(this)
            .setTitle(R.string.send_confirm_title)
            .setMessage(SendConfirm.preview(text))
            .setPositiveButton(R.string.send_confirm_send) { _, _ -> send(text) }
            .setNegativeButton(android.R.string.cancel) { _, _ -> finish() }
            .setOnCancelListener { finish() }
            .show()
    }

    private fun send(text: String) {
        val appCtx = applicationContext
        val engine = ClipboardRuntime.engine()
        if (engine != null) {
            ClipboardRuntime.async {
                val r = engine.sendText(text)
                Handler(Looper.getMainLooper()).post { Toast.makeText(appCtx, r.message, Toast.LENGTH_SHORT).show() }
            }
        }
        finish()
    }

    override fun onDestroy() {
        dialog?.dismiss()
        super.onDestroy()
    }
}
