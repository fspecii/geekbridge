// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.content.Intent
import android.widget.Toast

/** Queue an update and keep the broadcast alive (goAsync) until the worker has pushed it. */
private fun AppWidgetProvider.refresh(ctx: Context, mask: Int) {
    val pending = goAsync()
    WidgetUpdater.request(ctx, mask) { pending?.finish() }
}

/** 2x1 "Connection": the paired computer and the link state in dots. Updated by [WidgetUpdater]; no timer. */
class ConnectionWidget : AppWidgetProvider() {
    override fun onUpdate(context: Context, appWidgetManager: AppWidgetManager, appWidgetIds: IntArray) =
        refresh(context, WidgetUpdater.CONNECTION)
}

/** 2x2 "Quick actions": send file, send clipboard, paste from computer, answer-from-computer switch. */
class QuickActionsWidget : AppWidgetProvider() {
    override fun onUpdate(context: Context, appWidgetManager: AppWidgetManager, appWidgetIds: IntArray) =
        refresh(context, WidgetUpdater.QUICK)

    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            WidgetViews.ACTION_PASTE_HINT -> Toast.makeText(context, R.string.widget_paste_hint, Toast.LENGTH_LONG).show()
            WidgetViews.ACTION_ANSWER_OFF -> {
                AppState.init(context)
                CallControlRuntime.setEnabled(false)
                WidgetUpdater.request(context, WidgetUpdater.QUICK)
                Toast.makeText(context, R.string.widget_answer_off_toast, Toast.LENGTH_SHORT).show()
            }
            else -> super.onReceive(context, intent)
        }
    }
}

/**
 * 4x2 "Agenda": today's next three events. Its content observer and its single inexact alarm exist only while an
 * instance of this widget does: [onEnabled] arms them through the first update, [onDisabled] removes both.
 */
class AgendaWidget : AppWidgetProvider() {
    override fun onUpdate(context: Context, appWidgetManager: AppWidgetManager, appWidgetIds: IntArray) =
        refresh(context, WidgetUpdater.AGENDA)

    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            WidgetUpdater.ACTION_TICK, Intent.ACTION_TIMEZONE_CHANGED, Intent.ACTION_TIME_CHANGED, Intent.ACTION_DATE_CHANGED ->
                refresh(context, WidgetUpdater.AGENDA)
            else -> super.onReceive(context, intent)
        }
    }

    override fun onDisabled(context: Context) = WidgetUpdater.agendaDisabled(context)
}
