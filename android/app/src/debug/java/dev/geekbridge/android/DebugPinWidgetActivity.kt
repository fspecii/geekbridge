// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.appwidget.AppWidgetManager
import android.content.ComponentName
import android.os.Bundle

/** Debug builds only: lets an adb script pin a widget (the launcher still asks to confirm). Needs a foreground app, hence an activity. */
class DebugPinWidgetActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Screenshots only: push a fake state straight to the widgets (`-e demo connected|ringing|offline`); the next
        // real update replaces it. Nothing here touches the real link state.
        intent.getStringExtra("demo")?.let { demo ->
            val word = when (demo) { "ringing" -> ConnWord.Ringing; "offline" -> ConnWord.Offline; else -> ConnWord.Connected }
            val state = ConnectionState(word, "Studio PC")
            val mgr = getSystemService(AppWidgetManager::class.java)
            mgr.getAppWidgetIds(ComponentName(this, ConnectionWidget::class.java)).let { if (it.isNotEmpty()) mgr.updateAppWidget(it, WidgetViews.connection(this, state)) }
            mgr.getAppWidgetIds(ComponentName(this, QuickActionsWidget::class.java)).let { if (it.isNotEmpty()) mgr.updateAppWidget(it, WidgetViews.quick(this, state, QuickInputs(answerOn = true, clipboardOn = true))) }
        }
        val cls = when (intent.getStringExtra("w")) {
            "connection" -> ConnectionWidget::class.java
            "quick" -> QuickActionsWidget::class.java
            "agenda" -> AgendaWidget::class.java
            else -> null
        }
        if (cls != null) getSystemService(AppWidgetManager::class.java).requestPinAppWidget(ComponentName(this, cls), null, null)
        finish()
    }
}
