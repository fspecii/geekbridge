// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.app.AlertDialog
import android.widget.LinearLayout

/** The clipboard rows of the Settings panel on the home screen. */
object ClipboardSettingsUi {
    fun addRows(activity: Activity, kit: UiKit, parent: LinearLayout, onChanged: () -> Unit) {
        ClipboardRuntime.init(activity)
        val s = ClipboardRuntime.settings
        kit.divider(parent)
        kit.row(
            parent, R.drawable.ic_clipboard, activity.getString(R.string.clip_sync_title), ClipboardText.summary(s, activity.strings),
            kit.toggle(s.enabled) { on ->
                ClipboardRuntime.update { it.copy(enabled = on) }
                if (on) ClipboardRuntime.ensureWaiting()
                onChanged()
            },
        )
        if (s.enabled) {
            kit.divider(parent)
            kit.row(
                parent, R.drawable.ic_check, activity.getString(R.string.clip_window_title), activity.getString(R.string.clip_window_summary, ClipboardText.windowLabel(s.windowMs, activity.strings)), null,
            ) { chooseWindow(activity, onChanged) }
            kit.divider(parent)
            kit.row(parent, R.drawable.ic_bell, activity.getString(R.string.clip_allows_title), activity.getString(R.string.clip_honest_note), null)
        }
    }

    private fun chooseWindow(activity: Activity, onChanged: () -> Unit) {
        val choices = ClipboardLimits.WINDOW_CHOICES_MS
        AlertDialog.Builder(activity)
            .setTitle(activity.getString(R.string.clip_window_dialog))
            .setItems(choices.map { ClipboardText.windowLabel(it, activity.strings) }.toTypedArray()) { _, i ->
                ClipboardRuntime.update { it.copy(windowMs = choices[i]) }
                onChanged()
            }
            .show()
    }
}
