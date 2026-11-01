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
            parent, R.drawable.ic_clipboard, "Clipboard sync", ClipboardText.summary(s),
            kit.toggle(s.enabled) { on ->
                ClipboardRuntime.update { it.copy(enabled = on) }
                if (on) ClipboardRuntime.ensureWaiting()
                onChanged()
            },
        )
        if (s.enabled) {
            kit.divider(parent)
            kit.row(
                parent, R.drawable.ic_check, "Time window", "Items older than ${ClipboardText.windowLabel(s.windowMs)} are not offered", null,
            ) { chooseWindow(activity, onChanged) }
            kit.divider(parent)
            kit.row(parent, R.drawable.ic_bell, "What Android allows", ClipboardText.HONEST_NOTE, null)
        }
    }

    private fun chooseWindow(activity: Activity, onChanged: () -> Unit) {
        val choices = ClipboardLimits.WINDOW_CHOICES_MS
        AlertDialog.Builder(activity)
            .setTitle("Offer items copied in the last…")
            .setItems(choices.map { ClipboardText.windowLabel(it) }.toTypedArray()) { _, i ->
                ClipboardRuntime.update { it.copy(windowMs = choices[i]) }
                onChanged()
            }
            .show()
    }
}
