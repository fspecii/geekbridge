package dev.geekbridge.android

import android.app.Activity
import android.app.AlertDialog
import android.widget.LinearLayout
import android.widget.ScrollView

/**
 * Shows the pairing decision for exactly the prompt the core listed, and sends back exactly what was shown.
 * Same safety rules as the Diagnostics link card: not cancellable by a stray tap, approval needs the user.
 */
class PairPromptDialog(
    private val activity: Activity,
    private val ui: Ui,
    private val controller: TransportController,
) {
    private var dialog: AlertDialog? = null
    private var shownId = -1L

    fun sync() {
        val pending = controller.pendingPrompt()
        if (dialog != null && (pending == null || pending.requestId != shownId)) dismiss()
        if (pending != null && dialog == null && !activity.isFinishing) show(pending)
    }

    fun dismiss() {
        dialog?.dismiss()
        dialog = null
        shownId = -1
    }

    private fun show(p: PairPrompt) {
        val outgoing = p.direction == "outgoing"
        val name = p.peerName.ifEmpty { activity.getString(R.string.pair_this_computer) }
        val secs = (p.expiresInMs / 1000).coerceAtLeast(0).toInt()
        val pad = ui.dp(20)
        val body = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, ui.dp(8), pad, 0)
        }
        if (p.qrPinned) {
            // The certificate came from the scanned code, so there is nothing to compare: just a clear question.
            body.addView(ui.text(activity.getString(R.string.qr_dlg_body)))
            body.addView(ui.text(activity.getString(R.string.qr_dlg_cert), Tone.Muted, 12f))
            body.addView(ui.mono(TransportText.group(p.peerCertSha256), 12f))
        } else {
            body.addView(
                ui.text(
                    activity.getString(if (outgoing) R.string.pair_dlg_outgoing else R.string.pair_dlg_incoming, name),
                ),
            )
            // Big and spaced out: this is the one thing both screens must agree on.
            body.addView(ui.mono(p.code, 44f, bold = true).apply { letterSpacing = 0.12f })
            body.addView(ui.text(activity.getString(R.string.pair_dlg_cert), Tone.Muted, 12f))
            body.addView(ui.mono(TransportText.group(p.peerCertSha256), 12f))
        }
        if (!p.qrPinned) {
            body.addView(
                ui.text(
                    activity.resources.getQuantityString(R.plurals.pair_dlg_warning, secs, secs),
                    Tone.Muted,
                ),
            )
        }
        val d = AlertDialog.Builder(activity)
            .setTitle(
                when {
                    p.qrPinned -> activity.getString(R.string.qr_dlg_title, name)
                    outgoing -> activity.getString(R.string.pair_dlg_title_outgoing, name)
                    else -> activity.getString(R.string.pair_dlg_title_incoming)
                },
            )
            .setView(ScrollView(activity).apply { addView(body) })
            .setCancelable(false)
            .setPositiveButton(R.string.pair_dlg_pair) { _, _ -> finish(p, true) }
            .setNegativeButton(R.string.pair_dlg_decline) { _, _ -> finish(p, false) }
            .create()
        dialog = d
        shownId = p.requestId
        d.show()
    }

    private fun finish(p: PairPrompt, approve: Boolean) {
        dialog = null
        shownId = -1
        controller.decide(p, approve)
    }
}
