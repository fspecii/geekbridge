// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import android.text.format.DateUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView
import java.util.Calendar
import java.util.Locale

/**
 * Pure month layout for the dot calendar: which weekday the 1st falls on, how many days, how many week rows.
 * [firstDayOfWeek] uses [Calendar] constants (SUNDAY = 1 ... SATURDAY = 7). No Android types, so it is unit-tested.
 */
data class MonthLayout(val year: Int, val month: Int, val leadingBlanks: Int, val days: Int) {
    val rows: Int get() = (leadingBlanks + days + 6) / 7

    /** Day of month shown in grid cell [index] (0-based, row-major), or null for a blank cell. */
    fun dayAt(index: Int): Int? = (index - leadingBlanks + 1).takeIf { it in 1..days }

    companion object {
        /** [month] is 0-based like [Calendar.MONTH]. */
        fun of(year: Int, month: Int, firstDayOfWeek: Int): MonthLayout {
            val c = Calendar.getInstance().apply { clear(); set(year, month, 1) }
            val dow = c.get(Calendar.DAY_OF_WEEK) // 1..7
            val blanks = ((dow - firstDayOfWeek) % 7 + 7) % 7
            return MonthLayout(year, month, blanks, c.getActualMaximum(Calendar.DAY_OF_MONTH))
        }
    }
}

/** One agenda line: [time] is already formatted for display (and for TalkBack). */
data class AgendaEntry(val title: String, val time: String, val detail: String? = null)

/**
 * Dot-matrix month grid: every day is a dot (grey = nothing, ink = has events, red = today, ring = selected),
 * with the day number above it so it can be found by eye. Each day is its own 48dp-tall focus stop that
 * TalkBack reads as the full date with its event count; weekday letters are decorative column heads.
 * Static: [bind] redraws on state change, nothing animates.
 */
class DotCalendar(private val kit: UiKit) : LinearLayout(kit.context) {
    private val cells = ArrayList<DayCell>()

    init { orientation = VERTICAL }

    /**
     * @param eventCounts events per day of month (missing = none)
     * @param today day of month that is today in this month, or null
     * @param selected the selected day of month, or null
     */
    fun bind(
        year: Int,
        month: Int,
        firstDayOfWeek: Int = Calendar.getInstance().firstDayOfWeek,
        eventCounts: Map<Int, Int> = emptyMap(),
        today: Int? = null,
        selected: Int? = null,
        onSelect: ((Int) -> Unit)? = null,
    ) {
        removeAllViews()
        cells.clear()
        val layout = MonthLayout.of(year, month, firstDayOfWeek)
        val heads = LinearLayout(context).apply { orientation = HORIZONTAL }
        val names = java.text.DateFormatSymbols.getInstance(Locale.getDefault()).shortWeekdays
        for (i in 0 until 7) {
            val dow = (firstDayOfWeek - 1 + i) % 7 + 1
            heads.addView(
                TextView(context).apply {
                    text = names[dow].take(1).uppercase(Locale.getDefault())
                    setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
                    typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
                    setTextColor(context.getColor(R.color.gb_muted))
                    gravity = Gravity.CENTER
                    importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
                },
                LayoutParams(0, LayoutParams.WRAP_CONTENT, 1f),
            )
        }
        addView(heads, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT).apply { bottomMargin = kit.dp(4) })
        val cal = Calendar.getInstance().apply { clear() }
        for (r in 0 until layout.rows) {
            val row = LinearLayout(context).apply { orientation = HORIZONTAL }
            for (c in 0 until 7) {
                val day = layout.dayAt(r * 7 + c)
                val cell = DayCell(context, kit.dp(Tok.TARGET))
                if (day != null) {
                    cal.set(year, month, day)
                    val n = eventCounts[day] ?: 0
                    val full = DateUtils.formatDateTime(context, cal.timeInMillis, DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_WEEKDAY or DateUtils.FORMAT_SHOW_YEAR)
                    val parts = ArrayList<String>()
                    parts += full
                    if (day == today) parts += context.getString(R.string.cal_today)
                    parts += if (n == 0) context.getString(R.string.cal_no_events) else context.resources.getQuantityString(R.plurals.cal_events, n, n)
                    cell.set(day, n, day == today, day == selected, parts.joinToString(", "), onSelect)
                    cells += cell
                }
                row.addView(cell, LayoutParams(0, LayoutParams.WRAP_CONTENT, 1f))
            }
            addView(row, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT))
        }
    }

    private class DayCell(context: Context, private val minH: Int) : View(context) {
        private val dens = resources.displayMetrics.density
        private val ink = context.getColor(R.color.gb_text)
        private val muted = context.getColor(R.color.gb_muted)
        private val dim = context.getColor(R.color.gb_dot_off)
        private val red = context.getColor(R.color.gb_red)
        private val p = Paint(Paint.ANTI_ALIAS_FLAG)
        private val num = Paint(Paint.ANTI_ALIAS_FLAG).apply { textAlign = Paint.Align.CENTER; typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL) }
        private var day = 0
        private var events = 0
        private var isToday = false
        private var isSelected = false

        fun set(day: Int, events: Int, today: Boolean, selected: Boolean, description: String, onSelect: ((Int) -> Unit)?) {
            this.day = day; this.events = events; isToday = today; isSelected = selected
            contentDescription = description
            isSelected = selected
            if (onSelect != null) {
                isClickable = true
                isFocusable = true
                setOnClickListener { onSelect(day) }
                accessibilityDelegate = object : AccessibilityDelegate() {
                    override fun onInitializeAccessibilityNodeInfo(host: View, info: android.view.accessibility.AccessibilityNodeInfo) {
                        super.onInitializeAccessibilityNodeInfo(host, info)
                        info.className = android.widget.Button::class.java.name
                        info.isSelected = selected
                    }
                }
            }
            invalidate()
        }

        override fun onMeasure(w: Int, h: Int) {
            val fs = resources.configuration.fontScale
            setMeasuredDimension(MeasureSpec.getSize(w), (minH * maxOf(1f, minOf(fs, 1.6f))).toInt())
        }

        override fun onDraw(canvas: Canvas) {
            if (day == 0) return
            val cx = width / 2f
            num.textSize = 12f * dens * minOf(resources.configuration.fontScale, 1.6f)
            val numColor = if (isToday) red else if (events > 0) ink else muted
            num.color = numColor
            val dotY = height - 11f * dens
            canvas.drawText(day.toString(), cx, dotY - 9f * dens, num)
            when {
                isToday -> { p.style = Paint.Style.FILL; p.color = red; canvas.drawCircle(cx, dotY, 4.6f * dens, p) }
                events > 0 -> { p.style = Paint.Style.FILL; p.color = ink; canvas.drawCircle(cx, dotY, 3.6f * dens, p) }
                else -> { p.style = Paint.Style.FILL; p.color = dim; canvas.drawCircle(cx, dotY, 2.2f * dens, p) }
            }
            if (isSelected) {
                p.style = Paint.Style.STROKE; p.strokeWidth = 1.6f * dens; p.color = ink
                canvas.drawCircle(cx, (height / 2f), minOf(width, height) / 2f - 3f * dens, p)
            }
        }
    }
}

/** Month card in the reference style: big dot-matrix day of month, red month/year, then the dot grid. */
fun UiKit.monthCard(parent: LinearLayout, year: Int, month: Int, dayOfMonth: Int, grid: DotCalendar): LinearLayout {
    val card = panel(parent, 16, color(R.color.gb_panel), null)
    val head = LinearLayout(context).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
    head.addView(DotText(context, "%02d".format(Locale.ROOT, dayOfMonth), maxPitchDp = Tok.DOT_NUMERAL, maxLines = 1), LinearLayout.LayoutParams(0, Ui.WRAP, 1f))
    val cal = Calendar.getInstance().apply { clear(); set(year, month, 1) }
    head.addView(
        TextView(context).apply {
            text = DateUtils.formatDateTime(context, cal.timeInMillis, DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_NO_MONTH_DAY or DateUtils.FORMAT_SHOW_YEAR)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            setTextColor(color(R.color.gb_warn))
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            if (android.os.Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
        },
    )
    card.addView(head, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { bottomMargin = dp(10) })
    card.addView(grid, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
    return card
}

/**
 * Agenda card for one day: dot-matrix day number + red month, then a vertical timeline. The first entry is the red
 * "now / next" dot, later ones are ink dots joined by a dotted line. [add], when given, shows a "+" pill.
 */
fun UiKit.agendaCard(parent: LinearLayout, dayOfMonth: Int, monthLabel: String, entries: List<AgendaEntry>, emptyText: String, add: (() -> Unit)? = null): LinearLayout {
    val card = panel(parent, 16, color(R.color.gb_panel), null)
    val head = LinearLayout(context).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
    head.addView(DotText(context, "%02d".format(Locale.ROOT, dayOfMonth), maxPitchDp = Tok.DOT_NUMERAL, maxLines = 1), LinearLayout.LayoutParams(0, Ui.WRAP, 1f))
    head.addView(text(monthLabel, 15f, Tone.Bad, bold = true).apply { setTextColor(color(R.color.gb_warn)) })
    card.addView(head, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { bottomMargin = dp(6) })
    if (entries.isEmpty()) card.addView(text(emptyText, 14f, Tone.Muted))
    entries.forEachIndexed { i, e ->
        val row = LinearLayout(context).apply {
            orientation = LinearLayout.HORIZONTAL
            minimumHeight = dp(Tok.TARGET)
            setPadding(0, dp(6), 0, dp(6))
            contentDescription = "${e.time}, ${e.title}" + (e.detail?.let { ", $it" } ?: "")
            isFocusable = true
        }
        row.addView(TimelineDot(context, i == 0, i == entries.lastIndex), LinearLayout.LayoutParams(dp(20), Ui.MATCH))
        val col = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
        col.addView(text(e.title, 15f))
        col.addView(text(e.time + (e.detail?.let { " · $it" } ?: ""), 13f, Tone.Muted))
        row.addView(col, LinearLayout.LayoutParams(0, Ui.WRAP, 1f).apply { marginStart = dp(10) })
        card.addView(row, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
    }
    if (add != null) {
        card.addView(
            pill(context.getString(R.string.cal_add_event), BtnStyle.Tonal) { add() },
            LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = dp(12) },
        )
    }
    return card
}

/** Timeline marker: red dot for the first entry, ink after; a dotted connector runs to the next one. */
private class TimelineDot(context: Context, private val first: Boolean, private val last: Boolean) : View(context) {
    private val dens = resources.displayMetrics.density
    private val p = Paint(Paint.ANTI_ALIAS_FLAG)
    init { importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO }

    override fun onDraw(canvas: Canvas) {
        val cx = width / 2f
        val top = 14f * dens
        p.color = context.getColor(R.color.gb_dot_off)
        if (!last) {
            var y = top + 9f * dens
            while (y < height + 6f * dens) { canvas.drawCircle(cx, y, 1f * dens, p); y += 5f * dens }
        }
        p.color = context.getColor(if (first) R.color.gb_red else R.color.gb_text)
        canvas.drawCircle(cx, top, (if (first) 4.5f else 3.2f) * dens, p)
    }
}
