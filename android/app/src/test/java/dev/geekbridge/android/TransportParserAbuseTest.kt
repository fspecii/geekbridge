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

/**
 * The native core's snapshot is parsed defensively: whatever it (or a compromised core) sends, the UI gets bounded,
 * identifier-safe values, and anything it cannot trust is dropped rather than shown. Wording for codes and events
 * must be total and must not depend on native text.
 */
class TransportParserAbuseTest {
    private val cert = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"
    private val peerId = "_peer_device_id_0123456789abcdef_"

    private fun base() = JSONObject()
        .put("state", "running")
        .put("local", JSONObject().put("device_id", "_local_device_id_0123456789abcdef").put("device_name", "Pixel").put("cert_sha256", cert))

    private fun arr(vararg items: Any?) = JSONArray().also { a -> items.forEach { a.put(it) } }

    @Test fun `a snapshot that is not an object or has a hostile state is rejected`() {
        assertNull(TransportProtocol.parseSnapshot(null))
        assertNull(TransportProtocol.parseSnapshot("running"))
        assertNull(TransportProtocol.parseSnapshot(JSONArray()))
        for (state in listOf("", "RUNNING", "running; rm -rf", "x".repeat(49), "has space", "../etc")) {
            assertNull("state '$state'", TransportProtocol.parseSnapshot(JSONObject().put("state", state)))
        }
        assertNull(TransportProtocol.parseSnapshot(JSONObject().put("state", 7)))
    }

    @Test fun `a running snapshot without a usable local identity is rejected`() {
        for (id in listOf("has space", "a/b", "", "x".repeat(129))) {
            val j = base()
            j.getJSONObject("local").put("device_id", id)
            assertNull("device_id '$id'", TransportProtocol.parseSnapshot(j))
        }
        val noCert = base()
        noCert.getJSONObject("local").remove("cert_sha256")
        assertNull(TransportProtocol.parseSnapshot(noCert))
        assertNull(TransportProtocol.parseSnapshot(JSONObject().put("state", "running")))
    }

    @Test fun `a stopped snapshot carries no peers even if the core sends some`() {
        val j = JSONObject().put("state", "stopped").put("paired", arr(JSONObject().put("device_id", peerId)))
        val s = TransportProtocol.parseSnapshot(j)!!
        assertTrue(s.paired.isEmpty() && s.connections.isEmpty() && s.prompts.isEmpty() && s.events.isEmpty())
        assertNull(s.local)
    }

    @Test fun `list entries with unsafe ids, missing ports or the wrong type are dropped one by one`() {
        val j = base()
            .put(
                "discovered",
                arr(
                    JSONObject().put("device_id", peerId).put("name", "Desk").put("device_type", "desktop").put("ip", "10.0.0.2").put("tcp_port", 1716).put("paired", true),
                    JSONObject().put("device_id", "bad id").put("tcp_port", 1716),
                    JSONObject().put("device_id", peerId),
                    JSONObject().put("device_id", 12345).put("tcp_port", 1716),
                    "not an object",
                    JSONObject.NULL,
                ),
            )
            .put(
                "connections",
                arr(
                    JSONObject().put("connection_id", 1).put("peer", JSONObject().put("device_id", peerId).put("name", "Desk")),
                    JSONObject().put("connection_id", 2),
                    JSONObject().put("peer", JSONObject().put("device_id", peerId)),
                    JSONObject().put("connection_id", 4).put("peer", JSONObject().put("device_id", "no good")),
                ),
            )
            .put("paired", arr(JSONObject().put("device_id", peerId).put("name", "Desk"), JSONObject().put("device_id", "x y"), JSONObject()))
            .put(
                "events",
                arr(
                    JSONObject().put("seq", 1).put("kind", "paired").put("device_id", peerId).put("code", "ok_code"),
                    JSONObject().put("seq", 2).put("kind", "Not A Kind!").put("code", "Bad Code").put("device_id", "bad id"),
                    JSONObject().put("kind", "paired"),
                ),
            )
        val s = TransportProtocol.parseSnapshot(j)!!
        assertEquals(1, s.discovered.size)
        assertTrue(s.discovered.single().paired)
        assertEquals(1, s.connections.size)
        assertEquals("", s.connections.single().peer.certSha256)
        assertEquals(0L, s.connections.single().pingsReceived)
        assertEquals(1, s.paired.size)
        assertEquals(2, s.events.size)
        assertEquals("paired", s.events[0].kind)
        assertEquals("unknown", s.events[1].kind)
        assertNull("a code that is not a plain token is not carried", s.events[1].code)
        assertNull(s.events[1].deviceId)
    }

    @Test fun `every list is capped and names are bounded and stripped of invisible controls`() {
        val many = JSONArray()
        for (i in 0 until 200) many.put(JSONObject().put("device_id", "dev_$i").put("name", "n"))
        val s = TransportProtocol.parseSnapshot(base().put("paired", many))!!
        assertEquals(TransportProtocol.MAX_LIST, s.paired.size)

        val rlo = "‮"
        val sneaky = "Desk${rlo}gpj.exe" + "x".repeat(500)
        val named = TransportProtocol.parseSnapshot(base().put("paired", arr(JSONObject().put("device_id", peerId).put("name", sneaky))))!!
        val shown = named.paired.single().name
        assertFalse("bidi override must not reach the UI", shown.contains(rlo))
        assertTrue(shown.length <= 48)
    }

    @Test fun `pairing prompts that cannot be shown safely are counted, never shown`() {
        fun prompt() = JSONObject()
            .put("request_id", 1).put("connection_id", 2).put("direction", "incoming").put("peer_device_id", peerId)
            .put("peer_name", "Desk").put("peer_cert_sha256", cert).put("own_cert_sha256", cert)
            .put("verification_code", "1A2B3C4D").put("expires_in_ms", 1000)
        val noCode = prompt().apply { remove("verification_code") }
        val badCert = prompt().put("peer_cert_sha256", "not hex!")
        val noRequest = prompt().apply { remove("request_id") }
        val badId = prompt().put("peer_device_id", "bad id")
        val s = TransportProtocol.parseSnapshot(base().put("prompts", arr(prompt(), noCode, badCert, noRequest, badId)))!!
        assertEquals(1, s.prompts.size)
        assertEquals(4, s.undisplayablePrompts)
    }

    @Test fun `the wait reply is trusted only when the core is running and the sequence is sane`() {
        assertNull(TransportProtocol.parseWait(null))
        assertNull(TransportProtocol.parseWait("x"))
        assertNull(TransportProtocol.parseWait(JSONObject()))
        assertNull(TransportProtocol.parseWait(JSONObject().put("stopped", true).put("last_event_seq", 5)))
        assertNull(TransportProtocol.parseWait(JSONObject().put("stopped", false)))
        assertNull(TransportProtocol.parseWait(JSONObject().put("stopped", false).put("last_event_seq", -1)))
        assertEquals(9L, TransportProtocol.parseWait(JSONObject().put("stopped", false).put("last_event_seq", 9)))
        val req = JSONObject(TransportProtocol.waitRequest(3, timeoutMs = Long.MAX_VALUE))
        assertEquals(TransportProtocol.MAX_WAIT_MS, req.getLong("timeout_ms"))
        assertEquals(0L, JSONObject(TransportProtocol.waitRequest(3, timeoutMs = -5)).getLong("timeout_ms"))
    }

    private val strings = object : Strings {
        override fun get(id: Int, vararg args: Any) = "$id|" + args.joinToString(",")
        override fun plural(id: Int, quantity: Int, vararg args: Any) = "$id#$quantity|" + args.joinToString(",")
    }

    @Test fun `error wording is total and unknown codes use the generic refusal`() {
        val known = listOf(
            "not_running", "already_running", "busy", "invalid_state_dir", "insecure_state", "identity_unusable", "state_corrupt",
            "state_unavailable", "invalid_device_name", "invalid_endpoint", "invalid_device_id", "loopback_not_allowed", "address_rejected",
            "already_connected", "too_many_connections", "unknown_connection", "wrong_state", "not_trusted", "unknown_request",
            "request_expired", "verification_mismatch", "certificate_mismatch", "too_many_attempts", "listen_failed",
            "discovery_requires_listener", "unsupported_platform", "runtime_failed", "store_error",
        )
        val generic = TransportText.error(strings, "something_new")
        assertTrue(generic.startsWith("${R.string.err_core_refused}|"))
        for (code in known) {
            val text = TransportText.error(strings, code)
            assertNotEquals("code $code fell through to the generic refusal", generic.substringBefore('|'), text.substringBefore('|'))
        }
        // Equivalent codes share one message; distinct ones do not.
        assertEquals(TransportText.error(strings, "unknown_request"), TransportText.error(strings, "request_expired"))
        assertNotEquals(TransportText.error(strings, "busy"), TransportText.error(strings, "not_running"))
    }

    @Test fun `event wording and tone cover every kind and security codes are never muted`() {
        fun ev(kind: String, code: String? = null) = LinkEvent(1, kind, null, null, code)
        val kinds = listOf(
            "pairing_prompt", "paired", "pairing_failed", "connect_failed", "link_closed", "peer_unpaired", "peer_requested_repair", "unpaired",
        )
        val texts = kinds.map { TransportText.event(strings, ev(it)).substringBefore('|') }
        assertEquals("each kind has its own wording", kinds.size, texts.toSet().size)
        // An unpair right after this phone accepted is the computer giving up, said calmly.
        assertNotEquals(TransportText.event(strings, ev("peer_unpaired")), TransportText.event(strings, ev("peer_unpaired", "link_unfinished")))
        assertTrue(TransportText.event(strings, ev("peer_unpaired", "link_unfinished")).startsWith("${R.string.event_link_unfinished}"))
        assertEquals(Tone.Muted, TransportText.eventTone(ev("peer_unpaired", "link_unfinished")))
        val unknown = TransportText.event(strings, ev("brand_new_kind"))
        assertTrue(unknown.startsWith("${R.string.event_unknown}|brand_new_kind"))
        for (code in listOf("pairing_rejected_locally", "pairing_rejected_by_peer", "pairing_timeout", "clock_skew", "pairing_binding_changed", "link_closed", "tls_failed", "other", null)) {
            assertTrue(TransportText.event(strings, ev("pairing_failed", code)).isNotEmpty())
        }
        for (code in listOf("pin_mismatch", "identity_mismatch", "timeout", "tls_failed", "other", null)) {
            assertTrue(TransportText.event(strings, ev("connect_failed", code)).isNotEmpty())
        }
        assertTrue(TransportText.event(strings, ev("link_closed", "reset")).contains("reset"))
        assertEquals(Tone.Bad, TransportText.eventTone(ev("connect_failed", "pin_mismatch")))
        assertEquals(Tone.Bad, TransportText.eventTone(ev("connect_failed", "identity_mismatch")))
        assertEquals(Tone.Bad, TransportText.eventTone(ev("peer_requested_repair")))
        assertEquals(Tone.Ok, TransportText.eventTone(ev("paired")))
        assertEquals(Tone.Warn, TransportText.eventTone(ev("pairing_failed", "clock_skew")))
        assertEquals(Tone.Muted, TransportText.eventTone(ev("link_closed")))
        assertTrue(TransportText.isSecurity("pin_mismatch") && !TransportText.isSecurity("timeout") && !TransportText.isSecurity(null))
    }

    @Test fun `fingerprints are grouped for reading only when they look like a fingerprint`() {
        assertEquals("aabb ccdd eeff 0011", TransportText.group("aabbccddeeff0011"))
        assertEquals("short", TransportText.group("short"))
        assertEquals("has space in it!", TransportText.group("has space in it!"))
        assertEquals("a".repeat(129), TransportText.group("a".repeat(129)))
    }
}
