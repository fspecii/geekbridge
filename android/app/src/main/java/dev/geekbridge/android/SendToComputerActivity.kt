package dev.geekbridge.android

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.Toast

/**
 * "Send to computer" in the text selection menu (ACTION_PROCESS_TEXT). The selected text is handed to
 * us by the system, so no clipboard read is needed. Read-only: nothing is returned to the calling app.
 */
class SendToComputerActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ClipboardRuntime.init(this)
        val text = intent?.getCharSequenceExtra(Intent.EXTRA_PROCESS_TEXT)?.toString()
        val appCtx = applicationContext
        val engine = ClipboardRuntime.engine()
        if (!text.isNullOrEmpty() && engine != null) {
            ClipboardRuntime.async {
                val r = engine.sendText(text)
                Handler(Looper.getMainLooper()).post { Toast.makeText(appCtx, r.message, Toast.LENGTH_SHORT).show() }
            }
        }
        finish()
    }
}
