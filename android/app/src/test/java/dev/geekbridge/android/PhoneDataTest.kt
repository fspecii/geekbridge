package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneDataTest {
    // ------------------------------------------------------------------ fakes

    private class FakeCore(var reject: String? = null) {
        val sent = ArrayList<Pair<String, JSONObject>>()
        fun call(req: String): CoreOutcome {
            val o = JSONObject(req)
            if (o.getString("action") == "transport_data_send") {
                if (reject != null) return CoreOutcome.Rejected(reject!!, "x")
                sent += o.getString("kind") to o.getJSONObject("body")
            }
            return CoreOutcome.Ok(JSONObject().put("queued", 1))
        }
        fun of(kind: String) = sent.filter { it.first == kind }.map { it.second }
    }

    private class FakeContacts(val list: List<Pair<String, String>>, val cards: Map<String, String> = emptyMap()) : ContactsPort {
        override fun listUids(max: Int) = list.take(max)
        override fun vcard(uid: String) = cards[uid]?.let { it to list.first { p -> p.first == uid }.second }
    }

    private class FakeCal(var items: List<CalInstance> = emptyList()) : CalendarPort {
        override fun calendars() = listOf(CalendarInfo("1", "Personal"), CalendarInfo("2", "Work"))
        override fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int) =
            items.filter { it.calendarId in calendarIds }.take(max)
    }

    private class FakeSms(val rows: List<SmsRow> = emptyList(), val sims: List<SimInfo> = emptyList(), val unread: Map<Long, Int> = emptyMap()) : SmsPort {
        override fun subscriptions() = sims
        override fun unreadCounts() = unread
        override fun latestPerThread(max: Int) = rows.groupBy { it.threadId }.map { (_, v) -> v.maxBy { it.date } }.take(max)
        override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int) =
            rows.filter { it.threadId == threadId && (rangeStartMs == null || it.date <= rangeStartMs) }
                .sortedByDescending { it.date }.take(limit)
    }

    private class FakeSender(val script: (SmsStatusListener) -> Unit = { it.onStatus("accepted") }) : SmsSender {
        val calls = ArrayList<Triple<String, String, Int?>>()
        override fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener) {
            calls += Triple(address, text, subId)
            script(listener)
        }
    }

    private fun ev(kind: String, body: JSONObject = JSONObject()) = DataEvent(1, 1, kind, body)

    private fun engine(
        core: FakeCore = FakeCore(),
        f: ActiveFeatures = ActiveFeatures(true, true, true),
        ids: Set<String> = setOf("1"),
        contacts: ContactsPort = FakeContacts(emptyList()),
        cal: FakeCal = FakeCal(),
        sms: SmsPort = FakeSms(),
        sender: SmsSender = FakeSender(),
        clock: () -> Long = { 1_000_000_000L },
    ) = PhoneDataEngine(core::call, { f }, { ids }, contacts, cal, sms, sender, clock, "boot")

    private fun inst(id: String, begin: Long, title: String = "T", cal: String = "1", allDay: Boolean = false) =
        CalInstance(id, begin, begin + 3_600_000, title, allDay, "Europe/Berlin", null, null, cal)

    // ------------------------------------------------------------------ gating and protocol

    @Test fun nothingIsActiveWithoutBothSettingAndPermission() {
        val on = PhoneDataSettings(contacts = true, calendar = true, sms = true)
        assertFalse(PhoneDataGate.active(on, PhoneDataPermissions()).any)
        assertFalse(PhoneDataGate.active(PhoneDataSettings(), PhoneDataPermissions(true, true, true, true, true)).any)
        assertEquals(
            ActiveFeatures(contacts = true),
            PhoneDataGate.active(on, PhoneDataPermissions(readContacts = true, readSms = true, sendSms = true)),
        )
        // SMS needs all three permissions.
        assertTrue(PhoneDataGate.active(on, PhoneDataPermissions(readSms = true, sendSms = true, receiveSms = true)).sms)
    }

    @Test fun startObjectIsAbsentWhenNothingIsActive() {
        assertNull(PhoneDataProtocol.startObject(ActiveFeatures()))
        val o = PhoneDataProtocol.startObject(ActiveFeatures(calendar = true))!!
        assertEquals(false, o.getBoolean("contacts")); assertEquals(true, o.getBoolean("calendar"))
        val start = JSONObject(TransportProtocol.startRequest(StartOptions("/s", "n", true, null, false, false, false, data = ActiveFeatures(sms = true))))
        assertTrue(start.getJSONObject("data").getBoolean("sms"))
        assertFalse(JSONObject(TransportProtocol.startRequest(StartOptions("/s", "n", true, null, false, false, false))).has("data"))
    }

    @Test fun requestBuildersUseTheDocumentedActions() {
        val c = JSONObject(PhoneDataProtocol.configRequest(ActiveFeatures(true, false, true)))
        assertEquals("transport_data_config", c.getString("action")); assertEquals(false, c.getBoolean("calendar"))
        val w = JSONObject(PhoneDataProtocol.waitRequest(5, 999_999))
        assertEquals("transport_data_wait", w.getString("action")); assertEquals(25_000L, w.getLong("timeout_ms"))
        val s = JSONObject(PhoneDataProtocol.sendRequest("k", JSONObject().put("a", 1)))
        assertEquals("transport_data_send", s.getString("action")); assertEquals(1, s.getJSONObject("body").getInt("a"))
    }

    @Test fun waitResultsAreParsedStrictly() {
        val r = JSONObject("""{"seq":9,"stopped":false,"events":[
            {"seq":8,"connection_id":3,"kind":"geekbridge.calendar.request","body":{"requestId":"a"}},
            {"seq":9,"connection_id":3,"kind":"BAD KIND!","body":{}},
            {"connection_id":3,"kind":"kdeconnect.sms.request","body":{}}]}""")
        val w = PhoneDataProtocol.parseWait(r)!!
        assertEquals(1, w.events.size); assertEquals(9L, w.seq)
        assertNull(PhoneDataProtocol.parseWait("nope")); assertNull(PhoneDataProtocol.parseWait(JSONObject()))
    }

    @Test fun vcardPhotosAreStrippedIncludingFoldedLinesAndRevAppended() {
        val raw = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada\r\nPHOTO;ENCODING=b;TYPE=JPEG:AAAA\r\n BBBB\r\n CCCC\r\nTEL:+1 555\r\nREV:old\r\nEND:VCARD\r\n"
        val out = PhoneDataProtocol.finishVcard(raw, "123")!!
        assertFalse(out.contains("PHOTO")); assertFalse(out.contains("BBBB")); assertFalse(out.contains("REV:old"))
        assertTrue(out.contains("FN:Ada")); assertTrue(out.contains("TEL:+1 555"))
        assertTrue(out.trimEnd().endsWith("REV:123\r\nEND:VCARD"))
        assertNull(PhoneDataProtocol.finishVcard("garbage", "1"))
        assertNull(PhoneDataProtocol.finishVcard("BEGIN:VCARD\r\nNOTE:" + "x".repeat(70_000) + "\r\nEND:VCARD", "1"))
    }

    // ------------------------------------------------------------------ contacts

    @Test fun contactsListingAndVcardsAreBounded() {
        val core = FakeCore()
        val list = (1..6000).map { "k$it" to "$it" } + ("uids" to "1")
        val e = engine(core, contacts = FakeContacts(list, mapOf("k1" to "BEGIN:VCARD\r\nFN:A\r\nEND:VCARD")))
        e.onEvents(listOf(ev(PhoneDataKinds.CONTACTS_REQ_UIDS)))
        val body = core.of(PhoneDataKinds.CONTACTS_RES_UIDS).single()
        assertEquals(5000, body.getJSONArray("uids").length()); assertEquals("1", body.getString("k1"))
        val req = JSONObject().put("uids", JSONArray((1..40).map { "k$it" }))
        e.onEvents(listOf(ev(PhoneDataKinds.CONTACTS_REQ_VCARDS, req)))
        val cards = core.of(PhoneDataKinds.CONTACTS_RES_VCARDS).single()
        assertEquals(listOf("k1"), (0 until cards.getJSONArray("uids").length()).map { cards.getJSONArray("uids").getString(it) })
        assertTrue(cards.getString("k1").contains("REV:1"))
        assertEquals(25, PhoneDataProtocol.requestedUids(req).size)
    }

    @Test fun disabledFeaturesAreNeverAnsweredOrRead() {
        val core = FakeCore()
        val e = engine(core, f = ActiveFeatures(), contacts = object : ContactsPort {
            override fun listUids(max: Int): List<Pair<String, String>> = error("must not read")
            override fun vcard(uid: String): Pair<String, String> = error("must not read")
        })
        e.onEvents(listOf(
            ev(PhoneDataKinds.CONTACTS_REQ_UIDS), ev(PhoneDataKinds.CALENDAR_REQ), ev(PhoneDataKinds.SMS_REQ_CONVERSATIONS),
            ev(PhoneDataKinds.SMS_REQUEST, JSONObject().put("messageBody", "x")),
        ))
        assertTrue(core.sent.isEmpty())
    }

    // ------------------------------------------------------------------ calendar

    @Test fun firstRequestIsFullThenMatchingTokenGivesDeltaWithTombstones() {
        val core = FakeCore(); val cal = FakeCal(listOf(inst("1", 1_000_100_000), inst("2", 1_000_200_000), inst("3", 1_000_300_000, cal = "2")))
        val e = engine(core, cal = cal)
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ, JSONObject().put("requestId", "r1"))))
        val full = core.of(PhoneDataKinds.CALENDAR_SYNC).single()
        assertTrue(full.getBoolean("full")); assertTrue(full.isNull("base"))
        assertEquals(2, full.getJSONArray("events").length()) // calendar 2 is not selected
        assertEquals("r1", full.getString("requestId"))
        assertEquals("1", full.getJSONArray("calendars").getJSONObject(0).getString("id"))
        val token = full.getString("token")

        cal.items = listOf(inst("1", 1_000_100_000), inst("2", 1_000_200_000, title = "Changed"), inst("4", 1_000_400_000))
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ, JSONObject().put("since", token))))
        val d = core.of(PhoneDataKinds.CALENDAR_SYNC).last()
        assertFalse(d.getBoolean("full")); assertEquals(token, d.getString("base")); assertNotEquals(token, d.getString("token"))
        val ids = (0 until d.getJSONArray("events").length()).map { d.getJSONArray("events").getJSONObject(it).getString("id") }.toSet()
        assertEquals(setOf("2@1000200000", "4@1000400000"), ids)
        assertEquals(0, d.getJSONArray("deleted").length())

        cal.items = listOf(inst("1", 1_000_100_000))
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ, JSONObject().put("since", d.getString("token")))))
        val t = core.of(PhoneDataKinds.CALENDAR_SYNC).last()
        assertEquals(0, t.getJSONArray("events").length())
        assertEquals(setOf("2@1000200000", "4@1000400000"), (0 until t.getJSONArray("deleted").length()).map { t.getJSONArray("deleted").getJSONObject(it).getString("id") }.toSet())
    }

    @Test fun unknownTokenAfterResetOrSelectionChangeForcesFull() {
        val core = FakeCore(); val e = engine(core, cal = FakeCal(listOf(inst("1", 1_000_100_000))))
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ)))
        val token = core.of(PhoneDataKinds.CALENDAR_SYNC).last().getString("token")
        e.reset()
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ, JSONObject().put("since", token))))
        assertTrue(core.of(PhoneDataKinds.CALENDAR_SYNC).last().getBoolean("full"))
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ, JSONObject().put("since", "stale"))))
        assertTrue(core.of(PhoneDataKinds.CALENDAR_SYNC).last().getBoolean("full"))
    }

    @Test fun noSelectedCalendarMeansEmptyFullSnapshot() {
        val core = FakeCore(); val e = engine(core, ids = emptySet(), cal = FakeCal(listOf(inst("1", 1_000_100_000))))
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ)))
        val b = core.of(PhoneDataKinds.CALENDAR_SYNC).single()
        assertTrue(b.getBoolean("full")); assertEquals(0, b.getJSONArray("calendars").length()); assertEquals(0, b.getJSONArray("events").length())
    }

    @Test fun eventsAreCappedNearestFirstAndTextBounded() {
        val core = FakeCore()
        val now = 1_000_000_000L
        val many = (1..1500).map { inst("$it", now + it * 60_000L) } +
            CalInstance("x", now + 5, now + 9, "t".repeat(2000), false, null, "l".repeat(2000), "d".repeat(5000), "1")
        val e = engine(core, cal = FakeCal(many), clock = { now })
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ)))
        val b = core.of(PhoneDataKinds.CALENDAR_SYNC).single()
        assertEquals(1000, b.getJSONArray("events").length()); assertTrue(b.getBoolean("truncated"))
        val x = (0 until 1000).map { b.getJSONArray("events").getJSONObject(it) }.first { it.getString("id") == "x@${now + 5}" }
        assertEquals(512, x.getString("title").length); assertEquals(512, x.getString("location").length)
        assertEquals(2000, x.getString("description").length); assertEquals("UTC", x.getString("tz"))
    }

    @Test fun aRepeatedInstanceRowIsSentOnceButDistinctEventsWithTheSameTextAreKept() {
        val core = FakeCore(); val now = 1_000_000_000L
        val twice = inst("7", now + 60_000, "Dentist")
        // Same provider row twice (window overlap / re-expansion): one entry. Different event ids with identical
        // text are the user's own separate events (the provider keeps them apart), as are other calendars' copies.
        val e = engine(
            core, ids = setOf("1", "2"),
            cal = FakeCal(listOf(twice, twice, inst("8", now + 60_000, "Dentist"), inst("7", now + 60_000, "Dentist", cal = "2"))),
            clock = { now },
        )
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ)))
        val b = core.of(PhoneDataKinds.CALENDAR_SYNC).single()
        assertEquals(3, b.getJSONArray("events").length())
        assertFalse(b.getBoolean("truncated"))
    }

    @Test fun anAllDayEventKeepsItsUtcMidnightsWhateverTheTimeZoneColumnSays() {
        val day = 1_793_491_200_000L // 2026-11-01T00:00Z
        val o = PhoneDataProtocol.calendarEvent(
            CalInstance("3", day, day + 86_400_000, "Team offsite", true, "America/Los_Angeles", null, null, "1"),
        )
        assertEquals("UTC", o.getString("tz")); assertEquals(day, o.getLong("startMs")); assertEquals(day + 86_400_000, o.getLong("endMs"))
        assertEquals("3@$day", o.getString("id"))
    }

    @Test fun revisionIsContentHashAndAllDayUsesUtc() {
        val a = PhoneDataProtocol.calendarEvent(inst("1", 0, allDay = true)); val b = PhoneDataProtocol.calendarEvent(inst("1", 0, allDay = true))
        assertEquals(a.getString("rev"), b.getString("rev")); assertEquals("UTC", a.getString("tz"))
        assertNotEquals(a.getString("rev"), PhoneDataProtocol.calendarEvent(inst("1", 0, title = "other", allDay = true)).getString("rev"))
    }

    @Test fun unsolicitedChangesNeedAPriorRequestAndAreSpacedBy30Seconds() {
        var now = 1_000_000_000L
        val core = FakeCore(); val e = engine(core, cal = FakeCal(listOf(inst("1", now + 1000))), clock = { now })
        assertFalse(e.onCalendarChanged()); assertTrue(core.sent.isEmpty())
        e.onEvents(listOf(ev(PhoneDataKinds.CALENDAR_REQ)))
        now += 10_000
        assertFalse(e.onCalendarChanged()); assertEquals(1, core.sent.size)
        now += 25_000
        assertTrue(e.onCalendarChanged()); assertEquals(2, core.sent.size)
        assertFalse(core.of(PhoneDataKinds.CALENDAR_SYNC).last().getBoolean("full"))
    }

    // ------------------------------------------------------------------ SMS

    private fun row(id: Long, thread: Long, date: Long, type: Int = 1) = SmsRow(id, thread, "+15551234567", "hi $id", date, type, false, 1)

    @Test fun conversationListIsBatchedAndMessageShapeIsStock() {
        val core = FakeCore(); val rows = (1L..250L).map { row(it, it, it) }
        val e = engine(core, sms = FakeSms(rows))
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQ_CONVERSATIONS)))
        val packets = core.of(PhoneDataKinds.SMS_MESSAGES)
        assertEquals(2, packets.size); assertEquals(2, packets[0].getInt("version"))
        assertTrue(packets.all { it.getJSONArray("messages").length() <= 100 })
        val m = packets[0].getJSONArray("messages").getJSONObject(0)
        for (k in listOf("addresses", "body", "date", "type", "read", "thread_id", "_id", "sub_id", "event")) assertTrue(k, m.has(k))
        assertEquals("+15551234567", m.getJSONArray("addresses").getJSONObject(0).getString("address"))
        assertEquals(0, m.getInt("read")); assertFalse(m.has("attachments"))
    }

    @Test fun conversationPagingIsInclusiveAndCapped() {
        val core = FakeCore(); val rows = (1L..80L).map { row(it, 7, it * 10) }
        val e = engine(core, sms = FakeSms(rows))
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQ_CONVERSATION, JSONObject().put("threadID", 7).put("rangeStartTimestamp", 500).put("numberToRequest", 1000))))
        val ms = core.of(PhoneDataKinds.SMS_MESSAGES).single().getJSONArray("messages")
        assertEquals(50, ms.length()); assertEquals(500L, ms.getJSONObject(0).getLong("date")); assertEquals(10L, ms.getJSONObject(49).getLong("date"))
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQ_CONVERSATION, JSONObject().put("threadID", -1))))
        assertEquals(1, core.of(PhoneDataKinds.SMS_MESSAGES).size)
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQ_CONVERSATION, JSONObject().put("threadID", 99))))
        assertEquals(0, core.of(PhoneDataKinds.SMS_MESSAGES).last().getJSONArray("messages").length()) // end of history
    }

    @Test fun incomingSmsIsPushedOnlyAfterTheComputerAsked() {
        val core = FakeCore(); val e = engine(core, sms = FakeSms())
        e.onIncomingSms(row(1, 1, 1)); assertTrue(core.sent.isEmpty())
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQ_CONVERSATIONS))); core.sent.clear()
        e.onIncomingSms(row(2, 1, 2))
        assertEquals(1, core.of(PhoneDataKinds.SMS_MESSAGES).single().getJSONArray("messages").length())
        e.reset(); core.sent.clear(); e.onIncomingSms(row(3, 1, 3)); assertTrue(core.sent.isEmpty())
    }

    private fun req(text: String? = "hello", vararg addr: String, id: String? = "req-1", extra: JSONObject.() -> Unit = {}) = JSONObject().apply {
        put("version", 2); put("sendSms", true)
        put("addresses", JSONArray(addr.map { JSONObject().put("address", it) }))
        if (text != null) put("messageBody", text)
        if (id != null) put("requestId", id)
        extra()
    }

    @Test fun smsRequestValidation() {
        fun bad(b: JSONObject) = (PhoneDataProtocol.parseSmsRequest(b) as PhoneDataProtocol.SmsParse.Bad).reason
        assertTrue(PhoneDataProtocol.parseSmsRequest(req("hi", "+1555")) is PhoneDataProtocol.SmsParse.Ok)
        assertEquals("group_unsupported", bad(req("hi", "+1", "+2")))
        assertEquals("invalid", bad(req("hi")))
        assertEquals("invalid", bad(req("  ", "+1")))
        assertEquals("invalid", bad(req(null, "+1")))
        assertEquals("invalid", bad(req("x".repeat(2001), "+1")))
        assertEquals("invalid", bad(req("a\u0000b", "+1")))
        assertEquals("mms_unsupported", bad(req("hi", "+1") { put("attachments", JSONArray().put(JSONObject())) }))
        val ok = PhoneDataProtocol.parseSmsRequest(req("hi", "+1", "+1") { put("subID", 3) }) as PhoneDataProtocol.SmsParse.Ok
        assertEquals(3, ok.subId); assertEquals("req-1", ok.requestId)
        assertNull((PhoneDataProtocol.parseSmsRequest(req("hi", "+1") { put("subID", -1) }) as PhoneDataProtocol.SmsParse.Ok).subId)
        assertNull((PhoneDataProtocol.parseSmsRequest(req("hi", "+1", id = "bad id!")) as PhoneDataProtocol.SmsParse.Ok).requestId)
        val legacy = JSONObject().put("phoneNumber", "+4").put("messageBody", "yo")
        assertEquals("+4", (PhoneDataProtocol.parseSmsRequest(legacy) as PhoneDataProtocol.SmsParse.Ok).address)
    }

    @Test fun sendWalksAcceptedSentDeliveredAndFailuresAreReported() {
        val core = FakeCore()
        val sender = FakeSender { l -> l.onStatus("accepted"); l.onStatus("sent"); l.onStatus("delivered") }
        val e = engine(core, sender = sender)
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("hi", "+4915551234567"))))
        assertEquals(listOf("accepted", "sent", "delivered"), core.of(PhoneDataKinds.SMS_STATUS).map { it.getString("state") })
        assertTrue(core.of(PhoneDataKinds.SMS_STATUS).all { it.getString("requestId") == "req-1" })
        assertEquals(Triple("+4915551234567", "hi", null), sender.calls.single())

        core.sent.clear()
        engine(core, sender = FakeSender { throw SecurityException() }).onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("hi", "+4915551234567"))))
        assertEquals("no_permission", core.of(PhoneDataKinds.SMS_STATUS).single().getString("reason"))
        core.sent.clear()
        engine(core).onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("hi", "+4915551234567", "+4915551234568"))))
        assertEquals("failed", core.of(PhoneDataKinds.SMS_STATUS).single().getString("state"))
        core.sent.clear()
        // A stock request without requestId still sends, but nothing can be reported.
        val s2 = FakeSender(); engine(core, sender = s2).onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("hi", "+4915551234567", id = null))))
        assertEquals(1, s2.calls.size); assertTrue(core.sent.isEmpty())
    }

    @Test fun theConversationListCarriesPerThreadUnreadCountsOnce() {
        val core = FakeCore()
        val rows = listOf(row(1, 1, 1), row(2, 2, 2))
        val e = engine(core, sms = FakeSms(rows, unread = mapOf(1L to 3, 2L to 0, 9L to 2)))
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQ_CONVERSATIONS)))
        val u = core.of(PhoneDataKinds.SMS_MESSAGES).first().getJSONObject("unread")
        assertEquals(3, u.getInt("1")); assertFalse(u.has("2")); assertEquals(2, u.getInt("9"))
        // A single-thread page or a live push never repeats it.
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQ_CONVERSATION, JSONObject().put("threadID", 1))))
        assertFalse(core.of(PhoneDataKinds.SMS_MESSAGES).last().has("unread"))
    }

    @Test fun messagePacketsCarryTheSimListAndOnlyListedSimsCanBeChosen() {
        val sims = listOf(SimInfo(3, 1, "Personal"), SimInfo(5, 2, "Work\u0007"))
        val core = FakeCore(); val sender = FakeSender()
        val e = engine(core, sms = FakeSms(listOf(row(1, 1, 1)), sims), sender = sender)
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQ_CONVERSATIONS)))
        val arr = core.of(PhoneDataKinds.SMS_MESSAGES).first().getJSONArray("sims")
        assertEquals(2, arr.length())
        assertEquals(5, arr.getJSONObject(1).getInt("id")); assertEquals(2, arr.getJSONObject(1).getInt("slot"))
        assertFalse(arr.getJSONObject(1).getString("label").any { it.isISOControl() })
        // A listed SIM is used; an unlisted one is refused instead of silently using another SIM.
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("hi", "+4915551234567", id = "ok1") { put("subID", 5) })))
        assertEquals(5, sender.calls.single().third)
        e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("hi", "+4915551234567", id = "bad1") { put("subID", 9) })))
        assertEquals(1, sender.calls.size)
        assertEquals("unknown_sim", core.of(PhoneDataKinds.SMS_STATUS).last().getString("reason"))
        // No SIM list at all: the default SIM is still used, a named SIM is refused.
        val s2 = FakeSender(); val core2 = FakeCore()
        val e2 = engine(core2, sender = s2)
        e2.onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("hi", "+4915551234567", id = "d1"))))
        e2.onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("hi", "+4915551234567", id = "d2") { put("subID", 3) })))
        assertEquals(1, s2.calls.size)
    }

    @Test fun smsSendsAreRateLimitedPerMinute() {
        val core = FakeCore(); val sender = FakeSender(); val e = engine(core, sender = sender)
        repeat(12) { e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("m$it", "+4915551234567", id = "r$it")))) }
        assertEquals(PhoneDataLimits.SMS_SENDS_PER_MINUTE, sender.calls.size)
        assertEquals("limit", core.of(PhoneDataKinds.SMS_STATUS).last().getString("reason"))
    }

    @Test fun aRepeatedSmsRequestIdIsSentOnlyOnce() {
        val core = FakeCore(); val sender = FakeSender(); val ledger = RequestLedger()
        fun e() = PhoneDataEngine(core::call, { ActiveFeatures(sms = true) }, { emptySet() }, FakeContacts(emptyList()), FakeCal(), FakeSms(), sender, { 1_000_000_000L }, "boot", smsLedger = ledger)
        val ev = ev(PhoneDataKinds.SMS_REQUEST, req("hi", "+4915551234567", id = "same-1"))
        e().onEvents(listOf(ev)); e().onEvents(listOf(ev)); e().reset()
        e().onEvents(listOf(ev))
        assertEquals(1, sender.calls.size)
        e().onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("hi", "+4915551234567", id = "other-2"))))
        assertEquals(2, sender.calls.size)
    }

    @Test fun theRequestLedgerSurvivesARestartAndIsBounded() {
        var saved = ""
        val blob = object : BlobStore { override fun read() = saved; override fun write(text: String) { saved = text } }
        val a = RequestLedger(blob, max = 3)
        assertTrue(a.firstTime("a")); assertFalse(a.firstTime("a"))
        listOf("b", "c", "d").forEach { a.firstTime(it) }
        val b = RequestLedger(blob, max = 3)
        assertFalse(b.firstTime("d")); assertTrue(b.firstTime("a")) // "a" fell out of the bounded list
    }

    @Test fun shortCodesAndPremiumNumbersAreRefusedUnlessAllowed() {
        for (n in listOf("1234", "+1555", "12345", "+1 900 555 0100", "0044 9 0123 4567", "+49 900 1234567", "09001234567", "Bank", "+49abc1234567")) {
            assertTrue(n, SmsDestination.check(n) != SmsDestination.Verdict.Ok)
        }
        for (n in listOf("+49 151 1234 5678", "0151 1234567", "+1 (415) 555-0100", "+44 7700 900123")) {
            assertEquals(n, SmsDestination.Verdict.Ok, SmsDestination.check(n))
        }
        val core = FakeCore(); val sender = FakeSender()
        engine(core, sender = sender).onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("STOP", "12345"))))
        assertTrue(sender.calls.isEmpty())
        assertEquals("blocked_number", core.of(PhoneDataKinds.SMS_STATUS).single().getString("reason"))
        val s2 = FakeSender()
        PhoneDataEngine(FakeCore()::call, { ActiveFeatures(sms = true) }, { emptySet() }, FakeContacts(emptyList()), FakeCal(), FakeSms(), s2, allowSpecialNumbers = { true })
            .onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("STOP", "12345"))))
        assertEquals(1, s2.calls.size)
    }

    @Test fun smsSendsAreCappedPerHour() {
        var now = 1_000_000_000L
        val core = FakeCore(); val sender = FakeSender(); val e = engine(core, sender = sender, clock = { now })
        repeat(PhoneDataLimits.SMS_SENDS_PER_HOUR + 5) {
            now += 30_000
            e.onEvents(listOf(ev(PhoneDataKinds.SMS_REQUEST, req("m", "+4915551234567", id = "h$it"))))
        }
        assertEquals(PhoneDataLimits.SMS_SENDS_PER_HOUR, sender.calls.size)
    }

    @Test fun partTrackerReportsSentOnceAllPartsAndFirstFailure() {
        val seen = ArrayList<String>()
        val l = object : SmsStatusListener { override fun onStatus(state: String, reason: String?) { seen += state + (reason?.let { ":$it" } ?: "") } }
        val t = SmsPartTracker(2, l)
        assertFalse(t.onSent(-1 + 0)) // RESULT_OK is -1
        assertEquals(emptyList<String>(), seen)
        t.onSent(-1); assertEquals(listOf("sent"), seen)
        assertFalse(t.onDelivered(-1)); assertTrue(t.onDelivered(-1)); assertEquals(listOf("sent", "delivered"), seen)
        seen.clear(); val f = SmsPartTracker(2, l)
        assertTrue(f.onSent(4)); assertEquals(listOf("failed:no_service"), seen)
        assertTrue(f.onSent(-1)); assertEquals(1, seen.size)
        assertEquals("radio_off", PhoneDataProtocol.failureReason(2))
    }

    // ------------------------------------------------------------------ sending limits

    @Test fun rateLimitedRelayBacksOffAndDrops() {
        var now = 1_000_000_000L
        val core = FakeCore("rate_limited"); val e = engine(core, contacts = FakeContacts(listOf("a" to "1")), clock = { now })
        e.onEvents(listOf(ev(PhoneDataKinds.CONTACTS_REQ_UIDS)))
        core.reject = null
        e.onEvents(listOf(ev(PhoneDataKinds.CONTACTS_REQ_UIDS))) // inside the back-off: dropped, not retried
        assertTrue(core.sent.isEmpty())
        now += 11_000
        e.onEvents(listOf(ev(PhoneDataKinds.CONTACTS_REQ_UIDS)))
        assertEquals(1, core.sent.size)
    }

    @Test fun uiStateWording() {
        assertEquals("Off", PhoneDataUi.state(false, true)); assertEquals("On", PhoneDataUi.state(true, true))
        assertEquals("Waiting for permission", PhoneDataUi.state(true, false))
        assertNotNull(PhoneDataProtocol.clean("a\u0000b", 10)); assertEquals("a b", PhoneDataProtocol.clean("a\u0000b", 10))
    }
}
