// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.time.Instant
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/** Pure text builders for the UI, kept free of Android types so they run in JVM tests. */
object Format {
    fun contactLine(c: ContactPreview): String =
        if (c.number == null) c.name else "${c.name} · ${c.number}"

    fun eventLines(e: AgendaEvent, zone: ZoneId, locale: Locale): String {
        val day = DateTimeFormatter.ofPattern("EEE, MMM d", locale)
        val time = DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT).withLocale(locale)
        val span = if (e.allDay) {
            // All-day instances are stored as UTC midnight; converting to the local zone would shift the day.
            "${day.format(Instant.ofEpochMilli(e.beginMillis).atZone(ZoneOffset.UTC))} · all day"
        } else {
            val begin = Instant.ofEpochMilli(e.beginMillis).atZone(zone)
            val end = Instant.ofEpochMilli(e.endMillis).atZone(zone)
            "${day.format(begin)} · ${time.format(begin)}–${time.format(end)}"
        }
        return buildString {
            append(e.title).append('\n').append(span)
            if (e.location != null) append(" · ").append(e.location)
        }
    }

    fun truncationNote(shown: Int, truncated: Boolean, noun: String): String =
        if (truncated) "Showing the first $shown $noun; more exist on this phone." else "Showing $shown $noun."

    /** Result line for the clipboard "prepare" action. */
    fun clipboardOutcome(o: CoreOutcome, bytes: Int): String = when (o) {
        is CoreOutcome.Ok -> "Prepared: the native core accepts this text ($bytes bytes). Held in memory only. " +
            "Not sent — this build cannot send clipboard text to a computer."
        is CoreOutcome.Rejected -> "Not prepared: ${rejection(o)}"
        is CoreOutcome.Unavailable -> "Cannot validate: native core unavailable (${o.reason})."
        is CoreOutcome.Malformed -> "Cannot validate: unreadable native response (${o.reason})."
    }

    /** Result line for SMS validation. */
    fun smsOutcome(o: CoreOutcome): String = when (o) {
        is CoreOutcome.Ok -> "Valid according to the native core. Nothing was sent and nothing will be."
        is CoreOutcome.Rejected -> "Not valid: ${rejection(o)}"
        is CoreOutcome.Unavailable -> "Cannot validate: native core unavailable (${o.reason})."
        is CoreOutcome.Malformed -> "Cannot validate: unreadable native response (${o.reason})."
    }

    private fun rejection(o: CoreOutcome.Rejected): String =
        if (o.message.isEmpty()) o.code else "${o.message} [${o.code}]"

    /** Display name for the Calls card. */
    fun callState(s: CallState?): String = when (s) {
        null -> "waiting for Android\u2026"
        CallState.Idle -> "Idle"
        CallState.Ringing -> "Ringing (incoming call)"
        CallState.OffHook -> "In call"
    }

    /** Wall-clock time of the last observed change, in [zone]; "none yet" before the first report. */
    fun callChangedAt(millis: Long?, zone: ZoneId): String =
        if (millis == null) "none yet" else DateTimeFormatter.ofPattern("HH:mm:ss").format(Instant.ofEpochMilli(millis).atZone(zone))
}
