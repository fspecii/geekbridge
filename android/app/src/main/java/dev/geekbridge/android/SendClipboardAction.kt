package dev.geekbridge.android

import android.app.Activity

/** Home screen "Send clipboard": a deliberate tap, so the app may look at the clipboard. */
object SendClipboardAction : QuickActionHandler {
    override fun isAvailable(activity: Activity): Boolean {
        ClipboardRuntime.init(activity)
        return ClipboardRuntime.settings.enabled
    }

    override fun unavailableReason(activity: Activity): String? =
        "Turn on Clipboard sync in Settings first."

    override fun launch(activity: Activity) {
        activity.startActivity(ClipboardFloatingActivity.intent(activity))
    }
}
