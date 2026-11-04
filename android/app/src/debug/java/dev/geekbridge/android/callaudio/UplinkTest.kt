// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import java.util.Locale
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.sin

/*
 * DEBUG BUILDS ONLY, EXPERIMENT. "Test speaking": can audio played on the phone reach the OTHER person of a carrier
 * call? Nothing on the phone can tell for sure (the uplink leaves through the modem), so each candidate route plays
 * a different number of short 1 kHz beeps into the call: route 1 one beep, route 2 two, route 3 three. The other
 * person says how many they heard, and that picks the route Speak uses. Meanwhile, where the phone allows it, the
 * VOICE_UPLINK tap is recorded and the 1 kHz energy during beeps is compared with the gaps: a hint, not a proof.
 * Only numbers come out; no audio is kept. Runs only during a call (the helper refuses otherwise).
 *
 * The routes, in the order tried (docs/reports/call-audio-wifi-realphone.md):
 *   tx_interception  AudioManager.getCallUplinkInjectionAudioTrack (USAGE_CALL_ASSISTANT, CALL_AUDIO_INTERCEPTION)
 *   tx_voice         an AudioTrack (VOICE_COMMUNICATION) with the TELEPHONY_TX device preferred ("in-call music")
 *   tx_music         the same with USAGE_MEDIA
 */
object UplinkRoutes {
    val ALL = listOf("tx_interception", "tx_voice", "tx_music")

    /** A route name the helper and the runtime accept for Speak. "telephony_tx" = the first one that opens. */
    fun valid(r: String) = r == "telephony_tx" || r in ALL

    /** Which route the other person heard, from the number of beeps they counted (1..3), or null. */
    fun ofBeeps(n: Int): String? = ALL.getOrNull(n - 1)
}

class UplinkTest(
    private val openInjector: (String) -> PcmInjector?,
    private val lastError: () -> String,
    private val openCapture: (String) -> PcmCapture?,
    private val sleepMs: (Long) -> Unit = { Thread.sleep(it) },
    private val routes: List<String> = UplinkRoutes.ALL,
) {
    private val rate = HelperProtocol.RATE
    private val frame = HelperProtocol.FRAME_SAMPLES

    /** One JSON object: {"uplinkTest":[{route, beeps, outcome, written, loop, error}...]}. */
    fun run(): String {
        val parts = routes.mapIndexed { i, r ->
            val res = oneRoute(r, i + 1)
            sleepMs(700)
            res
        }
        return "{\"uplinkTest\":[" + parts.joinToString(",") + "]}"
    }

    private fun q(s: String) = "\"" + s.filter { it.isLetterOrDigit() || it in " .,:_()=/-" }.take(120) + "\""

    private fun oneRoute(route: String, beeps: Int): String {
        val inj = openInjector(route)
            ?: return "{\"route\":\"$route\",\"beeps\":$beeps,\"outcome\":\"refused\",\"error\":${q(lastError())}}"
        val tap = openCapture("VOICE_UPLINK")
        var written = 0
        var onPow = 0.0
        var offPow = 0.0
        var onN = 0
        var offN = 0
        val out = ShortArray(frame)
        val inBuf = ShortArray(frame)
        var n = 0L
        try {
            // Each beep 300 ms on, 300 ms off; 200 ms of silence first so the track has started.
            val plan = ArrayList<Boolean>()
            repeat(10) { plan.add(false) }
            repeat(beeps) {
                repeat(15) { plan.add(true) }
                repeat(15) { plan.add(false) }
            }
            for (on in plan) {
                for (k in 0 until frame) {
                    out[k] = if (on) (sin(2 * PI * 1000.0 * n / rate) * 0.3 * 32767).toInt().toShort() else 0
                    n++
                }
                if (inj.write(out)) written++
                if (tap != null) {
                    val r = tap.read(inBuf)
                    if (r > 0) {
                        val p = goertzel(inBuf, r, 1000.0) / r
                        if (on) {
                            onPow += p
                            onN++
                        } else {
                            offPow += p
                            offN++
                        }
                    }
                } else {
                    sleepMs(20)
                }
            }
            sleepMs(300) // let the track play out its buffer before it is closed
        } finally {
            runCatching { inj.close() }
            tap?.let { runCatching { it.close() } }
        }
        val loop = if (onN > 0 && offN > 0) {
            String.format(Locale.ROOT, "%.1f", (onPow / onN) / (offPow / offN + 1.0))
        } else {
            "null"
        }
        return "{\"route\":\"$route\",\"beeps\":$beeps,\"outcome\":\"played\",\"via\":${q(inj.route)}," +
            "\"written\":$written,\"loop\":$loop,\"tap\":${tap != null}}"
    }

    private fun goertzel(x: ShortArray, n: Int, hz: Double): Double {
        val c = 2 * cos(2 * PI * hz / rate)
        var s1 = 0.0
        var s2 = 0.0
        for (i in 0 until n) {
            val s0 = x[i] + c * s1 - s2
            s2 = s1
            s1 = s0
        }
        return s1 * s1 + s2 * s2 - c * s1 * s2
    }
}
