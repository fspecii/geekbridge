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

private const val CERT_A = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"
private const val CERT_B = "99887766554433221100ffeeddccbbaa99887766554433221100ffeeddccbbaa"
private const val ID_PEER = "_peer_device_id_0123456789abcdef_"

private fun prompt(code: String = "1A2B3C4D", cert: String = CERT_B) = JSONObject()
    .put("request_id", 7).put("connection_id", 3).put("direction", "incoming")
    .put("peer_device_id", ID_PEER).put("peer_name", "Desk").put("peer_cert_sha256", cert)
    .put("own_cert_sha256", CERT_A).put("verification_code", code).put("expires_in_ms", 24000)

private fun running(prompts: JSONArray = JSONArray().put(prompt()), connectionTrusted: Boolean = false) = JSONObject()
    .put("state", "running")
    .put("local", JSONObject().put("device_id", "_local_device_id_0123456789abcdef").put("device_name", "Pixel").put("cert_sha256", CERT_A))
    .put("listen", JSONObject().put("host", "0.0.0.0").put("port", 1716))
    .put("discovery", JSONObject().put("listening", false).put("port", JSONObject.NULL).put("announcing", false).put("error", JSONObject.NULL))
    .put("discovered", JSONArray())
    .put(
        "connections",
        JSONArray().put(
            JSONObject().put("connection_id", 3).put("role", "acceptor").put("phase", if (connectionTrusted) "trusted" else "pairing_await")
                .put("trusted", connectionTrusted).put("pings_received", 2)
                .put(
                    "peer",
                    JSONObject().put("device_id", ID_PEER).put("name", "Desk").put("device_type", "desktop").put("cert_sha256", CERT_B).put("ip", "192.168.1.5"),
                ),
        ),
    )
    .put("prompts", prompts)
    .put("attempts", JSONArray())
    .put("paired", JSONArray())
    .put("paired_error", false)
    .put("events", JSONArray().put(JSONObject().put("seq", 5).put("kind", "pairing_prompt").put("connection_id", 3)))
    .put("last_event_seq", 5)
    .put("counters", JSONObject())

private fun ok(result: JSONObject) = CoreOutcome.Ok(result)

class TransportProtocolTest {
    private val opts = StartOptions("/data/user/0/x/files", "Pixel", true, null, false, false, false)

    @Test fun startRequestDefaultsOmitOptionalObjects() {
        val j = JSONObject(TransportProtocol.startRequest(opts))
        assertEquals("transport_start", j.getString("action"))
        assertEquals(1, j.getInt("version"))
        assertFalse(j.has("listen")); assertFalse(j.has("discovery")); assertFalse(j.has("allow_loopback"))
    }

    @Test fun startRequestOutboundOnlyAndDiscovery() {
        val j = JSONObject(TransportProtocol.startRequest(opts.copy(listen = false, discoveryListen = true, allowLoopback = true)))
        assertFalse(j.getJSONObject("listen").getBoolean("enabled"))
        assertFalse(j.getJSONObject("listen").has("port"))
        assertTrue(j.getJSONObject("discovery").getBoolean("listen"))
        assertFalse(j.getJSONObject("discovery").getBoolean("announce"))
        assertTrue(j.getBoolean("allow_loopback"))
    }

    @Test fun startRequestWithPort() {
        assertEquals(40000, JSONObject(TransportProtocol.startRequest(opts.copy(listenPort = 40000))).getJSONObject("listen").getInt("port"))
    }

    @Test fun statusSinceOnlyWhenPositive() {
        assertFalse(JSONObject(TransportProtocol.statusRequest(0)).has("since"))
        assertEquals(12, JSONObject(TransportProtocol.statusRequest(12)).getLong("since"))
    }

    @Test fun approvalSendsExactlyTheDisplayedCodeAndCertificate() {
        val p = TransportProtocol.parseSnapshot(running())!!.prompts.single()
        val j = JSONObject(TransportProtocol.decisionRequest(p, approve = true))
        assertEquals("transport_pair_decision", j.getString("action"))
        assertEquals(7, j.getLong("request_id")); assertEquals(3, j.getLong("connection_id"))
        assertTrue(j.getBoolean("approve"))
        assertEquals("1A2B3C4D", j.getString("verification_code"))
        assertEquals(CERT_B, j.getString("peer_cert_sha256"))
    }

    @Test fun declineSendsOnlyIds() {
        val p = TransportProtocol.parseSnapshot(running())!!.prompts.single()
        val j = JSONObject(TransportProtocol.decisionRequest(p, approve = false))
        assertFalse(j.getBoolean("approve"))
        assertFalse(j.has("verification_code")); assertFalse(j.has("peer_cert_sha256"))
    }

    @Test fun pairAndOtherRequests() {
        assertEquals("await", JSONObject(TransportProtocol.pairRequest(3, true)).getString("mode"))
        assertFalse(JSONObject(TransportProtocol.pairRequest(3, false)).has("mode"))
        val c = JSONObject(TransportProtocol.connectRequest("192.168.1.5", 1716, ID_PEER))
        assertEquals("192.168.1.5", c.getString("host")); assertEquals(1716, c.getInt("port")); assertEquals(ID_PEER, c.getString("device_id"))
        assertEquals("transport_ping", JSONObject(TransportProtocol.pingRequest(3)).getString("action"))
        val screen = JSONObject(TransportProtocol.screenRequest(false))
        assertEquals("transport_screen", screen.getString("action"))
        assertEquals(false, screen.getBoolean("on"))
        assertEquals("transport_heartbeat_now", JSONObject(TransportProtocol.heartbeatNowRequest()).getString("action"))
        val bind = JSONObject(TransportProtocol.wifiBindRequest("192.168.1.20"))
        assertEquals("transport_wifi_bind", bind.getString("action"))
        assertEquals("192.168.1.20", bind.getString("ip"))
        assertFalse(JSONObject(TransportProtocol.wifiBindRequest(null)).has("ip")) // absent clears it
        assertEquals("transport_disconnect", JSONObject(TransportProtocol.disconnectRequest(3)).getString("action"))
        assertEquals("transport_unpair", JSONObject(TransportProtocol.unpairRequest(ID_PEER)).getString("action"))
        val wait = JSONObject(TransportProtocol.waitRequest(7, Long.MAX_VALUE))
        assertEquals("transport_wait", wait.getString("action"))
        assertEquals(7L, wait.getLong("since"))
        assertEquals(TransportProtocol.MAX_WAIT_MS, wait.getLong("timeout_ms"))
        assertEquals("transport_stop", JSONObject(TransportProtocol.stopRequest()).getString("action"))
    }
}

class TransportParseTest {
    @Test fun stoppedHasNoDetails() {
        val s = TransportProtocol.parseSnapshot(JSONObject().put("state", "stopped"))!!
        assertFalse(s.isRunning); assertNull(s.local); assertTrue(s.connections.isEmpty())
    }

    @Test fun runningSnapshotParses() {
        val s = TransportProtocol.parseSnapshot(running())!!
        assertTrue(s.isRunning)
        assertEquals("Pixel", s.local!!.deviceName)
        assertEquals(1716, s.listen!!.port)
        assertEquals(1, s.prompts.size); assertEquals("1A2B3C4D", s.prompts[0].code)
        assertFalse(s.connections.single().trusted)
        assertEquals(5, s.lastEventSeq); assertEquals("pairing_prompt", s.events.single().kind)
    }

    @Test fun trustedOnlyWhenCoreSaysSo() {
        assertTrue(TransportProtocol.parseSnapshot(running(connectionTrusted = true))!!.connections.single().trusted)
        assertFalse(TransportProtocol.parseSnapshot(running(connectionTrusted = false))!!.connections.single().trusted)
        // A snapshot without an explicit boolean is not trusted.
        val j = running()
        j.getJSONArray("connections").getJSONObject(0).remove("trusted")
        assertFalse(TransportProtocol.parseSnapshot(j)!!.connections.single().trusted)
    }

    @Test fun promptWithUnsafeCodeOrCertIsNeverApprovable() {
        val prompts = JSONArray().put(prompt(code = "12 34\n56")).put(prompt(cert = "not a fingerprint!")).put(prompt())
        val s = TransportProtocol.parseSnapshot(running(prompts))!!
        assertEquals(1, s.prompts.size)
        assertEquals(2, s.undisplayablePrompts)
    }

    @Test fun notASnapshot() {
        assertNull(TransportProtocol.parseSnapshot(null))
        assertNull(TransportProtocol.parseSnapshot("running"))
        assertNull(TransportProtocol.parseSnapshot(JSONObject().put("state", "Run ning")))
        assertNull(TransportProtocol.parseSnapshot(JSONObject().put("state", "running"))) // no local identity
    }

    @Test fun listsAreBoundedAndNamesClipped() {
        val j = running()
        val many = JSONArray()
        for (i in 0 until 200) many.put(JSONObject().put("device_id", "id_$i" + "x".repeat(30)).put("name", "n".repeat(500)).put("device_type", "desktop").put("ip", "10.0.0.1").put("tcp_port", 1716).put("paired", false))
        j.put("discovered", many)
        val s = TransportProtocol.parseSnapshot(j)!!
        assertEquals(TransportProtocol.MAX_LIST, s.discovered.size)
        assertTrue(s.discovered[0].name.length <= 48)
    }

    @Test fun controlCharactersInNamesBecomeSpaces() {
        val j = running()
        j.getJSONArray("connections").getJSONObject(0).getJSONObject("peer").put("name", "Evil\u0007\nName")
        assertEquals("Evil Name", TransportProtocol.parseSnapshot(j)!!.connections.single().peer.name)
    }
}

class TransportWaitTest {
    @Test fun aWaitResultGivesTheNewestSequenceUntilTheTransportStops() {
        assertEquals(9L, TransportProtocol.parseWait(JSONObject("""{"last_event_seq":9,"stopped":false}""")))
        assertEquals(null, TransportProtocol.parseWait(JSONObject("""{"last_event_seq":9,"stopped":true}""")))
        assertEquals(null, TransportProtocol.parseWait(JSONObject("""{"stopped":false}""")))
        assertEquals(null, TransportProtocol.parseWait("nope"))
    }
}

class TransportTextTest {
    @Test fun securityCodesAreFlagged() {
        assertTrue(TransportText.isSecurity("pin_mismatch"))
        assertTrue(TransportText.isSecurity("identity_mismatch"))
        assertFalse(TransportText.isSecurity("timeout"))
        val e = LinkEvent(1, "connect_failed", null, null, "pin_mismatch")
        assertEquals(Tone.Bad, TransportText.eventTone(e))
        assertTrue(TransportText.event(TestStrings, e).startsWith("SECURITY"))
    }

    @Test fun unknownKindsAndCodesStayGeneric() {
        assertEquals("Event (weird_kind).", TransportText.event(TestStrings, LinkEvent(1, "weird_kind", null, null, null)))
        assertTrue(TransportText.error(TestStrings, "something_new").contains("something_new"))
    }

    @Test fun pairedEventDoesNotClaimMoreThanTheCore() {
        assertTrue(TransportText.event(TestStrings, LinkEvent(1, "paired", 3, null, null)).contains("certificate"))
    }

    @Test fun fingerprintGrouping() {
        assertEquals("aabb ccdd eeff", TransportText.group("aabbccddeeff"))
        assertEquals("aa:bb:cc:dd:ee:ff", TransportText.group("aa:bb:cc:dd:ee:ff"))
    }
}

class TransportControllerTest {
    /** Fake native: records requests and answers from [answers] by action. */
    private class Fake {
        val requests = ArrayList<JSONObject>()
        val answers = HashMap<String, () -> CoreOutcome>()
        fun call(raw: String): CoreOutcome {
            val j = JSONObject(raw)
            requests += j
            return (answers[j.getString("action")] ?: { CoreOutcome.Ok(JSONObject()) })()
        }
        fun actions() = requests.map { it.getString("action") }
    }

    private val opts = StartOptions("/f", "Pixel", true, null, false, false, false)
    private fun controller(f: Fake) = TransportController(f::call, { it.run() }, { it.run() }, TestStrings)

    @Test fun nothingIsCalledUntilStart() {
        val f = Fake(); val c = controller(f)
        c.poll(); c.connect("10.0.0.1", 1, "x"); c.ping(1); c.disconnect(1)
        assertTrue(f.requests.isEmpty()); assertEquals(LinkPhase.Stopped, c.phase)
    }

    @Test fun startPollsThenStopStops() {
        val f = Fake(); f.answers["transport_status"] = { ok(running()) }
        val c = controller(f)
        c.start(opts)
        assertEquals(LinkPhase.Running, c.phase)
        assertEquals(listOf("transport_start", "transport_status"), f.actions())
        assertNotNull(c.snapshot)
        c.stop()
        assertEquals(LinkPhase.Stopped, c.phase); assertNull(c.snapshot)
        assertEquals("transport_stop", f.actions().last())
        c.stop() // idempotent
        assertEquals(1, f.actions().count { it == "transport_stop" })
    }

    @Test fun eventsTheCoreReportsAreFetchedOnceAndAPeerUnpairIsSaid() {
        val f = Fake()
        var status = running()
        f.answers["transport_status"] = { ok(status) }
        val c = controller(f); c.start(opts)
        val before = f.actions().count { it == "transport_status" }
        c.eventsUpTo(5) // nothing newer than what the controller already saw
        assertEquals(before, f.actions().count { it == "transport_status" })
        status = running()
            .put("last_event_seq", 6)
            .put("events", JSONArray().put(JSONObject().put("seq", 6).put("kind", "peer_unpaired")))
        c.eventsUpTo(6)
        assertEquals(before + 1, f.actions().count { it == "transport_status" })
        assertTrue(c.notice!!.text.contains("removed the pairing"))
        c.eventsUpTo(6) // already seen
        assertEquals(before + 1, f.actions().count { it == "transport_status" })
    }

    @Test fun aClosedLinkEventCarriesHowLongTheComputerWasSilent() {
        val ev = { q: Any? ->
            val e = JSONObject().put("seq", 9).put("kind", "link_closed").put("connection_id", 3).put("code", "ping_timeout")
            if (q != null) e.put("quiet_ms", q)
            TransportProtocol.parseSnapshot(running().put("events", JSONArray().put(e)).put("last_event_seq", 9))!!.events.single()
        }
        assertEquals(74_000L, ev(74_000).quietMs)
        assertNull(ev(null).quietMs)
        assertNull(ev(-5).quietMs)
        assertNull(ev("soon").quietMs)
    }

    @Test fun failedStartReturnsToStoppedWithAnExplanation() {
        val f = Fake(); f.answers["transport_start"] = { CoreOutcome.Rejected("listen_failed", "x") }
        val c = controller(f); c.start(opts)
        assertEquals(LinkPhase.Stopped, c.phase)
        assertTrue(c.notice!!.text.contains("listen"))
    }

    @Test fun unavailableCoreIsReportedNotHidden() {
        val f = Fake(); f.answers["transport_start"] = { CoreOutcome.Unavailable("library could not be loaded") }
        val c = controller(f); c.start(opts)
        assertEquals(LinkPhase.Stopped, c.phase); assertEquals(Tone.Bad, c.notice!!.tone)
    }

    @Test fun approvalIsNotPairedUntilTheCoreSaysTrusted() {
        val f = Fake()
        f.answers["transport_status"] = { ok(running()) }
        f.answers["transport_pair_decision"] = { ok(JSONObject().put("decision", "approved")) }
        val c = controller(f); c.start(opts)
        val p = c.pendingPrompt()!!
        c.decide(p, true)
        assertTrue(c.notice!!.text.contains("not paired until"))
        assertTrue(c.snapshot!!.connections.none { it.trusted })
        // The prompt that was answered is not offered again even if the core still lists it.
        assertNull(c.pendingPrompt())
        val sent = f.requests.first { it.getString("action") == "transport_pair_decision" }
        assertEquals("1A2B3C4D", sent.getString("verification_code")); assertEquals(CERT_B, sent.getString("peer_cert_sha256"))
    }

    @Test fun staleOrExpiredDecisionExplainsNothingWasApproved() {
        val f = Fake()
        f.answers["transport_status"] = { ok(running()) }
        f.answers["transport_pair_decision"] = { CoreOutcome.Rejected("request_expired", "x") }
        val c = controller(f); c.start(opts); c.decide(c.pendingPrompt()!!, true)
        assertTrue(c.notice!!.text.contains("Nothing was approved"))
    }

    @Test fun decliningSendsNoCodeOrCertificate() {
        val f = Fake(); f.answers["transport_status"] = { ok(running()) }
        val c = controller(f); c.start(opts); c.decide(c.pendingPrompt()!!, false)
        val sent = f.requests.first { it.getString("action") == "transport_pair_decision" }
        assertFalse(sent.getBoolean("approve")); assertFalse(sent.has("verification_code"))
    }

    @Test fun securityEventsBecomeAPersistentWarning() {
        val f = Fake()
        val j = running(JSONArray())
        j.put("events", JSONArray().put(JSONObject().put("seq", 9).put("kind", "connect_failed").put("code", "pin_mismatch")))
        j.put("last_event_seq", 9)
        f.answers["transport_status"] = { ok(j) }
        val c = controller(f); c.start(opts)
        assertEquals(Tone.Bad, c.notice!!.tone); assertTrue(c.notice!!.text.startsWith("SECURITY"))
    }

    @Test fun eventsAreNotRepeatedAndAreBounded() {
        val f = Fake(); var n = 0
        f.answers["transport_status"] = {
            val j = running(JSONArray()); val ev = JSONArray()
            for (i in 1..30) ev.put(JSONObject().put("seq", n + i).put("kind", "link_closed"))
            j.put("events", ev); j.put("last_event_seq", n + 30); n += 30; ok(j)
        }
        val c = controller(f); c.start(opts); c.poll()
        assertEquals(TransportController.MAX_EVENTS, c.events.size)
        assertEquals(c.events.map { it.seq }.distinct().size, c.events.size)
    }

    @Test fun aSnapshotSayingStoppedStopsTheUi() {
        val f = Fake(); f.answers["transport_status"] = { ok(JSONObject().put("state", "stopped")) }
        val c = controller(f); c.start(opts)
        assertEquals(LinkPhase.Stopped, c.phase)
    }

    @Test fun lateStatusAfterStopIsIgnored() {
        val work = ArrayDeque<Runnable>(); val main = ArrayDeque<Runnable>()
        fun drain(q: ArrayDeque<Runnable>) { while (q.isNotEmpty()) q.removeFirst().run() }
        val f = Fake(); f.answers["transport_status"] = { ok(running()) }
        val c = TransportController(f::call, { work.addLast(it) }, { main.addLast(it) }, TestStrings)
        c.start(opts)
        drain(work); drain(main)                    // start finished -> Running, a status poll is queued
        assertEquals(LinkPhase.Running, c.phase)
        drain(work)                                  // the status call has run, its reply is still in flight
        c.stop()                                     // user stops before that reply is delivered
        drain(main)                                  // late status reply arrives now
        assertNull(c.snapshot)                       // ...and must not bring the snapshot back
        assertEquals(LinkPhase.Stopping, c.phase)
        drain(work); drain(main)
        assertEquals(LinkPhase.Stopped, c.phase); assertNull(c.snapshot)
    }

    @Test fun commandsAreSentOnlyWhileRunning() {
        val f = Fake(); f.answers["transport_status"] = { ok(running()) }
        val c = controller(f); c.start(opts)
        c.connect(" 192.168.1.5 ", 1716, " $ID_PEER ")
        val r = f.requests.first { it.getString("action") == "transport_connect" }
        assertEquals("192.168.1.5", r.getString("host")); assertEquals(ID_PEER, r.getString("device_id"))
        c.stop(); val before = f.requests.size
        c.ping(3)
        assertEquals(before, f.requests.size)
    }
}
