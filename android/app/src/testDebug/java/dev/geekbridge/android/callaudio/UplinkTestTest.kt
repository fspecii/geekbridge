// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import kotlin.math.PI
import kotlin.math.sin
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class UplinkTestTest {
    /** Records what was written; counts the frames that carried the beep. */
    private class Sink(override val route: String) : PcmInjector {
        var frames = 0
        var loud = 0
        var closed = false
        val written = ArrayList<Short>()
        override fun write(buf: ShortArray): Boolean {
            frames++
            if (Levels.rms(buf) > 1000) loud++
            written.addAll(buf.toList())
            return true
        }
        override fun close() {
            closed = true
        }
    }

    /** A VOICE_UPLINK tap that hears whatever [sink] last wrote (a phone whose tap sees the injected audio). */
    private class Loop(val sink: () -> Sink?) : PcmCapture {
        override val name = "VOICE_UPLINK"
        override fun read(buf: ShortArray): Int {
            val w = sink()?.written ?: return buf.size.also { buf.fill(0) }
            for (i in buf.indices) buf[i] = w.getOrElse(w.size - buf.size + i) { 0 }
            return buf.size
        }
        override fun close() {}
    }

    @Test
    fun eachRouteBeepsItsOwnNumberOfTimesAndARefusedRouteSaysWhy() {
        val sinks = HashMap<String, Sink>()
        var current: Sink? = null
        val t = UplinkTest(
            openInjector = { r -> if (r == "tx_voice") null else Sink(r).also { sinks[r] = it; current = it } },
            lastError = { "IllegalStateException: no route" },
            openCapture = { Loop { current } },
            sleepMs = {},
        )
        val out = t.run()
        assertTrue(out, out.startsWith("{\"uplinkTest\":["))
        // 300 ms beeps of 20 ms frames: 15 loud frames per beep.
        assertEquals(15, sinks.getValue("tx_interception").loud)
        assertEquals(45, sinks.getValue("tx_music").loud)
        assertTrue(sinks.values.all { it.closed })
        assertTrue(out, out.contains("\"route\":\"tx_voice\",\"beeps\":2,\"outcome\":\"refused\",\"error\":\"IllegalStateException: no route\""))
        assertTrue(out, Regex("\"route\":\"tx_music\",\"beeps\":3,\"outcome\":\"played\"[^}]*\"tap\":true").containsMatchIn(out))
        // The tap heard the beeps: the 1 kHz energy during beeps dwarfs the gaps.
        val loop = Regex("\"route\":\"tx_interception\"[^}]*\"loop\":([0-9.]+)").find(out)!!.groupValues[1].toDouble()
        assertTrue("loop $loop", loop > 100)
    }

    @Test
    fun withoutATapThereIsNoLoopFigureAndRoutesMapToBeeps() {
        val out = UplinkTest({ Sink(it) }, { "" }, { null }, sleepMs = {}).run()
        assertTrue(out, out.contains("\"loop\":null") && out.contains("\"tap\":false"))
        assertEquals("tx_interception", UplinkRoutes.ofBeeps(1))
        assertEquals("tx_music", UplinkRoutes.ofBeeps(3))
        assertNull(UplinkRoutes.ofBeeps(0))
        assertNull(UplinkRoutes.ofBeeps(4))
        assertTrue(UplinkRoutes.valid("telephony_tx") && UplinkRoutes.valid("tx_voice"))
        assertFalse(UplinkRoutes.valid("none") || UplinkRoutes.valid("voip_mix"))
        // Unused here, but keep the sine helper honest: a 1 kHz frame is loud.
        val f = ShortArray(320) { (sin(2 * PI * 1000.0 * it / 16000) * 9000).toInt().toShort() }
        assertTrue(Levels.rms(f) > 1000)
    }
}
