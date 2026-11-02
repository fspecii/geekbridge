// SPDX-License-Identifier: GPL-3.0-or-later
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
        coordinator.setListener(this) { sync() }
        sync()
    }

    fun detach() {
        coordinator.clearListener(this)
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
            .setTitle(activity.getString(R.string.offer_wants_file, t.peerName.ifEmpty { activity.getString(R.string.offer_a_computer) }))
            .setMessage("${t.name}$size\n\n" + activity.getString(R.string.offer_saved_note))
            .setCancelable(false)
            .setPositiveButton(R.string.offer_accept) { _, _ -> coordinator.answer(t.id, true) }
            .setNegativeButton(R.string.offer_decline) { _, _ -> coordinator.answer(t.id, false) }
            .show()
    }

    private fun showText(t: ReceivedText) {
        val b = AlertDialog.Builder(activity)
            .setTitle(activity.getString(if (t.kind == TransferKind.Url) R.string.offer_link_from else R.string.offer_text_from, t.from.ifEmpty { activity.getString(R.string.offer_computer) }))
            .setMessage(Bounds.clip(t.text, 1000))
            .setNegativeButton(R.string.offer_close) { _, _ -> dismissedText += t.id }
        if (t.kind == TransferKind.Url && ShareIntentLogic.isWebUrl(t.text)) {
            b.setPositiveButton(R.string.offer_open) { _, _ ->
                dismissedText += t.id
                try {
                    activity.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(t.text.trim())))
                } catch (_: ActivityNotFoundException) {
                }
            }
        } else {
            b.setPositiveButton(R.string.offer_copy) { _, _ ->
                dismissedText += t.id
                coordinator.copyText(t.id)
            }
        }
        dialog = b.show()
    }
}
