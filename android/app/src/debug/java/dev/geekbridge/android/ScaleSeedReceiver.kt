// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.BroadcastReceiver
import android.content.ContentProviderOperation
import android.content.ContentResolver
import android.content.ContentUris
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.net.Uri
import android.provider.CalendarContract
import android.provider.ContactsContract
import android.util.Log
import java.io.ByteArrayOutputStream
import java.util.Random

/**
 * Debug builds only: fills the phone's own Contacts and Calendar providers with a realistic amount of made-up data so
 * scripts/scale-e2e.sh can measure the real query path (docs/reports/scale-report.md).
 *
 *   adb shell am broadcast -n dev.geekbridge.android/.ScaleSeedReceiver -a dev.geekbridge.debug.SEED \
 *       --es what contacts|calendar|cases|clear --ei n 5000
 *
 * Needs WRITE_CONTACTS / WRITE_CALENDAR (declared in the debug manifest only; grant with `pm grant`). Progress and the
 * final count go to logcat tag GBScale. Texts cannot be written this way on API 36 (only the default SMS app may), the
 * script sends them through the emulator modem instead.
 */
class ScaleSeedReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val what = intent.getStringExtra("what") ?: return
        val n = intent.getIntExtra("n", 0)
        val pending = goAsync()
        Thread({
            try {
                val t = System.currentTimeMillis()
                when (what) {
                    "contacts" -> contacts(context.contentResolver, if (n > 0) n else 5000)
                    "calendar" -> calendar(context.contentResolver, if (n > 0) n else 20_000)
                    "clear" -> clear(context.contentResolver)
                    "cases" -> cases(context.contentResolver)
                }
                Log.i(TAG, "done $what in ${System.currentTimeMillis() - t} ms")
            } catch (e: Exception) {
                Log.e(TAG, "failed $what: $e")
            } finally {
                pending.finish()
            }
        }, "gb-scale-seed").start()
    }

    private fun clear(r: ContentResolver) {
        r.delete(ContactsContract.RawContacts.CONTENT_URI.buildUpon().appendQueryParameter(ContactsContract.CALLER_IS_SYNCADAPTER, "true").build(), "1=1", null)
        listOf("scale1", "scale2", "scale3", "cc1", "cc2").forEach { r.delete(syncUri(CalendarContract.Calendars.CONTENT_URI, it, "LOCAL"), "1=1", null) }
        Log.i(TAG, "cleared")
    }

    // ------------------------------------------------------------------ correctness cases

    /**
     * Small, named scenarios for scripts/scale-e2e.sh `cases`: a series across a DST change with an exception, an
     * EXDATE and a cancelled occurrence, all-day events, a read-only calendar, an invitation the user does not
     * organise, two accounts with the same event titles, and contacts with right-to-left / override / long-note data.
     * Times are fixed in 2027 so the expected answers do not depend on today.
     */
    private fun cases(r: ContentResolver) {
        clear(r)
        val own = calendarRow(r, "cc1", "CC Own", CalendarContract.Calendars.CAL_ACCESS_OWNER)
        val ro = calendarRow(r, "cc2", "CC Readonly", CalendarContract.Calendars.CAL_ACCESS_READ)
        val stockholm = java.util.TimeZone.getTimeZone("Europe/Stockholm")
        fun at(y: Int, m: Int, d: Int, h: Int, tz: java.util.TimeZone = stockholm): Long =
            java.util.Calendar.getInstance(tz).apply { clear(); set(y, m - 1, d, h, 0, 0) }.timeInMillis
        fun insert(cal: Long, acct: String, v: ContentValues): Long {
            v.put(CalendarContract.Events.CALENDAR_ID, cal)
            return ContentUris.parseId(r.insert(syncUri(CalendarContract.Events.CONTENT_URI, acct, "LOCAL"), v) ?: error("event insert refused"))
        }
        fun ev(title: String, start: Long, end: Long?, tz: String = "Europe/Stockholm", allDay: Boolean = false) = ContentValues().apply {
            put(CalendarContract.Events.TITLE, title); put(CalendarContract.Events.DTSTART, start)
            if (end != null) put(CalendarContract.Events.DTEND, end)
            put(CalendarContract.Events.EVENT_TIMEZONE, if (allDay) "UTC" else tz)
            put(CalendarContract.Events.ALL_DAY, if (allDay) 1 else 0)
        }
        // 1. Weekly Monday 09:00 Stockholm, 4 times, 15 Mar 2027 .. 5 Apr 2027: the clocks change on 28 Mar.
        val s0 = at(2027, 3, 15, 9)
        val master = insert(own, "cc1", ev("CC Weekly", s0, null).apply {
            put(CalendarContract.Events.RRULE, "FREQ=WEEKLY;COUNT=4"); put(CalendarContract.Events.DURATION, "PT1H")
        })
        // 2. The second occurrence (22 Mar) moved one hour later and renamed: an exception row.
        val s1 = at(2027, 3, 22, 9)
        insert(own, "cc1", ev("CC Weekly moved", s1 + 3_600_000, s1 + 7_200_000).apply {
            put(CalendarContract.Events.ORIGINAL_ID, master); put(CalendarContract.Events.ORIGINAL_INSTANCE_TIME, s1)
        })
        // 3. The third occurrence (29 Mar, after the change) cancelled.
        val s2 = at(2027, 3, 29, 9)
        insert(own, "cc1", ev("CC Weekly", s2, s2 + 3_600_000).apply {
            put(CalendarContract.Events.ORIGINAL_ID, master); put(CalendarContract.Events.ORIGINAL_INSTANCE_TIME, s2)
            put(CalendarContract.Events.STATUS, CalendarContract.Events.STATUS_CANCELED)
        })
        // 4. A series with an EXDATE (second of three removed).
        val e0 = at(2027, 5, 3, 14)
        insert(own, "cc1", ev("CC Exdate", e0, null).apply {
            put(CalendarContract.Events.RRULE, "FREQ=WEEKLY;COUNT=3"); put(CalendarContract.Events.DURATION, "PT30M")
            put(CalendarContract.Events.EXDATE, java.text.SimpleDateFormat("yyyyMMdd'T'HHmmss'Z'", java.util.Locale.US).apply { timeZone = java.util.TimeZone.getTimeZone("UTC") }.format(java.util.Date(e0 + 7 * 86_400_000L)))
        })
        // 5. All-day: one day, and three days across a month end (UTC midnights, zone "UTC").
        val day = 86_400_000L
        val jun1 = at(2027, 6, 1, 0, java.util.TimeZone.getTimeZone("UTC"))
        insert(own, "cc1", ev("CC Allday", jun1, jun1 + day, allDay = true))
        val may30 = at(2027, 5, 30, 0, java.util.TimeZone.getTimeZone("UTC"))
        insert(own, "cc1", ev("CC Allday trip", may30, may30 + 3 * day, allDay = true))
        // 6. A timed event on the other side of the date line.
        insert(own, "cc1", ev("CC Tokyo", at(2027, 7, 1, 8, java.util.TimeZone.getTimeZone("Asia/Tokyo")), at(2027, 7, 1, 9, java.util.TimeZone.getTimeZone("Asia/Tokyo")), "Asia/Tokyo"))
        // 7. Same title in a read-only calendar of a second account, and an invitation I do not organise.
        val jul = at(2027, 7, 5, 10)
        insert(ro, "cc2", ev("CC Shared title", jul, jul + 3_600_000))
        insert(own, "cc1", ev("CC Shared title", jul, jul + 3_600_000))
        insert(own, "cc1", ev("CC Invitation", jul + day, jul + day + 3_600_000).apply {
            put(CalendarContract.Events.ORGANIZER, "boss@example.org"); put(CalendarContract.Events.HAS_ATTENDEE_DATA, 1)
            put(CalendarContract.Events.GUESTS_CAN_MODIFY, 0)
        })
        // 8. The one the conflict test edits on both ends.
        insert(own, "cc1", ev("CC Conflict", jul + 2 * day, jul + 2 * day + 3_600_000))
        Log.i(TAG, "cases calendars own=$own readonly=$ro")

        // Contacts: right-to-left, an override character, a long note, several numbers and addresses, a photo.
        fun contact(name: String, phones: List<String>, emails: List<String>, note: String? = null, photo: Boolean = false) {
            val ops = ArrayList<ContentProviderOperation>()
            ops += ContentProviderOperation.newInsert(ContactsContract.RawContacts.CONTENT_URI)
                .withValue(ContactsContract.RawContacts.ACCOUNT_TYPE, "com.example.scale")
                .withValue(ContactsContract.RawContacts.ACCOUNT_NAME, "scale@example.org").build()
            ops += data(0, ContactsContract.CommonDataKinds.StructuredName.CONTENT_ITEM_TYPE)
                .withValue(ContactsContract.CommonDataKinds.StructuredName.DISPLAY_NAME, name).build()
            phones.forEach { ops += data(0, ContactsContract.CommonDataKinds.Phone.CONTENT_ITEM_TYPE).withValue(ContactsContract.CommonDataKinds.Phone.NUMBER, it).build() }
            emails.forEach { ops += data(0, ContactsContract.CommonDataKinds.Email.CONTENT_ITEM_TYPE).withValue(ContactsContract.CommonDataKinds.Email.ADDRESS, it).build() }
            if (note != null) ops += data(0, ContactsContract.CommonDataKinds.Note.CONTENT_ITEM_TYPE).withValue(ContactsContract.CommonDataKinds.Note.NOTE, note).build()
            if (photo) ops += data(0, ContactsContract.CommonDataKinds.Photo.CONTENT_ITEM_TYPE).withValue(ContactsContract.CommonDataKinds.Photo.PHOTO, photo(3)).build()
            r.applyBatch(ContactsContract.AUTHORITY, ops)
        }
        contact("CC محمد علي", listOf("+971 50 123 4567"), listOf("m@example.org"))
        contact("CC \u202Egpj.exe", listOf("+46 70 111 22 33"), emptyList())
        contact("CC Multi", listOf("+46 70 123 45 67", "070-123 45 67", "+46701234567"), listOf("Multi@Example.org", "multi@example.org"), photo = true)
        contact("CC Longnote", listOf("+46 8 555 000"), emptyList(), note = "x".repeat(100_000))
        contact("CC Ünïcödé 山田 🎉", listOf("1"), listOf("u@example.org"))
        Log.i(TAG, "cases contacts 5")
    }

    private fun calendarRow(r: ContentResolver, acct: String, name: String, access: Int): Long {
        val v = ContentValues().apply {
            put(CalendarContract.Calendars.ACCOUNT_NAME, acct); put(CalendarContract.Calendars.ACCOUNT_TYPE, "LOCAL")
            put(CalendarContract.Calendars.NAME, name); put(CalendarContract.Calendars.CALENDAR_DISPLAY_NAME, name)
            put(CalendarContract.Calendars.CALENDAR_COLOR, Color.BLUE); put(CalendarContract.Calendars.CALENDAR_ACCESS_LEVEL, access)
            put(CalendarContract.Calendars.OWNER_ACCOUNT, acct); put(CalendarContract.Calendars.VISIBLE, 1)
            put(CalendarContract.Calendars.SYNC_EVENTS, 1); put(CalendarContract.Calendars.CALENDAR_TIME_ZONE, "Europe/Stockholm")
        }
        return ContentUris.parseId(r.insert(syncUri(CalendarContract.Calendars.CONTENT_URI, acct, "LOCAL"), v) ?: error("calendar insert refused"))
    }

    // ------------------------------------------------------------------ contacts

    private val given = listOf("Zoë", "Émile", "Åsa", "Łukasz", "Çiğdem", "Søren", "Ñandú", "José", "Ада", "山田", "李", "さくら", "민준", "🎉", "Ömer", "Anaïs", "Björn", "Chloé", "Dmitri", "Eve")
    private val family = listOf("Müller", "Zola", "Żółć", "Lavlace", "太郎", "小龍", "ひなた", "김", "Núñez", "O'Brien", "van der Berg", "Smith", "Öztürk", "Hernández", "Ångström", "Kowalski", "Dupont", "Rossi", "Ivanov", "Lee")

    private fun photo(seed: Int): ByteArray {
        val bmp = Bitmap.createBitmap(96, 96, Bitmap.Config.ARGB_8888)
        Canvas(bmp).drawColor(Color.HSVToColor(floatArrayOf((seed * 37 % 360).toFloat(), 0.5f, 0.8f)))
        return ByteArrayOutputStream().also { bmp.compress(Bitmap.CompressFormat.JPEG, 80, it) }.toByteArray()
    }

    private fun contacts(r: ContentResolver, n: Int) {
        val rnd = Random(7)
        var done = 0
        while (done < n) {
            val ops = ArrayList<ContentProviderOperation>()
            for (k in 0 until minOf(60, n - done)) {
                val i = done + k
                val back = ops.size
                ops += ContentProviderOperation.newInsert(ContactsContract.RawContacts.CONTENT_URI)
                    .withValue(ContactsContract.RawContacts.ACCOUNT_TYPE, "com.example.scale")
                    .withValue(ContactsContract.RawContacts.ACCOUNT_NAME, "scale@example.org").build()
                val name = "${given[rnd.nextInt(given.size)]} ${family[rnd.nextInt(family.size)]} $i"
                ops += data(back, ContactsContract.CommonDataKinds.StructuredName.CONTENT_ITEM_TYPE)
                    .withValue(ContactsContract.CommonDataKinds.StructuredName.DISPLAY_NAME, name).build()
                repeat(1 + i % 3) { p ->
                    ops += data(back, ContactsContract.CommonDataKinds.Phone.CONTENT_ITEM_TYPE)
                        .withValue(ContactsContract.CommonDataKinds.Phone.NUMBER, "+4670${(1_000_000 + i * 3 + p)}")
                        .withValue(ContactsContract.CommonDataKinds.Phone.TYPE, ContactsContract.CommonDataKinds.Phone.TYPE_MOBILE).build()
                }
                ops += data(back, ContactsContract.CommonDataKinds.Email.CONTENT_ITEM_TYPE)
                    .withValue(ContactsContract.CommonDataKinds.Email.ADDRESS, "p$i@example.org").build()
                if (i % 5 == 0) {
                    ops += data(back, ContactsContract.CommonDataKinds.Photo.CONTENT_ITEM_TYPE)
                        .withValue(ContactsContract.CommonDataKinds.Photo.PHOTO, photo(i)).build()
                }
            }
            r.applyBatch(ContactsContract.AUTHORITY, ops)
            done += minOf(60, n - done)
            if (done % 600 == 0) Log.i(TAG, "contacts $done")
        }
        Log.i(TAG, "contacts total $n")
    }

    private fun data(back: Int, mime: String) = ContentProviderOperation.newInsert(ContactsContract.Data.CONTENT_URI)
        .withValueBackReference(ContactsContract.Data.RAW_CONTACT_ID, back)
        .withValue(ContactsContract.Data.MIMETYPE, mime)

    // ------------------------------------------------------------------ calendar

    private fun syncUri(base: Uri, account: String, type: String): Uri = base.buildUpon()
        .appendQueryParameter(CalendarContract.CALLER_IS_SYNCADAPTER, "true")
        .appendQueryParameter(CalendarContract.Calendars.ACCOUNT_NAME, account)
        .appendQueryParameter(CalendarContract.Calendars.ACCOUNT_TYPE, type).build()

    private fun calendar(r: ContentResolver, n: Int) {
        // 8 calendars of 3 accounts (LOCAL type so the provider's account clean-up leaves them alone).
        val accounts = listOf("scale1", "scale2", "scale3")
        val ids = ArrayList<Long>()
        for (c in 0 until 8) {
            val acct = accounts[c % 3]
            val v = ContentValues().apply {
                put(CalendarContract.Calendars.ACCOUNT_NAME, acct)
                put(CalendarContract.Calendars.ACCOUNT_TYPE, "LOCAL")
                put(CalendarContract.Calendars.NAME, "Scale $c")
                put(CalendarContract.Calendars.CALENDAR_DISPLAY_NAME, "Scale calendar $c ($acct)")
                put(CalendarContract.Calendars.CALENDAR_COLOR, Color.HSVToColor(floatArrayOf(c * 45f, 0.7f, 0.8f)))
                put(CalendarContract.Calendars.CALENDAR_ACCESS_LEVEL, if (c % 4 == 3) CalendarContract.Calendars.CAL_ACCESS_READ else CalendarContract.Calendars.CAL_ACCESS_OWNER)
                put(CalendarContract.Calendars.OWNER_ACCOUNT, acct)
                put(CalendarContract.Calendars.VISIBLE, 1)
                put(CalendarContract.Calendars.SYNC_EVENTS, 1)
                put(CalendarContract.Calendars.CALENDAR_TIME_ZONE, "Europe/Berlin")
            }
            val uri = r.insert(syncUri(CalendarContract.Calendars.CONTENT_URI, acct, "LOCAL"), v) ?: error("calendar insert refused")
            ids += ContentUris.parseId(uri)
        }
        Log.i(TAG, "calendars ${ids.joinToString(",")}")
        val day = 86_400_000L
        val now = System.currentTimeMillis()
        val start0 = now - 3 * 365 * day
        val zones = listOf("Europe/Berlin", "Asia/Tokyo", "America/New_York", "Australia/Sydney", "UTC", "Europe/Bucharest")
        val rnd = Random(11)
        var written = 0
        val ops = ArrayList<ContentProviderOperation>()
        fun flush() {
            if (ops.isEmpty()) return
            r.applyBatch(CalendarContract.AUTHORITY, ArrayList(ops))
            written += ops.size
            ops.clear()
            if (written % 2000 == 0) Log.i(TAG, "events $written")
        }
        fun event(cal: Long, title: String, dtstart: Long, dtend: Long?, allDay: Boolean, tz: String, rrule: String? = null, duration: String? = null) {
            val b = ContentProviderOperation.newInsert(syncUri(CalendarContract.Events.CONTENT_URI, accounts[(ids.indexOf(cal)) % 3], "LOCAL"))
                .withValue(CalendarContract.Events.CALENDAR_ID, cal)
                .withValue(CalendarContract.Events.TITLE, title)
                .withValue(CalendarContract.Events.DTSTART, dtstart)
                .withValue(CalendarContract.Events.ALL_DAY, if (allDay) 1 else 0)
                .withValue(CalendarContract.Events.EVENT_TIMEZONE, if (allDay) "UTC" else tz)
                .withValue(CalendarContract.Events.EVENT_LOCATION, "Café ${rnd.nextInt(99)} 東京")
                .withValue(CalendarContract.Events.DESCRIPTION, "Notes ".repeat(20))
            if (rrule != null) b.withValue(CalendarContract.Events.RRULE, rrule).withValue(CalendarContract.Events.DURATION, duration)
            else b.withValue(CalendarContract.Events.DTEND, dtend)
            ops += b.build()
        }
        // Series first (5 years, from 3 years back to 2 years ahead): 6 daily + 20 weekly.
        val untilFmt = java.text.SimpleDateFormat("yyyyMMdd'T'HHmmss'Z'", java.util.Locale.US).apply { timeZone = java.util.TimeZone.getTimeZone("UTC") }
        val until = untilFmt.format(java.util.Date(now + 2 * 365 * day))
        for (s in 0 until 26) {
            val daily = s < 6
            event(ids[s % ids.size], "${if (daily) "Daily" else "Weekly"} series $s", start0 + s * 3_600_000L, null, false, zones[s % zones.size],
                rrule = "FREQ=${if (daily) "DAILY" else "WEEKLY"};UNTIL=$until", duration = "PT30M")
            if (ops.size >= 200) flush()
        }
        flush()
        var i = 0
        while (written < n) {
            val cal = ids[i % ids.size]
            val t = start0 + rnd.nextInt(5 * 365) * day + rnd.nextInt(24) * 3_600_000L
            when (i % 4) {
                0 -> { val d = t - t % day; event(cal, "All day $i", d, d + day * (1 + i % 3), true, "UTC") } // all-day and multi-day
                1 -> event(cal, "Trip $i", t, t + 3 * day, false, zones[i % zones.size]) // multi-day, other zone
                2 -> event(cal, "Call 🎉 $i", t, t + 3_600_000, false, zones[(i + 1) % zones.size])
                else -> event(cal, "Café $i", t, t + 1_800_000, false, zones[(i + 2) % zones.size])
            }
            i++
            if (ops.size >= 250) flush()
        }
        flush()
        Log.i(TAG, "events total $written")
    }

    private companion object {
        const val TAG = "GBScale"
    }
}
