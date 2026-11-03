// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class BatteryReportTest {
    private class Rig(var reading: BatteryReading? = BatteryReading(80, false, false), var on: Boolean = true) {
        var now = 1_000_000L
        val sent = ArrayList<JSONObject>()
        var reject: String? = null
        val scheduled = ArrayList<Pair<Long, () -> Unit>>()
        val reporter = BatteryReporter(
            read = { reading },
            core = { req ->
                val o = JSONObject(req)
                reject?.let { return@BatteryReporter CoreOutcome.Rejected(it, "x") }
                sent += o
                CoreOutcome.Ok(JSONObject().put("queued", 1))
            },
            enabled = { on },
            clock = { now },
            later = { ms, action -> scheduled += ms to action },
        )
        fun body(i: Int) = sent[i].getJSONObject("body")
    }

    @Test fun theBodyUsesTheStockFieldNames() {
        val r = Rig()
        r.reporter.onChanged()
        val o = r.sent.single()
        assertEquals("transport_data_send", o.getString("action"))
        assertEquals("kdeconnect.battery", o.getString("kind"))
        assertFalse(o.has("connection_id"))
        assertEquals(80, r.body(0).getInt("currentCharge"))
        assertEquals(false, r.body(0).getBoolean("isCharging"))
        assertEquals(0, r.body(0).getInt("thresholdEvent"))
    }

    @Test fun unchangedReadingsAreNotSentAgainAndPercentChangesAreDebounced() {
        val r = Rig()
        r.reporter.onChanged()
        r.reporter.onChanged()
        assertEquals(1, r.sent.size)
        r.reading = BatteryReading(79, false, false)
        r.now += 5_000
        r.reporter.onChanged()
        r.reading = BatteryReading(78, false, false)
        r.reporter.onChanged()
        assertEquals(1, r.sent.size)
        // One trailing send, scheduled once, carries the latest value.
        assertEquals(1, r.scheduled.size)
        r.now += 60_000
        r.scheduled.single().second()
        assertEquals(2, r.sent.size)
        assertEquals(78, r.body(1).getInt("currentCharge"))
    }

    @Test fun chargingAndLowChangesGoOutAtOnce() {
        val r = Rig()
        r.reporter.onChanged()
        r.now += 1_000
        r.reading = BatteryReading(80, true, false)
        r.reporter.onChanged()
        assertEquals(2, r.sent.size)
        assertEquals(true, r.body(1).getBoolean("isCharging"))
        r.now += 1_000
        r.reading = BatteryReading(14, false, true)
        r.reporter.onChanged()
        assertEquals(1, r.body(2).getInt("thresholdEvent"))
    }

    @Test fun nothingIsReadOrSentWhileDisabledAndARequestIsAnsweredOnItsConnection() {
        val r = Rig(on = false)
        r.reporter.onChanged()
        r.reporter.sendNow(7)
        assertTrue(r.sent.isEmpty())
        r.on = true
        r.reporter.onChanged()
        r.reporter.sendNow(7)
        assertEquals(7L, r.sent[1].getLong("connection_id"))
        // A reply to one computer does not count as "sent to everyone": the same level still goes to others later.
        r.reporter.reset()
        r.reporter.onChanged()
        assertEquals(3, r.sent.size)
    }

    @Test fun anUnknownLevelSendsNothingAndARateLimitRetriesLater() {
        val r = Rig(reading = null)
        r.reporter.onChanged()
        assertTrue(r.sent.isEmpty())
        r.reading = BatteryReading(50, false, false)
        r.reject = "rate_limited"
        r.reporter.onChanged()
        assertEquals(1, r.scheduled.size)
        r.reject = "not_connected"
        r.scheduled.single().second()
        assertTrue(r.sent.isEmpty()) // not connected: nothing to tell, no retry storm
        assertEquals(1, r.scheduled.size)
    }

    @Test fun readingsFromTheSystemExtras() {
        assertEquals(BatteryReading(50, false, false), BatteryRules.reading(50, 100, 0))
        assertEquals(BatteryReading(100, true, false), BatteryRules.reading(255, 255, 2))
        assertEquals(BatteryReading(10, false, true), BatteryRules.reading(10, 100, 0))
        assertEquals(BatteryReading(10, true, false), BatteryRules.reading(10, 100, 1)) // charging is never "low"
        assertNull(BatteryRules.reading(-1, 100, 0))
        assertNull(BatteryRules.reading(5, 0, 0))
        assertEquals(100, BatteryRules.reading(900, 100, 0)!!.percent)
    }

    @Test fun capabilityIsDeclaredOnlyWhenTheSwitchIsOnAndNothingIsAskedOfAndroid() {
        assertFalse(PhoneDataGate.active(PhoneDataSettings(), PhoneDataPermissions()).battery)
        val a = PhoneDataGate.active(PhoneDataSettings(battery = true), PhoneDataPermissions())
        assertTrue(a.battery && a.any)
        assertNull(PhoneDataProtocol.startObject(ActiveFeatures()))
        assertEquals(true, PhoneDataProtocol.startObject(a)!!.getBoolean("battery"))
        assertEquals(false, PhoneDataProtocol.startObject(ActiveFeatures(sms = true))!!.getBoolean("battery"))
        assertEquals(true, JSONObject(PhoneDataProtocol.configRequest(a)).getBoolean("battery"))
    }

    @Test fun theEngineAnswersARequestOnlyWhenActive() {
        val r = Rig()
        fun engine(f: ActiveFeatures) = PhoneDataEngine(
            core = { CoreOutcome.Ok(JSONObject()) }, features = { f }, calendarIds = { emptySet() },
            contacts = object : ContactsPort { override fun listUids(max: Int) = emptyList<Pair<String, String>>(); override fun vcard(uid: String) = null },
            calendar = object : CalendarPort { override fun instances(calendarIds: Set<String>, fromMs: Long, toMs: Long, max: Int) = emptyList<CalInstance>(); override fun calendars() = emptyList<CalendarInfo>() },
            sms = object : SmsPort { override fun latestPerThread(max: Int) = emptyList<SmsRow>(); override fun thread(threadId: Long, rangeStartMs: Long?, limit: Int) = emptyList<SmsRow>() },
            sender = object : SmsSender { override fun send(address: String, text: String, subId: Int?, listener: SmsStatusListener) {} },
            battery = r.reporter,
        )
        val req = DataEvent(1, 9, PhoneDataKinds.BATTERY_REQ, JSONObject().put("request", true))
        engine(ActiveFeatures()).onEvents(listOf(req))
        assertTrue(r.sent.isEmpty())
        engine(ActiveFeatures(battery = true)).onEvents(listOf(req))
        assertEquals(9L, r.sent.single().getLong("connection_id"))
    }

    @Test fun batteryIsClosedForEveryComputerUntilAllowed() {
        val laptop = "a".repeat(32)
        val none = ComputerGrants()
        assertEquals(0, none.permitsObject().getJSONArray("battery").length())
        val g = none.set(laptop, GrantFeature.Battery, true)
        assertTrue(g.allows(laptop, GrantFeature.Battery))
        assertEquals(laptop, g.permitsObject().getJSONArray("battery").getString(0))
        assertEquals(g, ComputerGrants.fromJson(g.toJson()))
        assertFalse(g.clear(GrantFeature.Battery).allows(laptop, GrantFeature.Battery))
        // Withdrawing sms leaves battery alone.
        assertTrue(g.set(laptop, GrantFeature.Sms, true).set(laptop, GrantFeature.Sms, false).allows(laptop, GrantFeature.Battery))
    }

    // ---- test-gaps-4: risky branches

    @Test fun theGapIsExactlyOneMinuteAndAnEarlierChangeWaitsOnlyTheRemainder() {
        val r = Rig()
        r.reporter.onChanged() // sent at t0
        r.reading = BatteryReading(79, false, false)
        r.now += 59_999
        r.reporter.onChanged()
        assertEquals(1, r.sent.size)
        assertEquals("the trailing send waits for the remainder, not a full gap", 1L, r.scheduled.single().first)
        r.now += 1
        r.reading = BatteryReading(78, false, false)
        r.reporter.onChanged() // exactly gapMs after the last send: goes out at once
        assertEquals(2, r.sent.size)
        assertEquals(78, r.body(1).getInt("currentCharge"))
    }

    @Test fun aTrailingSendThatFiresEarlyReschedulesInsteadOfSendingOrStalling() {
        val r = Rig()
        r.reporter.onChanged()
        r.reading = BatteryReading(70, false, false)
        r.now += 10_000
        r.reporter.onChanged()
        assertEquals(1, r.scheduled.size)
        // The scheduler fires before the gap is over (clock did not advance): still nothing sent, and a new one is queued.
        r.scheduled[0].second()
        assertEquals(1, r.sent.size)
        assertEquals(2, r.scheduled.size)
        r.now += 60_000
        r.scheduled[1].second()
        assertEquals(70, r.body(1).getInt("currentCharge"))
    }

    @Test fun aTrailingSendIsDroppedWhenTheFeatureWasSwitchedOffMeanwhile() {
        val r = Rig()
        r.reporter.onChanged()
        r.reading = BatteryReading(70, false, false)
        r.now += 1_000
        r.reporter.onChanged()
        r.on = false
        r.now += 60_000
        r.scheduled.single().second()
        assertEquals(1, r.sent.size)
        // pending was cleared, so a later change can schedule again
        r.on = true
        r.reading = BatteryReading(60, false, false)
        r.now += 1
        r.reporter.onChanged()
        assertEquals(2, r.sent.size)
    }

    @Test fun aRevertToTheLastSentValueSendsNothing() {
        val r = Rig()
        r.reporter.onChanged()
        r.reading = BatteryReading(79, false, false)
        r.now += 1_000
        r.reporter.onChanged()
        r.reading = BatteryReading(80, false, false)
        r.now += 60_000
        r.scheduled.single().second()
        assertEquals(1, r.sent.size)
    }

    @Test fun aFailedBroadcastIsNotRememberedAndOnlyOneRetryIsQueuedForRateLimits() {
        val r = Rig()
        r.reject = "rate_limited"
        r.reporter.onChanged()
        r.reporter.onChanged()
        r.reporter.onChanged()
        assertEquals(1, r.scheduled.size)
        assertEquals(5_000L, r.scheduled.single().first)
        r.reject = null
        r.scheduled.single().second()
        assertEquals(1, r.sent.size) // nothing had been remembered, so the same level goes out now
        // Any other refusal queues no retry.
        val q = Rig()
        for (code in listOf("data_disabled", "not_connected", "bad_request")) {
            q.reject = code
            q.reporter.reset()
            q.reporter.onChanged()
        }
        assertTrue(q.scheduled.isEmpty())
    }

    @Test fun aRateLimitOnAReplyRetriesTheBroadcastPathAndLeavesTheMemoryAlone() {
        val r = Rig()
        r.reporter.onChanged()
        r.reject = "rate_limited"
        r.reporter.sendNow(4)
        assertEquals(1, r.scheduled.size)
        r.reject = null
        r.now += 1
        r.reporter.sendNow(4)
        assertEquals(4L, r.sent.last().getLong("connection_id"))
        r.reporter.onChanged()
        assertEquals("the reply did not change what was broadcast", 2, r.sent.size)
    }

    @Test fun sendNowWithAnUnknownLevelSendsNothingAndTheReadingEdgeValuesAreExact() {
        val r = Rig(reading = null)
        r.reporter.sendNow()
        assertTrue(r.sent.isEmpty())
        assertEquals(BatteryReading(15, false, true), BatteryRules.reading(15, 100, 0))
        assertEquals(BatteryReading(16, false, false), BatteryRules.reading(16, 100, 0))
        assertEquals(BatteryReading(0, false, true), BatteryRules.reading(0, 100, 0))
        assertEquals(BatteryReading(33, false, false), BatteryRules.reading(1, 3, 0)) // truncates like the system UI
        assertEquals(BatteryReading(100, false, false), BatteryRules.reading(Int.MAX_VALUE, 1, 0)) // no Int overflow
        assertEquals(BatteryReading(0, true, false), BatteryRules.reading(0, 100, 4))
        assertEquals(BatteryReading(50, true, false), BatteryRules.reading(50, 100, -1)) // any plug source counts
        assertNull(BatteryRules.reading(0, -100, 0))
        assertEquals("kdeconnect.battery", BatteryRules.KIND)
        assertEquals("kdeconnect.battery.request", BatteryRules.REQUEST_KIND)
    }

    @Test fun theBodyAlwaysCarriesAllThreeStockFields() {
        val b = BatteryReading(14, false, true).body()
        assertEquals(setOf("currentCharge", "isCharging", "thresholdEvent"), b.keys().asSequence().toSet())
        assertEquals(1, b.getInt("thresholdEvent"))
    }
}
