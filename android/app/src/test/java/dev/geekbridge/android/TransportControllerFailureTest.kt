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

/**
 * Failure and abuse paths of the link state machine: QR pairing answers, workers that throw, a core that answers
 * with garbage, a link that must stay up for a share, and prompts that were already answered. The invariant is that
 * the UI always ends up in a state the user can read, and a stale reply never changes it.
 */
class TransportControllerFailureTest {
    private val cert = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"
    private val peer = "_peer_device_id_0123456789abcdef_"
    private val opts = StartOptions("/f", "Pixel", true, null, false, false, false)

    private class Fake {
        val requests = ArrayList<JSONObject>()
        val answers = HashMap<String, () -> CoreOutcome>()
        fun call(raw: String): CoreOutcome {
            val j = JSONObject(raw)
            requests += j
            return (answers[j.getString("action")] ?: { CoreOutcome.Ok(JSONObject()) })()
        }
        fun count(action: String) = requests.count { it.getString("action") == action }
    }

    private fun running(prompts: JSONArray = JSONArray()) = JSONObject()
        .put("state", "running")
        .put("local", JSONObject().put("device_id", "_local_device_id_0123456789abcdef").put("device_name", "Pixel").put("cert_sha256", cert))
        .put("prompts", prompts)

    private fun prompt(id: Long) = JSONObject()
        .put("request_id", id).put("connection_id", 3).put("direction", "incoming").put("peer_device_id", peer)
        .put("peer_name", "Desk").put("peer_cert_sha256", cert).put("own_cert_sha256", cert)
        .put("verification_code", "1A2B3C4D").put("expires_in_ms", 20000)

    private fun started(f: Fake): TransportController {
        f.answers.putIfAbsent("transport_status") { CoreOutcome.Ok(running()) }
        val c = TransportController(f::call, { it.run() }, { it.run() }, TestStrings)
        c.start(opts)
        assertEquals(LinkPhase.Running, c.phase)
        return c
    }

    @Test fun `qr pairing reports success and polls once more`() {
        val f = Fake()
        val c = started(f)
        val polls = f.count("transport_status")
        var result: String? = "unset"
        c.qrPairRequest(3, "tok", cert) { result = it }
        assertNull(result)
        assertEquals(1, f.count("transport_pair_request"))
        assertEquals(polls + 1, f.count("transport_status"))
        assertEquals("tok", f.requests.last { it.getString("action") == "transport_pair_request" }.getString("qr_token"))
    }

    @Test fun `qr pairing carries the core's refusal code, and an unreadable reply is reported as unavailable`() {
        val f = Fake()
        val c = started(f)
        f.answers["transport_pair_request"] = { CoreOutcome.Rejected("certificate_mismatch", "x") }
        var result: String? = null
        c.qrPairRequest(3, "tok", cert) { result = it }
        assertEquals("certificate_mismatch", result)
        assertTrue("a pin mismatch is never shown as success", c.notice!!.tone == Tone.Warn)
        f.answers["transport_pair_request"] = { CoreOutcome.Malformed("not json") }
        c.qrPairRequest(3, "tok", cert) { result = it }
        assertEquals("unavailable", result)
        assertEquals(Tone.Bad, c.notice!!.tone)
    }

    @Test fun `qr pairing on a stopped link never reaches the core`() {
        val f = Fake()
        val c = TransportController(f::call, { it.run() }, { it.run() }, TestStrings)
        var result: String? = null
        c.qrPairRequest(3, "tok", cert) { result = it }
        assertEquals("not_running", result)
        assertTrue(f.requests.isEmpty())
    }

    @Test fun `a worker that throws becomes an unavailable notice, not a crash`() {
        val f = Fake()
        f.answers["transport_start"] = { throw IllegalStateException("boom") }
        val c = TransportController(f::call, { it.run() }, { it.run() }, TestStrings)
        c.start(opts)
        assertEquals(LinkPhase.Stopped, c.phase)
        assertEquals(Tone.Bad, c.notice!!.tone)
        assertTrue(c.notice!!.text.contains("IllegalStateException"))
    }

    @Test fun `an unreadable status keeps the previous snapshot and says so`() {
        val f = Fake()
        val c = started(f)
        val before = c.snapshot
        assertNotNull(before)
        f.answers["transport_status"] = { CoreOutcome.Ok(JSONObject().put("state", "run ning")) }
        c.poll()
        assertEquals(before, c.snapshot)
        assertEquals(Tone.Warn, c.notice!!.tone)
        f.answers["transport_status"] = { CoreOutcome.Unavailable("native library missing") }
        c.poll()
        assertEquals(Tone.Bad, c.notice!!.tone)
        assertEquals(LinkPhase.Running, c.phase)
    }

    @Test fun `leaving the foreground does not cut a running share but an explicit stop does`() {
        val f = Fake()
        val c = started(f)
        c.hold()
        c.stop("left the app")
        assertEquals(LinkPhase.Running, c.phase)
        assertEquals(0, f.count("transport_stop"))
        c.release()
        c.release() // releasing more than was held never goes negative
        assertFalse(c.needsLink())
        c.hold()
        c.stop()
        assertEquals(LinkPhase.Stopped, c.phase)
        assertEquals(1, f.count("transport_stop"))
    }

    @Test fun `a failed stop is reported and the link still ends up stopped`() {
        val f = Fake()
        val c = started(f)
        f.answers["transport_stop"] = { CoreOutcome.Rejected("busy", "x") }
        c.stop()
        assertEquals(LinkPhase.Stopped, c.phase)
        assertEquals(Tone.Warn, c.notice!!.tone)
    }

    @Test fun `an answered prompt is not offered again but a new one is`() {
        val f = Fake()
        f.answers["transport_status"] = { CoreOutcome.Ok(running(JSONArray().put(prompt(1)).put(prompt(2)))) }
        val c = started(f)
        val first = c.pendingPrompt()!!
        assertEquals(1L, first.requestId)
        c.decide(first, approve = false)
        assertEquals("the declined request must not come back while the core still lists it", 2L, c.pendingPrompt()!!.requestId)
        assertEquals(1, f.count("transport_pair_decision"))
        // Once the core stops listing request 1 the bookkeeping is pruned; a reused id is a new request.
        f.answers["transport_status"] = { CoreOutcome.Ok(running(JSONArray().put(prompt(2)))) }
        c.poll()
        f.answers["transport_status"] = { CoreOutcome.Ok(running(JSONArray().put(prompt(1)))) }
        c.poll()
        assertEquals(1L, c.pendingPrompt()!!.requestId)
    }

    @Test fun `the core reporting itself stopped underneath us moves the phase to stopped`() {
        val f = Fake()
        val c = started(f)
        f.answers["transport_status"] = { CoreOutcome.Ok(JSONObject().put("state", "stopped")) }
        c.poll()
        assertEquals(LinkPhase.Stopped, c.phase)
        assertNull(c.snapshot)
        c.ping(1) // and commands are refused locally from then on
        assertEquals(0, f.count("transport_ping"))
    }

    @Test fun `a security event is shown as bad news and the event list stays bounded`() {
        val f = Fake()
        val events = JSONArray()
        for (i in 1..(TransportController.MAX_EVENTS + 15)) events.put(JSONObject().put("seq", i).put("kind", "link_closed"))
        events.put(JSONObject().put("seq", 1000).put("kind", "connect_failed").put("code", "pin_mismatch"))
        f.answers["transport_status"] = { CoreOutcome.Ok(running().put("events", events).put("last_event_seq", 1000)) }
        val c = started(f)
        assertEquals(TransportController.MAX_EVENTS, c.events.size)
        assertEquals(Tone.Bad, c.notice!!.tone)
        assertEquals(1000L, c.events.last().seq)
    }
}
