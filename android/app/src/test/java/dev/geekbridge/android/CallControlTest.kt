// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CallControlTest {
    private val all = CallPermissions(readPhoneState = true, answerPhoneCalls = true, readCallLog = true, readContacts = true)
    private val none = CallPermissions(false, false, false, false)

    private class FakeTelecom(var endResult: Boolean = true, var throwOnAccept: RuntimeException? = null) : TelecomPort {
        val log = ArrayList<String>()
        override fun acceptRingingCall() {
            throwOnAccept?.let { throw it }
            log += "accept"
        }
        override fun endCall(): Boolean {
            log += "end"
            return endResult
        }
    }

    // ---- rules

    @Test fun capabilitiesFollowPermissionAndApiLevel() {
        assertTrue(CallRules.canAnswer(26, all))
        assertFalse(CallRules.canAnswer(25, all))
        // endCall() exists from API 28: on 26-27 the phone can answer but must not claim it can end.
        assertTrue(CallRules.canAnswer(27, all))
        assertFalse(CallRules.canEnd(27, all))
        assertTrue(CallRules.canEnd(28, all))
        assertFalse(CallRules.canAnswer(33, all.copy(answerPhoneCalls = false)))
        // Without the call state the app cannot know a call is ringing.
        assertFalse(CallRules.canAnswer(33, all.copy(readPhoneState = false)))
        assertFalse(CallRules.canEnd(33, none))
    }

    @Test fun numberNeedsCallLogAndNameNeedsContactsAndIdleHasNoParty() {
        val r = CallRules.report(CallState.Ringing, "+15550100", "Ada", 34, all)
        assertEquals("+15550100", r.number)
        assertEquals("Ada", r.name)
        assertTrue(r.canAnswer && r.canEnd)
        val noLog = CallRules.report(CallState.Ringing, "+15550100", "Ada", 34, all.copy(readCallLog = false))
        assertNull(noLog.number)
        assertNull(noLog.name) // a name without a number would be unattributable
        val noContacts = CallRules.report(CallState.Ringing, "+15550100", "Ada", 34, all.copy(readContacts = false))
        assertEquals("+15550100", noContacts.number)
        assertNull(noContacts.name)
        val idle = CallRules.report(CallState.Idle, "+15550100", "Ada", 34, all)
        assertNull(idle.number)
        assertNull(idle.name)
    }

    @Test fun fieldsAreBoundedAndControlCharactersDropped() {
        assertNull(CallRules.cleanField("   "))
        assertNull(CallRules.cleanField("12\u0007"))
        assertNull(CallRules.cleanField("x".repeat(129)))
        assertEquals("x".repeat(128), CallRules.cleanField("x".repeat(128)))
        // 64 two-byte characters fit, 65 do not.
        assertNotNull(CallRules.cleanField("é".repeat(64)))
        assertNull(CallRules.cleanField("é".repeat(65)))
    }

    @Test fun reportToStringNeverShowsTheParty() {
        val r = CallRules.report(CallState.Ringing, "+15550100", "Ada", 34, all)
        assertFalse(r.toString().contains("5550100"))
        assertFalse(r.toString().contains("Ada"))
    }

    // ---- request handling

    @Test fun answerOnlyWorksWhileRingingAndOnlyWithPermission() {
        val t = FakeTelecom()
        assertNull(CallRules.handle(CallAction.Answer, CallState.Ringing, 34, all, t))
        assertEquals(listOf("accept"), t.log)
        assertEquals(CallFailure.NoSuchCall, CallRules.handle(CallAction.Answer, CallState.OffHook, 34, all, t))
        assertEquals(CallFailure.NoSuchCall, CallRules.handle(CallAction.Answer, CallState.Idle, 34, all, t))
        assertEquals(CallFailure.NoSuchCall, CallRules.handle(CallAction.Answer, null, 34, all, t))
        assertEquals(CallFailure.PermissionMissing, CallRules.handle(CallAction.Answer, CallState.Ringing, 34, none, t))
        assertEquals(listOf("accept"), t.log) // refusals never reached the telecom service
    }

    @Test fun rejectNeedsARingingCallAndHangupALiveOne() {
        val t = FakeTelecom()
        assertNull(CallRules.handle(CallAction.Reject, CallState.Ringing, 34, all, t))
        assertEquals(CallFailure.NoSuchCall, CallRules.handle(CallAction.Reject, CallState.OffHook, 34, all, t))
        assertNull(CallRules.handle(CallAction.Hangup, CallState.OffHook, 34, all, t))
        // Hang up must never end a call that is still ringing: that is what Reject means.
        assertEquals(CallFailure.NoSuchCall, CallRules.handle(CallAction.Hangup, CallState.Ringing, 34, all, t))
        assertEquals(listOf("end", "end"), t.log)
        assertEquals(CallFailure.PermissionMissing, CallRules.handle(CallAction.Reject, CallState.Ringing, 27, all, t))
    }

    @Test fun platformFailuresMapToTheFixedVocabulary() {
        assertEquals(
            CallFailure.PermissionMissing,
            CallRules.handle(CallAction.Answer, CallState.Ringing, 34, all, FakeTelecom(throwOnAccept = SecurityException())),
        )
        assertEquals(
            CallFailure.Failed,
            CallRules.handle(CallAction.Answer, CallState.Ringing, 34, all, FakeTelecom(throwOnAccept = IllegalStateException())),
        )
        assertEquals(CallFailure.Failed, CallRules.handle(CallAction.Hangup, CallState.OffHook, 34, all, FakeTelecom(endResult = false)))
    }

    // ---- protocol

    @Test fun requestBuildersUseTheDocumentedActionsAndFields() {
        val start = TransportProtocol.startRequest(
            StartOptions("/s", "Pixel", true, null, false, true, false, calls = CallLinkSettings(true)),
        )
        assertEquals(true, JSONObject(start).getJSONObject("calls").getBoolean("enabled"))
        assertFalse(JSONObject(TransportProtocol.startRequest(StartOptions("/s", "P", true, null, false, true, false))).has("calls"))

        val report = JSONObject(CallsProtocol.reportRequest(CallReport(CallState.OffHook, "1", null, true, false)))
        assertEquals("transport_calls_report", report.getString("action"))
        assertEquals("offhook", report.getString("state"))
        assertEquals("1", report.getString("number"))
        assertFalse(report.has("name"))
        assertEquals(false, report.getBoolean("can_end"))

        val ok = JSONObject(CallsProtocol.resultRequest(7, "r-1", null))
        assertEquals(7, ok.getLong("connection_id"))
        assertFalse(ok.has("failure"))
        assertEquals("permission_missing", JSONObject(CallsProtocol.resultRequest(7, "r-1", CallFailure.PermissionMissing)).getString("failure"))
        assertEquals(CallsProtocol.MAX_WAIT_MS, JSONObject(CallsProtocol.waitRequest(0, 10 * 60_000)).getLong("timeout_ms"))
    }

    @Test fun waitResultsAreParsedStrictly() {
        val good = JSONObject()
            .put("events", JSONArray().put(JSONObject().put("kind", "request").put("seq", 3).put("connection_id", 9).put("request_id", "c1").put("action", "answer")))
            .put("last_seq", 3).put("stopped", false).put("enabled", true)
        val w = CallsProtocol.parseWait(good)!!
        assertEquals(1, w.requests.size)
        assertEquals(CallAction.Answer, w.requests[0].action)
        assertEquals(3, w.lastSeq)
        assertTrue(w.enabled)

        val junk = JSONObject().put(
            "events",
            JSONArray()
                .put(JSONObject().put("kind", "request").put("seq", 1).put("connection_id", 1).put("request_id", "bad id!").put("action", "answer"))
                .put(JSONObject().put("kind", "request").put("seq", 2).put("connection_id", 1).put("request_id", "ok").put("action", "dial"))
                .put(JSONObject().put("kind", "other").put("seq", 3))
                .put(JSONObject().put("kind", "request").put("seq", 4).put("request_id", "ok").put("action", "hangup")),
        )
        assertEquals(0, CallsProtocol.parseWait(junk)!!.requests.size)
        assertNull(CallsProtocol.parseWait("nope"))
        assertNull(CallsProtocol.parseWait(JSONObject().put("events", JSONArray((0 until 17).map { JSONObject() }))))
    }

    // ---- engine

    private class Harness(var perms: CallPermissions, var enabled: Boolean = true, sdk: Int = 34, names: Map<String, String> = emptyMap()) {
        val sent = ArrayList<JSONObject>()
        var coreOk = true
        val telecom = FakeTelecom()
        val engine = CallControlEngine(
            core = { req ->
                sent += JSONObject(req)
                if (coreOk) CoreOutcome.Ok(JSONObject()) else CoreOutcome.Unavailable("down")
            },
            telecom = telecom,
            permissions = { perms },
            settings = { CallLinkSettings(enabled) },
            lookup = { names[it] },
            sdk = sdk,
        )
        fun actions() = sent.map { it.getString("action") }
    }

    @Test fun nothingIsSentUnlessTheUserTurnedItOn() {
        val h = Harness(all, enabled = false)
        h.engine.onState(CallState.Ringing, "+15550100")
        h.engine.onRequests(listOf(CallRequest(1, 1, "r", CallAction.Answer)))
        assertTrue(h.sent.isEmpty())
        assertTrue(h.telecom.log.isEmpty())
    }

    @Test fun ringingThenNumberThenAnswerThenIdle() {
        val h = Harness(all, names = mapOf("+15550100" to "Ada"))
        h.engine.onState(CallState.Ringing)
        h.engine.onNumber("+15550100") // Android delivers the number after the state
        h.engine.onRequests(listOf(CallRequest(1, 5, "r-1", CallAction.Answer)))
        h.engine.onState(CallState.OffHook)
        h.engine.onState(CallState.Idle)

        assertEquals(
            listOf("transport_calls_report", "transport_calls_report", "transport_calls_result", "transport_calls_report", "transport_calls_report"),
            h.actions(),
        )
        assertFalse(h.sent[0].has("number"))
        assertEquals("+15550100", h.sent[1].getString("number"))
        assertEquals("Ada", h.sent[1].getString("name"))
        assertEquals(listOf("accept"), h.telecom.log)
        assertFalse(h.sent[2].has("failure"))
        assertEquals("offhook", h.sent[3].getString("state"))
        // The party is remembered through the call and forgotten at idle.
        assertEquals("+15550100", h.sent[3].getString("number"))
        assertEquals("idle", h.sent[4].getString("state"))
        assertFalse(h.sent[4].has("number"))
    }

    @Test fun identicalReportsAreNotRepeatedAndFailedOnesAreRetried() {
        val h = Harness(all)
        h.coreOk = false
        h.engine.onState(CallState.Idle)
        h.coreOk = true
        h.engine.onState(CallState.Idle)
        h.engine.onState(CallState.Idle)
        assertEquals(2, h.sent.size)
        h.perms = all.copy(answerPhoneCalls = false)
        h.engine.permissionsChanged()
        assertEquals(3, h.sent.size)
        assertEquals(false, h.sent[2].getBoolean("can_answer"))
    }

    @Test fun staleRequestsAreRefusedWithAResultNotIgnored() {
        val h = Harness(all)
        h.engine.onState(CallState.Idle)
        h.engine.onRequests(listOf(CallRequest(1, 2, "late", CallAction.Answer)))
        val result = h.sent.last()
        assertEquals("transport_calls_result", result.getString("action"))
        assertEquals("no_such_call", result.getString("failure"))
        assertTrue(h.telecom.log.isEmpty())
    }

    @Test fun contactLookupOnlyWithContactsPermissionAndFailuresAreSwallowed() {
        val h = Harness(all.copy(readContacts = false), names = mapOf("1" to "Ada"))
        h.engine.onState(CallState.Ringing, "1")
        assertFalse(h.sent.last().has("name"))
        val boom = CallControlEngine(
            core = { CoreOutcome.Ok(JSONObject()) },
            telecom = FakeTelecom(),
            permissions = { all },
            settings = { CallLinkSettings(true) },
            lookup = { throw IllegalStateException("provider died") },
            sdk = 34,
        )
        boom.onState(CallState.Ringing, "1") // must not throw
    }

    @Test fun resetForgetsTheStateSoTheNextConnectionGetsAFreshReport() {
        val h = Harness(all)
        h.engine.onState(CallState.Idle)
        h.engine.reset()
        h.engine.onRequests(listOf(CallRequest(1, 1, "r", CallAction.Hangup)))
        assertEquals("no_such_call", h.sent.last().getString("failure"))
        h.engine.onState(CallState.Idle)
        assertEquals(3, h.sent.size)
    }

    // ---- wizard

    @Test fun setupListsTheTwoOptionalCallStepsAfterPhoneState() {
        val inputs = SetupInputs(
            sdk = 34, pairedCount = 1, notifications = PermissionStatus.Granted, contacts = PermissionStatus.Granted,
            calendar = PermissionStatus.Granted, phoneState = PermissionStatus.Granted,
            battery = BatteryState.Unrestricted, oemAcknowledged = true, oem = Oem.Other,
        )
        val steps = SetupModel.steps(inputs)
        val ids = steps.map { it.id }
        assertEquals(ids.indexOf(StepId.PhoneState) + 1, ids.indexOf(StepId.AnswerCalls))
        assertEquals(ids.indexOf(StepId.AnswerCalls) + 1, ids.indexOf(StepId.CallerNumber))
        assertFalse(steps.first { it.id == StepId.AnswerCalls }.required)
        assertFalse(steps.first { it.id == StepId.CallerNumber }.required)
        assertEquals("android.permission.ANSWER_PHONE_CALLS", SetupModel.permissionFor(StepId.AnswerCalls))
        assertEquals("android.permission.READ_CALL_LOG", SetupModel.permissionFor(StepId.CallerNumber))
        // Skipping them never blocks the required steps.
        assertNull(SetupModel.firstOpen(steps.filter { it.required }))
    }
}
