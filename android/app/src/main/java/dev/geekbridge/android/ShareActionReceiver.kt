package dev.geekbridge.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Notification buttons (Accept / Decline / Cancel / Copy). Not exported; only our own PendingIntents reach it. */
class ShareActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val id = intent.getLongExtra(EXTRA_ID, -1)
        if (id < 0) return
        val c = ShareCoordinator.get(context)
        when (intent.action) {
            ACTION_ACCEPT -> c.answer(id, accept = true)
            ACTION_DECLINE -> c.answer(id, accept = false)
            ACTION_CANCEL -> c.cancel(id)
            ACTION_COPY -> c.copyText(id)
        }
    }

    companion object {
        const val EXTRA_ID = "transfer_id"
        const val ACTION_ACCEPT = "dev.geekbridge.android.share.ACCEPT"
        const val ACTION_DECLINE = "dev.geekbridge.android.share.DECLINE"
        const val ACTION_CANCEL = "dev.geekbridge.android.share.CANCEL"
        const val ACTION_COPY = "dev.geekbridge.android.share.COPY"
    }
}
