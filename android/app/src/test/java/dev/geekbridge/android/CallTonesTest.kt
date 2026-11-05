// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Keypad tones (DTMF) on the phone: what a computer may ask for, and that nothing else gets through. */
class CallTonesTest {
    private val all = CallPermissions(readPhoneState = true, answerPhoneCalls = true, readCallLog = true, readContacts = true)

    private class Tel(var session: Boolean = true, var plays: Boolean = true) : TelecomPort {
        val played = ArrayList<Char>()
        var other = 0
        override fun acceptRingingCall() { other++ }
        override fun endCall(): Boolean { other++; return true }
        override fun placeCall(number: String) { other++ }
        override fun canPlayTones() = session
        override fun playTone(tone: Char): Boolean {
            if (!plays) return false
            played += tone
            return true
        }
    }

    @Test fun onlyTheKeypadCharactersAreValid() {
        for (c in "0123456789*#") assertTrue("$c", CallRules.validTone(c))
        for (c in listOf('A', 'D', 'a', 'd', ' ', '+', '\n', '\u0000', '١', '１', 'x')) assertFalse("$c", CallRules.validTone(c))
        assertFalse(CallRules.validTone(null))
    }

    @Test fun aToneIsPlayedOnlyIntoALiveCallThroughAnInCallSession() {
        val t = Tel()
        assertNull(CallRules.handle(CallAction.Tone, CallState.OffHook, 34, all, t, tone = '5'))
        assertNull(CallRules.handle(CallAction.Tone, CallState.OffHook, 34, all, t, tone = '#'))
        assertEquals(listOf('5', '#'), t.played)
        // Not ringing, not idle, not unknown: there is no call to press keys in.
        for (s in listOf(CallState.Ringing, CallState.Idle, null)) {
            assertEquals("$s", CallFailure.NoSuchCall, CallRules.handle(CallAction.Tone, s, 34, all, t, tone = '1'))
        }
        assertEquals(2, t.played.size)
        assertEquals(0, t.other)
    }

    @Test fun hostileTonesAreRefusedBeforeAnythingIsPlayed() {
        val t = Tel()
        for (bad in listOf(null, 'A', 'D', ' ', '\n', '١', '+', 'x')) {
            assertEquals("$bad", CallFailure.Failed, CallRules.handle(CallAction.Tone, CallState.OffHook, 34, all, t, tone = bad))
        }
        assertTrue(t.played.isEmpty())
    }

    @Test fun withoutASessionOrThePermissionThePhoneSaysSoAndPlaysNothing() {
        val noSession = Tel(session = false)
        assertEquals(CallFailure.PermissionMissing, CallRules.handle(CallAction.Tone, CallState.OffHook, 34, all, noSession, tone = '1'))
        val t = Tel()
        assertEquals(
            CallFailure.PermissionMissing,
            CallRules.handle(CallAction.Tone, CallState.OffHook, 34, all.copy(readPhoneState = false), t, tone = '1'),
        )
        assertTrue(noSession.played.isEmpty() && t.played.isEmpty())
        // Android refusing (the call ended between the check and the tone) is a plain failure.
        assertEquals(CallFailure.Failed, CallRules.handle(CallAction.Tone, CallState.OffHook, 34, all, Tel(plays = false), tone = '1'))
    }

    @Test fun canTonesIsReportedOnlyWhenTheSessionExists() {
        assertTrue(CallRules.report(CallState.OffHook, null, null, 34, all, tones = true).canTones)
        assertFalse(CallRules.report(CallState.OffHook, null, null, 34, all, tones = false).canTones)
        assertFalse(CallRules.report(CallState.OffHook, null, null, 34, all.copy(readPhoneState = false), tones = true).canTones)
        // The default telecom port (a stock phone) never claims it.
        val stock = object : TelecomPort {
            override fun acceptRingingCall() {}
            override fun endCall() = true
            override fun placeCall(number: String) {}
        }
        assertFalse(stock.canPlayTones())
        assertFalse(stock.playTone('1'))
        val body = JSONObject(CallsProtocol.reportRequest(CallReport(CallState.OffHook, null, null, true, true, false, true)))
        assertTrue(body.getBoolean("can_tones"))
    }

    @Test fun waitResultsKeepOnlyWellFormedToneRequests() {
        fun ev(seq: Int, tone: Any?) = JSONObject().put("kind", "request").put("seq", seq).put("connection_id", 1)
            .put("request_id", "t$seq").put("action", "tone").apply { if (tone != null) put("tone", tone) }
        val w = JSONObject()
            .put(
                "events",
                JSONArray().put(ev(1, "5")).put(ev(2, "*")).put(ev(3, "A")).put(ev(4, "12")).put(ev(5, "")).put(ev(6, null)).put(ev(7, 5)).put(ev(8, "١")),
            )
            .put("last_seq", 8)
        val r = CallsProtocol.parseWait(w)!!.requests
        assertEquals(listOf('5', '*'), r.map { it.tone })
        // The tone is never in the text form of a request (it may be a PIN).
        assertFalse(r[0].toString().contains("5"))
    }

    // ---- the engine

    private class Rig(val tel: Tel = Tel(), perms: CallPermissions) {
        val sent = ArrayList<JSONObject>()
        val engine = CallControlEngine(
            core = { sent += JSONObject(it); CoreOutcome.Ok(JSONObject()) },
            telecom = tel,
            permissions = { perms },
            settings = { CallLinkSettings(true) },
            lookup = { null },
            sdk = 34,
            strings = TestStrings,
        )
        fun results() = sent.filter { it.getString("action") == "transport_calls_result" }
        fun lastReport() = sent.last { it.getString("action") == "transport_calls_report" }
    }

    @Test fun theEngineAnswersEachToneRequestAndTellsTheComputerWhenTheSessionAppears() {
        val r = Rig(Tel(session = false), all)
        r.engine.onState(CallState.OffHook)
        assertFalse(r.lastReport().getBoolean("can_tones"))
        r.tel.session = true
        r.engine.tonesChanged()
        assertTrue(r.lastReport().getBoolean("can_tones"))

        r.engine.onRequests(listOf(CallRequest(1, 9, "t1", CallAction.Tone, tone = '7'), CallRequest(2, 9, "t2", CallAction.Tone, tone = null)))
        assertEquals(listOf('7'), r.tel.played)
        val res = r.results()
        assertFalse(res[0].has("failure"))
        assertEquals("failed", res[1].getString("failure"))

        // The call ends: a late tone is "no such call" and nothing is played.
        r.engine.onState(CallState.Idle)
        r.engine.onRequests(listOf(CallRequest(3, 9, "t3", CallAction.Tone, tone = '1')))
        assertEquals("no_such_call", r.results().last().getString("failure"))
        assertEquals(listOf('7'), r.tel.played)
        assertEquals(0, r.tel.other)
    }

    @Test fun toneRequestsAreLimitedTo24PerFiveSeconds() {
        var now = 1_000L
        val sent = ArrayList<JSONObject>()
        val tel = Tel()
        val engine = CallControlEngine(
            core = { sent += JSONObject(it); CoreOutcome.Ok(JSONObject()) },
            telecom = tel,
            permissions = { all },
            settings = { CallLinkSettings(true) },
            lookup = { null },
            sdk = 34,
            strings = TestStrings,
            clock = { now },
        )
        engine.onState(CallState.OffHook)
        engine.onRequests((1..30).map { CallRequest(it.toLong(), 9, "l$it", CallAction.Tone, tone = '1') })
        val res = sent.filter { it.getString("action") == "transport_calls_result" }
        assertEquals(24, res.count { !it.has("failure") })
        assertEquals(6, res.count { it.optString("failure") == "rate_limited" })
        assertEquals(24, tel.played.size)
        // Once the window has passed, tones flow again.
        now += 5_000L
        engine.onRequests(listOf(CallRequest(31, 9, "l31", CallAction.Tone, tone = '2')))
        assertFalse(sent.last().has("failure"))
    }

    @Test fun slidingLimitAndToneScheduleBehave() {
        val l = SlidingLimit(2, 100)
        assertTrue(l.tryAcquire(0))
        assertTrue(l.tryAcquire(10))
        assertFalse(l.tryAcquire(50))
        assertTrue(l.tryAcquire(100))
        // Tones queue one after another, never overlapping.
        val s = ToneSchedule(150, 50)
        assertEquals(1000L, s.reserve(1000))
        assertEquals(1200L, s.reserve(1010))
        assertEquals(1400L, s.reserve(1020))
        assertEquals(5000L, s.reserve(5000))
    }

    @Test fun aFloodOfValidTonesStillOnlyPlaysIntoTheLiveCall() {
        val r = Rig(Tel(), all)
        r.engine.onState(CallState.Ringing)
        r.engine.onRequests((1..50).map { CallRequest(it.toLong(), 9, "f$it", CallAction.Tone, tone = '1') })
        assertTrue("ringing: nothing played", r.tel.played.isEmpty())
        assertEquals(50, r.results().count { it.getString("failure") == "no_such_call" })
    }
}
