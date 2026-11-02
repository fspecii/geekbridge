// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.KeyguardManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.ClipData
import android.content.ClipboardManager
import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.os.PowerManager
import java.io.File
import java.io.FileOutputStream
import java.io.IOException

/**
 * The Android side of clipboard sync: reading (only ever from code that has window focus), writing
 * (allowed in the background), the lock check, and the "tap to send" notification.
 */
class AndroidClipboardHost(private val ctx: Context) : ClipboardHost {
    private val clipboard get() = ctx.getSystemService(ClipboardManager::class.java)
    private val spool = SpoolKeeper { File(it).delete() }

    override fun isLocked(): Boolean {
        val km = ctx.getSystemService(KeyguardManager::class.java)
        val pm = ctx.getSystemService(PowerManager::class.java)
        // Screen off counts as locked: the phone will be locked by the time anyone could use the result.
        return km.isKeyguardLocked || km.isDeviceLocked || !pm.isInteractive
    }

    override fun write(item: ClipEvent.Incoming) {
        val clip = when (item.kind) {
            ClipKind.Text, ClipKind.Url -> ClipData.newPlainText(LABEL, item.text ?: return)
            ClipKind.Html -> ClipData.newHtmlText(LABEL, item.text ?: return, item.html ?: return)
            ClipKind.Image -> {
                val path = item.imagePath ?: return
                val uri = ClipImageProvider.uriFor(ctx, path) ?: return
                ClipData.newUri(ctx.contentResolver, LABEL, uri)
            }
        }
        clipboard.setPrimaryClip(clip)
        // Only the newest received image is kept: a text or a newer image replaces it, and the file goes away.
        spool.received(if (item.kind == ClipKind.Image) item.imagePath else null)
    }

    override fun forgetSpooledImage() = spool.clear()

    override fun askForTap() {
        val nm = ctx.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL, ctx.getString(R.string.channel_clipboard), NotificationManager.IMPORTANCE_HIGH).apply {
                description = ctx.getString(R.string.channel_clipboard_desc)
                setShowBadge(false)
            },
        )
        val tap = PendingIntent.getActivity(
            ctx, 0, ClipboardFloatingActivity.intent(ctx), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        // Nothing about the clipboard's content appears on the lock screen or in the notification.
        val n = Notification.Builder(ctx, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_link)
            .setContentTitle(ctx.getString(R.string.clip_tap_title))
            .setContentText(ctx.getString(R.string.clip_tap_text))
            .setContentIntent(tap)
            .setAutoCancel(true)
            .setCategory(Notification.CATEGORY_MESSAGE)
            .setVisibility(Notification.VISIBILITY_SECRET)
            .setTimeoutAfter(ClipboardEngine.PENDING_MS)
            .build()
        try {
            nm.notify(NOTIFICATION_ID, n)
        } catch (_: SecurityException) {
            // Notifications not allowed: the computer will time out and say so.
        }
    }

    override fun dismissTapRequest() {
        ctx.getSystemService(NotificationManager::class.java).cancel(NOTIFICATION_ID)
    }

    override fun <T> withImageFd(snapshot: ClipSnapshot, maxBytes: Long, block: (Int) -> T): T? {
        val uri = snapshot.imageUri?.let(android.net.Uri::parse) ?: return null
        val copy = File(ctx.cacheDir, "clip-out-${System.nanoTime()}.tmp")
        try {
            // Opening the provider's stream is bounded at 2 s and cancelled through a CancellationSignal;
            // the copy is bounded too and closes the stream on timeout, so a stalled provider can't pin us.
            val src = BoundedCall.run(ClipReader.PROBE_TIMEOUT_MS) { token ->
                val signal = CancellationSignal()
                token.onCancel { signal.cancel() }
                ctx.contentResolver.openFileDescriptor(uri, "r", signal)
            } ?: return null
            val ok = BoundedCall.run(COPY_TIMEOUT_MS) { token ->
                token.onCancel { try { src.close() } catch (_: IOException) { } }
                ParcelFileDescriptor.AutoCloseInputStream(src).use { input ->
                    FileOutputStream(copy).use { out ->
                        val buf = ByteArray(16 * 1024)
                        var total = 0L
                        while (true) {
                            val n = input.read(buf)
                            if (n < 0) break
                            total += n
                            if (total > maxBytes) return@run false
                            out.write(buf, 0, n)
                        }
                    }
                }
                true
            }
            if (ok != true) {
                try { src.close() } catch (_: IOException) { }
                return null
            }
            ParcelFileDescriptor.open(copy, ParcelFileDescriptor.MODE_READ_ONLY).use { return block(it.fd) }
        } catch (_: IOException) {
            return null
        } catch (_: SecurityException) {
            return null
        } finally {
            copy.delete()
        }
    }

    companion object {
        private const val LABEL = "GeekBridge"
        private const val COPY_TIMEOUT_MS = 15_000L
        private const val CHANNEL = "clipboard_request"
        const val NOTIFICATION_ID = 2
        private const val EXTRA_IS_SENSITIVE = "android.content.extra.IS_SENSITIVE"

        /**
         * Main thread, window focus only: take the clip reference. Android only returns data to the app that
         * has window focus (or the default keyboard); anywhere else this yields nothing. Touches no content
         * provider; hand the result to [snapshot] on a worker.
         */
        fun grab(ctx: Context): ClipData? {
            val cm = ctx.getSystemService(ClipboardManager::class.java) ?: return null
            return try {
                cm.primaryClip
            } catch (_: SecurityException) {
                null
            }?.takeIf { it.itemCount > 0 }
        }

        /** Worker thread: classify [clip]; the provider's type probe is bounded by [ClipReader.PROBE_TIMEOUT_MS]. */
        fun snapshot(ctx: Context, clip: ClipData, nowMs: Long = System.currentTimeMillis()): ClipSnapshot? {
            val desc = clip.description
            val item = clip.getItemAt(0)
            val raw = RawClip(
                text = item.text,
                html = item.htmlText,
                uri = item.uri?.toString(),
                declaredMime = if (desc.mimeTypeCount > 0) desc.getMimeType(0) else null,
                sensitive = desc.extras?.getBoolean(EXTRA_IS_SENSITIVE, false) == true,
                timestamp = desc.timestamp,
            )
            val resolver = ctx.applicationContext.contentResolver
            return ClipReader.snapshot(raw, { uri, _ -> typeOf(resolver, uri) }, nowMs)
        }

        private fun typeOf(r: ContentResolver, uri: String): String? =
            try {
                r.getType(android.net.Uri.parse(uri))
            } catch (_: RuntimeException) {
                null
            }
    }
}
