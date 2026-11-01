package dev.geekbridge.android

import android.app.Activity
import android.app.AlertDialog
import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri

/**
 * In-app prompts while MainActivity is visible: an incoming file offer (Accept / Decline) and text or a link
 * received from the computer (Copy / Open / Close). Nothing is saved, copied or opened without a tap.
 */
class OfferDialogs(private val activity: Activity, private val coordinator: ShareCoordinator) {
    private var dialog: AlertDialog? = null
    private var shownKey: String? = null
    private val dismissedText = HashSet<Long>()

    fun attach() {
        coordinator.onChange = { sync() }
        sync()
    }

    fun detach() {
        coordinator.onChange = null
        dialog?.dismiss()
        dialog = null
        shownKey = null
    }

    private fun sync() {
        val offer = coordinator.offers().firstOrNull()
        val text = coordinator.texts.firstOrNull { it.id !in dismissedText }
        val key = offer?.let { "o${it.id}" } ?: text?.let { "t${it.id}" }
        if (key == shownKey) return
        dialog?.dismiss()
        dialog = null
        shownKey = key
        when {
            offer != null -> showOffer(offer)
            text != null -> showText(text)
        }
    }

    private fun showOffer(t: Transfer) {
        val size = t.size?.let { " (${ProgressText.bytes(it)})" } ?: ""
        dialog = AlertDialog.Builder(activity)
            .setTitle("${t.peerName.ifEmpty { "A computer" }} wants to send a file")
            .setMessage("${t.name}$size\n\nIt is saved to Downloads/GeekBridge only if you accept.")
            .setCancelable(false)
            .setPositiveButton("Accept") { _, _ -> coordinator.answer(t.id, true) }
            .setNegativeButton("Decline") { _, _ -> coordinator.answer(t.id, false) }
            .show()
    }

    private fun showText(t: ReceivedText) {
        val b = AlertDialog.Builder(activity)
            .setTitle(if (t.kind == TransferKind.Url) "Link from ${t.from.ifEmpty { "computer" }}" else "Text from ${t.from.ifEmpty { "computer" }}")
            .setMessage(Bounds.clip(t.text, 1000))
            .setNegativeButton("Close") { _, _ -> dismissedText += t.id }
        if (t.kind == TransferKind.Url && ShareIntentLogic.isWebUrl(t.text)) {
            b.setPositiveButton("Open") { _, _ ->
                dismissedText += t.id
                try {
                    activity.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(t.text.trim())))
                } catch (_: ActivityNotFoundException) {
                }
            }
        } else {
            b.setPositiveButton("Copy") { _, _ ->
                dismissedText += t.id
                coordinator.copyText(t.id)
            }
        }
        dialog = b.show()
    }
}
