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
    fun contactLine(s: Strings, c: ContactPreview): String =
        if (c.number == null) c.name else s.get(R.string.fmt_joined, c.name, c.number)

    fun eventLines(s: Strings, e: AgendaEvent, zone: ZoneId, locale: Locale): String {
        val day = DateTimeFormatter.ofPattern("EEE, MMM d", locale)
        val time = DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT).withLocale(locale)
        val span = if (e.allDay) {
            // All-day instances are stored as UTC midnight; converting to the local zone would shift the day.
            s.get(R.string.fmt_all_day, day.format(Instant.ofEpochMilli(e.beginMillis).atZone(ZoneOffset.UTC)))
        } else {
            val begin = Instant.ofEpochMilli(e.beginMillis).atZone(zone)
            val end = Instant.ofEpochMilli(e.endMillis).atZone(zone)
            s.get(R.string.fmt_joined, day.format(begin), s.get(R.string.fmt_time_range, time.format(begin), time.format(end)))
        }
        val withPlace = if (e.location != null) s.get(R.string.fmt_joined, span, e.location) else span
        return e.title + "\n" + withPlace
    }

    /** Result line for the clipboard "prepare" action. */
    fun clipboardOutcome(s: Strings, o: CoreOutcome, bytes: Int): String = when (o) {
        is CoreOutcome.Ok -> s.plural(R.plurals.fmt_clip_prepared, bytes, bytes)
        is CoreOutcome.Rejected -> s.get(R.string.fmt_clip_not_prepared, rejection(s, o))
        is CoreOutcome.Unavailable -> s.get(R.string.fmt_cannot_validate_unavailable, o.reason)
        is CoreOutcome.Malformed -> s.get(R.string.fmt_cannot_validate_unreadable, o.reason)
    }

    /** Result line for SMS validation. */
    fun smsOutcome(s: Strings, o: CoreOutcome): String = when (o) {
        is CoreOutcome.Ok -> s.get(R.string.fmt_sms_valid)
        is CoreOutcome.Rejected -> s.get(R.string.fmt_sms_not_valid, rejection(s, o))
        is CoreOutcome.Unavailable -> s.get(R.string.fmt_cannot_validate_unavailable, o.reason)
        is CoreOutcome.Malformed -> s.get(R.string.fmt_cannot_validate_unreadable, o.reason)
    }

    private fun rejection(s: Strings, o: CoreOutcome.Rejected): String =
        if (o.message.isEmpty()) o.code else s.get(R.string.fmt_rejection, o.message, o.code)

    /** Display name for the Calls card. */
    fun callState(s: Strings, state: CallState?): String = when (state) {
        null -> s.get(R.string.fmt_call_waiting)
        CallState.Idle -> s.get(R.string.fmt_call_idle)
        CallState.Ringing -> s.get(R.string.fmt_call_ringing)
        CallState.OffHook -> s.get(R.string.fmt_call_in_call)
    }

    /** Wall-clock time of the last observed change, in [zone]; "none yet" before the first report. */
    fun callChangedAt(s: Strings, millis: Long?, zone: ZoneId, locale: Locale = Locale.getDefault()): String =
        if (millis == null) s.get(R.string.dev_calls_none_yet)
        else DateTimeFormatter.ofPattern("HH:mm:ss", locale).format(Instant.ofEpochMilli(millis).atZone(zone))
}
