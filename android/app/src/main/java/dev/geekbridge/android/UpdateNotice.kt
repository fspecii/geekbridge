// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/** What the active network looks like to the update notice. */
enum class NoticeNet { None, Unmetered, Metered }

/**
 * When the "Update available" notice may ask GitHub, and what it may show. Pure functions of stored times and
 * facts (no clock, no Android), so they are tested with fake clocks. Everything else about a check (ETag, the
 * stable/dev channel, the "must be newer" rules) stays in [UpdateChecker] and [UpdateModel].
 */
object UpdateNoticePolicy {
    /** A successful check (from Home, the link service or Diagnostics) satisfies the notice for this long. */
    const val INTERVAL_MS = 24L * 60 * 60 * 1000

    /** After a failed attempt the next one waits this long. */
    const val RETRY_MS = UpdateModel.RETRY_GAP_MS

    /** On a metered network the check waits until the last good answer is this old (it is a tiny conditional request). */
    const val METERED_PATIENCE_MS = 7L * 24 * 60 * 60 * 1000

    /** A clock that moved backwards counts as due, like [UpdateModel.autoCheckDue]. */
    fun due(enabled: Boolean, net: NoticeNet, lastSuccessMs: Long, lastAttemptMs: Long, nowMs: Long): Boolean {
        if (!enabled || net == NoticeNet.None) return false
        val failedLast = lastAttemptMs > lastSuccessMs
        if (failedLast && !UpdateModel.attemptDue(lastAttemptMs, nowMs)) return false
        val sinceSuccess = nowMs - lastSuccessMs
        val neverOrBackwards = lastSuccessMs <= 0 || nowMs < lastSuccessMs
        if (!neverOrBackwards && sinceSuccess < INTERVAL_MS) return false
        if (net == NoticeNet.Metered && !neverOrBackwards && sinceSuccess < METERED_PATIENCE_MS) return false
        return true
    }

    /**
     * Whether a check result may be remembered as "an update is available". Stable never offers a prerelease and
     * only a higher version code; dev only follows the dev channel (whose manifest code was compared already).
     */
    fun accept(offer: UpdateOffer, channel: UpdateChannel, currentCode: Int): Boolean {
        if (offer.dev != (channel == UpdateChannel.Dev)) return false
        if (offer.dev) return true
        val v = AppVersion.parse(offer.version) ?: return false
        val code = v.toVersionCode() ?: return false
        return !v.isPrerelease && code > currentCode
    }

    /**
     * The remembered offer still counts only for the build and channel it was found for: after an install or a
     * channel switch the chip disappears until the next check says otherwise.
     */
    fun chipShown(enabled: Boolean, version: String?, foundFor: Int, foundChannel: String?, currentCode: Int, channel: UpdateChannel): Boolean =
        enabled && !version.isNullOrEmpty() && foundFor == currentCode && foundChannel == channel.id

    /** One notification per offered version, only from the background, only when the user switched it on. */
    fun shouldNotify(notifyOn: Boolean, background: Boolean, version: String, lastNotified: String?): Boolean =
        notifyOn && background && version != lastNotified
}

/** Runs the notice's check and keeps its tiny state in the existing `updates` preferences (no new file). */
object UpdateNotice {
    const val EXTRA_SHOW_UPDATE = "show_update"
    const val KEY_NOTIFY = "notice_notify"
    private const val KEY_VERSION = "notice_version"
    private const val KEY_FOUND_FOR = "notice_found_for"
    private const val KEY_FOUND_CHANNEL = "notice_found_channel"
    private const val KEY_NOTIFIED = "notice_notified"
    private const val CHANNEL = "update"
    private const val NOTICE_ID = 0x4000_0200

    private val running = AtomicBoolean(false)
    private val worker by lazy { Executors.newSingleThreadExecutor { r -> Thread(r, "geekbridge-update-notice").apply { isDaemon = true } } }

    private fun prefs(ctx: Context) = ctx.applicationContext.getSharedPreferences("updates", Context.MODE_PRIVATE)

    fun autoCheckOn(ctx: Context) = prefs(ctx).getBoolean("auto_check", true)
    fun notifyOn(ctx: Context) = prefs(ctx).getBoolean(KEY_NOTIFY, false)
    fun setNotify(ctx: Context, on: Boolean) = prefs(ctx).edit().putBoolean(KEY_NOTIFY, on).apply()

    /** The version the Home chip announces, or null. */
    fun pendingVersion(ctx: Context): String? {
        val p = prefs(ctx)
        val v = p.getString(KEY_VERSION, null)
        return v.takeIf {
            UpdateNoticePolicy.chipShown(
                autoCheckOn(ctx), v, p.getInt(KEY_FOUND_FOR, 0), p.getString(KEY_FOUND_CHANNEL, null),
                BuildConfig.VERSION_CODE, UpdateChecker.channelOf(ctx),
            )
        }
    }

    /** Remembers (or forgets) the offer from any check, including the one in Diagnostics. */
    fun record(ctx: Context, result: UpdateCheckResult) {
        val p = prefs(ctx)
        val channel = UpdateChecker.channelOf(ctx)
        when (result) {
            is UpdateCheckResult.Available ->
                if (UpdateNoticePolicy.accept(result.offer, channel, BuildConfig.VERSION_CODE)) {
                    p.edit().putString(KEY_VERSION, result.offer.version).putInt(KEY_FOUND_FOR, BuildConfig.VERSION_CODE)
                        .putString(KEY_FOUND_CHANNEL, channel.id).apply()
                } else {
                    p.edit().remove(KEY_VERSION).apply()
                }
            is UpdateCheckResult.UpToDate, is UpdateCheckResult.NoPublicRelease -> p.edit().remove(KEY_VERSION).apply()
            is UpdateCheckResult.Failed -> Unit
        }
    }

    private fun network(ctx: Context): NoticeNet {
        val cm = ctx.getSystemService(ConnectivityManager::class.java) ?: return NoticeNet.None
        val net = cm.activeNetwork ?: return NoticeNet.None
        val caps = cm.getNetworkCapabilities(net) ?: return NoticeNet.None
        if (!caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) ||
            !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
        ) return NoticeNet.None
        return if (caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)) NoticeNet.Unmetered else NoticeNet.Metered
    }

    /**
     * Asks GitHub if the schedule says so; never from a timer of its own. Callers are events that happen anyway:
     * Home becoming visible and the link service seeing a Wi-Fi network. [onDone] runs on the worker thread.
     */
    fun maybeCheck(ctx: Context, background: Boolean, onDone: () -> Unit = {}) {
        val app = ctx.applicationContext
        val p = prefs(app)
        val now = System.currentTimeMillis()
        if (!UpdateNoticePolicy.due(autoCheckOn(app), network(app), p.getLong("last_check_ms", 0), p.getLong("last_attempt_ms", 0), now)) return
        if (!running.compareAndSet(false, true)) return
        try {
            worker.execute {
                try {
                    val channel = UpdateChecker.channelOf(app)
                    val sfx = if (channel == UpdateChannel.Dev) "_dev" else ""
                    val cache = p.getString("etag$sfx", null)?.let { etag ->
                        p.getString("body$sfx", null)?.let { UpdateChecker.Cache(etag, it, p.getString("extra$sfx", null)) }
                    }
                    p.edit().putLong("last_attempt_ms", System.currentTimeMillis()).apply()
                    val result = UpdateChecker.forBuild(app).check(cache) { fresh ->
                        p.edit().putString("etag$sfx", fresh.etag).putString("body$sfx", fresh.body.takeIf { it.length <= 128 * 1024 })
                            .putString("extra$sfx", fresh.extra?.takeIf { it.length <= 16 * 1024 }).apply()
                    }
                    if (result !is UpdateCheckResult.Failed) p.edit().putLong("last_check_ms", System.currentTimeMillis()).apply()
                    record(app, result)
                    val v = pendingVersion(app)
                    if (v != null && UpdateNoticePolicy.shouldNotify(notifyOn(app), background, v, p.getString(KEY_NOTIFIED, null))) {
                        if (notify(app, v)) p.edit().putString(KEY_NOTIFIED, v).apply()
                    }
                    onDone()
                } catch (e: RuntimeException) {
                    GbLog.i("update", "notice check failed: ${e.javaClass.simpleName}")
                } finally {
                    running.set(false)
                }
            }
        } catch (e: java.util.concurrent.RejectedExecutionException) {
            running.set(false)
        }
    }

    /** One low-importance notification that opens the update card; it installs nothing. */
    private fun notify(ctx: Context, version: String): Boolean {
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return false
        if (!nm.areNotificationsEnabled()) return false
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL, ctx.getString(R.string.channel_update), NotificationManager.IMPORTANCE_LOW).apply {
                description = ctx.getString(R.string.channel_update_desc)
                setShowBadge(false)
            },
        )
        val open = PendingIntent.getActivity(
            ctx, 2, openCardIntent(ctx), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val n = android.app.Notification.Builder(ctx, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_link)
            .setContentTitle(ctx.getString(R.string.upd_notice_title))
            .setContentText(ctx.getString(R.string.upd_notice_text, Bidi.isolate(version)))
            .setContentIntent(open)
            .setAutoCancel(true)
            .setOnlyAlertOnce(true)
            .setCategory(android.app.Notification.CATEGORY_RECOMMENDATION)
            .build()
        return try {
            nm.notify(NOTICE_ID, n)
            true
        } catch (_: SecurityException) {
            false
        }
    }

    fun openCardIntent(ctx: Context): Intent =
        Intent(ctx, MainActivity::class.java).putExtra(EXTRA_SHOW_UPDATE, true).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
}
