// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.time.LocalDateTime
import java.time.ZoneId
import java.time.ZoneOffset
import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class WidgetModelTest {
    private val utc = ZoneId.of("UTC")
    private val berlin = ZoneId.of("Europe/Berlin")
    private val la = ZoneId.of("America/Los_Angeles")
    private val tokyo = ZoneId.of("Asia/Tokyo")

    private fun ms(z: ZoneId, y: Int, mo: Int, d: Int, h: Int = 0, mi: Int = 0) =
        LocalDateTime.of(y, mo, d, h, mi).atZone(z).toInstant().toEpochMilli()

    private fun ev(title: String, b: Long, e: Long, allDay: Boolean = false) = AgendaEvent(title, b, e, allDay, null)
    private fun allDay(title: String, y: Int, mo: Int, d: Int, days: Int = 1) =
        ev(title, ms(ZoneOffset.UTC, y, mo, d), ms(ZoneOffset.UTC, y, mo, d) + days * 86_400_000L, true)

    // ---------------------------------------------------------------- connection

    @Test fun unpairedWinsOverEverything() {
        val s = WidgetModel.connection(0, emptyList(), listOf("Desk"), ringing = true)
        assertEquals(ConnectionState(ConnWord.Unpaired, null), s)
    }

    @Test fun connectedShowsTheConnectedComputer() {
        val s = WidgetModel.connection(2, listOf("Laptop", "Desk"), listOf("Desk"), ringing = false)
        assertEquals(ConnectionState(ConnWord.Connected, "Desk"), s)
    }

    @Test fun ringingNeedsAConnectedComputer() {
        assertEquals(ConnWord.Ringing, WidgetModel.connection(1, listOf("Desk"), listOf("Desk"), true).word)
        assertEquals(ConnWord.Offline, WidgetModel.connection(1, listOf("Desk"), emptyList(), true).word)
    }

    @Test fun offlineNamesTheFirstPairedComputerOrNothing() {
        assertEquals(ConnectionState(ConnWord.Offline, "Desk"), WidgetModel.connection(1, listOf("Desk"), emptyList(), false))
        assertEquals(ConnectionState(ConnWord.Offline, null), WidgetModel.connection(1, emptyList(), emptyList(), false))
    }

    @Test fun namesAreBoundedAndCleaned() {
        val s = WidgetModel.connection(1, listOf("x".repeat(100)), emptyList(), false)
        assertTrue(s.computer!!.length <= 32)
        assertEquals("Desk", WidgetModel.connection(1, listOf("‮Desk"), emptyList(), false).computer)
    }

    @Test fun answerTapNeverEnablesWithoutTheSetupScreen() {
        assertEquals(AnswerTap.PairFirst, WidgetModel.answerTap(paired = false, answerOn = true))
        assertEquals(AnswerTap.OpenSetup, WidgetModel.answerTap(paired = true, answerOn = false))
        assertEquals(AnswerTap.TurnOff, WidgetModel.answerTap(paired = true, answerOn = true))
    }

    // ---------------------------------------------------------------- day boundaries

    @Test fun dayBoundsInDifferentZones() {
        val now = ms(tokyo, 2026, 11, 2, 0, 30) // 00:30 in Tokyo is still Nov 1 in UTC
        assertEquals(ms(tokyo, 2026, 11, 2) to ms(tokyo, 2026, 11, 3), WidgetModel.dayBounds(now, tokyo))
        assertEquals(ms(utc, 2026, 11, 1) to ms(utc, 2026, 11, 2), WidgetModel.dayBounds(now, utc))
    }

    @Test fun dstDaysAre23And25Hours() {
        val spring = WidgetModel.dayBounds(ms(berlin, 2026, 3, 29, 12), berlin)
        assertEquals(23 * 3_600_000L, spring.second - spring.first)
        val fall = WidgetModel.dayBounds(ms(berlin, 2026, 10, 25, 12), berlin)
        assertEquals(25 * 3_600_000L, fall.second - fall.first)
        val la = WidgetModel.dayBounds(ms(la, 2026, 11, 1, 12), la)
        assertEquals(25 * 3_600_000L, la.second - la.first)
    }

    @Test fun queryWindowCoversAllDayEventsStoredInUtc() {
        val now = ms(tokyo, 2026, 11, 2, 9)
        val (from, to) = WidgetModel.queryWindow(now, tokyo)
        val (a, b) = WidgetModel.dayBounds(now, tokyo)
        assertEquals(a - 86_400_000L, from)
        assertEquals(b + 86_400_000L, to)
    }

    // ---------------------------------------------------------------- selection

    @Test fun picksTheNextThreeTodayInOrder() {
        val now = ms(berlin, 2026, 11, 2, 8)
        val events = listOf(
            ev("d", ms(berlin, 2026, 11, 2, 15), ms(berlin, 2026, 11, 2, 16)),
            ev("a", ms(berlin, 2026, 11, 2, 9), ms(berlin, 2026, 11, 2, 10)),
            ev("c", ms(berlin, 2026, 11, 2, 13), ms(berlin, 2026, 11, 2, 14)),
            ev("b", ms(berlin, 2026, 11, 2, 11), ms(berlin, 2026, 11, 2, 12)),
            ev("tomorrow", ms(berlin, 2026, 11, 3, 9), ms(berlin, 2026, 11, 3, 10)),
        )
        val (shown, more) = WidgetModel.select(events, now, berlin)
        assertEquals(listOf("a", "b", "c"), shown.map { it.title })
        assertEquals(1, more)
    }

    @Test fun finishedEventsAreDroppedOngoingOnesStay() {
        val now = ms(berlin, 2026, 11, 2, 10, 30)
        val events = listOf(
            ev("over", ms(berlin, 2026, 11, 2, 9), ms(berlin, 2026, 11, 2, 10)),
            ev("now", ms(berlin, 2026, 11, 2, 10), ms(berlin, 2026, 11, 2, 11)),
            ev("later", ms(berlin, 2026, 11, 2, 12), ms(berlin, 2026, 11, 2, 13)),
        )
        assertEquals(listOf("now", "later"), WidgetModel.select(events, now, berlin).first.map { it.title })
    }

    @Test fun endingExactlyNowIsOver() {
        val now = ms(utc, 2026, 11, 2, 10)
        val e = ev("x", ms(utc, 2026, 11, 2, 9), now)
        assertTrue(WidgetModel.select(listOf(e), now, utc).first.isEmpty())
    }

    @Test fun anEventThatCrossesMidnightShowsOnBothDays() {
        val e = ev("night", ms(berlin, 2026, 11, 2, 23), ms(berlin, 2026, 11, 3, 1))
        assertEquals(1, WidgetModel.select(listOf(e), ms(berlin, 2026, 11, 2, 20), berlin).first.size)
        assertEquals(1, WidgetModel.select(listOf(e), ms(berlin, 2026, 11, 3, 0, 30), berlin).first.size)
        assertTrue(WidgetModel.select(listOf(e), ms(berlin, 2026, 11, 3, 1, 30), berlin).first.isEmpty())
    }

    @Test fun lateEventBelongsToTodayInItsOwnZoneNotUtc() {
        // 23:30 in Los Angeles is already Nov 3 in UTC.
        val e = ev("late", ms(la, 2026, 11, 2, 23, 30), ms(la, 2026, 11, 3, 0, 15))
        assertEquals(1, WidgetModel.select(listOf(e), ms(la, 2026, 11, 2, 12), la).first.size)
        assertTrue(WidgetModel.select(listOf(e), ms(utc, 2026, 11, 2, 12), utc).first.isEmpty()) // Nov 3 in UTC
    }

    @Test fun allDayCoversLocalDatesAndSortsAfterTimedEvents() {
        val now = ms(la, 2026, 11, 2, 8) // 16:00 UTC Nov 2
        val events = listOf(
            allDay("holiday", 2026, 11, 2),
            allDay("trip", 2026, 11, 1, days = 3), // Nov 1-3
            allDay("yesterday", 2026, 11, 1),
            allDay("tomorrow", 2026, 11, 3),
            ev("lunch", ms(la, 2026, 11, 2, 12), ms(la, 2026, 11, 2, 13)),
        )
        val (shown, more) = WidgetModel.select(events, now, la)
        assertEquals(listOf("lunch", "trip", "holiday"), shown.map { it.title })
        assertEquals(0, more)
    }

    @Test fun allDayDatesAreNotShiftedByTheZone() {
        // An all-day Nov 2 event is Nov 2 for someone in Tokyo (UTC+9) and in Los Angeles (UTC-8), never Nov 1 or 3.
        val e = allDay("day", 2026, 11, 2)
        assertEquals(1, WidgetModel.select(listOf(e), ms(tokyo, 2026, 11, 2, 1), tokyo).first.size)
        assertEquals(1, WidgetModel.select(listOf(e), ms(la, 2026, 11, 2, 23), la).first.size)
        assertTrue(WidgetModel.select(listOf(e), ms(tokyo, 2026, 11, 3, 1), tokyo).first.isEmpty())
        assertTrue(WidgetModel.select(listOf(e), ms(la, 2026, 11, 1, 23), la).first.isEmpty())
    }

    @Test fun zeroLengthEventAtOrAfterNowCounts() {
        val now = ms(utc, 2026, 11, 2, 10)
        assertEquals(1, WidgetModel.select(listOf(ev("ping", now, now)), now, utc).first.size)
        assertTrue(WidgetModel.select(listOf(ev("ping", now - 1, now - 1)), now, utc).first.isEmpty())
    }

    @Test fun emptyWhenNothingLeft() {
        assertEquals(emptyList<AgendaEvent>() to 0, WidgetModel.select(emptyList(), 0L, utc))
    }

    // ---------------------------------------------------------------- refresh time

    @Test fun nextRefreshIsTheEarliestEndOrJustAfterMidnight() {
        val now = ms(berlin, 2026, 11, 2, 9)
        val shown = listOf(
            ev("a", ms(berlin, 2026, 11, 2, 10), ms(berlin, 2026, 11, 2, 11)),
            ev("b", ms(berlin, 2026, 11, 2, 9), ms(berlin, 2026, 11, 2, 9, 45)),
        )
        assertEquals(ms(berlin, 2026, 11, 2, 9, 45), WidgetModel.nextRefresh(shown, now, berlin))
        assertEquals(ms(berlin, 2026, 11, 3, 0, 1), WidgetModel.nextRefresh(emptyList(), now, berlin))
        assertEquals(ms(berlin, 2026, 11, 3, 0, 1), WidgetModel.nextRefresh(listOf(allDay("x", 2026, 11, 2)), now, berlin))
    }

    @Test fun nextRefreshNeverFiresInThePastAndNeverSkipsMidnight() {
        val now = ms(utc, 2026, 11, 2, 23, 59)
        val soon = ev("x", now - 1000, now + 1000)
        assertEquals(now + 60_000L, WidgetModel.nextRefresh(listOf(soon), now, utc).coerceAtMost(now + 60_000L))
        val far = ev("y", now, ms(utc, 2026, 11, 5, 9))
        assertEquals(ms(utc, 2026, 11, 3, 0, 1), WidgetModel.nextRefresh(listOf(far), now, utc))
    }

    // ---------------------------------------------------------------- labels

    @Test fun dotTimeIn24And12Hours() {
        assertEquals("09:05", WidgetModel.dotTime(ms(utc, 2026, 11, 2, 9, 5), utc, true))
        assertEquals("9:05A", WidgetModel.dotTime(ms(utc, 2026, 11, 2, 9, 5), utc, false))
        assertEquals("12:00A", WidgetModel.dotTime(ms(utc, 2026, 11, 2, 0, 0), utc, false))
        assertEquals("12:30P", WidgetModel.dotTime(ms(utc, 2026, 11, 2, 12, 30), utc, false))
        assertEquals("11:59P", WidgetModel.dotTime(ms(utc, 2026, 11, 2, 23, 59), utc, false))
    }

    @Test fun dotTimeUsesTheZone() {
        val t = ms(utc, 2026, 11, 2, 23, 30)
        assertEquals("08:30", WidgetModel.dotTime(t, tokyo, true))
        assertEquals("15:30", WidgetModel.dotTime(t, la, true))
    }

    @Test fun dotLabelsOnlyUseGlyphsTheDotFontHas() {
        for (t in listOf("09:05", "12:30P", "ALL DAY")) for (c in t) {
            assertTrue("no glyph for '$c'", DotFont.glyph(c).any { r -> r.contains('1') } || c == ' ')
        }
    }

    @Test fun rowsCarryWordsForTalkBackNotDots() {
        val rows = WidgetModel.rows(
            listOf(ev("Team sync", ms(utc, 2026, 11, 2, 9, 30), ms(utc, 2026, 11, 2, 10)), allDay("Holiday", 2026, 11, 2), ev("", ms(utc, 2026, 11, 2, 12), ms(utc, 2026, 11, 2, 13))),
            utc, true, "ALL DAY", "All day", "No title", Locale.US,
        )
        assertEquals("09:30", rows[0].dots)
        assertEquals("9:30 AM", rows[0].spoken)
        assertEquals(WidgetAgendaRow("Holiday", "ALL DAY", "All day"), rows[1])
        assertEquals("No title", rows[2].title)
    }
}
