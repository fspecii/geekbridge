// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent

/** When the phone tells the person that the background connection could not restart by itself. */
object ReconnectPolicy {
    /**
     * Only when a computer is paired and the connection is wanted, Android refused the background start, and
     * notifications are allowed. Silence otherwise: an unpaired phone has nothing to reconnect.
     */
    fun shouldShow(wantService: Boolean, startRefused: Boolean, notificationsEnabled: Boolean): Boolean =
        wantService && startRefused && notificationsEnabled
}

/**
 * A quiet "tap to reconnect" notice for the case where Android refuses to restart the connection from the
 * background (after a reboot or an update, or under a restricted battery mode). A tap opens the app, which can start
 * the connection again because it is then in the foreground. Fixed text, no personal data; removed as soon as the
 * connection starts.
 */
object ReconnectNotice {
    private const val CHANNEL = "reconnect"
    private const val ID = 2

    fun show(ctx: Context) {
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return
        if (!ReconnectPolicy.shouldShow(AppState.wantService(), startRefused = true, notificationsEnabled = nm.areNotificationsEnabled())) return
        try {
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL, ctx.getString(R.string.channel_reconnect), NotificationManager.IMPORTANCE_LOW).apply {
                    setShowBadge(false)
                },
            )
            val open = PendingIntent.getActivity(
                ctx, 2, Intent(ctx, HomeActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            nm.notify(
                ID,
                Notification.Builder(ctx, CHANNEL)
                    .setSmallIcon(R.drawable.ic_stat_link)
                    .setContentTitle(ctx.getString(R.string.app_name))
                    .setContentText(ctx.getString(R.string.notif_reconnect))
                    .setContentIntent(open)
                    .setAutoCancel(true)
                    .setOnlyAlertOnce(true)
                    .setShowWhen(false)
                    .setCategory(Notification.CATEGORY_STATUS)
                    .build(),
            )
        } catch (_: RuntimeException) {
            // A ROM that refuses the notification: the next app start retries the connection anyway.
        }
    }

    fun clear(ctx: Context) {
        try { ctx.getSystemService(NotificationManager::class.java)?.cancel(ID) } catch (_: RuntimeException) { }
    }
}
