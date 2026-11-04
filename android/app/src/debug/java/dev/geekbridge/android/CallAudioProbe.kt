// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Build
import kotlin.math.sqrt

/*
 * DEBUG BUILDS ONLY, EXPERIMENT. Feasibility probe for "call audio on the computer without Bluetooth"
 * (docs/reports/call-audio-over-wifi-feasibility.md). It tries to read the downlink (far party's voice) of a
 * cellular call for five seconds and records what Android said. It does NOT send audio anywhere and keeps no
 * samples: only a result code, the RMS and peak level. Hidden behind the `call_audio_experiment` switch
 * (DebugControlReceiver), off by default, and absent from release builds.
 */

/** What the probe saw. Names are the codes the debug state dump and scripts/realdevice-call-audio-probe.sh print. */
enum class CallAudioOutcome {
    /** The switch `call_audio_experiment` is off; nothing was tried. */
    DISABLED,

    /** RECORD_AUDIO is not granted to the app. */
    NO_RECORD_AUDIO,

    /** Android refused the source: the app does not hold CAPTURE_AUDIO_OUTPUT (the expected result for a normal app). */
    SECURITY_EXCEPTION,

    /** AudioRecord would not initialise or start (policy refused the source, or no call). */
    ILLEGAL_STATE,

    /** Capture ran but the system muted the client (AudioRecordingConfiguration.isClientSilenced), e.g. app in background. */
    SILENCED_BY_POLICY,

    /** Capture ran for the full time and every sample was zero (RMS = 0): the source is wired but empty. */
    SILENT,

    /** Capture ran and non-zero audio arrived. The only outcome that would mean the route can work. */
    AUDIO_PRESENT,
}

data class CallAudioResult(
    val outcome: CallAudioOutcome,
    val detail: String = "",
    val samples: Long = 0,
    val rms: Double = 0.0,
    val peak: Int = 0,
    val atMs: Long = 0,
) {
    fun line(): String = "call audio probe (VOICE_DOWNLINK, 5 s): ${outcome.name}" +
        (if (samples > 0) "; samples=$samples rms=${"%.1f".format(java.util.Locale.ROOT, rms)} peak=$peak" else "") +
        (if (detail.isNotEmpty()) "; $detail" else "")
}

/** Level maths and the decision, with no Android types so a JVM test covers it. */
object CallAudioLevels {
    class Meter {
        var count = 0L
            private set
        var peak = 0
            private set
        private var sumSq = 0.0
        fun add(buf: ShortArray, n: Int) {
            for (i in 0 until n) {
                val v = buf[i].toInt()
                sumSq += v.toDouble() * v
                val a = if (v < 0) -v else v
                if (a > peak) peak = a
            }
            count += n
        }
        val rms: Double get() = if (count == 0L) 0.0 else sqrt(sumSq / count)
    }

    /** [silenced]: the system reported the client as silenced at any point; [read] samples were read. */
    fun classify(meter: Meter, silenced: Boolean): CallAudioOutcome = when {
        meter.count == 0L -> CallAudioOutcome.ILLEGAL_STATE
        meter.peak > 0 -> CallAudioOutcome.AUDIO_PRESENT
        silenced -> CallAudioOutcome.SILENCED_BY_POLICY
        else -> CallAudioOutcome.SILENT
    }
}

object CallAudioProbe {
    private const val PREFS = "gb_debug_experiments"
    private const val KEY = "call_audio_experiment"
    private const val RATE = 16_000
    private const val SECONDS = 5

    @Volatile private var last: CallAudioResult? = null
    @Volatile private var running = false

    fun enabled(ctx: Context): Boolean = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY, false)

    fun setEnabled(ctx: Context, on: Boolean) {
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean(KEY, on).apply()
    }

    /** Lines for the debug state dump. */
    fun lines(ctx: Context): List<String> {
        val l = last
        return buildList {
            add("call_audio_experiment: ${if (enabled(ctx)) "ON" else "off"}")
            if (l != null) add(l.line())
        }
    }

    /** Runs one probe on a worker thread; the result lands in [lines]. A second call while one runs is ignored. */
    fun runAsync(ctx: Context) {
        val app = ctx.applicationContext
        synchronized(this) {
            if (running) return
            running = true
        }
        Thread({
            try {
                last = run(app)
                GbLog.i("debug", "call audio probe: ${last?.outcome}")
            } finally {
                running = false
            }
        }, "call-audio-probe").start()
    }

    fun run(ctx: Context): CallAudioResult {
        val now = System.currentTimeMillis()
        if (!enabled(ctx)) return CallAudioResult(CallAudioOutcome.DISABLED, atMs = now)
        if (ctx.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            return CallAudioResult(CallAudioOutcome.NO_RECORD_AUDIO, "adb shell pm grant ${ctx.packageName} android.permission.RECORD_AUDIO", atMs = now)
        }
        var rec: AudioRecord? = null
        try {
            val min = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
            val size = maxOf(min, RATE / 5 * 2)
            rec = AudioRecord(MediaRecorder.AudioSource.VOICE_DOWNLINK, RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, size)
            if (rec.state != AudioRecord.STATE_INITIALIZED) {
                return CallAudioResult(CallAudioOutcome.ILLEGAL_STATE, "AudioRecord not initialised (source refused)", atMs = now)
            }
            val am = ctx.getSystemService(AudioManager::class.java)
            var silenced = false
            rec.startRecording()
            val meter = CallAudioLevels.Meter()
            val buf = ShortArray(RATE / 10)
            val end = System.nanoTime() + SECONDS * 1_000_000_000L
            while (System.nanoTime() < end) {
                val n = rec.read(buf, 0, buf.size)
                if (n < 0) {
                    return CallAudioResult(CallAudioOutcome.ILLEGAL_STATE, "read error $n", meter.count, meter.rms, meter.peak, now)
                }
                meter.add(buf, n)
                if (am != null && Build.VERSION.SDK_INT >= 29 && am.activeRecordingConfigurations.any { it.isClientSilenced }) silenced = true
            }
            val outcome = CallAudioLevels.classify(meter, silenced)
            return CallAudioResult(outcome, "audio mode=${am?.mode}", meter.count, meter.rms, meter.peak, now)
        } catch (e: SecurityException) {
            return CallAudioResult(CallAudioOutcome.SECURITY_EXCEPTION, e.javaClass.simpleName, atMs = now)
        } catch (e: IllegalStateException) {
            return CallAudioResult(CallAudioOutcome.ILLEGAL_STATE, e.javaClass.simpleName, atMs = now)
        } catch (e: UnsupportedOperationException) {
            return CallAudioResult(CallAudioOutcome.ILLEGAL_STATE, e.javaClass.simpleName, atMs = now)
        } catch (e: IllegalArgumentException) {
            return CallAudioResult(CallAudioOutcome.ILLEGAL_STATE, e.javaClass.simpleName, atMs = now)
        } finally {
            try {
                rec?.stop()
            } catch (_: IllegalStateException) {
            }
            rec?.release()
        }
    }
}
