// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import java.io.PipedInputStream
import java.io.PipedOutputStream
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SelfTestTest {
    private fun cap(opened: Boolean, rms: Int, silenced: Boolean = false) = Capture(opened, rms, silenced)

    /** Same vectors as crates/bridge-callwifi/src/selftest.rs. */
    @Test
    fun verdictMatchesTheRustVectors() {
        val quiet = cap(true, 2)
        assertEquals(Verdict.NotRun, SelfTestVerdict.classify(quiet, null))
        assertEquals(Verdict.Refused, SelfTestVerdict.classify(quiet, cap(false, 0)))
        assertEquals(Verdict.Silent, SelfTestVerdict.classify(quiet, cap(true, 5)))
        assertEquals(Verdict.Silent, SelfTestVerdict.classify(quiet, cap(true, 4000, true)))
        assertEquals(Verdict.Works, SelfTestVerdict.classify(quiet, cap(true, 4000)))
        assertEquals(Verdict.Works, SelfTestVerdict.classify(null, cap(true, 4000)))
        assertEquals(Verdict.Works, SelfTestVerdict.classify(cap(false, 0), cap(true, 4000)))
        assertEquals(Verdict.Unsure, SelfTestVerdict.classify(cap(true, 15000), cap(true, 15000)))
        assertEquals(Verdict.Works, SelfTestVerdict.classify(cap(true, 1000), cap(true, 4000)))
        for (v in Verdict.entries) assertEquals(v, Verdict.fromWire(v.wire))
        assertEquals(Verdict.NotRun, Verdict.fromWire("WORKS"))
    }

    private class FakePort(val results: Map<String, Capture>, val uplink: String = "telephony_tx(interception)") : CapturePort {
        val asked = mutableListOf<String>()
        var uplinkAsked = 0
        override fun capture(source: String, ms: Long): Capture {
            asked += source
            return results.getValue(source)
        }
        override fun uplinkRoute(): String {
            uplinkAsked++
            return uplink
        }
    }

    @Test
    fun theRunnerUsesTheBetterSourceAndOnlyAsksForTheUplinkWhenItWorks() {
        val control = mapOf("VOICE_DOWNLINK" to cap(true, 1), "DOWNLINK_EXTRACTION" to cap(true, 1))
        val silent = FakePort(mapOf("VOICE_DOWNLINK" to cap(true, 3), "DOWNLINK_EXTRACTION" to cap(true, 0)))
        val r1 = SelfTestRunner(silent).duringCall(control)
        assertEquals(Verdict.Silent, r1.verdict)
        assertEquals("none", r1.uplink)
        assertEquals(0, silent.uplinkAsked)

        val second = FakePort(mapOf("VOICE_DOWNLINK" to cap(true, 2), "DOWNLINK_EXTRACTION" to cap(true, 5000)))
        val r2 = SelfTestRunner(second).duringCall(control)
        assertEquals(Verdict.Works, r2.verdict)
        assertEquals("DOWNLINK_EXTRACTION", r2.source)
        assertEquals("telephony_tx(interception)", r2.uplink)

        val first = FakePort(mapOf("VOICE_DOWNLINK" to cap(true, 5000), "DOWNLINK_EXTRACTION" to cap(false, 0)))
        SelfTestRunner(first).duringCall(control)
        assertEquals("a working source ends the search", listOf("VOICE_DOWNLINK"), first.asked)

        val refused = FakePort(mapOf("VOICE_DOWNLINK" to cap(false, 0), "DOWNLINK_EXTRACTION" to cap(false, 0)))
        assertEquals(Verdict.Refused, SelfTestRunner(refused).duringCall(control).verdict)
    }

    @Test
    fun aLoudControlMakesTheAnswerUnsure() {
        val control = mapOf("VOICE_DOWNLINK" to cap(true, 15000), "DOWNLINK_EXTRACTION" to cap(true, 15000))
        val p = FakePort(mapOf("VOICE_DOWNLINK" to cap(true, 15000), "DOWNLINK_EXTRACTION" to cap(true, 15000)))
        val r = SelfTestRunner(p).duringCall(control)
        assertEquals(Verdict.Unsure, r.verdict)
        assertEquals("none", r.uplink)
    }

    /** The real HelperSession on the other end of pipes: the port measures a tone, sees a refusal, reads the uplink route. */
    private fun withHelper(open: (String) -> PcmCapture?, inject: (String) -> PcmInjector, body: (HelperCapturePort, HelperConnection) -> Unit) {
        val toHelper = PipedOutputStream()
        val helperIn = PipedInputStream(toHelper, 1 shl 16)
        val fromHelper = PipedInputStream(1 shl 20)
        val helperOut = PipedOutputStream(fromHelper)
        val session = HelperSession(helperIn, helperOut, "tok12345", "{\"v\":1}", open, inject, { "{}" })
        val t = Thread { session.run() }
        t.start()
        val conn = HelperConnection(fromHelper, toHelper, "tok12345")
        conn.start()
        try {
            body(HelperCapturePort(conn), conn)
        } finally {
            conn.close()
            t.join(5000)
        }
    }

    private class Route(override val route: String) : PcmInjector {
        var wrote = 0
        override fun write(buf: ShortArray): Boolean { wrote++; return true }
        override fun close() {}
    }

    @Test
    fun theLivePortMeasuresLevelsSeesRefusalAndNeverWritesWhenAskedForTheUplink() {
        val route = Route("telephony_tx(interception)")
        withHelper({ if (it == "TONE") ToneCapture(440.0, paced = false) else null }, { route }) { port, _ ->
            val tone = port.capture("TONE", 300)
            assertTrue(tone.opened)
            assertTrue("rms ${tone.rms}", tone.rms in 5000..12000)
            val refused = port.capture("VOICE_DOWNLINK", 300)
            assertFalse(refused.opened)
            assertEquals("telephony_tx(interception)", port.uplinkRoute())
            assertEquals("asking for the route writes nothing", 0, route.wrote)
        }
    }

    @Test
    fun noUplinkIsReportedAsNone() {
        withHelper({ null }, { NoInjector }) { port, _ -> assertEquals("none", port.uplinkRoute()) }
    }
}
