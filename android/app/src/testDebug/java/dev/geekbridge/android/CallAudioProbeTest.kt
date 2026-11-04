// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class CallAudioProbeTest {
    private fun meter(vararg v: Int): CallAudioLevels.Meter {
        val m = CallAudioLevels.Meter()
        val a = ShortArray(v.size) { v[it].toShort() }
        m.add(a, a.size)
        return m
    }

    @Test fun allZeroIsSilent() {
        val m = meter(0, 0, 0, 0)
        assertEquals(0.0, m.rms, 0.0)
        assertEquals(CallAudioOutcome.SILENT, CallAudioLevels.classify(m, silenced = false))
    }

    @Test fun nonZeroIsAudio() {
        val m = meter(0, -300, 400, 0)
        assertEquals(400, m.peak)
        assertTrue(m.rms > 0)
        assertEquals(CallAudioOutcome.AUDIO_PRESENT, CallAudioLevels.classify(m, silenced = false))
    }

    @Test fun zeroWhileSilencedIsPolicyNotSource() {
        assertEquals(CallAudioOutcome.SILENCED_BY_POLICY, CallAudioLevels.classify(meter(0, 0), silenced = true))
    }

    @Test fun noSamplesIsAFailure() {
        assertEquals(CallAudioOutcome.ILLEGAL_STATE, CallAudioLevels.classify(CallAudioLevels.Meter(), silenced = false))
    }

    @Test fun lineNamesTheOutcomeAndLevels() {
        val l = CallAudioResult(CallAudioOutcome.SILENT, "audio mode=2", 80000, 0.0, 0).line()
        assertTrue(l, l.contains("SILENT") && l.contains("rms=0.0") && l.contains("samples=80000"))
        assertTrue(CallAudioResult(CallAudioOutcome.SECURITY_EXCEPTION, "SecurityException").line().contains("SECURITY_EXCEPTION"))
    }
}
