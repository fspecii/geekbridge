// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Notification
import android.app.PendingIntent
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.provider.Settings
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification

/**
 * Android's notification listener, used only for "Show phone notifications on the computer". Android binds it only
 * after the user grants Notification access to GeekBridge. It does nothing unless the feature is on, the link is up
 * and the notification is from an app the user picked ([NotificationMirror]); it keeps nothing and logs no text.
 */
class MirrorListenerService : NotificationListenerService() {
    override fun onListenerConnected() {
        instance = this
        PhoneDataRuntime.init(this)
        PhoneDataRuntime.onListenerChanged()
    }

    override fun onListenerDisconnected() {
        if (instance === this) instance = null
        PhoneDataRuntime.onListenerChanged()
    }

    override fun onNotificationPosted(sbn: StatusBarNotification?, rankingMap: RankingMap?) {
        val s = sbn ?: return
        val info = MirrorAndroid.info(this, s, rankingMap) ?: return
        PhoneDataRuntime.onMirrorPosted(info)
    }

    override fun onNotificationRemoved(sbn: StatusBarNotification?, rankingMap: RankingMap?, reason: Int) {
        val s = sbn ?: return
        PhoneDataRuntime.onMirrorRemoved(s.key)
    }

    override fun onDestroy() {
        if (instance === this) instance = null
        super.onDestroy()
    }

    companion object {
        @Volatile var instance: MirrorListenerService? = null
            private set
    }
}

/** Reads Android's notification objects into [MirrorInfo] and acts on them. */
object MirrorAndroid {
    /** The settings screen where the user grants Notification access to this app (the detail page where Android has one). */
    fun accessSettingsIntent(ctx: Context): Intent {
        val component = ComponentName(ctx, MirrorListenerService::class.java)
        val detail = Intent(Settings.ACTION_NOTIFICATION_LISTENER_DETAIL_SETTINGS)
            .putExtra(Settings.EXTRA_NOTIFICATION_LISTENER_COMPONENT_NAME, component.flattenToString())
        return if (android.os.Build.VERSION.SDK_INT >= 30 && detail.resolveActivity(ctx.packageManager) != null) {
            detail
        } else {
            Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS)
        }
    }

    /** Whether the user has granted Notification access to this app. */
    fun accessGranted(ctx: Context): Boolean {
        val enabled = Settings.Secure.getString(ctx.contentResolver, "enabled_notification_listeners").orEmpty()
        return enabled.split(':').any { ComponentName.unflattenFromString(it)?.packageName == ctx.packageName }
    }

    private val labels = HashMap<String, String>()

    @Synchronized
    private fun label(ctx: Context, pkg: String): String = labels.getOrPut(pkg) {
        try {
            val pm = ctx.packageManager
            pm.getApplicationLabel(pm.getApplicationInfo(pkg, 0)).toString()
        } catch (_: Exception) {
            pkg
        }
    }

    private fun replyLess(a: Notification.Action) = a.remoteInputs.isNullOrEmpty() && a.actionIntent != null && !a.title.isNullOrBlank()

    fun info(ctx: Context, sbn: StatusBarNotification, ranking: NotificationListenerService.RankingMap?): MirrorInfo? {
        val n = sbn.notification ?: return null
        val extras = n.extras
        val importance = ranking?.let {
            val r = NotificationListenerService.Ranking()
            if (it.getRanking(sbn.key, r)) r.importance else null
        }
        return MirrorInfo(
            key = sbn.key,
            pkg = sbn.packageName,
            appName = label(ctx, sbn.packageName),
            title = extras?.getCharSequence(Notification.EXTRA_TITLE)?.toString().orEmpty(),
            text = (extras?.getCharSequence(Notification.EXTRA_TEXT) ?: extras?.getCharSequence(Notification.EXTRA_BIG_TEXT))?.toString().orEmpty(),
            whenMs = if (n.`when` > 0) n.`when` else sbn.postTime,
            ongoing = (n.flags and Notification.FLAG_ONGOING_EVENT) != 0,
            foregroundService = (n.flags and Notification.FLAG_FOREGROUND_SERVICE) != 0,
            groupSummary = (n.flags and Notification.FLAG_GROUP_SUMMARY) != 0,
            category = n.category,
            secret = n.visibility == Notification.VISIBILITY_SECRET,
            clearable = sbn.isClearable,
            quiet = importance != null && importance < android.app.NotificationManager.IMPORTANCE_DEFAULT,
            actions = n.actions.orEmpty().filter(::replyLess).map { it.title.toString() },
        )
    }

    /** The port the mirror uses on a real phone. */
    class Port(private val ctx: Context) : MirrorPort {
        private fun service() = MirrorListenerService.instance

        override fun active(): List<MirrorInfo> {
            val s = service() ?: return emptyList()
            val ranking = try { s.currentRanking } catch (_: RuntimeException) { null }
            return try {
                s.activeNotifications.orEmpty().mapNotNull { info(ctx, it, ranking) }
            } catch (_: RuntimeException) {
                emptyList()
            }
        }

        override fun cancel(key: String): Boolean {
            val s = service() ?: return false
            return try { s.cancelNotification(key); true } catch (_: RuntimeException) { false }
        }

        override fun press(key: String, title: String): Boolean {
            val s = service() ?: return false
            val sbn = try { s.activeNotifications.orEmpty().firstOrNull { it.key == key } } catch (_: RuntimeException) { null } ?: return false
            val action = sbn.notification.actions.orEmpty().firstOrNull { replyLess(it) && MirrorRules.clip(it.title.toString(), MirrorRules.MAX_ACTION) == title } ?: return false
            return try { action.actionIntent.send(); true } catch (_: PendingIntent.CanceledException) { false }
        }
    }
}
