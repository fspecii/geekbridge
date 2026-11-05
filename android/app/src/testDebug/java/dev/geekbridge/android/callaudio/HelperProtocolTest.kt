// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.IOException
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class HelperProtocolTest {
    /** Same vectors as crates/bridge-callwifi/src/helper.rs. */
    @Test
    fun matchesTheRustVectors() {
        assertArrayEquals("\u0001\u0000\u0000\u0007{\"v\":1}".toByteArray(), HelperProtocol.text(HelperProtocol.HELLO, "{\"v\":1}"))
        val samples = ShortArray(320) { (it * 3 - 400).toShort() }
        val v = HelperProtocol.pcm(HelperProtocol.PCM, false, 0x01020304, 0xA0B0C0D0.toInt(), samples)
        val head = byteArrayOf(2, 0, 0x02, 0x88.toByte(), 1, 2, 3, 4, 0xA0.toByte(), 0xB0.toByte(), 0xC0.toByte(), 0xD0.toByte())
        assertArrayEquals(head, v.copyOfRange(0, 12))
        assertArrayEquals(byteArrayOf(0x70, 0xFE.toByte()), v.copyOfRange(12, 14))
        val back = HelperProtocol.read(ByteArrayInputStream(v)) as HelperProtocol.Frame.Pcm
        assertEquals(0x01020304, back.seq)
        assertArrayEquals(samples, back.samples)
    }

    @Test
    fun hostileFramesAreErrors() {
        val bad = listOf(
            byteArrayOf(9, 0, 0, 0),
            byteArrayOf(2, 0, 0, 3, 1, 2, 3),
            byteArrayOf(5, 1, 0, 2, '{'.code.toByte(), '}'.code.toByte()),
            byteArrayOf(3, 0, 0, 2, 0xFF.toByte(), 0xFE.toByte()),
            byteArrayOf(1, 0, 0xFF.toByte(), 0xFF.toByte()),
            byteArrayOf(1, 0, 0, 5, 1),
        )
        for (b in bad) {
            try {
                HelperProtocol.read(ByteArrayInputStream(b))
                throw AssertionError("accepted ${b.toList()}")
            } catch (_: IOException) {
            }
        }
    }

    private class FakeInjector : PcmInjector {
        override val route = "fake"
        var got = 0
        var closed = false
        override fun write(buf: ShortArray): Boolean {
            got++
            return true
        }
        override fun close() {
            closed = true
        }
    }

    @Test
    fun aSessionNeedsTheTokenThenRelaysBothWays() {
        val a = TestPipe(1 shl 16)
        val b = TestPipe()
        val toHelper = a.output
        val helperIn = a.input
        val fromHelper = b.input
        val helperOut = b.output
        val inj = FakeInjector()
        val s = HelperSession(
            helperIn, helperOut, "tok12345", "{\"v\":1}",
            openCapture = { if (it == "TONE") ToneCapture(440.0, paced = false) else null },
            openInjector = { inj },
            probe = { "{\"probe\":1}" },
        )
        val t = Thread { s.run() }
        t.start()
        assertTrue((HelperProtocol.read(fromHelper) as HelperProtocol.Frame.Text).text.contains("\"v\":1"))
        toHelper.write(HelperProtocol.text(HelperProtocol.CMD, "{\"cmd\":\"start\",\"source\":\"TONE\",\"token\":\"tok12345\",\"inject\":\"x\"}"))
        repeat(5) { toHelper.write(HelperProtocol.pcm(HelperProtocol.INJECT, false, it, 0, ShortArray(320))) }
        toHelper.flush()
        var pcm = 0
        var started = false
        while (pcm < 60) {
            when (val f = HelperProtocol.read(fromHelper)) {
                is HelperProtocol.Frame.Pcm -> pcm++
                is HelperProtocol.Frame.Text -> if (f.text.contains("\"started\":\"TONE\"")) started = true
            }
        }
        assertTrue(started)
        toHelper.close()
        t.join(5000)
        assertEquals(5, inj.got)
        assertTrue("injector released when the client goes", inj.closed)
    }

    /** The app's own client (HelperConnection.inject, the Speak path) must produce frames the helper accepts. */
    @Test
    fun theAppsInjectFramesReachTheRouteAndAFailingRouteDoesNotEndTheSession() {
        val a = TestPipe(1 shl 16)
        val b = TestPipe()
        val toHelper = a.output
        val helperIn = a.input
        val fromHelper = b.input
        val helperOut = b.output
        var writes = 0
        val inj = object : PcmInjector {
            override val route = "tx_interception"
            override fun write(buf: ShortArray): Boolean {
                if (++writes == 3) throw IllegalStateException("play() called on uninitialized AudioTrack")
                return true
            }
            override fun close() {}
        }
        val s = HelperSession(
            helperIn, helperOut, "tok12345", "{\"v\":1}",
            openCapture = { if (it == "TONE") ToneCapture(440.0, paced = false) else null },
            openInjector = { inj },
            probe = { "" },
        )
        val t = Thread { s.run() }
        t.start()
        val c = HelperConnection(fromHelper, toHelper, "tok12345")
        c.start()
        c.command("start", "source" to "TONE", "inject" to "tx_interception")
        val tone = ShortArray(320) { (it * 37).toShort() }
        repeat(10) { c.inject(it, tone) }
        val end = System.currentTimeMillis() + 5000
        while (s.framesIn < 10 && System.currentTimeMillis() < end) Thread.sleep(10)
        assertEquals(10L, s.framesIn)
        assertEquals(9L, s.injected)
        assertEquals(1L, s.injectErrors)
        assertTrue("session still up", t.isAlive)
        c.close()
        t.join(5000)
    }

    @Test
    fun aWrongTokenEndsTheSessionWithoutAudio() {
        val out = ByteArrayOutputStream()
        val cmd = HelperProtocol.text(HelperProtocol.CMD, "{\"cmd\":\"start\",\"source\":\"TONE\",\"token\":\"nope\"}")
        var opened = false
        HelperSession(ByteArrayInputStream(cmd), out, "tok12345", "{}", { opened = true; ToneCapture(paced = false) }, { NoInjector }, { "" }).run()
        val input = ByteArrayInputStream(out.toByteArray())
        HelperProtocol.read(input) // hello
        assertTrue((HelperProtocol.read(input) as HelperProtocol.Frame.Text).text.contains("bad_token"))
        assertEquals(0, input.available())
        assertTrue(!opened)
    }
}
