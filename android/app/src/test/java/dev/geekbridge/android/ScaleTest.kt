// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * Phone-data logic at realistic size (docs/reports/scale-report.md): a real phone has thousands of contacts, years of
 * recurring events and tens of thousands of texts. The default sizes match a heavy phone; set GEEKBRIDGE_SCALE=1 for
 * the 5x variant. Everything here runs on the JVM against fakes of the provider ports; the emulator run is
 * scripts/scale-e2e.sh.
 */
class ScaleTest {
    private val big = System.getenv("GEEKBRIDGE_SCALE") == "1"
    private val contactCount = if (big) 25_000 else 5_000
    private val eventCount = if (big) 100_000 else 20_000
    private val smsCount = if (big) 100_000 else 20_000
    private val threadCount = if (big) 4_000 else 800

    private class Core {
        val sent = ArrayList<Pair<String, JSONObject>>()
        var bytes = 0L
        var maxPacket = 0
        fun call(req: String): CoreOutcome {
            val o = JSONObject(req)
            if (o.getString("action") == "transport_data_send") {
                val body = o.getJSONObject("body")
                sent += o.getString("kind") to body
                val n = Bounds.utf8Size(body.toString())
                bytes += n
                maxPacket = maxOf(maxPacket, n)
            }
            return CoreOutcome.Ok(JSONObject().put("queued", 1))
        }
        fun of(kind: String) = sent.filter { it.first == kind }.map { it.second }
    }

    private val names = listOf("Zoë Müller", "Émile Zola", "山田太郎", "李小龍", "Łukasz Żółć", "Ада Лавлейс", "🎉 Party Planner", "Ñandú", "Søren", "Çelik")

    private fun name(i: Int) = "${names[i % names.size]} $i"

    private fun vcard(i: Int, photo: Boolean): String = buildString {
        append("BEGIN:VCARD\nVERSION:3.0\nFN:${name(i)}\nN:;${name(i)};;;\n")
        append("TEL;TYPE=CELL:+4670${1000000 + i}\nTEL;TYPE=HOME:+4687${2000000 + i}\nEMAIL:p$i@example.org\n")
        if (photo) {
            append("PHOTO;ENCODING=b;TYPE=JPEG:")
            append("A".repeat(30_000)).append('\n')
        }
        append("END:VCARD\n")
    }

    private class Contacts(val n: Int, val card: (Int) -> String) : ContactsPort {
        val uids = (0 until n).map { "key$it" to (1_700_000_000_000L + it).toString() }
        override fun listUids(max: Int) = uids.take(max)
        override fun vcard(uid: String): Pair<String, String>? {
            val i = uid.removePrefix("key").toInt()
            return card(i) to uids[i].second
        }
    }

    private class Cal(val all: List<CalInstance>) : CalendarPort {
        var queries = 0
        override fun calendars() = (1..8).map { CalendarInfo("$it", "Cal $it", account = "acct${it % 3}@example.org") }
        override fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int): List<CalInstance> {
            queries++
            return all.filter { it.calendarId in calendarIds && it.end >= fromMs && it.begin <= toMs }.sortedBy { it.begin }.take(max)
        }
        // Like the Android port: the nearest `max` on each side of the pivot.
        override fun instancesAround(calendarIds: Set<String>, fromMs: Long, toMs: Long, pivotMs: Long, max: Int): List<CalInstance> {
            queries++
            val inWindow = all.filter { it.calendarId in calendarIds && it.end >= fromMs && it.begin <= toMs }
            return inWindow.filter { it.begin >= pivotMs }.sortedBy { it.begin }.take(max) +
                inWindow.filter { it.begin < pivotMs }.sortedByDescending { it.begin }.take(max)
        }
    }

    private fun engine(core: Core, contacts: ContactsPort = Contacts(0) { "" }, cal: CalendarPort = Cal(emptyList()), sms: SmsPort = object : SmsPort {
        override fun latestPerThread(max: Int) = emptyList<SmsRow>()
        override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int) = emptyList<SmsRow>()
    }, now: Long = NOW, cancel: QueryCancel = QueryCancel()) = PhoneDataEngine(
        core::call, { ActiveFeatures(true, true, true) }, { (1..8).map { "$it" }.toSet() }, contacts, cal, sms,
        object : SmsSender { override fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener) {} },
        { now }, "boot", cancel = cancel,
    )

    private fun ev(kind: String, body: JSONObject = JSONObject()) = DataEvent(1, 1, kind, body)

    // ------------------------------------------------------------------ contacts

    @Test fun fiveThousandContactsListAndFetchInBoundedPackets() {
        val core = Core()
        val port = Contacts(contactCount) { vcard(it, photo = it % 10 == 0) }
        val e = engine(core, port)
        e.onEvents(listOf(ev(PhoneDataKinds.CONTACTS_REQ_UIDS)))
        val listing = core.of(PhoneDataKinds.CONTACTS_RES_UIDS).single()
        val listed = listing.getJSONArray("uids").length()
        assertEquals(minOf(contactCount, PhoneDataLimits.MAX_CONTACTS), listed)
        assertTrue("uid listing ${core.maxPacket} B", core.maxPacket < PhoneDataLimits.MAX_PACKET_BYTES)
        // The desktop asks for them in batches; every answer stays small and photos never travel.
        val before = core.sent.size
        var fetched = 0
        for (batch in (0 until listed).chunked(PhoneDataLimits.MAX_VCARD_REQUEST)) {
            e.onEvents(listOf(ev(PhoneDataKinds.CONTACTS_REQ_VCARDS, JSONObject().put("uids", JSONArray(batch.map { "key$it" })))))
        }
        val answers = core.sent.drop(before).filter { it.first == PhoneDataKinds.CONTACTS_RES_VCARDS }.map { it.second }
        assertEquals((listed + 24) / 25, answers.size)
        for (a in answers) {
            val n = a.getJSONArray("uids").length()
            fetched += n
            assertTrue(Bounds.utf8Size(a.toString()) < 64 * 1024 * 4)
            for (k in 0 until n) assertFalse(a.getString(a.getJSONArray("uids").getString(k)).contains("PHOTO", ignoreCase = true))
        }
        assertEquals(listed, fetched)
        println("scale contacts: listing ${Bounds.utf8Size(listing.toString())} B, ${answers.size} vcard packets, ${core.bytes} B total")
    }

    @Test fun contactsBeyondTheCapAreCutAtTheCapNotSilentlyUnbounded() {
        val core = Core()
        engine(core, Contacts(PhoneDataLimits.MAX_CONTACTS + 1500) { vcard(it, false) })
            .onEvents(listOf(ev(PhoneDataKinds.CONTACTS_REQ_UIDS)))
        assertEquals(PhoneDataLimits.MAX_CONTACTS, core.of(PhoneDataKinds.CONTACTS_RES_UIDS).single().getJSONArray("uids").length())
    }

    // ------------------------------------------------------------------ calendar

    private fun instances(): List<CalInstance> {
        val out = ArrayList<CalInstance>()
        val day = 86_400_000L
        val start = NOW - 3L * 365 * day
        var i = 0
        // Daily and weekly series spanning five years, expanded to instances like the provider's table.
        for (cal in 1..8) {
            val step = if (cal % 2 == 0) day else 7 * day
            var t = start - (start % day) + cal * 3_600_000L
            var n = 0
            while (t < NOW + 2L * 365 * day && out.size < eventCount / 2) {
                out += CalInstance("${cal}000", t, t + 1_800_000, "Series $cal", false, "Europe/Berlin", null, null, "$cal", recurring = true)
                t += step; n++
                if (n > eventCount / 16) break
            }
        }
        // One-off timed, all-day, multi-day and foreign-time-zone events fill the rest.
        while (out.size < eventCount) {
            val t = start + (i * 37L % (5L * 365)) * day + (i % 24) * 3_600_000L
            val cal = 1 + i % 8
            out += when (i % 4) {
                0 -> CalInstance("e$i", t - t % day, t - t % day + day, "All day $i", true, "UTC", null, null, "$cal")
                1 -> CalInstance("e$i", t, t + 3 * day, "Trip $i", false, "Asia/Tokyo", "東京", "Notes ".repeat(40), "$cal")
                2 -> CalInstance("e$i", t, t + 3_600_000, "Call 🎉 $i", false, "America/New_York", null, null, "$cal")
                else -> CalInstance("e$i", t, t + 1_800_000, "Café $i", false, "Europe/Berlin", "Rue", null, "$cal")
            }
            i++
        }
        return out
    }

    @Test fun calendarKeepsTheNearestEventsAndAlwaysFitsOnePacket() {
        val core = Core()
        val cal = Cal(instances())
        val e = engine(core, cal = cal)
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ)))
        val full = core.of(PhoneDataKinds.CALENDAR_SYNC).single()
        val events = full.getJSONArray("events")
        assertTrue(events.length() in 1..PhoneDataLimits.MAX_EVENTS)
        assertTrue(full.getBoolean("truncated"))
        assertTrue(core.maxPacket <= PhoneDataLimits.MAX_PACKET_BYTES)
        // Nearest first: the cut dropped the far ends, not the events around today.
        val farthest = (0 until events.length()).maxOf { kotlin.math.abs(events.getJSONObject(it).getLong("startMs") - NOW) }
        val day = 86_400_000L
        assertTrue("farthest kept event is ${farthest / day} days from now", farthest < 400 * day)
        // No instance twice, even when the provider returns the same row twice.
        val ids = (0 until events.length()).map { events.getJSONObject(it).getString("id") }
        assertEquals(ids.size, ids.toSet().size)
        assertEquals("one provider read per side", 1, cal.queries)
        println("scale calendar: ${events.length()} events, ${Bounds.utf8Size(full.toString())} B full packet")
    }

    @Test fun oneChangedEventSendsOneEventNotTheCalendar() {
        val core = Core()
        val all = instances()
        val cal = Cal(all)
        val e = engine(core, cal = cal)
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ)))
        val first = core.of(PhoneDataKinds.CALENDAR_SYNC).single()
        val fullBytes = Bounds.utf8Size(first.toString())
        // Retitle the event nearest to now.
        val near = all.filter { it.calendarId == "1" }.minBy { kotlin.math.abs(it.begin - NOW) }
        val changed = all.map { if (it === near) it.copy(title = "Moved") else it }
        // Deltas need the same engine instance (it remembers what it sent): swap the provider's contents under it.
        val swap = SwapCal(all)
        val core3 = Core()
        val e4 = engine(core3, cal = swap)
        e4.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ)))
        val token = core3.of(PhoneDataKinds.CALENDAR_SYNC).single().getString("token")
        swap.items = changed
        e4.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ, JSONObject().put("since", token))))
        val delta = core3.of(PhoneDataKinds.CALENDAR_SYNC).last()
        assertFalse(delta.getBoolean("full"))
        assertEquals(1, delta.getJSONArray("events").length())
        assertEquals(0, delta.getJSONArray("deleted").length())
        val deltaBytes = Bounds.utf8Size(delta.toString())
        assertTrue("delta $deltaBytes B vs full $fullBytes B", deltaBytes * 50 < fullBytes)
        println("scale calendar delta: $deltaBytes B vs full $fullBytes B")
    }

    private class SwapCal(var items: List<CalInstance>) : CalendarPort {
        override fun calendars() = (1..8).map { CalendarInfo("$it", "Cal $it") }
        override fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int) = Cal(items).instances(calendarIds, fromMs, toMs, max)
        override fun instancesAround(calendarIds: Set<String>, fromMs: Long, toMs: Long, pivotMs: Long, max: Int) =
            Cal(items).instancesAround(calendarIds, fromMs, toMs, pivotMs, max)
    }

    @Test fun duplicatedRecurringInstancesAreSentOnce() {
        val core = Core()
        val one = CalInstance("5", NOW + 1000, NOW + 2000, "Standup", false, "UTC", null, null, "1", recurring = true)
        val e = engine(core, cal = Cal(listOf(one, one.copy(), one.copy(title = "Standup"))))
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ)))
        assertEquals(1, core.of(PhoneDataKinds.CALENDAR_SYNC).single().getJSONArray("events").length())
    }

    // ------------------------------------------------------------------ SMS

    private fun smsRows(): List<SmsRow> {
        // Skewed like a real phone: a few chatty threads own most rows and most of the recent ones.
        var seed = 12345L
        fun next(n: Int): Int { seed = (seed * 6364136223846793005L + 1442695040888963407L); return ((seed ushr 33) % n).toInt() }
        return (0 until smsCount).map { i ->
            val chatty = next(100) < 70
            val thread = if (chatty) next(20) else next(threadCount)
            SmsRow(id = i + 1L, threadId = thread + 1L, address = "+4670${1000000 + thread}", body = "message $i 🎉", date = NOW - (smsCount - i) * 60_000L, type = 1 + i % 2, read = true, subId = 1)
        }
    }

    @Test fun newestMessagePerThreadFindsEveryThreadWithBoundedReads() {
        val rows = smsRows().sortedWith(compareByDescending<SmsRow> { it.date }.thenByDescending { it.id })
        var reads = 0
        var rowsRead = 0
        val out = SmsPaging.latestPerThread(PhoneDataLimits.SMS_THREADS_LISTED) { date, id, limit ->
            reads++
            rows.filter { it.date < date || (it.date == date && it.id < id) }.take(limit).also { rowsRead += it.size }
        }
        val allThreads = rows.map { it.threadId }.toSet()
        assertEquals(minOf(PhoneDataLimits.SMS_THREADS_LISTED, allThreads.size), out.size)
        assertEquals("one row per thread", out.size, out.map { it.threadId }.toSet().size)
        assertEquals("newest thread first", out.sortedByDescending { it.date }, out)
        // The newest-2000-rows read this replaces would have seen far fewer threads.
        val oldWay = rows.take(2000).map { it.threadId }.toSet().size
        println("scale sms: ${out.size} of ${allThreads.size} threads in $reads reads / $rowsRead rows (newest-2000 alone: $oldWay)")
        assertTrue(rowsRead <= PhoneDataLimits.SMS_SCAN_ROWS + PhoneDataLimits.SMS_SCAN_CHUNK)
        assertTrue(out.size > oldWay || oldWay >= PhoneDataLimits.SMS_THREADS_LISTED)
    }

    @Test fun conversationListGoesOutInBoundedPacketsWithTheThreadTotal() {
        val rows = smsRows()
        val core = Core()
        val sms = object : SmsPort {
            override fun latestPerThread(max: Int) = SmsPaging.latestPerThread(max) { date, id, limit ->
                rows.filter { it.date < date || (it.date == date && it.id < id) }
                    .sortedWith(compareByDescending<SmsRow> { it.date }.thenByDescending { it.id }).take(limit)
            }
            override fun threadCount() = rows.map { it.threadId }.toSet().size
            override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int) = emptyList<SmsRow>()
        }
        engine(core, sms = sms).onEvents(listOf(ev(PhoneDataKinds.SMS_REQ_CONVERSATIONS)))
        val packets = core.of(PhoneDataKinds.SMS_MESSAGES)
        assertTrue(packets.size >= 3) // 300 threads / 100 per packet
        for (p in packets) {
            assertTrue(p.getJSONArray("messages").length() <= PhoneDataLimits.MAX_SMS_PER_PACKET)
        }
        assertTrue(core.maxPacket <= PhoneDataLimits.MAX_PACKET_BYTES)
        assertEquals(rows.map { it.threadId }.toSet().size, packets.first().getInt("threadsTotal"))
        assertFalse(packets.drop(1).any { it.has("threadsTotal") })
    }

    @Test fun aThousandMessageThreadPagesInFiftyMessageAnswers() {
        val rows = (1..2000).map { SmsRow(it.toLong(), 7, "+46701", "m$it", NOW - (2000 - it) * 1000L, 1 + it % 2, true, 1) }
        val core = Core()
        val sms = object : SmsPort {
            override fun latestPerThread(max: Int) = emptyList<SmsRow>()
            override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int) =
                rows.filter { rangeStartMs == null || it.date <= rangeStartMs }.sortedByDescending { it.date }.take(limit)
        }
        val e = engine(core, sms = sms)
        var cursor: Long? = null
        var got = 0
        var pages = 0
        while (got < 2000 && pages < 100) {
            val body = JSONObject().put("threadID", 7).put("numberToRequest", 500)
            cursor?.let { body.put("rangeStartTimestamp", it) }
            val before = core.sent.size
            e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQ_CONVERSATION, body)))
            val msgs = core.sent.drop(before).flatMap { p -> (0 until p.second.getJSONArray("messages").length()).map { p.second.getJSONArray("messages").getJSONObject(it) } }
            assertTrue("a page never exceeds ${PhoneDataLimits.MAX_CONVERSATION_PAGE}", msgs.size <= PhoneDataLimits.MAX_CONVERSATION_PAGE)
            if (msgs.isEmpty()) break
            got += msgs.size
            cursor = msgs.minOf { it.getLong("date") }.let { it - 1 }
            pages++
        }
        assertEquals(2000, got)
    }

    // ------------------------------------------------------------------ cancellation

    @Test fun leavingCancelsALongReadAndTheNextLinkStartsClean() {
        val core = Core()
        val cancel = QueryCancel()
        var fired = 0
        val port = object : ContactsPort {
            override fun listUids(max: Int): List<Pair<String, String>>? = cancel.guard({ fired++ }) {
                for (i in 0 until 10_000) {
                    if (i == 100) cancel.cancel() // the link drops mid-read
                    cancel.check()
                }
                emptyList()
            }
            override fun vcard(uid: String): Pair<String, String>? = null
        }
        val e = engine(core, port, cancel = cancel)
        e.onEvents(listOf(ev(PhoneDataKinds.CONTACTS_REQ_UIDS))) // must not throw
        assertEquals(1, fired)
        assertTrue("nothing sent for a cancelled read", core.of(PhoneDataKinds.CONTACTS_RES_UIDS).isEmpty())
        try { cancel.check(); fail("still cancelled until reset") } catch (_: QueryCancelled) {}
        e.reset()
        cancel.check()
    }

    @Test fun aRateLimitedAnswerIsRetriedNotDropped() {
        // The core refills 8 packets a second; a 5,000-contact sync must wait for it, not stall on the first refusal.
        var refusals = 3
        var delivered = 0
        val pauses = ArrayList<Long>()
        val e = PhoneDataEngine(
            { req ->
                if (JSONObject(req).getString("action") != "transport_data_send") CoreOutcome.Ok(JSONObject())
                else if (refusals-- > 0) CoreOutcome.Rejected("rate_limited", "x")
                else { delivered++; CoreOutcome.Ok(JSONObject().put("queued", 1)) }
            },
            { ActiveFeatures(true, true, true) }, { emptySet() }, Contacts(3) { vcard(it, false) }, Cal(emptyList()),
            object : SmsPort {
                override fun latestPerThread(max: Int) = emptyList<SmsRow>()
                override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int) = emptyList<SmsRow>()
            },
            object : SmsSender { override fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener) {} },
            { NOW }, "boot", pause = { pauses += it },
        )
        e.onEvents(listOf(ev(PhoneDataKinds.CONTACTS_REQ_UIDS)))
        assertEquals(1, delivered)
        assertEquals(3, pauses.size)
    }

    private companion object {
        const val NOW = 1_800_000_000_000L
    }
}
