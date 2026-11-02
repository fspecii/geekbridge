// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.Manifest
import android.annotation.SuppressLint
import android.app.AlarmManager
import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.database.ContentObserver
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.provider.CalendarContract
import java.time.ZoneId
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * Pushes state to the home-screen widgets. Nothing polls: updates come from the existing link observer, the call
 * state, the settings screens, a calendar [ContentObserver] and one inexact alarm. The observer and the alarm exist
 * only while an Agenda widget does (see [AgendaWidget.onDisabled]). Work runs on one worker thread that is not kept
 * alive while idle, requests coalesce, and nothing here logs titles, names or any other personal data.
 */
@SuppressLint("StaticFieldLeak") // application context only
object WidgetUpdater {
    const val ACTION_TICK = "dev.geekbridge.android.widget.TICK"
    const val CONNECTION = 1
    const val QUICK = 2
    const val AGENDA = 4
    const val ALL = CONNECTION or QUICK or AGENDA

    private val worker = ThreadPoolExecutor(0, 1, 10, TimeUnit.SECONDS, LinkedBlockingQueue()) { r ->
        Thread(r, "geekbridge-widgets").apply { isDaemon = true }
    }
    private val dirty = AtomicInteger(0)
    private val callbacks = ConcurrentLinkedQueue<() -> Unit>()
    private val main = Handler(Looper.getMainLooper())

    @Volatile private var app: Context? = null
    @Volatile private var connected: List<String> = emptyList()
    @Volatile private var ringing = false
    private var linkSig: List<Any>? = null
    private var calendarObserver: ContentObserver? = null
    /** An Agenda widget exists. A queued [observeCalendar] re-checks it on the main thread, so a post that
     * outlives [agendaDisabled] cannot leave an observer registered. */
    @Volatile private var agendaWanted = false
    private val debounced = Runnable { app?.let { request(it, AGENDA) } }

    /** Link observer (main thread, after [AppState.recordPaired]): push only when what the widgets show changed. */
    fun onLink(connectedNames: List<String>) {
        val ctx = app ?: return
        connected = connectedNames
        val sig = listOf(connectedNames, AppState.pairedNames, AppState.pairedCount)
        if (sig == linkSig) return
        linkSig = sig
        request(ctx, CONNECTION or QUICK)
    }

    /** Coarse call state (main thread): Ringing lights the Connection widget up. */
    fun onCall(isRinging: Boolean) {
        val ctx = app ?: return
        if (ringing == isRinging) return
        ringing = isRinging
        request(ctx, CONNECTION)
    }

    /** A setting or permission the widgets show may have changed (Setup screen, answer-calls switch). */
    fun refreshAll(ctx: Context) = request(ctx, ALL)

    /** Application.onCreate: remember the context the observers push with (nothing else happens here). */
    fun attach(ctx: Context) { app = ctx.applicationContext }

    /** Process start (worker thread of the application): restore the calendar observer if an Agenda widget exists. */
    fun onProcessStart(ctx: Context) {
        app = ctx.applicationContext
        worker.execute { if (ids(ctx, AgendaWidget::class.java).isNotEmpty() && hasCalendar(ctx)) { agendaWanted = true; main.post { observeCalendar(ctx) } } }
    }

    /** Queues an update of the widgets in [mask]; [done] runs after it (used to finish a goAsync broadcast). */
    fun request(ctx: Context, mask: Int, done: (() -> Unit)? = null) {
        app = ctx.applicationContext
        if (done != null) callbacks.add(done)
        if (dirty.getAndUpdate { it or mask } == 0) {
            try {
                worker.execute { drain(ctx.applicationContext) }
            } catch (_: RuntimeException) {
                dirty.set(0)
                runCallbacks()
            }
        }
    }

    private fun drain(ctx: Context) {
        val m = dirty.getAndSet(0)
        try {
            AppState.init(ctx) // widgets can update in a process the app never started
            val mgr = AppWidgetManager.getInstance(ctx)
            if (m and CONNECTION != 0) push(mgr, ctx, ConnectionWidget::class.java) { WidgetViews.connection(ctx, connectionState()) }
            if (m and QUICK != 0) push(mgr, ctx, QuickActionsWidget::class.java) { WidgetViews.quick(ctx, connectionState(), WidgetViews.quickInputs(ctx)) }
            if (m and AGENDA != 0) updateAgenda(ctx, mgr)
        } catch (e: RuntimeException) {
            // A widget update must never take the process down; the next event retries. Class name only: no content in logs.
            android.util.Log.w("GBWidget", "update failed: " + e.javaClass.simpleName)
        } finally {
            runCallbacks()
        }
    }

    private fun runCallbacks() {
        while (true) (callbacks.poll() ?: return).invoke()
    }

    private fun connectionState(): ConnectionState =
        WidgetModel.connection(AppState.pairedCount, AppState.pairedNames, connected, ringing)

    private fun push(mgr: AppWidgetManager, ctx: Context, cls: Class<*>, views: () -> android.widget.RemoteViews) {
        val ids = mgr.getAppWidgetIds(ComponentName(ctx, cls))
        if (ids.isNotEmpty()) mgr.updateAppWidget(ids, views())
    }

    private fun ids(ctx: Context, cls: Class<*>): IntArray =
        AppWidgetManager.getInstance(ctx).getAppWidgetIds(ComponentName(ctx, cls))

    // ------------------------------------------------------------------ agenda

    private fun updateAgenda(ctx: Context, mgr: AppWidgetManager) {
        val ids = mgr.getAppWidgetIds(ComponentName(ctx, AgendaWidget::class.java))
        if (ids.isEmpty()) return
        val now = System.currentTimeMillis()
        val zone = ZoneId.systemDefault()
        if (!hasCalendar(ctx)) {
            agendaWanted = false
            stopObservingCalendar(ctx)
            mgr.updateAppWidget(ids, WidgetViews.agenda(ctx, AgendaWidgetState.NoPermission, now, zone))
            scheduleTick(ctx, WidgetModel.dayBounds(now, zone).second + 60_000L)
            return
        }
        val (from, to) = WidgetModel.queryWindow(now, zone)
        val events = WidgetCalendar.load(ctx, from, to)
        val (shown, more) = WidgetModel.select(events, now, zone)
        val rows = WidgetModel.rows(
            shown, zone, android.text.format.DateFormat.is24HourFormat(ctx),
            ctx.getString(R.string.widget_agenda_all_day).uppercase(), ctx.getString(R.string.widget_agenda_all_day),
            ctx.getString(R.string.widget_agenda_untitled), java.util.Locale.getDefault(),
        )
        mgr.updateAppWidget(ids, WidgetViews.agenda(ctx, AgendaWidgetState.Day(rows, more), now, zone))
        scheduleTick(ctx, WidgetModel.nextRefresh(shown, now, zone))
        agendaWanted = true
        main.post { observeCalendar(ctx) }
    }

    private fun hasCalendar(ctx: Context) =
        ctx.checkSelfPermission(Manifest.permission.READ_CALENDAR) == PackageManager.PERMISSION_GRANTED

    private fun tickIntent(ctx: Context): PendingIntent = PendingIntent.getBroadcast(
        ctx, 0, Intent(ctx, AgendaWidget::class.java).setAction(ACTION_TICK),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    /** The one alarm: replaces itself on every update, inexact (no permission, may be batched by the system). */
    private fun scheduleTick(ctx: Context, atMs: Long) {
        ctx.getSystemService(AlarmManager::class.java)?.set(AlarmManager.RTC, atMs, tickIntent(ctx))
    }

    @Synchronized // main thread
    private fun observeCalendar(ctx: Context) {
        if (!agendaWanted || calendarObserver != null) return
        val o = object : ContentObserver(main) {
            override fun onChange(selfChange: Boolean) {
                // A sync can fire a burst of changes: one refresh after things settle.
                main.removeCallbacks(debounced)
                main.postDelayed(debounced, 2_000L)
            }
        }
        val uri: Uri = CalendarContract.CONTENT_URI
        ctx.applicationContext.contentResolver.registerContentObserver(uri, true, o)
        calendarObserver = o
    }

    @Synchronized
    private fun stopObservingCalendar(ctx: Context) {
        main.removeCallbacks(debounced)
        calendarObserver?.let { ctx.applicationContext.contentResolver.unregisterContentObserver(it) }
        calendarObserver = null
    }

    /** Last Agenda widget removed: no observer, no alarm. */
    fun agendaDisabled(ctx: Context) {
        agendaWanted = false
        main.post { stopObservingCalendar(ctx) }
        ctx.getSystemService(AlarmManager::class.java)?.cancel(tickIntent(ctx))
    }
}

/** Reads today's instances from the Calendar provider (READ_CALENDAR checked by the caller), bounded, worker thread only. */
object WidgetCalendar {
    private const val MAX_ROWS = 64

    fun load(ctx: Context, fromMs: Long, toMs: Long): List<AgendaEvent> {
        val uri = CalendarContract.Instances.CONTENT_URI.buildUpon().also {
            android.content.ContentUris.appendId(it, fromMs)
            android.content.ContentUris.appendId(it, toMs)
        }.build()
        val projection = arrayOf(
            CalendarContract.Instances.TITLE, CalendarContract.Instances.BEGIN,
            CalendarContract.Instances.END, CalendarContract.Instances.ALL_DAY,
        )
        val out = ArrayList<AgendaEvent>()
        try {
            ctx.contentResolver.query(uri, projection, null, null, "${CalendarContract.Instances.BEGIN} ASC")?.use {
                while (it.moveToNext() && out.size < MAX_ROWS) {
                    out += AgendaEvent(it.getString(0).orEmpty(), it.getLong(1), it.getLong(2), it.getInt(3) != 0, null)
                }
            }
        } catch (_: SecurityException) {
            // Permission revoked meanwhile: the widget shows what it can (nothing).
        } catch (_: RuntimeException) {
        }
        return out
    }
}
