// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Texts a paired computer asks the phone to send: limits per computer, replay of status, delivery-state order. */
class SmsGuardTest {
    private class FakeCore {
        val sent = ArrayList<Pair<String, JSONObject>>()
        fun call(req: String): CoreOutcome {
            val o = JSONObject(req)
            if (o.getString("action") == "transport_data_send") sent += o.getString("kind") to o.getJSONObject("body")
            return CoreOutcome.Ok(JSONObject().put("queued", 1))
        }
        fun statuses() = sent.filter { it.first == PhoneDataKinds.SMS_STATUS }.map { it.second }
    }

    private class FakeSender(val script: (SmsStatusListener) -> Unit = { it.onStatus("accepted") }) : SmsSender {
        val calls = ArrayList<Pair<String, String>>()
        override fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener) {
            calls += address to text
            script(listener)
        }
    }

    private class NoSms : SmsPort {
        override fun subscriptions() = emptyList<SimInfo>()
        override fun unreadCounts() = emptyMap<Long, Int>()
        override fun latestPerThread(max: Int) = emptyList<SmsRow>()
        override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int) = emptyList<SmsRow>()
    }

    private class Rig(
        var now: Long = 1_000_000_000L,
        sender: FakeSender = FakeSender(),
        val notices: ArrayList<String> = ArrayList(),
    ) {
        val core = FakeCore()
        val sender = sender
        val engine = PhoneDataEngine(
            core::call, { ActiveFeatures(sms = true) }, { emptySet() }, FakeContactsNone, FakeCalNone, NoSms(), sender,
            { now }, "boot", smsLimited = { notices += it },
        )
        var n = 0
        fun send(device: String, computer: String, to: String = "+4915551234567", id: String? = "r${n++}") {
            val body = JSONObject().put("version", 2).put("messageBody", "hi")
                .put("addresses", JSONArray().put(JSONObject().put("address", to)))
            if (id != null) body.put("requestId", id)
            engine.onEvents(listOf(DataEvent(1, 1, PhoneDataKinds.SMS_REQUEST, body, device, computer)))
        }
    }

    private object FakeContactsNone : ContactsPort {
        override fun listUids(max: Int) = emptyList<Pair<String, String>>()
        override fun vcard(uid: String): Pair<String, String>? = null
    }

    private object FakeCalNone : CalendarPort {
        override fun calendars() = emptyList<CalendarInfo>()
        override fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int) = emptyList<CalInstance>()
        override fun instancesAround(calendarIds: Set<String>, fromMs: Long, toMs: Long, pivotMs: Long, max: Int) = emptyList<CalInstance>()
    }

    @Test fun eventsCarryWhichComputerAskedAndOnlyTheCoreSetsIt() {
        val r = JSONObject(
            """{"seq":1,"events":[{"seq":1,"connection_id":4,"kind":"kdeconnect.sms.request","device":"dev1","computer":"Desk",
            "body":{"device":"forged","computer":"Forged"}}]}""",
        )
        val e = PhoneDataProtocol.parseWait(r)!!.events.single()
        assertEquals("dev1", e.device); assertEquals("Desk", e.computer)
        assertEquals("forged", e.body.getString("device")) // untouched, never read as the identity
        val none = PhoneDataProtocol.parseWait(JSONObject("""{"seq":1,"events":[{"seq":1,"kind":"kdeconnect.sms.request","body":{}}]}"""))!!.events.single()
        assertEquals("", none.device)
    }

    @Test fun oneComputerHittingItsLimitDoesNotBlockAnother() {
        val rig = Rig()
        repeat(PhoneDataLimits.SMS_SENDS_PER_MINUTE + 3) { rig.send("A", "Desk A") }
        assertEquals(PhoneDataLimits.SMS_SENDS_PER_MINUTE, rig.sender.calls.size)
        rig.send("B", "Desk B")
        assertEquals(PhoneDataLimits.SMS_SENDS_PER_MINUTE + 1, rig.sender.calls.size)
    }

    @Test fun theLimitSurvivesAReconnect() {
        // A computer that drops and reopens its link must not get a fresh allowance.
        val rig = Rig()
        repeat(PhoneDataLimits.SMS_SENDS_PER_MINUTE) { rig.send("A", "Desk A") }
        rig.engine.reset()
        rig.send("A", "Desk A")
        assertEquals(PhoneDataLimits.SMS_SENDS_PER_MINUTE, rig.sender.calls.size)
        assertEquals("limit", rig.core.statuses().last().getString("reason"))
    }

    @Test fun theMinuteWindowSlidesAndTheHourStillCounts() {
        val rig = Rig()
        repeat(PhoneDataLimits.SMS_SENDS_PER_MINUTE) { rig.send("A", "Desk A") }
        rig.now += 61_000
        rig.send("A", "Desk A")
        assertEquals(PhoneDataLimits.SMS_SENDS_PER_MINUTE + 1, rig.sender.calls.size)
    }

    @Test fun sprayingManyDifferentNumbersIsStoppedEvenWhenTheRateIsLow() {
        val rig = Rig()
        // One text a minute, each to a new number: the per-minute rate never trips, the recipient spread does.
        repeat(PhoneDataLimits.SMS_RECIPIENTS_PER_WINDOW + 2) { i ->
            rig.now += 61_000
            rig.send("A", "Desk A", to = "+49151100000${10 + i}")
        }
        assertEquals(PhoneDataLimits.SMS_RECIPIENTS_PER_WINDOW, rig.sender.calls.size)
        assertEquals("recipients", rig.core.statuses().last().getString("reason"))
        // The same number again is still fine (an ordinary conversation).
        rig.send("A", "Desk A", to = "+4915110000010")
        assertEquals(PhoneDataLimits.SMS_RECIPIENTS_PER_WINDOW + 1, rig.sender.calls.size)
    }

    @Test fun sameNumberWrittenTwoWaysCountsOnce() {
        val rig = Rig()
        repeat(PhoneDataLimits.SMS_RECIPIENTS_PER_WINDOW) { i ->
            rig.now += 61_000
            rig.send("A", "Desk A", to = if (i % 2 == 0) "+49 151 1234 5678" else "0151 1234567" + "8")
        }
        assertEquals(PhoneDataLimits.SMS_RECIPIENTS_PER_WINDOW, rig.sender.calls.size)
        assertEquals(SmsGuard.recipientKey("+49 151 1234 5678"), SmsGuard.recipientKey("0151 12345678"))
    }

    @Test fun theOwnerGetsOneNoticePerComputerPerWindowNamingIt() {
        val rig = Rig()
        repeat(PhoneDataLimits.SMS_SENDS_PER_MINUTE + 5) { rig.send("A", "Desk A") }
        assertEquals(listOf("Desk A"), rig.notices)
        rig.now += 61_000; repeat(PhoneDataLimits.SMS_SENDS_PER_MINUTE + 2) { rig.send("A", "Desk A") }
        assertEquals("one notice per window, not one per refused text", listOf("Desk A"), rig.notices)
        repeat(PhoneDataLimits.SMS_SENDS_PER_MINUTE + 2) { rig.send("B", "Desk B") }
        assertEquals(listOf("Desk A", "Desk B"), rig.notices)
        rig.now += SmsGuard.NOTICE_EVERY_MS + 1
        repeat(PhoneDataLimits.SMS_SENDS_PER_MINUTE + 2) { rig.send("A", "Desk A") }
        assertEquals(3, rig.notices.size)
    }

    @Test fun refusedTextsForOtherReasonsRaiseNoLimitNotice() {
        val rig = Rig()
        rig.send("A", "Desk A", to = "12345") // short code, not allowed
        assertEquals("blocked_number", rig.core.statuses().single().getString("reason"))
        assertTrue(rig.notices.isEmpty())
    }

    @Test fun guardMemoryIsBounded() {
        val g = SmsGuard()
        for (i in 0 until 100) assertEquals(SmsGuard.Verdict.Ok, g.admit("dev$i", "+4915112345678", 1_000L + i))
        assertTrue(g.trackedComputers() <= SmsGuard.MAX_COMPUTERS)
    }

    @Test fun aRepeatedRequestGetsTheStatusItAlreadyHadInsteadOfSilence() {
        // After a reconnect the computer may send the same request id again. It must not text twice, and it must
        // learn what happened the first time, otherwise the send stays "unknown" on its screen.
        val rig = Rig(sender = FakeSender { it.onStatus("accepted"); it.onStatus("sent") })
        rig.send("A", "Desk A", id = "same")
        assertEquals(listOf("accepted", "sent"), rig.core.statuses().map { it.getString("state") })
        rig.core.sent.clear()
        rig.send("A", "Desk A", id = "same")
        assertEquals(1, rig.sender.calls.size)
        assertEquals(listOf("sent"), rig.core.statuses().map { it.getString("state") })
        assertEquals("same", rig.core.statuses().single().getString("requestId"))
    }

    @Test fun aRepeatedRequestKeepsItsFailureReasonToo() {
        val rig = Rig(sender = FakeSender { it.onStatus("accepted"); it.onStatus("failed", "no_service") })
        rig.send("A", "Desk A", id = "f1")
        rig.core.sent.clear()
        rig.send("A", "Desk A", id = "f1")
        val s = rig.core.statuses().single()
        assertEquals("failed", s.getString("state")); assertEquals("no_service", s.getString("reason"))
    }

    @Test fun aLateAcceptedNeverWalksTheStatusBackwards() {
        // The "sent" report can overtake our own "accepted" (separate threads).
        val rig = Rig(sender = FakeSender { it.onStatus("sent"); it.onStatus("accepted") })
        rig.send("A", "Desk A", id = "race")
        assertEquals(listOf("sent"), rig.core.statuses().map { it.getString("state") })
    }

    @Test fun aRepeatWithoutAnyRememberedStatusStaysSilentAndSendsNothing() {
        val ledger = RequestLedger()
        ledger.firstTime("old") // as if remembered from an earlier run of the app
        val rig = Rig()
        val e = PhoneDataEngine(rig.core::call, { ActiveFeatures(sms = true) }, { emptySet() }, FakeContactsNone, FakeCalNone, NoSms(), rig.sender, { rig.now }, "boot", smsLedger = ledger)
        val body = JSONObject().put("messageBody", "hi").put("requestId", "old")
            .put("addresses", JSONArray().put(JSONObject().put("address", "+4915551234567")))
        e.onEvents(listOf(DataEvent(1, 1, PhoneDataKinds.SMS_REQUEST, body)))
        assertTrue(rig.sender.calls.isEmpty())
        assertFalse(rig.core.sent.any { it.first == PhoneDataKinds.SMS_STATUS })
    }

    @Test fun androidResultCodesBecomeTheReasonsTheComputerCanExplain() {
        // Values are SmsManager.RESULT_* (checked against android.jar of API 36).
        val want = mapOf(
            1 to "generic", 2 to "radio_off", 9 to "radio_off", 100 to "radio_off", 4 to "no_service", 135 to "no_service",
            116 to "no_service", 5 to "limit", 106 to "limit", 7 to "short_code", 8 to "short_code", 120 to "no_sim",
            134 to "no_sim", 128 to "no_sim", 129 to "no_sim", 3 to "invalid", 11 to "invalid", 14 to "invalid", 18 to "invalid",
            19 to "invalid", 6 to "fdn", 10 to "network", 17 to "network", 102 to "network", 29 to "emergency",
            23 to "cancelled", 119 to "cancelled", 121 to "call_active", 123 to "call_active", 15 to "generic", 32 to "generic",
            0 to "generic", -1 to "generic", 9999 to "generic",
        )
        for ((code, reason) in want) assertEquals("code $code", reason, PhoneDataProtocol.failureReason(code))
    }

    @Test fun everyReasonIsShortAndSafeOnTheWire() {
        for (code in -5..200) {
            val r = PhoneDataProtocol.failureReason(code)
            assertTrue(r, r.length <= 32 && r.all { it.isLetter() || it == '_' })
        }
    }
}
