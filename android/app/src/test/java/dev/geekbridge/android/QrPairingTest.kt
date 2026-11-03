// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class QrPairingTest {
    private val id = "a".repeat(32)
    private val cert = "cd".repeat(32)
    private val token = "00112233445566778899aabbccddeeff"

    private fun target(vararg hosts: String) =
        QrTarget(id, "Work laptop", cert, token, hosts.map { QrAddress(it, 1716) })

    private fun conn(phase: String, trusted: Boolean = false, cid: Long = 7) = LinkConnection(
        cid, "initiator", phase, trusted, PeerInfo(id, "Work laptop", "desktop", cert, "192.168.1.20"), 0,
    )

    private fun snap(conns: List<LinkConnection> = emptyList(), paired: List<PairedPeer> = emptyList()) =
        TransportSnapshot("running", null, null, null, emptyList(), conns, emptyList(), 0, paired, false, emptyList(), 0)

    @Test fun onlyPairingLinksAreAccepted() {
        assertEquals("geekbridge://pair?v=1", QrProtocol.linkText("  geekbridge://pair?v=1 \n"))
        assertNull(QrProtocol.linkText("https://example.com/geekbridge://pair?"))
        assertNull(QrProtocol.linkText("geekbridge://other?v=1"))
        assertNull(QrProtocol.linkText(null))
        assertNull(QrProtocol.linkText("geekbridge://pair?" + "x".repeat(QrProtocol.MAX_TEXT_CHARS)))
    }

    private fun result(addresses: JSONArray = JSONArray().put(JSONObject().put("host", "192.168.1.20").put("port", 1716))) =
        JSONObject().put("device_id", id).put("name", "Work laptop").put("cert_sha256", cert)
            .put("token", token).put("addresses", addresses)

    @Test fun targetIsParsedFromTheCoreResult() {
        val t = QrProtocol.parseTarget(result())!!
        assertEquals(id, t.deviceId)
        assertEquals(cert, t.certSha256)
        assertEquals(listOf(QrAddress("192.168.1.20", 1716)), t.addresses)
    }

    @Test fun malformedCoreResultsAreRefused() {
        assertNull(QrProtocol.parseTarget(null))
        assertNull(QrProtocol.parseTarget(result(JSONArray())))
        assertNull(QrProtocol.parseTarget(result().put("token", "short")))
        assertNull(QrProtocol.parseTarget(result().put("cert_sha256", "ab")))
        assertNull(QrProtocol.parseTarget(result(JSONArray().put(JSONObject().put("host", "x").put("port", 70000)))))
    }

    @Test fun theTokenNeverAppearsInText() {
        assertFalse(target("10.0.0.5").toString().contains(token))
    }

    @Test fun addressesAreTriedInOrderEachWithItsOwnTimeout() {
        val plan = QrPlan(target("10.0.0.5", "192.168.1.20"), dialTimeoutMs = 6_000)
        assertEquals(QrStep.Waiting, plan.next(null, 0))
        assertEquals(QrStep.Dial(QrAddress("10.0.0.5", 1716)), plan.next(snap(), 100))
        assertEquals(QrStep.Waiting, plan.next(snap(), 3_000))
        assertEquals(QrStep.Dial(QrAddress("192.168.1.20", 1716)), plan.next(snap(), 6_200))
        assertEquals(QrStep.Failed(QrFailure.NoAddressAnswered), plan.next(snap(), 12_500))
    }

    @Test fun pairRequestIsSentOnceWhenTheChannelIsParked() {
        val plan = QrPlan(target("192.168.1.20"))
        plan.next(snap(), 0) // dial
        assertEquals(QrStep.Pair(7), plan.next(snap(listOf(conn("parked"))), 500))
        assertEquals(QrStep.Waiting, plan.next(snap(listOf(conn("parked"))), 600))
        assertEquals(QrStep.Waiting, plan.next(snap(listOf(conn("pairing_request"))), 700))
        assertEquals(QrStep.Paired, plan.next(snap(listOf(conn("trusted", trusted = true))), 800))
    }

    @Test fun aVanishedConnectionAfterTheRequestIsAFailureNotARetry() {
        val plan = QrPlan(target("192.168.1.20"))
        plan.next(snap(), 0)
        plan.next(snap(listOf(conn("parked"))), 100)
        // The computer refused the token (expired / used): the link is gone and the plan must not re-send it.
        assertEquals(QrStep.Failed(QrFailure.ConnectionLost), plan.next(snap(), 200))
        assertEquals(QrStep.Failed(QrFailure.ConnectionLost), plan.next(snap(), 300))
    }

    @Test fun aRememberedComputerIsForgottenThenPairedForReal() {
        // The computer may have forgotten this phone: "Paired" without a request would be a lie (E2E: B_qr).
        val plan = QrPlan(target("192.168.1.20"))
        val paired = listOf(PairedPeer(id, "Work laptop", "desktop", cert))
        assertEquals(QrStep.Forget(id), plan.next(snap(paired = paired), 0))
        assertEquals(QrStep.Waiting, plan.next(snap(paired = paired), 500)) // the core has not dropped it yet
        assertEquals(QrStep.Dial(QrAddress("192.168.1.20", 1716)), plan.next(snap(), 1_000))
        assertEquals(QrStep.Pair(7), plan.next(snap(listOf(conn("parked"))), 1_500))
        assertEquals(QrStep.Paired, plan.next(snap(paired = paired), 2_000)) // trust recorded by this scan
    }

    @Test fun aForgetThatNeverShowsDoesNotStallTheScan() {
        val plan = QrPlan(target("192.168.1.20"))
        val paired = listOf(PairedPeer(id, "Work laptop", "desktop", cert))
        plan.next(snap(paired = paired), 0)
        assertEquals(QrStep.Dial(QrAddress("192.168.1.20", 1716)), plan.next(snap(paired = paired), 6_000))
    }

    @Test fun pairRequestCarriesTheTokenAndThePinnedCertificate() {
        val o = JSONObject(QrProtocol.pairRequest(9, token, cert))
        assertEquals("transport_pair_request", o.getString("action"))
        assertEquals(9L, o.getLong("connection_id"))
        assertEquals(token, o.getString("qr_token"))
        assertEquals(cert, o.getString("qr_cert_sha256"))
        assertFalse(o.has("mode"))
    }

    @Test fun parseErrorsAreInPlainLanguage() {
        for (c in listOf("qr_not_geekbridge", "qr_version", "qr_malformed", "whatever")) {
            val m = QrProtocol.parseError(TestStrings, c)
            assertTrue(m, m.endsWith("."))
            assertFalse(m.contains("qr_"))
        }
    }
}

class PairingHelpTest {
    private fun facts(probe: Probe, peer: String? = "192.168.1.20", own: String? = "192.168.1.35", wifi: Boolean = true, vpn: Boolean = false) =
        NetFacts(wifi, own, vpn, peer, probe)

    @Test fun reachableComputerIsOk() {
        val r = NetCheck.report(TestStrings, facts(Probe.Connected))
        assertTrue(r.ok)
        assertTrue(r.advice.isEmpty())
        assertTrue(r.steps.all { it.ok })
    }

    @Test fun refusedMeansTheServiceIsNotRunning() {
        val r = NetCheck.report(TestStrings, facts(Probe.Refused))
        assertFalse(r.ok)
        assertTrue(r.headline, r.headline.contains("not listening"))
    }

    @Test fun silenceOnTheSameSubnetBlamesFirewallAndIsolation() {
        val r = NetCheck.report(TestStrings, facts(Probe.TimedOut))
        assertTrue(r.advice.any { it.contains("firewall") })
        assertTrue(r.advice.any { it.contains("Guest") })
        assertFalse(r.advice.any { it.contains("VPN") })
    }

    @Test fun anotherSubnetIsCalledOutAndVpnIsMentionedOnlyWhenActive() {
        val r = NetCheck.report(TestStrings, facts(Probe.TimedOut, peer = "10.0.0.7", vpn = true))
        assertTrue(r.headline, r.headline.contains("different network"))
        assertTrue(r.advice.any { it.contains("VPN") })
        assertTrue(r.steps.any { !it.ok && it.label.contains("same network") })
    }

    @Test fun noWifiAndNoComputerSeen() {
        assertTrue(NetCheck.report(TestStrings, facts(Probe.NotTried, wifi = false)).headline.contains("not on Wi-Fi"))
        assertTrue(NetCheck.report(TestStrings, facts(Probe.NotTried, peer = null)).headline.contains("No computer"))
    }

    @Test fun subnetComparison() {
        assertTrue(NetCheck.sameSubnet("192.168.1.2", "192.168.1.200"))
        assertFalse(NetCheck.sameSubnet("192.168.1.2", "192.168.2.200"))
        assertTrue(NetCheck.sameSubnet(null, "192.168.1.2"))
        assertTrue(NetCheck.sameSubnet("fe80::1", "fe80::2"))
    }

    @Test fun failuresGetPlainWords() {
        assertTrue(PairingHelp.explain(TestStrings, "pairing_failed", "pairing_rejected_by_peer", viaQr = true).headline.contains("QR code"))
        assertTrue(PairingHelp.explain(TestStrings, "pairing_failed", "pairing_rejected_by_peer").headline.contains("declined"))
        assertTrue(PairingHelp.explain(TestStrings, "connect_failed", "timeout").headline.contains("did not answer"))
        assertTrue(PairingHelp.explain(TestStrings, null, "qr_certificate_mismatch", viaQr = true).headline.contains("not the computer"))
        assertTrue(PairingHelp.explain(TestStrings, "pairing_failed", "clock_skew").headline.contains("clocks"))
        assertTrue(PairingHelp.explain(TestStrings, "x", "unheard-of").headline.contains("did not work"))
    }

    @Test fun recreatedActivityReadsTheLinkOnlyWhileItsConfirmationWasWaiting() {
        assertTrue(QrReplay.shouldHandle(hasSavedState = false, wasWaiting = false))
        assertTrue(QrReplay.shouldHandle(hasSavedState = true, wasWaiting = true))
        assertFalse(QrReplay.shouldHandle(hasSavedState = true, wasWaiting = false))
    }
}
