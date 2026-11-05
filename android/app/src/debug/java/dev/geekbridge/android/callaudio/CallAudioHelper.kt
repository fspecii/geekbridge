// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import android.annotation.SuppressLint
import android.content.AttributionSource
import android.content.Context
import android.content.ContextWrapper
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioTrack
import android.media.MediaRecorder
import android.net.LocalServerSocket
import android.os.Build
import android.os.Process
import java.util.Locale
import kotlin.math.PI
import kotlin.math.cos

/*
 * DEBUG BUILDS ONLY, EXPERIMENT (docs/design/call-audio-over-wifi.md). The shell-uid helper. It is not started by
 * the app: it runs only when someone with adb (Wireless debugging, or the emulator e2e) starts it, as uid 2000:
 *
 *   adb shell 'CLASSPATH=$(pm path dev.geekbridge.android | sed s/^package://) app_process /system/bin \
 *       dev.geekbridge.android.callaudio.CallAudioHelper --probe'
 *   adb shell '… CallAudioHelper --listen gb-callaudio-<random> --token <random>'   (then adb forward …)
 *
 * --probe prints one JSON line: what each call source and each uplink route did on this device (numbers and
 * exception names only, never audio). --listen serves one client at a time on an abstract unix socket; the client
 * must present the token, and its uid must be shell (adb forward), root, or the --app-uid given.
 */
// The helper runs under app_process as the shell uid, where the hidden-API filter does not apply (that is why scrcpy's
// server works); inside the app these calls would be blocked. It never runs inside the app process.
@SuppressLint("PrivateApi", "SoonBlockedPrivateApi", "DiscouragedPrivateApi", "MissingPermission")
object CallAudioHelper {
    private const val SHELL_PKG = "com.android.shell"
    private val RATE = HelperProtocol.RATE

    /** Context stand-in for a process without an Application: the shell's identity, nothing else. */
    /** Constructed only when SDK_INT >= 31 (see [context]). */
    @SuppressLint("NewApi")
    class ShellContext : ContextWrapper(null) {
        override fun getPackageName(): String = SHELL_PKG
        override fun getOpPackageName(): String = SHELL_PKG
        override fun getAttributionTag(): String? = null
        override fun getApplicationContext(): Context = this
        override fun getDeviceId(): Int = 0

        // Client-side pre-checks (AudioPolicy.Builder, AudioManager) ask the context; the shell holds these
        // permissions and audioserver/system_server enforce them again on the real uid, so answering here grants
        // nothing the uid does not already have.
        override fun checkCallingOrSelfPermission(permission: String): Int = android.content.pm.PackageManager.PERMISSION_GRANTED
        override fun checkSelfPermission(permission: String): Int = android.content.pm.PackageManager.PERMISSION_GRANTED
        override fun getMainLooper(): android.os.Looper = android.os.Looper.getMainLooper()
        override fun getAttributionSource(): AttributionSource =
            AttributionSource.Builder(Process.myUid()).setPackageName(SHELL_PKG).build()
    }

    private val context: Context? by lazy {
        if (Build.VERSION.SDK_INT >= 31) ShellContext() else null
    }

    /** AudioManager built on the shell context (hidden constructor; app_process has no hidden-API filter). */
    private val audioManager: AudioManager? by lazy {
        try {
            AudioManager::class.java.getDeclaredConstructor(Context::class.java).newInstance(context) as AudioManager
        } catch (e: ReflectiveOperationException) {
            null
        } catch (e: RuntimeException) {
            null
        }
    }

    private fun fmtIn() = AudioFormat.Builder().setSampleRate(RATE).setChannelMask(AudioFormat.CHANNEL_IN_MONO)
        .setEncoding(AudioFormat.ENCODING_PCM_16BIT).build()

    private fun fmtOut() = AudioFormat.Builder().setSampleRate(RATE).setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
        .setEncoding(AudioFormat.ENCODING_PCM_16BIT).build()

    private fun sourceId(name: String): Int? = when (name) {
        "MIC" -> MediaRecorder.AudioSource.MIC
        "VOICE_UPLINK" -> MediaRecorder.AudioSource.VOICE_UPLINK
        "VOICE_DOWNLINK" -> MediaRecorder.AudioSource.VOICE_DOWNLINK
        "VOICE_CALL" -> MediaRecorder.AudioSource.VOICE_CALL
        "VOICE_COMMUNICATION" -> MediaRecorder.AudioSource.VOICE_COMMUNICATION
        "REMOTE_SUBMIX" -> MediaRecorder.AudioSource.REMOTE_SUBMIX
        else -> null
    }

    private fun newRecord(source: Int): AudioRecord {
        val size = maxOf(AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT), RATE / 5 * 2)
        val ctx = context
        return if (Build.VERSION.SDK_INT >= 31 && ctx != null) {
            AudioRecord.Builder().setContext(ctx).setAudioSource(source).setAudioFormat(fmtIn()).setBufferSizeInBytes(size).build()
        } else {
            @Suppress("MissingPermission")
            AudioRecord(source, RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, size)
        }
    }

    class RecordCapture(override val name: String, private val rec: AudioRecord) : PcmCapture {
        override fun read(buf: ShortArray): Int = rec.read(buf, 0, buf.size)
        override fun silenced(): Boolean {
            if (Build.VERSION.SDK_INT < 29) return false
            val cfg = try {
                rec.activeRecordingConfiguration
            } catch (_: RuntimeException) {
                null
            }
            return cfg?.isClientSilenced == true
        }
        override fun close() {
            try {
                rec.stop()
            } catch (_: IllegalStateException) {
            }
            rec.release()
        }
    }

    /** Hidden/system API through reflection; null when absent or refused (the reason goes to [lastError]). */
    @Volatile private var lastError = ""

    private fun note(e: Throwable): Nothing? {
        val c = (e as? java.lang.reflect.InvocationTargetException)?.targetException ?: e
        lastError = c.javaClass.simpleName + ": " + (c.message ?: "").take(120).replace(Regex("[^A-Za-z0-9 .,:_()=/-]"), "?")
        return null
    }

    fun openCapture(name: String): PcmCapture? {
        if (name == "TONE") return ToneCapture()
        if (name == "DOWNLINK_EXTRACTION") {
            val rec = callInterception("getCallDownlinkExtractionAudioRecord") as? AudioRecord ?: return null
            return try {
                rec.startRecording()
                RecordCapture(name, rec)
            } catch (e: RuntimeException) {
                rec.release()
                note(e)
            }
        }
        val src = sourceId(name) ?: return null
        return try {
            val rec = newRecord(src)
            if (rec.state != AudioRecord.STATE_INITIALIZED) {
                rec.release()
                lastError = "not initialised"
                return null
            }
            rec.startRecording()
            RecordCapture(name, rec)
        } catch (e: RuntimeException) {
            note(e)
        }
    }

    /** AudioManager.getCallUplinkInjectionAudioTrack / getCallDownlinkExtractionAudioRecord (SystemApi, API 33,
     *  CALL_AUDIO_INTERCEPTION, only while a call is in progress). */
    private fun callInterception(method: String): Any? {
        val am = audioManager ?: return null.also { lastError = "no AudioManager" }
        return try {
            AudioManager::class.java.getMethod(method, AudioFormat::class.java)
                .invoke(am, if (method.contains("Track")) fmtOut() else fmtIn())
        } catch (e: ReflectiveOperationException) {
            note(e)
        } catch (e: RuntimeException) {
            note(e)
        }
    }

    class TrackInjector(override val route: String, private val track: AudioTrack, private val keep: Any? = null) : PcmInjector {
        private var writes = 0

        // The first three frames (60 ms) are written before play(): a small cushion against Wi-Fi jitter, so the
        // track does not start by running dry. After that the track's own buffer (200 ms) absorbs bursts.
        override fun write(buf: ShortArray): Boolean {
            val ok = track.write(buf, 0, buf.size, AudioTrack.WRITE_NON_BLOCKING) > 0
            if (++writes == 3) track.play() // may throw IllegalStateException: the session counts it, never dies
            return ok
        }
        override fun close() {
            try {
                track.stop()
            } catch (_: IllegalStateException) {
            }
            track.release()
            (keep as? AutoCloseable)?.close()
        }
    }

    private fun telephonyTxDevice(): AudioDeviceInfo? = try {
        @Suppress("UNCHECKED_CAST")
        val all = AudioManager::class.java.getDeclaredMethod("getDevicesStatic", Int::class.javaPrimitiveType)
            .apply { isAccessible = true }.invoke(null, AudioManager.GET_DEVICES_OUTPUTS) as Array<AudioDeviceInfo>
        all.firstOrNull { it.type == AudioDeviceInfo.TYPE_TELEPHONY }
    } catch (e: ReflectiveOperationException) {
        note(e)
    }

    fun openInjector(route: String): PcmInjector = openRoute(route) ?: NoInjector

    /**
     * One uplink route (see [UplinkRoutes]), or null with the reason in [lastError]. "telephony_tx" is the first of
     * them that opens: the interception API, then an AudioTrack with the TELEPHONY_TX ("in-call music") device
     * preferred, as a voice track and then as a media track (AudioPolicyManager gives an explicitly routed music or
     * voice track the INCALL_MUSIC output during a call, for a caller with MODIFY_PHONE_STATE).
     */
    fun openRoute(route: String): PcmInjector? {
        lastError = ""
        return when (route) {
            "telephony_tx" -> UplinkRoutes.ALL.firstNotNullOfOrNull { openRoute(it) }
            "tx_interception" -> (callInterception("getCallUplinkInjectionAudioTrack") as? AudioTrack)
                ?.let { TrackInjector("tx_interception", it) }
            "tx_voice" -> preferredTx("tx_voice", AudioAttributes.USAGE_VOICE_COMMUNICATION)
            "tx_music" -> preferredTx("tx_music", AudioAttributes.USAGE_MEDIA)
            "voip_mix" -> voipMix()?.first
            else -> null.also { lastError = "unknown route" }
        }
    }

    private fun preferredTx(name: String, usage: Int): PcmInjector? {
        val dev = telephonyTxDevice() ?: return null.also { if (lastError.isEmpty()) lastError = "no TELEPHONY output device" }
        return try {
            val t = AudioTrack.Builder()
                .setAudioAttributes(AudioAttributes.Builder().setUsage(usage).build())
                .setAudioFormat(fmtOut()).setBufferSizeInBytes(RATE / 5 * 2).build()
            if (t.state != AudioTrack.STATE_INITIALIZED) {
                t.release()
                lastError = "track not initialised"
                return null
            }
            if (!t.setPreferredDevice(dev)) {
                // Without the preferred device the track would play out of the phone's own route (speaker, earpiece
                // or headphones at media volume): never use it.
                t.release()
                lastError = "preferred device refused"
                return null
            }
            TrackInjector(name, t)
        } catch (e: RuntimeException) {
            note(e)
        }
    }

    /** "Test speaking" during a call: beeps through each route (see [UplinkTest]). */
    fun uplinkTest(): String {
        if (audioManager?.mode != AudioManager.MODE_IN_CALL) return "{\"uplinkTest\":[],\"error\":\"no_call\"}"
        return UplinkTest(::openRoute, { lastError }, ::openCapture).run()
    }

    /**
     * The scrcpy #3880 technique, re-implemented from the public description: a dynamic AudioPolicy mix with the
     * INJECTOR role that loops our track into every recorder using capture preset MIC or VOICE_COMMUNICATION. It
     * needs MODIFY_AUDIO_ROUTING (shell has it). It feeds apps that record (VoIP); a cellular call's uplink does not
     * go through an AudioRecord, so it is not expected to reach the far party of a carrier call.
     */
    fun voipMix(): Pair<PcmInjector, Any>? {
        val am = audioManager ?: return null.also { lastError = "no AudioManager" }
        return try {
            val ruleB = Class.forName("android.media.audiopolicy.AudioMixingRule\$Builder")
            val mixB = Class.forName("android.media.audiopolicy.AudioMix\$Builder")
            val policyB = Class.forName("android.media.audiopolicy.AudioPolicy\$Builder")
            val mixingRule = Class.forName("android.media.audiopolicy.AudioMixingRule")
            val audioMix = Class.forName("android.media.audiopolicy.AudioMix")
            val policyC = Class.forName("android.media.audiopolicy.AudioPolicy")
            val rule = ruleB.getConstructor().newInstance()
            val setPreset = AudioAttributes.Builder::class.java.getMethod("setCapturePreset", Int::class.javaPrimitiveType)
            val matchPreset = mixingRule.getField("RULE_MATCH_ATTRIBUTE_CAPTURE_PRESET").getInt(null)
            for (p in listOf(MediaRecorder.AudioSource.MIC, MediaRecorder.AudioSource.VOICE_COMMUNICATION)) {
                val ab = AudioAttributes.Builder()
                setPreset.invoke(ab, p)
                ruleB.getMethod("addMixRule", Int::class.javaPrimitiveType, Any::class.java).invoke(rule, matchPreset, ab.build())
            }
            val injector = mixingRule.getField("MIX_ROLE_INJECTOR").getInt(null)
            ruleB.getMethod("setTargetMixRole", Int::class.javaPrimitiveType).invoke(rule, injector)
            val builtRule = ruleB.getMethod("build").invoke(rule)
            val mb = mixB.getConstructor(mixingRule).newInstance(builtRule)
            mixB.getMethod("setFormat", AudioFormat::class.java).invoke(mb, fmtIn())
            mixB.getMethod("setRouteFlags", Int::class.javaPrimitiveType).invoke(mb, audioMix.getField("ROUTE_FLAG_LOOP_BACK").getInt(null))
            val mix = mixB.getMethod("build").invoke(mb)
            val pb = policyB.getConstructor(Context::class.java).newInstance(context)
            policyB.getMethod("addMix", audioMix).invoke(pb, mix)
            val policy = policyB.getMethod("build").invoke(pb)
            val rc = AudioManager::class.java.getMethod("registerAudioPolicy", policyC).invoke(am, policy) as Int
            if (rc != 0) {
                lastError = "registerAudioPolicy=$rc"
                return null
            }
            val track = policyC.getMethod("createAudioTrackSource", audioMix).invoke(policy, mix) as AudioTrack
            val unregister = AutoCloseable {
                try {
                    AudioManager::class.java.getMethod("unregisterAudioPolicy", policyC).invoke(am, policy)
                } catch (_: ReflectiveOperationException) {
                }
            }
            TrackInjector("voip_mix", track, unregister) to policy
        } catch (e: ReflectiveOperationException) {
            note(e)
        } catch (e: RuntimeException) {
            note(e)
        }
    }

    private fun goertzel(x: ShortArray, n: Int, hz: Double): Double {
        val c = 2 * cos(2 * PI * hz / RATE)
        var s1 = 0.0
        var s2 = 0.0
        for (i in 0 until n) {
            val s0 = x[i] + c * s1 - s2
            s2 = s1
            s1 = s0
        }
        return s1 * s1 + s2 * s2 - c * s1 * s2
    }

    private fun q(s: String) = "\"" + s.replace("\\", "\\\\").replace("\"", "'") + "\""

    /** Open [name] for [ms], return one JSON object: outcome, level, and whether the system silenced us. */
    private fun probeSource(name: String, ms: Int = 600): String {
        lastError = ""
        val cap = openCapture(name) ?: return "{\"source\":${q(name)},\"outcome\":\"refused\",\"error\":${q(lastError)}}"
        val buf = ShortArray(HelperProtocol.FRAME_SAMPLES)
        var n = 0L
        var peak = 0
        var sum = 0.0
        var silenced = false
        var err = 0
        val window = ShortArray(RATE / 2)
        var w = 0
        val end = System.nanoTime() + ms * 1_000_000L
        while (System.nanoTime() < end) {
            val r = cap.read(buf)
            if (r < 0) {
                err = r
                break
            }
            for (i in 0 until r) {
                window[w % window.size] = buf[i]
                w++
                sum += buf[i].toDouble() * buf[i]
                peak = maxOf(peak, kotlin.math.abs(buf[i].toInt()))
            }
            n += r
            if (cap.silenced()) silenced = true
        }
        cap.close()
        val rms = if (n == 0L) 0.0 else kotlin.math.sqrt(sum / n)
        val outcome = when {
            err < 0 -> "read_error"
            n == 0L -> "no_samples"
            peak > 0 -> "audio"
            silenced -> "silenced"
            else -> "silent"
        }
        val hz = if (peak > 0) dominantHz(window, minOf(w, window.size)) else 0
        return "{\"source\":${q(name)},\"outcome\":\"$outcome\",\"samples\":$n,\"rms\":${rms.toInt()},\"peak\":$peak," +
            "\"hz\":$hz,\"silenced\":$silenced,\"readError\":$err}"
    }

    /** Strongest frequency 100..4000 Hz (10 Hz steps). Numbers only; the samples are dropped. */
    private fun dominantHz(x: ShortArray, n: Int): Int {
        var best = 0
        var bp = -1.0
        var f = 100
        while (f <= 4000) {
            val p = goertzel(x, n, f.toDouble())
            if (p > bp) {
                bp = p
                best = f
            }
            f += 10
        }
        return best
    }

    /**
     * During a call: does audio written to the call-uplink injection track show up in what the platform reports
     * as the uplink? Plays 1 kHz through getCallUplinkInjectionAudioTrack while recording VOICE_UPLINK, and
     * compares the 1 kHz power with the same recording without injection. On a real phone the decisive test is the
     * far party hearing it; this is the on-device half.
     */
    private fun uplinkLoopTest(): String {
        lastError = ""
        fun power1k(withTone: Boolean): Double? {
            val track = if (withTone) (callInterception("getCallUplinkInjectionAudioTrack") as? AudioTrack ?: return null) else null
            val rec = openCapture("VOICE_UPLINK") ?: return null.also { track?.release() }
            track?.play()
            val tone = ToneCapture(1000.0, paced = false, amplitude = 0.12, lengthSamples = RATE * 85L / 100)
            val out = ShortArray(HelperProtocol.FRAME_SAMPLES)
            val window = ShortArray(RATE)
            var w = 0
            val end = System.nanoTime() + 900_000_000L
            val inBuf = ShortArray(HelperProtocol.FRAME_SAMPLES)
            while (System.nanoTime() < end) {
                if (track != null) {
                    tone.read(out)
                    track.write(out, 0, out.size, AudioTrack.WRITE_NON_BLOCKING)
                }
                val r = rec.read(inBuf)
                if (r <= 0) break
                for (i in 0 until r) {
                    window[w % window.size] = inBuf[i]
                    w++
                }
            }
            rec.close()
            track?.let {
                try {
                    it.stop()
                } catch (_: IllegalStateException) {
                }
                it.release()
            }
            val n = minOf(w, window.size)
            return if (n == 0) null else goertzel(window, n, 1000.0) / n
        }
        val base = power1k(false) ?: return "{\"outcome\":\"no_uplink_capture\",\"error\":${q(lastError)}}"
        val inj = power1k(true) ?: return "{\"outcome\":\"no_injection_track\",\"error\":${q(lastError)}}"
        val ratio = inj / (base + 1.0)
        return "{\"outcome\":\"${if (ratio > 10) "tone_in_uplink" else "tone_not_seen"}\"," +
            "\"ratio\":${String.format(Locale.ROOT, "%.1f", ratio)}}"
    }

    /** Register the VoIP mix, play a 1 kHz tone into it and listen with a VOICE_COMMUNICATION recorder. */
    private fun voipMixSelfTest(): String {
        lastError = ""
        val (inj, _) = voipMix() ?: return "{\"route\":\"voip_mix\",\"outcome\":\"refused\",\"error\":${q(lastError)}}"
        val tone = ToneCapture(1000.0, paced = false, amplitude = 0.12, lengthSamples = RATE * 14L / 10)
        val rec = openCapture("VOICE_COMMUNICATION")
        if (rec == null) {
            inj.close()
            return "{\"route\":\"voip_mix\",\"outcome\":\"registered_but_no_recorder\",\"error\":${q(lastError)}}"
        }
        val out = ShortArray(HelperProtocol.FRAME_SAMPLES)
        val inBuf = ShortArray(HelperProtocol.FRAME_SAMPLES)
        val window = ShortArray(RATE)
        var w = 0
        val writer = Thread {
            val end = System.nanoTime() + 1_500_000_000L
            while (System.nanoTime() < end) {
                tone.read(out)
                inj.write(out)
                Thread.sleep(19)
            }
        }
        writer.start()
        val end = System.nanoTime() + 1_400_000_000L
        while (System.nanoTime() < end) {
            val r = rec.read(inBuf)
            if (r <= 0) break
            for (i in 0 until r) {
                window[w % window.size] = inBuf[i]
                w++
            }
        }
        writer.join()
        rec.close()
        inj.close()
        val n = minOf(w, window.size)
        val at1k = goertzel(window, n, 1000.0)
        val at700 = goertzel(window, n, 700.0)
        val ok = n > 0 && at1k > 100 * (at700 + 1)
        return "{\"route\":\"voip_mix\",\"outcome\":\"${if (ok) "tone_received" else "no_tone"}\",\"samples\":$n," +
            "\"rms\":${Levels.rms(window, n).toInt()},\"ratio1kTo700\":${String.format(Locale.ROOT, "%.1f", at1k / (at700 + 1))}}"
    }

    private fun interceptable(): String {
        val am = audioManager ?: return "null"
        return try {
            AudioManager::class.java.getMethod("isPstnCallAudioInterceptable").invoke(am).toString()
        } catch (e: ReflectiveOperationException) {
            q((note(e) ?: "") + lastError)
        } catch (e: RuntimeException) {
            q((note(e) ?: "") + lastError)
        }
    }

    fun probe(): String {
        val mode = audioManager?.mode ?: -1
        val sources = listOf("VOICE_DOWNLINK", "VOICE_UPLINK", "VOICE_CALL", "REMOTE_SUBMIX", "MIC", "VOICE_COMMUNICATION", "DOWNLINK_EXTRACTION")
            .joinToString(",") { probeSource(it) }
        lastError = ""
        val tx = telephonyTxDevice()
        val txNote = lastError
        lastError = ""
        val inj = (callInterception("getCallUplinkInjectionAudioTrack") as? AudioTrack)
        val injOutcome = if (inj != null) "track" else "refused"
        inj?.release()
        val injErr = lastError
        return "{\"v\":1,\"uid\":${Process.myUid()},\"sdk\":${Build.VERSION.SDK_INT},\"device\":${q(Build.MODEL)}," +
            "\"audioMode\":$mode,\"sources\":[$sources]," +
            "\"telephonyTxDevice\":${tx != null},\"telephonyTxNote\":${q(txNote)}," +
            "\"pstnInterceptable\":${interceptable()}," +
            "\"uplinkInjectionTrack\":{\"outcome\":\"$injOutcome\",\"error\":${q(injErr)}}," +
            "\"uplinkLoop\":${if (mode == AudioManager.MODE_IN_CALL) uplinkLoopTest() else "null"}," +
            "\"voipMix\":${voipMixSelfTest()}}"
    }

    @JvmStatic
    fun main(args: Array<String>) {
        val a = args.toList()
        fun arg(k: String) = a.indexOf(k).takeIf { it >= 0 }?.let { a.getOrNull(it + 1) }
        // AudioPolicy and AudioManager want a main looper for their event handlers.
        if (android.os.Looper.getMainLooper() == null) android.os.Looper.prepareMainLooper()
        if ("--probe" in a) {
            println(probe())
            System.out.flush()
            // Audio framework threads are not daemons; without this the process outlives its answer.
            System.exit(0)
        }
        val token = arg("--token") ?: return System.err.println("--token is required")
        // The app (an untrusted_app) cannot connect to a socket owned by the shell: SELinux denies `connectto`
        // (measured on a user-build emulator). So the helper connects OUT to the app's loopback listener instead.
        // The app learns the port from its own listener, hands it (with the token) to the enable script, and accepts
        // only a peer whose first frame carries that token.
        arg("--connect")?.let { p ->
            val port = p.toIntOrNull()?.takeIf { it in 1024..65535 } ?: return System.err.println("bad port")
            if (!Regex("[A-Za-z0-9_-]{8,64}").matches(token)) return System.err.println("bad token")
            val sock = java.net.Socket()
            sock.connect(java.net.InetSocketAddress(java.net.InetAddress.getByName("127.0.0.1"), port), 5000)
            sock.tcpNoDelay = true
            val hello = "{\"v\":1,\"uid\":${Process.myUid()},\"sdk\":${Build.VERSION.SDK_INT},\"token\":\"$token\"}"
            println("connected $port uid=${Process.myUid()} sdk=${Build.VERSION.SDK_INT}")
            System.out.flush()
            HelperSession(sock.getInputStream(), sock.getOutputStream(), token, hello, ::openCapture, ::openInjector, ::probe, uplinkTest = ::uplinkTest).run()
            sock.close()
            System.exit(0)
        }
        val name = arg("--listen") ?: return System.err.println("usage: --probe | --connect PORT --token T | --listen NAME --token T [--app-uid U]")
        val ok = Regex("[A-Za-z0-9_-]{8,64}")
        if (!ok.matches(name) || !ok.matches(token)) return System.err.println("bad name or token")
        val appUid = arg("--app-uid")?.toIntOrNull()
        val server = LocalServerSocket(name)
        println("listening $name uid=${Process.myUid()} sdk=${Build.VERSION.SDK_INT}")
        System.out.flush()
        while (true) {
            val s = server.accept()
            val peer = try {
                s.peerCredentials.uid
            } catch (_: java.io.IOException) {
                -1
            }
            if (peer != 0 && peer != 2000 && peer != appUid) {
                s.close()
                continue
            }
            val hello = "{\"v\":1,\"uid\":${Process.myUid()},\"sdk\":${Build.VERSION.SDK_INT},\"peerUid\":$peer}"
            HelperSession(s.inputStream, s.outputStream, token, hello, ::openCapture, ::openInjector, ::probe, uplinkTest = ::uplinkTest).run()
            s.close()
            if ("--once" in a) return
        }
    }
}
