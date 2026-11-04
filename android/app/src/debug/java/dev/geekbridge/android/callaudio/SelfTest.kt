// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import kotlin.math.sqrt

/*
 * DEBUG BUILDS ONLY, EXPERIMENT. The self-test: record the far party's source for a few seconds with NO call (the
 * control) and again DURING a call, and say in plain words whether this phone lets it through. Numbers only; the
 * samples are measured and dropped. Pure Kotlin over [CapturePort], so a JVM test runs it with fakes.
 */
interface CapturePort {
    /** Open [source] for about [ms] milliseconds and measure it. */
    fun capture(source: String, ms: Long): Capture

    /** The uplink route the helper can open (nothing is written): "none" or a route name. */
    fun uplinkRoute(): String
}

data class SelfTestResult(
    val verdict: Verdict,
    /** The source that gave [verdict] ("VOICE_DOWNLINK" or "DOWNLINK_EXTRACTION"), or null. */
    val source: String?,
    val uplink: String,
    val control: Capture?,
    val call: Capture?,
)

class SelfTestRunner(
    private val port: CapturePort,
    private val sources: List<String> = listOf("VOICE_DOWNLINK", "DOWNLINK_EXTRACTION"),
    private val controlMs: Long = 3_000,
    private val callMs: Long = 5_000,
) {
    /** Step 1, with no call: what each source delivers on its own. */
    fun control(): Map<String, Capture> = sources.associateWith { port.capture(it, controlMs) }

    /** Step 2, during a call. The best source wins; the uplink route is only asked for, never used. */
    fun duringCall(control: Map<String, Capture>): SelfTestResult {
        var best: SelfTestResult? = null
        for (s in sources) {
            val c = port.capture(s, callMs)
            val r = SelfTestResult(SelfTestVerdict.classify(control[s], c), s, "none", control[s], c)
            if (best == null || SelfTestVerdict.rank(r.verdict) < SelfTestVerdict.rank(best.verdict)) best = r
            if (r.verdict == Verdict.Works) break
        }
        val b = best ?: return SelfTestResult(Verdict.NotRun, null, "none", null, null)
        return b.copy(uplink = if (b.verdict == Verdict.Works) port.uplinkRoute() else "none")
    }
}

/** [CapturePort] over a live [HelperConnection]. */
class HelperCapturePort(
    private val conn: HelperConnection,
    private val sleep: (Long) -> Unit = { Thread.sleep(it) },
    private val now: () -> Long = { System.nanoTime() / 1_000_000 },
) : CapturePort {
    private class Meter : HelperConnection.Listener {
        @Volatile var sumSq = 0.0
        @Volatile var n = 0L
        @Volatile var refused = false
        @Volatile var silenced = false
        @Volatile var uplink: String? = null
        override fun onPcm(samples: ShortArray, cap: Int) {
            var s = 0.0
            for (v in samples) s += v.toDouble() * v
            sumSq += s
            n += samples.size
        }
        override fun onStatus(json: String) {
            if (json.contains("\"error\"")) refused = true
            if (json.contains("\"silenced\":true")) silenced = true
            Regex("\"uplink\":\"([^\"]*)\"").find(json)?.let { uplink = it.groupValues[1] }
        }
        override fun onClosed() {
            refused = true
        }
    }

    override fun capture(source: String, ms: Long): Capture {
        val m = Meter()
        conn.listener = m
        try {
            conn.command("start", "source" to source, "inject" to "none")
            val end = now() + ms
            while (now() < end && !m.refused && !conn.closed) sleep(20)
            conn.command("stop")
            if (m.refused) return Capture(opened = false, rms = 0)
            val rms = if (m.n > 0) sqrt(m.sumSq / m.n).toInt() else 0
            return Capture(opened = true, rms = rms, silenced = m.silenced)
        } finally {
            conn.listener = null
        }
    }

    override fun uplinkRoute(): String {
        val m = Meter()
        conn.listener = m
        try {
            conn.command("uplink_check")
            val end = now() + 3_000
            while (now() < end && m.uplink == null && !conn.closed) sleep(20)
            return m.uplink ?: "none"
        } finally {
            conn.listener = null
        }
    }
}
