// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.security.MessageDigest
import kotlin.math.PI
import kotlin.math.sin
import kotlin.math.sqrt

/*
 * DEBUG BUILDS ONLY, EXPERIMENT. One client connection to the shell-uid helper, with no Android types so a JVM test
 * drives it with fakes. Fixed command set (start / stop / probe), no shell passthrough, no file access; the first
 * command must carry the per-launch token. Audio is relayed, never stored or logged; STATUS carries numbers only.
 */

/** Where the far party's audio is read from (an AudioRecord on a call source, or a test tone). */
interface PcmCapture {
    val name: String

    /** Blocks for one frame; returns the samples read, or a negative error. */
    fun read(buf: ShortArray): Int

    /** The system reported this client as silenced (privacy / concurrency policy). */
    fun silenced(): Boolean = false
    fun close()
}

/** Where the computer's microphone is played into (the uplink route under test), or nowhere. */
interface PcmInjector {
    /** "telephony_tx", "voip_mix", "none"; with the device the policy actually picked, when known. */
    val route: String
    fun write(buf: ShortArray): Boolean
    fun close()
}

object NoInjector : PcmInjector {
    override val route = "none"
    override fun write(buf: ShortArray) = false
    override fun close() {}
}

/** A sine wave, paced to real time unless [paced] is false (tests). Stands in for call audio. */
class ToneCapture(
    private val hz: Double = 440.0,
    private val paced: Boolean = true,
    /** Fraction of full scale. */
    private val amplitude: Double = 0.3,
    /** > 0: a 10 ms fade in at the start and out at the end of this many samples, silence after it (0 = endless). */
    private val lengthSamples: Long = 0,
) : PcmCapture {
    override val name = "TONE"
    private var n = 0L
    private var next = System.nanoTime()
    override fun read(buf: ShortArray): Int {
        if (paced) {
            next += 20_000_000L
            val wait = next - System.nanoTime()
            if (wait > 0) Thread.sleep(wait / 1_000_000, (wait % 1_000_000).toInt())
        }
        for (i in buf.indices) {
            var g = 1.0
            if (lengthSamples > 0) {
                val fade = HelperProtocol.RATE / 100.0
                g = if (n >= lengthSamples) 0.0 else minOf(1.0, n / fade, (lengthSamples - 1 - n) / fade)
            }
            buf[i] = (sin(2 * PI * hz * n / HelperProtocol.RATE) * amplitude * g * 32767).toInt().toShort()
            n++
        }
        return buf.size
    }
    override fun close() {}
}

object Levels {
    fun rms(b: ShortArray, n: Int = b.size): Double {
        if (n <= 0) return 0.0
        var s = 0.0
        for (i in 0 until n) s += b[i].toDouble() * b[i]
        return sqrt(s / n)
    }
}

class HelperSession(
    private val input: InputStream,
    private val output: OutputStream,
    private val token: String,
    private val hello: String,
    private val openCapture: (String) -> PcmCapture?,
    private val openInjector: (String) -> PcmInjector,
    private val probe: () -> String,
    private val clockMs: () -> Long = { System.nanoTime() / 1_000_000 },
    /** "Test speaking" ([UplinkTest]); only during a call, and it plays beeps the other person hears. */
    private val uplinkTest: () -> String = { "{\"uplinkTest\":[],\"error\":\"unsupported\"}" },
) {
    @Volatile private var capture: PcmCapture? = null
    @Volatile private var injector: PcmInjector = NoInjector
    @Volatile private var authed = false
    private var captureThread: Thread? = null
    @Volatile var framesOut = 0L
        private set
    @Volatile var framesIn = 0L
        private set
    @Volatile var injected = 0L
        private set

    /** Sum of squares of the injected samples since the last STATUS (the level that went into the route). */
    @Volatile var injectErrors = 0L
        private set
    @Volatile private var injectError = ""
    @Volatile private var injSumSq = 0.0
    @Volatile private var injSamples = 0L

    private fun status(json: String) = HelperProtocol.write(output, HelperProtocol.text(HelperProtocol.STATUS, json))

    /** Runs until the client goes away or sends something malformed. Always releases audio on the way out. */
    fun run() {
        try {
            HelperProtocol.write(output, HelperProtocol.text(HelperProtocol.HELLO, hello))
            while (true) {
                when (val f = HelperProtocol.read(input)) {
                    is HelperProtocol.Frame.Text -> if (f.type == HelperProtocol.CMD) {
                        if (!command(f.text)) return
                    }
                    is HelperProtocol.Frame.Pcm -> if (f.type == HelperProtocol.INJECT && authed) {
                        framesIn++
                        var sq = 0.0
                        for (v in f.samples) sq += v.toDouble() * v
                        injSumSq += sq
                        injSamples += f.samples.size
                        // A route that fails while playing (the track was refused, the call ended) must not end the
                        // helper: the failure is counted and named in STATUS instead.
                        try {
                            if (injector.write(f.samples)) injected++
                        } catch (e: RuntimeException) {
                            injectErrors++
                            injectError = e.javaClass.simpleName + ": " + (e.message ?: "").take(80)
                        }
                    }
                }
            }
        } catch (_: IOException) {
        } finally {
            stop()
        }
    }

    /** Minimal JSON field reader for our own fixed commands ("key":"value" with [A-Za-z0-9_] values only). */
    internal fun field(json: String, key: String): String? =
        Regex("\"" + Regex.escape(key) + "\"\\s*:\\s*\"([A-Za-z0-9_]{0,64})\"").find(json)?.groupValues?.get(1)

    private fun tokenOk(t: String?): Boolean =
        t != null && MessageDigest.isEqual(t.toByteArray(), token.toByteArray())

    /** Returns false to end the session. */
    private fun command(json: String): Boolean {
        if (!authed) {
            if (!tokenOk(field(json, "token"))) {
                status("{\"error\":\"bad_token\"}")
                return false
            }
            authed = true
        }
        when (field(json, "cmd")) {
            "start" -> start(field(json, "source") ?: "VOICE_DOWNLINK", field(json, "inject") ?: "none")
            "stop" -> stop()
            "probe" -> status(probe())
            // Beeps into the call through each uplink route in turn; capture stops first (one thing at a time).
            "uplink_test" -> {
                stop()
                status(uplinkTest())
            }
            // Does the platform hand out an uplink route? Opens and releases it; nothing is ever written, so the far
            // party cannot hear a test (the full probe injects a tone and must not run during a real call).
            "uplink_check" -> {
                val i = openInjector("telephony_tx")
                val route = i.route
                try {
                    i.close()
                } catch (_: RuntimeException) {
                }
                status("{\"uplink\":\"${route.replace("\"", "")}\"}")
            }
            else -> status("{\"error\":\"unknown_cmd\"}")
        }
        return true
    }

    private fun start(source: String, inject: String) {
        stop()
        val cap = openCapture(source)
        if (cap == null) {
            status("{\"error\":\"capture_refused\",\"source\":\"$source\"}")
            return
        }
        capture = cap
        injector = openInjector(inject)
        status("{\"started\":\"${cap.name}\",\"injectRoute\":\"${injector.route}\"}")
        val t = Thread({ pumpCapture(cap) }, "gb-callaudio-capture")
        captureThread = t
        t.start()
    }

    private fun pumpCapture(cap: PcmCapture) {
        val buf = ShortArray(HelperProtocol.FRAME_SAMPLES)
        var seq = 0
        var sumSq = 0.0
        var peak = 0
        var silencedSeen = false
        try {
            while (capture === cap) {
                var got = 0
                while (got < buf.size) {
                    val part = ShortArray(buf.size - got)
                    val n = cap.read(part)
                    if (n < 0) {
                        status("{\"error\":\"read\",\"code\":$n}")
                        return
                    }
                    System.arraycopy(part, 0, buf, got, n)
                    got += n
                }
                for (v in buf) {
                    sumSq += v.toDouble() * v
                    val a = if (v < 0) -v.toInt() else v.toInt()
                    if (a > peak) peak = a
                }
                if (cap.silenced()) silencedSeen = true
                HelperProtocol.write(output, HelperProtocol.pcm(HelperProtocol.PCM, false, seq, clockMs().toInt(), buf))
                framesOut++
                seq++
                if (seq % 50 == 0) {
                    val rms = sqrt(sumSq / (50.0 * buf.size))
                    val injRms = if (injSamples > 0) sqrt(injSumSq / injSamples).toInt() else 0
                    injSumSq = 0.0
                    injSamples = 0
                    status(
                        "{\"frames\":$framesOut,\"rms\":${rms.toInt()},\"peak\":$peak,\"silenced\":$silencedSeen," +
                            "\"injectRoute\":\"${injector.route}\",\"injectIn\":$framesIn,\"injected\":$injected," +
                            "\"injectRms\":$injRms,\"injectErrors\":$injectErrors," +
                            "\"injectError\":\"${injectError.filter { it.isLetterOrDigit() || it in " .,:_()=/-" }}\"}",
                    )
                    sumSq = 0.0
                    peak = 0
                }
            }
        } catch (_: IOException) {
        }
    }

    fun stop() {
        val c = capture
        capture = null
        try {
            c?.close()
        } catch (_: RuntimeException) {
        }
        val i = injector
        injector = NoInjector
        try {
            i.close()
        } catch (_: RuntimeException) {
        }
        captureThread?.join(500)
        captureThread = null
    }
}
