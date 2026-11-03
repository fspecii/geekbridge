// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Random

/**
 * Hostile input to the phone's contact and calendar parsers (the Kotlin counterpart of the `vcard` / `phone_wire`
 * fuzz targets): a provider vCard with folded lines, quoted-printable and huge photos, a computer's write request with
 * mutated fields, and a calendar row with odd text. Seeded, so a failure replays.
 */
class ContactsCalendarAbuseTest {
    private val seeds = listOf(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada\r\nPHOTO;ENCODING=b;TYPE=JPEG:AAAA\r\n BBBB\r\nTEL:1\r\nEND:VCARD\r\n",
        "BEGIN:VCARD\nVERSION:2.1\nN;ENCODING=QUOTED-PRINTABLE;CHARSET=UTF-8:=C3=89;mil=\nie\nitem1.PHOTO;BASE64:\n QUJD\n\nEND:VCARD\n",
        "BEGIN:VCARD\r\nFN:‮evil‬\r\nNOTE:" + "x".repeat(5000) + "\r\nEND:VCARD",
    )

    private fun mutate(r: Random, s: String): String {
        val sb = StringBuilder(s)
        repeat(1 + r.nextInt(6)) {
            if (sb.isEmpty()) sb.append('A')
            val at = r.nextInt(sb.length)
            when (r.nextInt(6)) {
                0 -> sb.insert(at, "\r\n ")
                1 -> sb.insert(at, "\n")
                2 -> sb.insert(at, "=")
                3 -> sb.deleteCharAt(at)
                4 -> sb.insert(at, String(Character.toChars(0x1F600 + r.nextInt(80))))
                else -> sb.insert(at, listOf("PHOTO:", "BEGIN:VCARD", "END:VCARD", "\u0000", "\\", ";").random(kotlin.random.Random(r.nextLong())))
            }
        }
        return sb.toString()
    }

    @Test fun mutatedVcardsNeverCrashNeverKeepBinaryPropertiesAndStayWithinTheLimit() {
        val r = Random(20261103)
        repeat(3000) {
            val raw = mutate(r, seeds[r.nextInt(seeds.size)])
            val stripped = PhoneDataProtocol.stripBinaryProperties(raw)
            // A property name that is itself folded across lines ("item1" + " .PHOTO") is a known gap; the size cap bounds it.
            // Lines end in CRLF or LF (a lone CR is not a line break in vCard 2.1 / 3.0 and is not treated as one).
            val bad = stripped.split("\r\n", "\n").filter { l -> !l.startsWith(" ") && !l.startsWith("\t") && l.substringBefore(':').substringBefore(';').uppercase().let { it == "PHOTO" || it.endsWith(".PHOTO") } }
            assertTrue("kept $bad from ${raw.replace("\r", "<CR>").replace("\n", "<LF>")}", bad.isEmpty())
            val card = PhoneDataProtocol.finishVcard(raw, "ts\u0007${r.nextInt()}")
            if (card != null) {
                assertTrue(Bounds.utf8Size(card) <= PhoneDataLimits.MAX_VCARD_BYTES)
                assertTrue(card.startsWith("BEGIN:VCARD", ignoreCase = true) && card.trimEnd().endsWith("END:VCARD"))
                assertEquals(1, card.lines().count { it.startsWith("REV:") })
            }
        }
    }

    @Test fun mutatedWriteRequestsAreParsedOrRefusedNeverThrown() {
        val r = Random(7)
        val base = JSONObject("""{"version":1,"opId":"op1","action":"update","eventId":"10","baseRev":"abc123",
            "event":{"title":"Lunch","allDay":false,"startMs":1800000000000,"endMs":1800003600000,"tz":"Europe/Berlin","reminderMin":10}}""")
        val junk = listOf<Any?>(JSONObject.NULL, -1, Long.MAX_VALUE, 1e300, "", "x".repeat(300), "a b", "\u0000", true, JSONObject(), org.json.JSONArray())
        repeat(3000) {
            val body = JSONObject(base.toString())
            val target = if (r.nextBoolean()) body else body.getJSONObject("event")
            val keys = target.keys().asSequence().toList()
            val k = keys[r.nextInt(keys.size)]
            if (r.nextInt(4) == 0) target.remove(k) else target.put(k, junk[r.nextInt(junk.size)])
            when (val p = CalendarWriteProtocol.parse(body)) {
                is WriteParse.Ok -> {
                    val req = p.request
                    if (req is WriteRequest.Update) {
                        // Whatever got through is something the writer would also validate, never an exception.
                        CalendarWriteProtocol.validate(req.draft)
                    }
                }
                is WriteParse.Bad -> assertTrue(p.opId == null || PhoneDataProtocol.validRequestId(p.opId))
            }
        }
    }

    @Test fun calendarRowsWithOddTextStayBoundedAndFreeOfControlCharacters() {
        val r = Random(11)
        repeat(2000) {
            fun txt() = String(CharArray(r.nextInt(3000)) { (r.nextInt(0x3000)).toChar() })
            val i = CalInstance("1", r.nextLong(), r.nextLong(), txt(), r.nextBoolean(), txt(), txt(), txt(), "2", attendees = List(r.nextInt(20)) { txt() }, reminders = List(r.nextInt(20)) { r.nextInt() })
            val o = PhoneDataProtocol.calendarEvent(i)
            assertTrue(o.getString("title").length <= PhoneDataLimits.MAX_TITLE)
            assertTrue(o.optString("location").length <= PhoneDataLimits.MAX_LOCATION)
            assertTrue(o.optString("description").length <= PhoneDataLimits.MAX_DESCRIPTION)
            assertTrue(o.getString("tz").length <= 64)
            assertFalse(o.getString("title").any { Character.isISOControl(it) })
            assertTrue(o.optJSONArray("reminders")?.length() ?: 0 <= PhoneDataLimits.MAX_REMINDERS)
        }
        assertNull(CalendarWriteProtocol.validate(EventDraft("ok", false, 1, 2, "UTC", null, null, null)))
    }
}
