// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.view.View
import android.widget.RemoteViews
import java.time.Instant
import java.time.ZoneId
import java.time.format.TextStyle
import java.util.Locale

/** What the Quick actions widget needs besides the link state. */
data class QuickInputs(val answerOn: Boolean, val clipboardOn: Boolean)

/**
 * Builds the three widgets' [RemoteViews]. Every tap is an immutable [PendingIntent] to one of our own
 * non-exported components, so it works with the app process dead and cannot be redirected by another app.
 */
object WidgetViews {
    const val ACTION_PASTE_HINT = "dev.geekbridge.android.widget.PASTE_HINT"
    const val ACTION_ANSWER_OFF = "dev.geekbridge.android.widget.ANSWER_OFF"
    private const val FLAGS = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    private const val NEW_TASK = Intent.FLAG_ACTIVITY_NEW_TASK

    fun quickInputs(ctx: Context): QuickInputs {
        ClipboardRuntime.init(ctx)
        return QuickInputs(CallControlRuntime.settings.enabled, ClipboardRuntime.settings.enabled)
    }

    private fun activity(ctx: Context, code: Int, i: Intent): PendingIntent =
        PendingIntent.getActivity(ctx, code, i.addFlags(NEW_TASK), FLAGS)

    private fun broadcast(ctx: Context, code: Int, action: String): PendingIntent =
        PendingIntent.getBroadcast(ctx, code, Intent(ctx, QuickActionsWidget::class.java).setAction(action), FLAGS)

    private fun openApp(ctx: Context, code: Int) = activity(ctx, code, Intent(ctx, HomeActivity::class.java))
    private fun pair(ctx: Context, code: Int) = activity(ctx, code, SetupActivity.intent(ctx, step = StepId.Pair))

    // ------------------------------------------------------------------ Connection (2x1)

    fun connection(ctx: Context, s: ConnectionState): RemoteViews {
        val v = RemoteViews(ctx.packageName, R.layout.widget_connection)
        val word = when (s.word) {
            ConnWord.Connected -> R.string.widget_word_connected
            ConnWord.Ringing -> R.string.widget_word_ringing
            ConnWord.Offline -> R.string.widget_word_offline
            ConnWord.Unpaired -> R.string.widget_word_unpaired
        }
        v.setImageViewBitmap(R.id.conn_word, WidgetDots.mask(ctx.getString(word)))
        v.setImageViewResource(R.id.conn_dot, if (s.word == ConnWord.Connected || s.word == ConnWord.Ringing) R.drawable.widget_dot_red else R.drawable.widget_dot_ring)
        v.setTextViewText(R.id.conn_name, s.computer ?: ctx.getString(R.string.widget_computer_none))
        val name = s.computer.orEmpty()
        val spoken = when (s.word) {
            ConnWord.Connected -> ctx.getString(R.string.widget_a11y_connected, name)
            ConnWord.Ringing -> ctx.getString(R.string.widget_a11y_ringing, name)
            ConnWord.Offline -> if (name.isEmpty()) ctx.getString(R.string.widget_a11y_offline_nameless) else ctx.getString(R.string.widget_a11y_offline, name)
            ConnWord.Unpaired -> ctx.getString(R.string.widget_a11y_unpaired)
        }
        v.setContentDescription(android.R.id.background, spoken)
        v.setOnClickPendingIntent(android.R.id.background, if (s.word == ConnWord.Unpaired) pair(ctx, 10) else openApp(ctx, 10))
        return v
    }

    // ------------------------------------------------------------------ Quick actions (2x2)

    fun quick(ctx: Context, link: ConnectionState, q: QuickInputs): RemoteViews {
        val v = RemoteViews(ctx.packageName, R.layout.widget_quick)
        val paired = link.word != ConnWord.Unpaired
        val dim = if (paired) 1f else 0.45f
        for (id in intArrayOf(R.id.q_file, R.id.q_clip, R.id.q_paste, R.id.q_answer)) v.setFloat(id, "setAlpha", dim)

        val on = ctx.getString(R.string.word_on).uppercase()
        val off = ctx.getString(R.string.word_off).uppercase()
        v.setTextViewText(R.id.q_answer_label, ctx.getString(R.string.widget_q_answer, if (q.answerOn) on else off))
        v.setImageViewResource(R.id.q_answer_dot, if (q.answerOn) R.drawable.widget_dot_red else R.drawable.widget_dot_ring)

        fun said(id: Int, text: Int) {
            val t = ctx.getString(text)
            v.setContentDescription(id, if (paired) t else ctx.getString(R.string.widget_q_pair_first_a11y, t))
        }
        said(R.id.q_file, R.string.widget_q_send_file_a11y)
        said(R.id.q_clip, R.string.widget_q_send_clip_a11y)
        said(R.id.q_paste, R.string.widget_q_paste_a11y)
        said(R.id.q_answer, if (q.answerOn) R.string.widget_q_answer_on_a11y else R.string.widget_q_answer_off_a11y)

        if (!paired) {
            for ((i, id) in intArrayOf(R.id.q_file, R.id.q_clip, R.id.q_paste, R.id.q_answer).withIndex()) v.setOnClickPendingIntent(id, pair(ctx, 20 + i))
            return v
        }
        v.setOnClickPendingIntent(R.id.q_file, activity(ctx, 20, Intent(ctx, WidgetSendFileActivity::class.java)))
        v.setOnClickPendingIntent(
            R.id.q_clip,
            // Clipboard sync off: the activity that explains and switches it on, not a silent no-op.
            if (q.clipboardOn) activity(ctx, 21, ClipboardFloatingActivity.intent(ctx)) else activity(ctx, 21, Intent(ctx, MainActivity::class.java)),
        )
        v.setOnClickPendingIntent(
            R.id.q_paste,
            if (q.clipboardOn) broadcast(ctx, 22, ACTION_PASTE_HINT) else activity(ctx, 22, Intent(ctx, MainActivity::class.java)),
        )
        v.setOnClickPendingIntent(
            R.id.q_answer,
            when (WidgetModel.answerTap(paired = true, answerOn = q.answerOn)) {
                AnswerTap.TurnOff -> broadcast(ctx, 23, ACTION_ANSWER_OFF)
                else -> activity(ctx, 23, SetupActivity.intent(ctx, step = StepId.AnswerCalls))
            },
        )
        return v
    }

    // ------------------------------------------------------------------ Agenda (4x2)

    fun agenda(ctx: Context, s: AgendaWidgetState, nowMs: Long, zone: ZoneId): RemoteViews {
        val v = RemoteViews(ctx.packageName, R.layout.widget_agenda)
        val locale = Locale.getDefault()
        val today = Instant.ofEpochMilli(nowMs).atZone(zone)
        val weekday = today.dayOfWeek.getDisplayName(TextStyle.SHORT, locale)
        val month = today.month.getDisplayName(TextStyle.SHORT, locale)
        v.setTextViewText(R.id.ag_weekday, weekday.uppercase(locale))
        v.setTextViewText(R.id.ag_month, month.uppercase(locale))
        v.setImageViewBitmap(R.id.ag_day, WidgetDots.mask(today.dayOfMonth.toString()))
        val dayWords = today.dayOfWeek.getDisplayName(TextStyle.FULL, locale) + " " + today.dayOfMonth + " " + today.month.getDisplayName(TextStyle.FULL, locale)

        val rowIds = intArrayOf(R.id.ag_row1, R.id.ag_row2, R.id.ag_row3)
        val timeIds = intArrayOf(R.id.ag_time1, R.id.ag_time2, R.id.ag_time3)
        val titleIds = intArrayOf(R.id.ag_title1, R.id.ag_title2, R.id.ag_title3)
        for (id in rowIds) v.setViewVisibility(id, View.GONE)
        v.setViewVisibility(R.id.ag_msg, View.GONE)

        val tap: PendingIntent
        val body: String
        when (s) {
            AgendaWidgetState.NoPermission -> {
                v.setTextViewText(R.id.ag_msg, ctx.getString(R.string.widget_agenda_allow))
                v.setViewVisibility(R.id.ag_msg, View.VISIBLE)
                body = ctx.getString(R.string.widget_agenda_allow_a11y)
                tap = activity(ctx, 30, SetupActivity.intent(ctx, step = StepId.Calendar))
            }
            is AgendaWidgetState.Day -> {
                tap = openApp(ctx, 30)
                if (s.rows.isEmpty()) {
                    v.setTextViewText(R.id.ag_msg, ctx.getString(R.string.widget_agenda_none))
                    v.setViewVisibility(R.id.ag_msg, View.VISIBLE)
                    body = ctx.getString(R.string.widget_agenda_none)
                } else {
                    s.rows.forEachIndexed { i, r ->
                        v.setViewVisibility(rowIds[i], View.VISIBLE)
                        v.setImageViewBitmap(timeIds[i], WidgetDots.mask(r.dots))
                        v.setTextViewText(titleIds[i], r.title)
                    }
                    body = s.rows.joinToString(". ") { ctx.getString(R.string.widget_agenda_row_a11y, it.spoken, it.title) } +
                        if (s.more > 0) ". " + ctx.resources.getQuantityString(R.plurals.widget_agenda_more_a11y, s.more, s.more) else ""
                }
            }
        }
        v.setContentDescription(android.R.id.background, ctx.getString(R.string.widget_agenda_today_a11y, dayWords, body))
        v.setOnClickPendingIntent(android.R.id.background, tap)
        return v
    }
}
