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
        override fun placeCall(number: String) {
            log += "dial"
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

    @Test fun dialNeedsPermissionAnIdlePhoneAndAPlainNumber() {
        val t = FakeTelecom()
        val may = all.copy(callPhone = true)
        // Fictional number; FakeTelecom never places anything.
        assertNull(CallRules.handle(CallAction.Dial, CallState.Idle, 34, may, t, "+15550100"))
        assertEquals(listOf("dial"), t.log)
        assertEquals(CallFailure.PermissionMissing, CallRules.handle(CallAction.Dial, CallState.Idle, 34, all, t, "5550100"))
        assertEquals(CallFailure.NoSuchCall, CallRules.handle(CallAction.Dial, CallState.OffHook, 34, may, t, "5550100"))
        assertEquals(CallFailure.NoSuchCall, CallRules.handle(CallAction.Dial, CallState.Ringing, 34, may, t, "5550100"))
        assertEquals(CallFailure.NoSuchCall, CallRules.handle(CallAction.Dial, null, 34, may, t, "5550100"))
        for (bad in listOf(null, "", "*#06#", "555 0100", "+", "1".repeat(21), "tel:5")) {
            assertEquals("$bad", CallFailure.Failed, CallRules.handle(CallAction.Dial, CallState.Idle, 34, may, t, bad))
        }
        assertEquals(listOf("dial"), t.log) // refusals never reached the telecom service
        assertTrue(CallRules.report(CallState.Idle, null, null, 34, may).canDial)
        assertFalse(CallRules.report(CallState.Idle, null, null, 34, all).canDial)
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

    private class FakePrompts(var canAsk: Boolean = true) : DialPrompts {
        val announced = ArrayList<String>()
        val asked = ArrayList<Pair<String, String>>()
        val withdrawn = ArrayList<String>()
        override fun announce(text: String) { announced += text }
        override fun ask(requestId: String, text: String): Boolean { asked += requestId to text; return canAsk }
        override fun withdraw(requestId: String) { withdrawn += requestId }
    }

    private class Harness(var perms: CallPermissions, var enabled: Boolean = true, sdk: Int = 34, names: Map<String, String> = emptyMap(), var confirm: Boolean = false) {
        val prompts = FakePrompts()
        val sent = ArrayList<JSONObject>()
        var coreOk = true
        val telecom = FakeTelecom()
        var now = 1_000L
        val engine = CallControlEngine(
            core = { req ->
                sent += JSONObject(req)
                if (coreOk) CoreOutcome.Ok(JSONObject()) else CoreOutcome.Unavailable("down")
            },
            telecom = telecom,
            permissions = { perms },
            settings = { CallLinkSettings(enabled, confirm) },
            lookup = { names[it] },
            sdk = sdk,
            strings = TestStrings,
            clock = { now },
            onReport = { reports += it },
            prompts = prompts,
        )
        val reports = ArrayList<String>()
        fun actions() = sent.map { it.getString("action") }
        fun lastResult() = sent.last { it.getString("action") == "transport_calls_result" }
    }

    private val may = all.copy(callPhone = true)
    private fun dialReq(id: String = "d-1", computer: String? = "Desk") =
        CallRequest(1, 2, id, CallAction.Dial, "+15550100", computer)

    @Test fun aRequestedDialIsAnnouncedOnThePhoneWithNumberAndComputerThenPlaced() {
        val h = Harness(may)
        h.engine.onState(CallState.Idle)
        h.engine.onRequests(listOf(dialReq()))
        assertEquals(listOf("Dialing \u2066+15550100\u2069 requested by \u2068Desk\u2069"), h.prompts.announced)
        assertEquals(listOf("dial"), h.telecom.log)
        assertFalse(h.lastResult().has("failure"))
        assertTrue(h.prompts.asked.isEmpty())
    }

    @Test fun everyReportAttemptLeavesAReasonForTheDebugLog() {
        val h = Harness(may)
        h.engine.onState(CallState.Ringing)
        h.engine.onState(CallState.Ringing)
        h.coreOk = false
        h.engine.onState(CallState.Idle)
        h.enabled = false
        h.engine.onState(CallState.Ringing)
        assertEquals(listOf("ringing -> sent queued=-1", "unchanged", "idle -> core unavailable", "skipped_calls_off"), h.reports)
        assertTrue(h.reports.none { it.contains("+1") })
    }

    @Test fun aDialThatCannotBePlacedIsNotAnnounced() {
        val h = Harness(may)
        h.engine.onState(CallState.OffHook)
        h.engine.onRequests(listOf(dialReq()))
        assertTrue(h.prompts.announced.isEmpty())
        assertEquals("no_such_call", h.lastResult().getString("failure"))
    }

    @Test fun computerNamesAreMadeSafeToShow() {
        assertEquals("a paired computer", CallRules.cleanComputer(null, TestStrings))
        assertEquals("a paired computer", CallRules.cleanComputer("\u202e\n ", TestStrings))
        assertEquals("Desk PC", CallRules.cleanComputer("De\u202esk\n\u200bPC", TestStrings).replace("  ", " ").replace("De sk", "Desk"))
        assertTrue(CallRules.cleanComputer("x".repeat(500), TestStrings).length <= 40)
        assertEquals("Dialing \u20665550100\u2069 requested by \u2068a paired computer\u2069", CallRules.dialNotice("5550100", "", TestStrings))
    }

    @Test fun withConfirmationOnNothingIsDialedUntilTheOwnerAllows() {
        val h = Harness(may, confirm = true)
        h.engine.onState(CallState.Idle)
        val before = h.sent.size
        h.engine.onRequests(listOf(dialReq("d-7")))
        assertEquals(listOf("d-7" to "Dialing \u2066+15550100\u2069 requested by \u2068Desk\u2069"), h.prompts.asked)
        assertTrue(h.telecom.log.isEmpty())
        assertEquals(before, h.sent.size) // no answer yet
        h.engine.resolveDial("d-7", allow = true)
        assertEquals(listOf("dial"), h.telecom.log)
        assertEquals(listOf("d-7"), h.prompts.withdrawn)
        assertFalse(h.lastResult().has("failure"))
        // An answer is used once.
        h.engine.resolveDial("d-7", allow = true)
        assertEquals(listOf("dial"), h.telecom.log)
    }

    @Test fun denyingExpiryAndAnUnaskableQuestionAllRefuseAsDeclined() {
        val h = Harness(may, confirm = true)
        h.engine.onState(CallState.Idle)
        h.engine.onRequests(listOf(dialReq("a")))
        h.engine.resolveDial("a", allow = false)
        assertEquals("declined", h.lastResult().getString("failure"))
        h.engine.onRequests(listOf(dialReq("b")))
        h.now += 46_000
        h.engine.resolveDial("b", allow = true) // too late
        assertEquals("declined", h.lastResult().getString("failure"))
        assertEquals("b", h.lastResult().getString("request_id"))
        h.prompts.canAsk = false // e.g. notifications are blocked
        h.engine.onRequests(listOf(dialReq("c")))
        assertEquals("declined", h.lastResult().getString("failure"))
        assertTrue(h.telecom.log.isEmpty())
    }

    @Test fun aConfirmedDialIsStillCheckedAgainstThePhoneAsItIsNow() {
        val h = Harness(may, confirm = true)
        h.engine.onState(CallState.Idle)
        h.engine.onRequests(listOf(dialReq("a")))
        h.engine.onState(CallState.Ringing) // a call came in while the question was open
        h.engine.resolveDial("a", allow = true)
        assertTrue(h.telecom.log.isEmpty())
        assertEquals("no_such_call", h.lastResult().getString("failure"))
        assertTrue(h.prompts.announced.isEmpty())
    }

    @Test fun onlyAFewQuestionsCanBeOpenAndResetWithdrawsThem() {
        val h = Harness(may, confirm = true)
        h.engine.onState(CallState.Idle)
        for (i in 1..5) h.engine.onRequests(listOf(dialReq("q$i")))
        assertEquals(3, h.prompts.asked.size)
        assertEquals("declined", h.lastResult().getString("failure"))
        h.engine.reset()
        assertEquals(listOf("q1", "q2", "q3"), h.prompts.withdrawn)
    }

    @Test fun refusalWordsAndWaitParsingCarryTheComputerName() {
        assertEquals("rate_limited", JSONObject(CallsProtocol.resultRequest(7, "r-1", CallFailure.RateLimited)).getString("failure"))
        assertEquals("declined", JSONObject(CallsProtocol.resultRequest(7, "r-1", CallFailure.Declined)).getString("failure"))
        val ev = JSONObject().put("kind", "request").put("seq", 1).put("connection_id", 2).put("request_id", "d1")
            .put("action", "dial").put("number", "+15550100").put("computer", "Desk")
        val w = CallsProtocol.parseWait(JSONObject().put("events", JSONArray().put(ev)).put("last_seq", 1).put("enabled", true))!!
        assertEquals("Desk", w.requests[0].computer)
        assertEquals("CallRequest(1, dial)", w.requests[0].toString())
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

    @Test fun aNumberThatArrivesBeforeTheRingingStateIsNotLost() {
        val h = Harness(all)
        h.engine.onNumber("+15550100") // the broadcast beat the telephony callback
        h.now += 500
        h.engine.onState(CallState.Ringing)
        assertEquals("+15550100", h.sent.last().getString("number"))
    }

    @Test fun anOldEarlyNumberIsNotAttachedToALaterCall() {
        val h = Harness(all)
        h.engine.onNumber("+15550100")
        h.now += 10_000
        h.engine.onState(CallState.Ringing)
        assertFalse(h.sent.last().has("number"))
    }

    @Test fun aSecondCallNeverShowsTheFirstCallersNumber() {
        val h = Harness(all)
        h.engine.onState(CallState.Ringing, "+15550100")
        h.engine.onState(CallState.OffHook)
        h.engine.onState(CallState.Ringing) // call waiting, number not (yet) known
        assertEquals("ringing", h.sent.last().getString("state"))
        assertFalse(h.sent.last().has("number"))
        h.engine.onNumber("+15550199")
        assertEquals("+15550199", h.sent.last().getString("number"))
    }

    @Test fun afterAWaitingCallEndsTheLineIsShownWithoutAGuessedParty() {
        val h = Harness(all)
        h.engine.onState(CallState.Ringing, "+15550100")
        h.engine.onState(CallState.OffHook)
        h.engine.onState(CallState.Ringing, "+15550199")
        h.engine.onState(CallState.OffHook) // declined or answered: not knowable
        assertEquals("offhook", h.sent.last().getString("state"))
        assertFalse(h.sent.last().has("number"))
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
            strings = TestStrings,
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
        val steps = SetupModel.steps(TestStrings, inputs)
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

    // ---- real-device robustness (call chain review)

    @Test fun everyPermissionCombinationReportsOnlyWhatItAllows() {
        for (mask in 0 until 32) {
            val p = CallPermissions(
                readPhoneState = mask and 1 != 0, answerPhoneCalls = mask and 2 != 0, readCallLog = mask and 4 != 0,
                readContacts = mask and 8 != 0, callPhone = mask and 16 != 0,
            )
            val r = CallRules.report(CallState.Ringing, "+15550100", "Ada", 34, p)
            assertEquals("mask $mask number", if (p.readCallLog) "+15550100" else null, r.number)
            assertEquals("mask $mask name", if (p.readCallLog && p.readContacts) "Ada" else null, r.name)
            assertEquals("mask $mask answer", p.answerPhoneCalls && p.readPhoneState, r.canAnswer)
            assertEquals("mask $mask end", p.answerPhoneCalls && p.readPhoneState, r.canEnd)
            assertEquals("mask $mask dial", p.callPhone && p.readPhoneState, r.canDial)
            // Idle never carries a party, whatever is allowed.
            val idle = CallRules.report(CallState.Idle, "+15550100", "Ada", 34, p)
            assertNull(idle.number); assertNull(idle.name)
        }
    }

    @Test fun dialIsNotOfferedWithoutTheCallStateItNeedsToCheckTheLineIsFree() {
        val p = all.copy(readPhoneState = false, callPhone = true)
        assertFalse(CallRules.canDial(p))
        assertEquals(CallFailure.PermissionMissing, CallRules.dialPreflight(CallState.Idle, p, "+15550100"))
    }

    @Test fun aCallPlacedOnThePhoneItselfIsReportedAsALiveCallThatCanBeEnded() {
        val h = Harness(all, names = mapOf("+15550100" to "Ada"))
        h.engine.onState(CallState.Idle)
        h.engine.onState(CallState.OffHook) // no ringing before it: dialed on the phone
        assertEquals("offhook", h.sent.last().getString("state"))
        assertFalse(h.sent.last().has("number"))
        assertTrue(h.sent.last().getBoolean("can_end"))
        h.engine.onRequests(listOf(CallRequest(1, 1, "h-1", CallAction.Hangup)))
        assertEquals(listOf("end"), h.telecom.log)
        h.engine.onState(CallState.Idle) // the other side hung up, or ours did
        assertEquals("idle", h.sent.last { it.getString("action") == "transport_calls_report" }.getString("state"))
    }

    @Test fun aMissedCallEndsAsIdleAndANumberArrivingLateIsNotKeptForever() {
        val h = Harness(all)
        h.engine.onState(CallState.Ringing)
        h.engine.onState(CallState.Idle)
        h.engine.onNumber("+15550100") // the number broadcast lost the race with the state
        h.now += 10_000
        h.engine.onState(CallState.Ringing)
        assertFalse(h.sent.last().has("number"))
    }

    @Test fun decliningAWaitingCallLeavesTheLiveCallReportedWithoutAGuessedParty() {
        val h = Harness(all)
        h.engine.onState(CallState.Ringing)
        h.engine.onNumber("+15550100")
        h.engine.onState(CallState.OffHook)
        h.engine.onState(CallState.Ringing) // call waiting
        h.engine.onRequests(listOf(CallRequest(1, 1, "x-1", CallAction.Reject)))
        assertEquals(listOf("end"), h.telecom.log)
        h.engine.onState(CallState.OffHook) // the first call continues
        assertEquals("offhook", h.sent.last { it.getString("action") == "transport_calls_report" }.getString("state"))
        h.engine.onState(CallState.Idle)
        assertEquals("idle", h.sent.last { it.getString("action") == "transport_calls_report" }.getString("state"))
    }

    @Test fun aRestartedProcessReportsTheCurrentStateOnceTheCoreIsUp() {
        val h = Harness(all)
        h.coreOk = false // the core is not running yet after the restart
        h.engine.onState(CallState.OffHook)
        h.coreOk = true
        h.engine.permissionsChanged() // nothing was accepted before, so this sends the live state
        assertEquals("offhook", h.sent.last().getString("state"))
    }

    @Test fun permissionChangesAreNoticedOnceAgainstTheFirstLookAsBaseline() {
        val w = PermissionWatch()
        assertFalse(w.changed(all)) // baseline
        assertFalse(w.changed(all))
        assertTrue(w.changed(all.copy(readCallLog = false)))
        assertFalse(w.changed(all.copy(readCallLog = false)))
        w.reset()
        assertFalse(w.changed(all)) // after a reset the next look is a baseline again
        assertTrue(w.changed(none))
    }

    @Test fun promptsAreOnlyAskedWhereTheyCanBeSeen() {
        assertTrue(DialPromptRules.canShow(appEnabled = true, channelImportance = 4))
        assertFalse(DialPromptRules.canShow(appEnabled = false, channelImportance = 4)) // POST_NOTIFICATIONS denied (Android 13+)
        assertFalse(DialPromptRules.canShow(appEnabled = true, channelImportance = 0)) // channel switched off
        assertTrue(DialPromptRules.canShow(appEnabled = true, channelImportance = null)) // no channel info: try
    }

    @Test fun theOutgoingAccountIsOnlyChosenWhenTheUserHasNoDefaultOne() {
        val a = "simA"; val b = "simB"
        // The user's own "always use this SIM" choice is never overridden.
        assertNull(DialAccounts.choose(hasDefault = true, accounts = listOf(a to 1, b to 2), defaultVoiceSub = 2))
        // "Ask every time": the default voice subscription, else the only one, else leave the choice to the system.
        assertEquals(b, DialAccounts.choose(false, listOf(a to 1, b to 2), 2))
        assertEquals(a, DialAccounts.choose(false, listOf(a to 1), null))
        assertNull(DialAccounts.choose(false, listOf(a to 1, b to 2), null))
        assertNull(DialAccounts.choose(false, listOf(a to 1, b to 2), 9))
        assertNull(DialAccounts.choose(false, emptyList<Pair<String, Int?>>(), 1))
    }
}
