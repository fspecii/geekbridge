// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/** The word on the Connection widget, in dot-matrix. Pure, so the mapping is unit-tested. */
enum class ConnWord { Connected, Ringing, Offline, Unpaired }

/** [computer] is the name shown above the word; null when no computer is paired. */
data class ConnectionState(val word: ConnWord, val computer: String?)

/** What a tap on the Answer-from-computer button does. Turning it on needs the setup screen (permission + per-computer grant). */
enum class AnswerTap { PairFirst, OpenSetup, TurnOff }

/** One agenda row: [dots] is the time drawn in dots ("09:30", "ALL DAY"); [spoken] is what TalkBack reads. */
data class WidgetAgendaRow(val title: String, val dots: String, val spoken: String)

sealed interface AgendaWidgetState {
    data object NoPermission : AgendaWidgetState
    data class Day(val rows: List<WidgetAgendaRow>, val more: Int) : AgendaWidgetState
}

object WidgetModel {
    const val AGENDA_ROWS = 3
    private const val NAME_CHARS = 32
    private const val TITLE_CHARS = 60

    /** CONNECTED / RINGING only when a computer is really connected; ringing means the phone is ringing right now. */
    fun connection(pairedCount: Int, pairedNames: List<String>, connectedNames: List<String>, ringing: Boolean): ConnectionState {
        if (pairedCount <= 0) return ConnectionState(ConnWord.Unpaired, null)
        val connected = connectedNames.firstOrNull()
        if (connected != null) {
            return ConnectionState(if (ringing) ConnWord.Ringing else ConnWord.Connected, name(connected))
        }
        return ConnectionState(ConnWord.Offline, pairedNames.firstOrNull()?.let(::name))
    }

    fun answerTap(paired: Boolean, answerOn: Boolean): AnswerTap = when {
        !paired -> AnswerTap.PairFirst
        answerOn -> AnswerTap.TurnOff
        else -> AnswerTap.OpenSetup
    }

    private fun name(raw: String): String = Bounds.name(raw.trim(), NAME_CHARS)

    // ---------------------------------------------------------------- agenda

    /** [startOfToday, startOfTomorrow) in [zone]; correct on 23- and 25-hour DST days. */
    fun dayBounds(nowMs: Long, zone: ZoneId): Pair<Long, Long> {
        val today = Instant.ofEpochMilli(nowMs).atZone(zone).toLocalDate()
        return today.atStartOfDay(zone).toInstant().toEpochMilli() to today.plusDays(1).atStartOfDay(zone).toInstant().toEpochMilli()
    }

    /**
     * The provider query window: a day either side of today, because all-day events are stored as UTC midnights and
     * can overlap local "today" from 14 hours before or after it. [select] does the exact filtering.
     */
    fun queryWindow(nowMs: Long, zone: ZoneId): Pair<Long, Long> {
        val (a, b) = dayBounds(nowMs, zone)
        return (a - DAY_MS) to (b + DAY_MS)
    }

    /**
     * Today's next events: timed events that have not ended (ongoing ones included) by start time, then all-day
     * events for the remaining slots. An all-day event covers whole local dates (its UTC dates are the local dates).
     * Returns at most [max] events and how many more there were.
     */
    fun select(events: List<AgendaEvent>, nowMs: Long, zone: ZoneId, max: Int = AGENDA_ROWS): Pair<List<AgendaEvent>, Int> {
        val dayEnd = dayBounds(nowMs, zone).second
        val today = Instant.ofEpochMilli(nowMs).atZone(zone).toLocalDate()
        val timed = ArrayList<AgendaEvent>()
        val allDay = ArrayList<AgendaEvent>()
        for (e in events) {
            if (e.allDay) {
                val from = utcDate(e.beginMillis)
                val to = utcDate(maxOf(e.endMillis, e.beginMillis + DAY_MS)) // exclusive
                if (!today.isBefore(from) && today.isBefore(to)) allDay += e
            } else {
                val end = maxOf(e.endMillis, e.beginMillis)
                // Not over yet (or starting now), and starting before tomorrow: includes one that began before midnight.
                if (e.beginMillis < dayEnd && (end > nowMs || e.beginMillis >= nowMs)) timed += e
            }
        }
        timed.sortWith(compareBy({ it.beginMillis }, { it.endMillis }))
        allDay.sortBy { it.beginMillis }
        val all = timed + allDay
        return all.take(max) to (all.size - max).coerceAtLeast(0)
    }

    /**
     * When the widget next changes without any calendar event: the earliest end among shown timed events, else the
     * next local midnight, never earlier than [nowMs] + one minute and never later than one minute past midnight.
     */
    fun nextRefresh(shown: List<AgendaEvent>, nowMs: Long, zone: ZoneId): Long {
        val midnight = dayBounds(nowMs, zone).second + MINUTE_MS
        val nextEnd = shown.filter { !it.allDay && maxOf(it.endMillis, it.beginMillis) > nowMs }
            .minOfOrNull { maxOf(it.endMillis, it.beginMillis) }
        return minOf(midnight, maxOf(nextEnd ?: midnight, nowMs + MINUTE_MS))
    }

    fun rows(selected: List<AgendaEvent>, zone: ZoneId, is24h: Boolean, allDayDots: String, allDaySpoken: String, untitled: String, locale: Locale): List<WidgetAgendaRow> =
        selected.map { e ->
            val title = Bounds.name(e.title, TITLE_CHARS).ifEmpty { untitled }
            if (e.allDay) WidgetAgendaRow(title, allDayDots, allDaySpoken)
            else WidgetAgendaRow(title, dotTime(e.beginMillis, zone, is24h), spokenTime(e.beginMillis, zone, locale))
        }

    /** "09:30" (24 h) or "9:30A" / "9:30P" (12 h; the dot font has no lower case and AM/PM is language-specific). */
    fun dotTime(ms: Long, zone: ZoneId, is24h: Boolean): String {
        val t = Instant.ofEpochMilli(ms).atZone(zone)
        return if (is24h) "%02d:%02d".format(Locale.ROOT, t.hour, t.minute)
        else "%d:%02d%s".format(Locale.ROOT, if (t.hour % 12 == 0) 12 else t.hour % 12, t.minute, if (t.hour < 12) "A" else "P")
    }

    fun spokenTime(ms: Long, zone: ZoneId, locale: Locale): String =
        DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT).withLocale(locale).withZone(zone)
            .format(Instant.ofEpochMilli(ms)).replace(' ', ' ').replace(' ', ' ')

    private fun utcDate(ms: Long): LocalDate = Instant.ofEpochMilli(ms).atZone(ZoneOffset.UTC).toLocalDate()

    private const val MINUTE_MS = 60_000L
    private const val DAY_MS = 24L * 60 * 60 * 1000
}
