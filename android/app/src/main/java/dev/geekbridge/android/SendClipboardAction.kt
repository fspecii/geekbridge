// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity

/** Home screen "Send clipboard": a deliberate tap, so the app may look at the clipboard. */
object SendClipboardAction : QuickActionHandler {
    override fun isAvailable(activity: Activity): Boolean {
        ClipboardRuntime.init(activity)
        return ClipboardRuntime.settings.enabled
    }

    override fun unavailableReason(activity: Activity): String? =
        activity.getString(R.string.clip_turn_on_first)

    override fun launch(activity: Activity) {
        activity.startActivity(ClipboardFloatingActivity.intent(activity))
    }
}
