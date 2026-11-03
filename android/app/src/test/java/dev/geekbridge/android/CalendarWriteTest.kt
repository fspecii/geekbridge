// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CalendarWriteTest {
    private class Mem : BlobStore {
        var text = ""
        override fun read() = text
        override fun write(text: String) { this.text = text }
    }

    /** A pretend Calendar Provider that records every write. */
    private class FakeStore : CalendarWriteStore {
        val calendars = mutableMapOf("1" to CalendarAccess(true), "2" to CalendarAccess(false))
        val events = mutableMapOf(
            "10" to ExistingEvent("1", false, rev = "rev10"), "11" to ExistingEvent("1", true), "12" to ExistingEvent("2", false),
            "13" to ExistingEvent("1", false, rev = "rev13", guest = true), "14" to ExistingEvent("1", false), // 14: rev unknown
        )
        val writes = ArrayList<String>()
        var next = 100
        var denied = false
        override fun calendar(id: String) = calendars[id]
        override fun event(id: String) = events[id]
        override fun insert(calendarId: String, draft: EventDraft): String? {
            if (denied) throw SecurityException("no WRITE_CALENDAR")
            writes += "insert:$calendarId:${draft.title}"
            return (next++).toString()
        }
        override fun update(eventId: String, draft: EventDraft): Boolean { writes += "update:$eventId:${draft.title}"; return true }
        override fun delete(eventId: String): Boolean { writes += "delete:$eventId"; return events.remove(eventId) != null }
    }

    private val draft = EventDraft("Lunch", false, 1_800_000_000_000, 1_800_003_600_000, "Europe/Berlin", "Cafe", null, 10)

    private fun body(action: String, op: String = "op1", extra: JSONObject.() -> Unit = {}): JSONObject =
        JSONObject().put("version", 1).put("opId", op).put("action", action).apply(extra)

    private fun event(title: String = "Lunch") = JSONObject().put("title", title).put("allDay", false)
        .put("startMs", 1_800_000_000_000).put("endMs", 1_800_003_600_000).put("tz", "Europe/Berlin").put("reminderMin", 10)

    private fun writer(store: FakeStore = FakeStore(), shared: Set<String> = setOf("1", "2"), blob: Mem = Mem()) =
        Triple(CalendarWriter(store, OpLedger(blob)) { shared }, store, blob)

    // ------------------------------------------------------------------ gating

    @Test fun writingNeedsTheSwitchTheCalendarReadAndTheWritePermission() {
        val on = PhoneDataSettings(calendar = true, calendarEdit = true)
        val read = PhoneDataPermissions(readCalendar = true)
        assertFalse(PhoneDataGate.active(on, read).calendarEdit)                              // no WRITE_CALENDAR
        assertFalse(PhoneDataGate.active(on, PhoneDataPermissions(writeCalendar = true)).calendarEdit) // no READ_CALENDAR
        assertFalse(PhoneDataGate.active(PhoneDataSettings(calendar = true), PhoneDataPermissions(readCalendar = true, writeCalendar = true)).calendarEdit) // switch off
        assertFalse(PhoneDataGate.active(PhoneDataSettings(calendarEdit = true), PhoneDataPermissions(readCalendar = true, writeCalendar = true)).calendarEdit) // reading off
        assertTrue(PhoneDataGate.active(on, PhoneDataPermissions(readCalendar = true, writeCalendar = true)).calendarEdit)
        // The default is read-only.
        assertFalse(PhoneDataSettings().calendarEdit)
        val start = PhoneDataProtocol.startObject(ActiveFeatures(calendar = true))!!
        assertFalse(start.getBoolean("calendar_edit"))
        assertTrue(PhoneDataProtocol.startObject(ActiveFeatures(calendar = true, calendarEdit = true))!!.getBoolean("calendar_edit"))
    }

    // ------------------------------------------------------------------ parsing

    @Test fun parsesCreateUpdateDeleteAndRejectsGarbage() {
        val create = CalendarWriteProtocol.parse(body("create") { put("calendarId", "1"); put("event", event()) })
        val c = (create as WriteParse.Ok).request as WriteRequest.Create
        assertEquals("1", c.calendarId); assertEquals("Lunch", c.draft.title); assertEquals(10, c.draft.reminderMin)
        val update = CalendarWriteProtocol.parse(body("update") { put("eventId", "10"); put("event", event("New")) })
        assertEquals("New", ((update as WriteParse.Ok).request as WriteRequest.Update).draft.title)
        assertTrue(((CalendarWriteProtocol.parse(body("delete") { put("eventId", "10") }) as WriteParse.Ok).request) is WriteRequest.Delete)

        for (bad in listOf(
            body("create") { put("event", event()) },                              // no calendar chosen
            body("create") { put("calendarId", "1; DROP"); put("event", event()) }, // not a numeric id
            body("update") { put("eventId", "abc"); put("event", event()) },
            body("delete"),
            body("explode"),
            body("create", op = "bad id") { put("calendarId", "1"); put("event", event()) },
            JSONObject().put("opId", "x").put("action", "delete").put("eventId", "1"),   // wrong version
            body("create") { put("calendarId", "1"); put("event", event().put("reminderMin", 99_999_999)) },
        )) {
            assertTrue("$bad", CalendarWriteProtocol.parse(bad) is WriteParse.Bad)
        }
        // A bad operation id cannot be answered (there is nothing to echo), a bad body can.
        assertNull((CalendarWriteProtocol.parse(body("delete", op = "bad id")) as WriteParse.Bad).opId)
        assertEquals("op1", (CalendarWriteProtocol.parse(body("delete")) as WriteParse.Bad).opId)
    }

    @Test fun validationMatchesTheDesktopsRules() {
        assertNull(CalendarWriteProtocol.validate(draft))
        assertEquals("invalid", CalendarWriteProtocol.validate(draft.copy(title = "  ")))
        assertEquals("invalid", CalendarWriteProtocol.validate(draft.copy(endMs = draft.startMs)))
        assertEquals("invalid", CalendarWriteProtocol.validate(draft.copy(allDay = true)))            // not on UTC midnights
        assertNull(CalendarWriteProtocol.validate(draft.copy(allDay = true, startMs = 86_400_000L * 20_000, endMs = 86_400_000L * 20_001, tz = "UTC")))
        assertEquals("invalid", CalendarWriteProtocol.validate(draft.copy(tz = "../etc/passwd")))
        assertEquals("invalid", CalendarWriteProtocol.validate(draft.copy(title = "a\u0000b")))
        assertEquals("invalid", CalendarWriteProtocol.validate(draft.copy(description = "x".repeat(2001))))
        assertEquals("invalid", CalendarWriteProtocol.validate(draft.copy(reminderMin = 50_000)))
    }

    // ------------------------------------------------------------------ applying

    @Test fun createGoesToTheChosenWritableSharedCalendarOnceAndRepeatsAreNotWrittenAgain() {
        val (w, store, _) = writer()
        val req = WriteRequest.Create("op1", "1", draft)
        val (first, dup1) = w.handle(req)
        assertEquals("applied", first.state); assertEquals("100", first.eventId); assertFalse(dup1)
        val (again, dup2) = w.handle(req)
        assertTrue(dup2); assertEquals(first, again)
        assertEquals(listOf("insert:1:Lunch"), store.writes)
    }

    @Test fun nothingIsWrittenToReadOnlyUnsharedOrMissingCalendars() {
        val (w, store, _) = writer(shared = setOf("1", "2"))
        assertEquals("not_writable", w.handle(WriteRequest.Create("a", "2", draft)).first.reason)
        assertEquals("calendar_missing", w.handle(WriteRequest.Create("b", "9", draft)).first.reason)
        val (w2, store2, _) = writer(shared = setOf("2")) // calendar 1 exists and is writable but the user did not tick it
        assertEquals("calendar_missing", w2.handle(WriteRequest.Create("c", "1", draft)).first.reason)
        assertEquals("invalid", w.handle(WriteRequest.Create("d", "1", draft.copy(title = ""))).first.reason)
        assertTrue(store.writes.isEmpty() && store2.writes.isEmpty())
    }

    @Test fun repeatingMissingAndReadOnlyEventsCannotBeChangedOrDeleted() {
        val (w, store, _) = writer()
        assertEquals("recurring", w.handle(WriteRequest.Delete("a", "11")).first.reason)
        assertEquals("recurring", w.handle(WriteRequest.Update("b", "11", draft)).first.reason)
        assertEquals("not_found", w.handle(WriteRequest.Delete("c", "404")).first.reason)
        assertEquals("not_writable", w.handle(WriteRequest.Delete("d", "12")).first.reason)
        assertTrue(store.writes.isEmpty())
        assertEquals("applied", w.handle(WriteRequest.Update("e", "10", draft.copy(title = "Moved"))).first.state)
        assertEquals("applied", w.handle(WriteRequest.Delete("f", "10")).first.state)
        assertEquals(listOf("update:10:Moved", "delete:10"), store.writes)
    }

    // ------------------------------------------------------------------ edit conflicts and invitations

    @Test fun anEditMadeFromAnOlderVersionIsRefusedAndNothingIsOverwritten() {
        val (w, store, blob) = writer()
        val stale = w.handle(WriteRequest.Update("a", "10", draft, baseRev = "old")).first
        assertEquals(WriteOutcome("failed", "changed"), stale)
        assertEquals("changed", w.handle(WriteRequest.Delete("b", "10", baseRev = "old")).first.reason)
        // A phone that cannot tell what its current version is must not guess: refuse.
        assertEquals("changed", w.handle(WriteRequest.Update("c", "14", draft, baseRev = "rev10")).first.reason)
        assertTrue(store.writes.isEmpty())
        // The refusal is the operation's answer: a retry with the same id says the same and still writes nothing.
        val (again, dup) = w.handle(WriteRequest.Update("a", "10", draft, baseRev = "rev10"))
        assertTrue(dup); assertEquals("changed", again.reason)
        assertTrue(store.writes.isEmpty() && blob.text.isNotEmpty())
        // The current version goes through; so does an older desktop that sends no version at all.
        assertEquals("applied", w.handle(WriteRequest.Update("d", "10", draft, baseRev = "rev10")).first.state)
        assertEquals("applied", w.handle(WriteRequest.Update("e", "10", draft.copy(title = "Legacy"))).first.state)
        assertEquals(listOf("update:10:Lunch", "update:10:Legacy"), store.writes)
    }

    @Test fun theBaseVersionIsParsedAndKeptToASafeShape() {
        val up = CalendarWriteProtocol.parse(body("update") { put("eventId", "10"); put("event", event()); put("baseRev", "a1b2c3d4e5f60718") })
        assertEquals("a1b2c3d4e5f60718", ((up as WriteParse.Ok).request as WriteRequest.Update).baseRev)
        val del = CalendarWriteProtocol.parse(body("delete") { put("eventId", "10"); put("baseRev", "r1") })
        assertEquals("r1", ((del as WriteParse.Ok).request as WriteRequest.Delete).baseRev)
        assertNull((((CalendarWriteProtocol.parse(body("update") { put("eventId", "10"); put("event", event()) })) as WriteParse.Ok).request as WriteRequest.Update).baseRev)
        for (bad in listOf("", "x".repeat(65), "a b", "a\nb", 42)) {
            val p = CalendarWriteProtocol.parse(body("delete") { put("eventId", "10"); put("baseRev", bad) })
            assertTrue("$bad", p is WriteParse.Bad)
        }
    }

    @Test fun anInvitationYouDoNotOrganiseCannotBeEditedButCanBeDeclinedAway() {
        // Google would overwrite the local change at the next sync, so the user would see it silently undone.
        val (w, store, _) = writer()
        assertEquals("not_organizer", w.handle(WriteRequest.Update("a", "13", draft, baseRev = "rev13")).first.reason)
        assertEquals("applied", w.handle(WriteRequest.Delete("b", "13", baseRev = "rev13")).first.state)
        assertEquals(listOf("delete:13"), store.writes)
    }

    @Test fun guestIsDecidedFromTheProvidersOrganiserColumns() {
        assertTrue(CalendarGuest.isGuest("boss@corp.com", "me@gmail.com", hasAttendees = true, guestsCanModify = false))
        assertFalse(CalendarGuest.isGuest("ME@gmail.com", "me@gmail.com", true, false))      // my own event
        assertFalse(CalendarGuest.isGuest("boss@corp.com", "me@gmail.com", true, true))      // guests may modify
        assertFalse(CalendarGuest.isGuest("boss@corp.com", "me@gmail.com", false, false))    // no attendees: a plain event
        assertFalse(CalendarGuest.isGuest(null, "me@gmail.com", true, false))                // unknown organiser: let the provider judge
        assertFalse(CalendarGuest.isGuest("boss@corp.com", "", true, false))
    }

    @Test fun androidDenyingTheWriteIsReportedNotHidden() {
        val (w, store, _) = writer(FakeStore().apply { denied = true })
        val (o, _) = w.handle(WriteRequest.Create("a", "1", draft))
        assertEquals(WriteOutcome("failed", "no_permission"), o)
        assertTrue(store.writes.isEmpty())
    }

    @Test fun anInterruptedOperationIsReportedUnknownAfterARestartAndNeverRewritten() {
        val blob = Mem()
        val store = FakeStore()
        // Simulate: the id was recorded as started, then the process died before the provider answered.
        OpLedger(blob).begin("op9")
        val (restarted) = writer(store, blob = blob)
        val (o, dup) = restarted.handle(WriteRequest.Create("op9", "1", draft))
        assertEquals("unknown", o.state); assertEquals("interrupted", o.reason); assertTrue(dup)
        assertTrue("an unknown outcome must not be written a second time", store.writes.isEmpty())
    }

    @Test fun finishedOperationsSurviveARestart() {
        val blob = Mem()
        val (w, store, _) = writer(blob = blob)
        val done = w.handle(WriteRequest.Create("op1", "1", draft)).first
        val (w2) = writer(store, blob = blob)
        val (again, dup) = w2.handle(WriteRequest.Create("op1", "1", draft))
        assertEquals(done, again); assertTrue(dup)
        assertEquals(1, store.writes.size)
        assertFalse("the ledger must not hold event text", blob.text.contains("Lunch"))
    }

    @Test fun withdrawingEditingForgetsEveryRememberedOperation() {
        val blob = Mem()
        val (w, store, _) = writer(blob = blob)
        w.handle(WriteRequest.Create("op1", "1", draft))
        assertTrue(blob.text.contains("op1"))
        w.forget()
        assertEquals("[]", blob.text)
        assertFalse(OpLedger(blob).known("op1"))
        // After a fresh grant the same id is a new operation, not a replay.
        assertFalse(w.handle(WriteRequest.Create("op1", "1", draft)).second)
        assertEquals(2, store.writes.size)
    }

    @Test fun theLedgerIsBounded() {
        val blob = Mem()
        val ledger = OpLedger(blob, max = 5)
        for (i in 0 until 12) ledger.finish("op$i", WriteOutcome("applied", eventId = "$i"))
        assertEquals(5, JSONArray(blob.text).length())
        assertFalse(ledger.known("op0")); assertTrue(ledger.known("op11"))
        // Corrupt storage never crashes and starts empty.
        assertFalse(OpLedger(Mem().apply { text = "not json{" }).known("x"))
    }

    // ------------------------------------------------------------------ the engine

    private class Core {
        val sent = ArrayList<Pair<String, JSONObject>>()
        fun call(req: String): CoreOutcome {
            val o = JSONObject(req)
            if (o.getString("action") == "transport_data_send") sent += o.getString("kind") to o.getJSONObject("body")
            return CoreOutcome.Ok(JSONObject())
        }
    }

    private class Cal(val items: List<CalInstance> = emptyList()) : CalendarPort {
        override fun calendars() = listOf(
            CalendarInfo("1", "Personal", account = "me@gmail.com", accountType = "com.google", color = -16711936, writable = true),
            CalendarInfo("2", "Holidays", account = "me@gmail.com", accountType = "com.google", color = 255, writable = false),
        )
        override fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int) = items.filter { it.calendarId in calendarIds }
    }

    private class NoContacts : ContactsPort {
        override fun listUids(max: Int) = emptyList<Pair<String, String>>()
        override fun vcard(uid: String): Pair<String, String>? = null
    }

    private class NoSms : SmsPort {
        override fun latestPerThread(max: Int) = emptyList<SmsRow>()
        override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int) = emptyList<SmsRow>()
    }

    private class NoSender : SmsSender {
        override fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener) = Unit
    }

    private fun engine(core: Core, features: ActiveFeatures, store: FakeStore = FakeStore(), cal: Cal = Cal()) =
        PhoneDataEngine(
            core::call, { features }, { setOf("1") }, NoContacts(), cal, NoSms(), NoSender(), { 1_000_000_000L }, "boot",
            CalendarWriter(store, OpLedger(Mem())) { setOf("1") },
        )

    @Test fun theEngineAnswersEveryWriteAndIgnoresThemWhenEditingIsOff() {
        val core = Core()
        val store = FakeStore()
        val ev = DataEvent(1, 1, PhoneDataKinds.CALENDAR_WRITE, body("create") { put("calendarId", "1"); put("event", event()) })

        engine(core, ActiveFeatures(calendar = true), store).onEvents(listOf(ev))
        assertTrue("editing off: ignored, nothing written", core.sent.isEmpty() && store.writes.isEmpty())

        val e = engine(core, ActiveFeatures(calendar = true, calendarEdit = true), store)
        e.onEvents(listOf(ev))
        val result = core.sent.first { it.first == PhoneDataKinds.CALENDAR_WRITE_RESULT }.second
        assertEquals("op1", result.getString("opId")); assertEquals("applied", result.getString("state"))
        assertEquals("100", result.getString("eventId")); assertFalse(result.has("duplicate"))
        // The computer retries the same operation: same answer flagged duplicate, still one write.
        e.onEvents(listOf(ev))
        val again = core.sent.filter { it.first == PhoneDataKinds.CALENDAR_WRITE_RESULT }.last().second
        assertTrue(again.getBoolean("duplicate")); assertEquals("100", again.getString("eventId"))
        assertEquals(1, store.writes.size)
        // A refused request is still answered, with the reason.
        e.onEvents(listOf(DataEvent(2, 1, PhoneDataKinds.CALENDAR_WRITE, body("delete", op = "op2") { put("eventId", "11") })))
        assertEquals("recurring", core.sent.last { it.first == PhoneDataKinds.CALENDAR_WRITE_RESULT }.second.getString("reason"))
    }

    @Test fun calendarsEventsAndRevisionsCarryAccountColourRemindersAndAttendees() {
        val core = Core()
        val inst = CalInstance(
            "5", 1_000_000_100L, 1_000_003_700L, "Dentist", false, "Europe/Berlin", "Main St", null, "1",
            recurring = true, reminders = listOf(30, 10, 10, -1), attendeeCount = 9, attendees = listOf("Ann", "Bob"),
        )
        val e = engine(core, ActiveFeatures(calendar = true), cal = Cal(listOf(inst)))
        e.onEvents(listOf(DataEvent(1, 1, PhoneDataKinds.CALENDAR_REQ, JSONObject().put("requestId", "r1"))))
        val sync = core.sent.first { it.first == PhoneDataKinds.CALENDAR_SYNC }.second
        val cal = sync.getJSONArray("calendars").getJSONObject(0)
        assertEquals("me@gmail.com", cal.getString("account")); assertEquals("com.google", cal.getString("accountType"))
        assertEquals(-16711936, cal.getInt("color")); assertTrue(cal.getBoolean("writable"))
        val ev = sync.getJSONArray("events").getJSONObject(0)
        assertEquals("5", ev.getString("eventId")); assertTrue(ev.getBoolean("recurring"))
        assertEquals(listOf(10, 30), (0 until ev.getJSONArray("reminders").length()).map { ev.getJSONArray("reminders").getInt(it) })
        assertEquals(9, ev.getJSONObject("attendees").getInt("count"))
        assertEquals(2, ev.getJSONObject("attendees").getJSONArray("names").length())
        // A changed reminder changes the revision, so the delta sync carries it.
        val a = PhoneDataProtocol.calendarEvent(inst).getString("rev")
        assertNotEquals(a, PhoneDataProtocol.calendarEvent(inst.copy(reminders = listOf(5))).getString("rev"))
        assertNotEquals(a, PhoneDataProtocol.calendarEvent(inst.copy(attendeeCount = 3)).getString("rev"))
        assertEquals(a, PhoneDataProtocol.calendarEvent(inst.copy()).getString("rev"))
    }
}
