package dev.geekbridge.android

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build

/**
 * Notifications for sharing. The app works without the notification permission: every call is a no-op when
 * it is missing and never throws.
 */
class ShareNotifications(private val ctx: Context) {
    private val nm: NotificationManager? = ctx.getSystemService(NotificationManager::class.java)

    init {
        nm?.createNotificationChannel(
            NotificationChannel(CHANNEL, "File sharing", NotificationManager.IMPORTANCE_LOW).apply {
                description = "Progress and results of sharing files with a paired computer"
            },
        )
    }

    fun canPost(): Boolean =
        Build.VERSION.SDK_INT < 33 || ctx.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

    private fun builder(title: String, text: String): Notification.Builder =
        Notification.Builder(ctx, CHANNEL).setSmallIcon(android.R.drawable.stat_sys_upload_done)
            .setContentTitle(title).setContentText(Bounds.clip(text, 120)).setOnlyAlertOnce(true)

    private fun post(id: Int, n: Notification) {
        if (!canPost()) return
        try {
            nm?.notify(id, n)
        } catch (_: SecurityException) {
            // Permission revoked between the check and the call.
        }
    }

    fun cancel(id: Int) { nm?.cancel(id) }

    /** Without the notification permission a finished transfer would be silent, so say it once on screen. */
    private fun toastIfSilent(text: String) {
        if (canPost()) return
        android.os.Handler(android.os.Looper.getMainLooper()).post {
            android.widget.Toast.makeText(ctx, Bounds.clip(text, 120), android.widget.Toast.LENGTH_LONG).show()
        }
    }

    private fun open(): PendingIntent = PendingIntent.getActivity(
        ctx, 0, Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    private fun action(code: String, transferId: Long): PendingIntent = PendingIntent.getBroadcast(
        ctx, (code.hashCode() xor transferId.toInt()),
        Intent(ctx, ShareActionReceiver::class.java).setAction(code).putExtra(ShareActionReceiver.EXTRA_ID, transferId),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    /** Ongoing progress for the outgoing transfers; [percent] null means indeterminate. */
    fun progress(summary: String, percent: Int?, cancelTransferId: Long?) {
        val b = builder("Sending to computer", summary).setOngoing(true).setContentIntent(open())
            .setProgress(100, percent ?: 0, percent == null)
        if (cancelTransferId != null) {
            b.addAction(Notification.Action.Builder(null, "Cancel", action(ShareActionReceiver.ACTION_CANCEL, cancelTransferId)).build())
        }
        post(ID_PROGRESS, b.build())
    }

    fun offer(t: Transfer) {
        val size = t.size?.let { " (${ProgressText.bytes(it)})" } ?: ""
        val b = builder("${t.peerName.ifEmpty { "A computer" }} wants to send a file", t.name + size)
            .setContentIntent(open()).setAutoCancel(true)
            .addAction(Notification.Action.Builder(null, "Accept", action(ShareActionReceiver.ACTION_ACCEPT, t.id)).build())
            .addAction(Notification.Action.Builder(null, "Decline", action(ShareActionReceiver.ACTION_DECLINE, t.id)).build())
        post(ID_BASE + t.id.toInt(), b.build())
    }

    fun clearOffer(transferId: Long) = cancel(ID_BASE + transferId.toInt())

    fun received(transferId: Long, name: String, uri: Uri?) {
        toastIfSilent("File received: $name (Downloads/GeekBridge)")
        val b = builder("File received", name).setAutoCancel(true)
        if (uri != null) {
            val view = Intent(Intent.ACTION_VIEW).setDataAndType(uri, NameRules.mimeFor(name))
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
            b.setContentIntent(PendingIntent.getActivity(ctx, transferId.toInt(), view, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT))
        } else {
            b.setContentIntent(open())
        }
        post(ID_BASE + transferId.toInt(), b.build())
    }

    fun failed(transferId: Long, what: String) {
        toastIfSilent("Transfer failed: $what")
        post(ID_BASE + transferId.toInt(), builder("Transfer failed", what).setAutoCancel(true).setContentIntent(open()).build())
    }

    fun text(item: ReceivedText) {
        val b = builder(if (item.kind == TransferKind.Url) "Link from computer" else "Text from computer", item.text).setAutoCancel(true)
        if (item.kind == TransferKind.Url && ShareIntentLogic.isWebUrl(item.text)) {
            val view = Intent(Intent.ACTION_VIEW, Uri.parse(item.text.trim())).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            b.setContentIntent(PendingIntent.getActivity(ctx, item.id.toInt(), view, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT))
        } else {
            b.setContentIntent(open())
            b.addAction(Notification.Action.Builder(null, "Copy", action(ShareActionReceiver.ACTION_COPY, item.id)).build())
        }
        post(ID_BASE + item.id.toInt(), b.build())
    }

    companion object {
        const val CHANNEL = "sharing"
        const val ID_PROGRESS = 7000
        const val ID_BASE = 8000

        /** The only place received text reaches the clipboard: an explicit tap on Copy. */
        fun copyToClipboard(ctx: Context, text: String) {
            ctx.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText("GeekBridge", text))
        }
    }
}
