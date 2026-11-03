// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.time.ZoneId
import java.time.ZonedDateTime
import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FormatTest {
    @Test
    fun contactLine() {
        assertEquals("\u2068Ada\u2069", Format.contactLine(TestStrings, ContactPreview("Ada", null)))
        assertEquals("\u2068Ada\u2069 · \u2066123\u2069", Format.contactLine(TestStrings, ContactPreview("Ada", "123")))
    }

    @Test
    fun allDayEventKeepsUtcDateInAnyZone() {
        val begin = ZonedDateTime.of(2026, 11, 2, 0, 0, 0, 0, ZoneId.of("UTC")).toInstant().toEpochMilli()
        val e = AgendaEvent("Holiday", begin, begin + 86_400_000, allDay = true, location = "Home")
        for (zone in listOf("America/Los_Angeles", "Pacific/Auckland")) {
            val text = Format.eventLines(TestStrings, e, ZoneId.of(zone), Locale.US)
            assertEquals("Holiday\nMon, Nov 2 · all day · Home", text)
        }
    }

    @Test
    fun timedEventUsesLocalZone() {
        val begin = ZonedDateTime.of(2026, 11, 2, 9, 30, 0, 0, ZoneId.of("Europe/Paris")).toInstant().toEpochMilli()
        val e = AgendaEvent("Standup", begin, begin + 900_000, allDay = false, location = null)
        val text = Format.eventLines(TestStrings, e, ZoneId.of("Europe/Paris"), Locale.US)
        assertTrue(text, text.startsWith("Standup\nMon, Nov 2 · "))
        assertTrue(text, text.contains("9:30"))
        assertFalse(text.contains("all day"))
    }

    @Test
    fun outcomesNeverClaimDeliveryOrConnection() {
        val ok = Format.smsOutcome(TestStrings, CoreOutcome.Ok(null))
        assertTrue(ok.contains("Nothing was sent"))
        val prepared = Format.clipboardOutcome(TestStrings, CoreOutcome.Ok(null), 12)
        assertTrue(prepared.contains("Not sent"))
        assertTrue(prepared.contains("12 bytes"))
        for (o in listOf(CoreOutcome.Unavailable("x"), CoreOutcome.Malformed("y"), CoreOutcome.Rejected("c", "m"))) {
            assertFalse(Format.smsOutcome(TestStrings, o).startsWith("Valid"))
            assertFalse(Format.clipboardOutcome(TestStrings, o, 1).startsWith("Prepared"))
        }
    }

    @Test
    fun rejectionShowsCodeAndMessage() {
        assertEquals("Not valid: too long [text_too_long]", Format.smsOutcome(TestStrings, CoreOutcome.Rejected("text_too_long", "too long")))
        assertEquals("Not valid: text_too_long", Format.smsOutcome(TestStrings, CoreOutcome.Rejected("text_too_long", "")))
    }
}
