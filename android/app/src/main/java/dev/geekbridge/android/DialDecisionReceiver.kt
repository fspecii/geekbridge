// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Dial / Deny buttons of the phone's "place this call?" notification. Not exported; only our own PendingIntents reach it. */
class DialDecisionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val id = intent.getStringExtra(EXTRA_REQUEST) ?: return
        when (intent.action) {
            ACTION_ALLOW -> CallControlRuntime.resolveDial(id, allow = true)
            ACTION_DENY -> CallControlRuntime.resolveDial(id, allow = false)
        }
    }

    companion object {
        const val EXTRA_REQUEST = "request_id"
        const val ACTION_ALLOW = "dev.geekbridge.android.dial.ALLOW"
        const val ACTION_DENY = "dev.geekbridge.android.dial.DENY"
    }
}
