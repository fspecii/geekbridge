// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.media.AudioManager
import android.util.Base64
import dev.geekbridge.android.CoreClient
import dev.geekbridge.android.CoreOutcome
import dev.geekbridge.android.CoreProtocol
import dev.geekbridge.android.R
import org.json.JSONArray
import org.json.JSONObject
import java.nio.ByteBuffer
import java.nio.ByteOrder

/*
 * DEBUG BUILDS ONLY, EXPERIMENT (docs/design/call-audio-over-wifi.md). Bluetooth stays the primary call-audio path.
 *
 * The phone side of "Call audio over Wi-Fi (ADB helper)": owns the connection to the shell-uid helper, the self-test,
 * the per-call notification and the loop that moves the far party's audio from the helper into the Rust hub (which
 * holds the consent gate, the codec and the link). Off by default; nothing here runs until the user turns the
 * switch on. Audio is relayed frame by frame and never stored or logged; the log carries codes and counts only.
 */
@SuppressLint("StaticFieldLeak") // only the application context is held
object CallWifiRuntime {
    private const val FILE = "geekbridge_callwifi"
    private const val CHANNEL = "callwifi"
    private const val NOTIFICATION_ID = 0x4357
    const val ACTION_STOP_SESSION = "dev.geekbridge.callwifi.STOP_SESSION"

    /** Both ends of the app/Rust boundary in one place so a JVM test can swap the core. */
    @Volatile internal var core: ((String) -> CoreOutcome)? = null

    @Volatile private var app: Context? = null
    @Volatile private var prefs: SharedPreferences? = null
    private val lock = Any()

    @Volatile var enabled = false
        private set
    @Volatile var speakOptIn = false
        private set
    @Volatile var verdict: Verdict = Verdict.NotRun
        private set
    @Volatile var source: String = "VOICE_DOWNLINK"
        private set
    @Volatile var uplink: String = "none"
        private set

    @Volatile var helperUp = false
        private set

    /** Plain-words line for the screen: what the self-test or the helper is doing right now. */
    @Volatile var progress: String = ""
        private set

    /** The computer a live session is shared with, or null. */
    @Volatile var sharingWith: String? = null
        private set
    @Volatile var speaking = false
        private set

    private var conn: HelperConnection? = null
    private var control: Map<String, Capture> = emptyMap()
    private var loop: Thread? = null
    @Volatile private var testRunning = false
    @Volatile private var lastCallMode = false
    private var injectThread: Thread? = null

    private val listeners = java.util.concurrent.CopyOnWriteArrayList<() -> Unit>()
    fun addListener(l: () -> Unit) { listeners += l }
    fun removeListener(l: () -> Unit) { listeners -= l }
    private fun changed() { listeners.forEach { runCatching(it) } }

    private fun client(): (String) -> CoreOutcome {
        core?.let { return it }
        val c = CoreClient.native()
        return { c.transportAction(it) }
    }

    private fun request(action: String, build: JSONObject.() -> Unit = {}): CoreOutcome =
        client()(JSONObject().put("version", CoreProtocol.VERSION).put("action", action).apply(build).toString())

    @Synchronized
    fun init(ctx: Context) {
        if (app != null) return
        val a = ctx.applicationContext
        app = a
        val p = a.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        prefs = p
        enabled = p.getBoolean("enabled", false)
        speakOptIn = p.getBoolean("speak", false)
        verdict = Verdict.fromWire(p.getString("verdict", null))
        source = p.getString("source", null) ?: "VOICE_DOWNLINK"
        uplink = p.getString("uplink", null) ?: "none"
        // The helper does not survive a reboot or a process death with us; a stored verdict still says what to expect.
        if (enabled) ensureLoop()
    }

    fun setEnabled(on: Boolean) {
        enabled = on
        prefs?.edit()?.putBoolean("enabled", on)?.apply()
        if (!on) {
            endSession()
            disconnect()
        } else {
            ensureLoop()
        }
        pushConfig()
        changed()
    }

    fun setSpeak(on: Boolean) {
        speakOptIn = on
        prefs?.edit()?.putBoolean("speak", on)?.apply()
        if (!on) stopSpeaking()
        pushConfig()
        changed()
    }

    // ---- helper connection ---------------------------------------------------------------------------------

    private var listener: java.net.ServerSocket? = null

    /**
     * The enable script asks where the helper should connect. The app listens on 127.0.0.1 (random port) and makes a
     * token; the helper (shell uid) connects out, because SELinux does not let an app connect to a shell socket. Only
     * a peer whose first frame carries the token is accepted. Null if the switch is off. The listener closes after
     * 90 s or after one helper has connected.
     */
    fun openListener(): Pair<Int, String>? {
        if (!enabled) return null
        val token = "t" + java.security.SecureRandom().let { r -> ByteArray(16).also(r::nextBytes) }.joinToString("") { "%02x".format(it) }
        val server = try {
            java.net.ServerSocket(0, 1, java.net.InetAddress.getByName("127.0.0.1"))
        } catch (_: java.io.IOException) {
            return null
        }
        synchronized(lock) {
            runCatching { listener?.close() }
            listener = server
        }
        server.soTimeout = 90_000
        Thread({ acceptHelper(server, token) }, "gb-callwifi-accept").apply { isDaemon = true; start() }
        progress = "Waiting for the helper to connect..."
        changed()
        return server.localPort to token
    }

    private fun acceptHelper(server: java.net.ServerSocket, token: String) {
        try {
            while (enabled && !server.isClosed) {
                val s = server.accept()
                s.soTimeout = 5_000
                val hello = try {
                    val f = HelperProtocol.read(s.getInputStream())
                    (f as? HelperProtocol.Frame.Text)?.takeIf { it.type == HelperProtocol.HELLO }?.text
                } catch (_: java.io.IOException) {
                    null
                }
                val ok = hello != null && Regex("\"token\"\\s*:\\s*\"([A-Za-z0-9_-]+)\"").find(hello)?.groupValues?.get(1)
                    ?.let { java.security.MessageDigest.isEqual(it.toByteArray(), token.toByteArray()) } == true
                if (!ok) {
                    s.close()
                    continue
                }
                s.soTimeout = 0
                val c = HelperConnection(s.getInputStream(), s.getOutputStream(), token, hello)
                c.listener = quietListener
                c.start()
                synchronized(lock) {
                    disconnectLocked()
                    conn = c
                }
                helperUp = true
                progress = "Helper connected."
                pushConfig()
                changed()
                return
            }
        } catch (_: java.io.IOException) {
            progress = "The helper did not connect in time. Run scripts/callaudio-enable.sh again."
            changed()
        } finally {
            runCatching { server.close() }
        }
    }

    fun disconnect() {
        synchronized(lock) { disconnectLocked() }
        helperUp = false
        pushConfig()
        changed()
    }

    private fun disconnectLocked() {
        conn?.close()
        conn = null
    }

    /** The user pressed Stop (notification or screen), or the helper went away. */
    fun endSession() {
        stopSpeaking()
        runCatching { request("transport_callwifi_stop") }
        sessionEnded()
    }

    private fun sessionEnded() {
        conn?.let {
            it.listener = quietListener
            it.command("stop")
        }
        sharingWith = null
        speaking = false
        cancelNotification()
        changed()
    }

    private val quietListener = object : HelperConnection.Listener {
        override fun onPcm(samples: ShortArray, cap: Int) {}
        override fun onStatus(json: String) {}
        override fun onClosed() {
            helperUp = false
            pushConfig()
            changed()
        }
    }

    // ---- config to the Rust hub ----------------------------------------------------------------------------

    fun pushConfig() {
        runCatching {
            request("transport_callwifi_config") {
                put("enabled", enabled)
                put("helper_up", helperUp)
                put("verdict", verdict.wire)
                put("speak_allowed", speakOptIn && uplink != "none")
                put("uplink", if (uplink == "none") "none" else "telephony_tx")
            }
        }
    }

    // ---- self-test -----------------------------------------------------------------------------------------

    fun callActive(): Boolean {
        val am = app?.getSystemService(AudioManager::class.java) ?: return false
        return am.mode == AudioManager.MODE_IN_CALL
    }

    /** Step 1, no call: what the sources deliver by themselves. Runs on its own thread; progress via [progress]. */
    fun runControl() {
        val c = conn
        if (c == null || !helperUp) return say("Start the helper first (scripts/callaudio-enable.sh).")
        if (sharingWith != null || testRunning) return say("Wait until the current test or sharing is finished.")
        if (callActive()) return say("A call is active. Hang up, then run this check, so it can tell silence from the call.")
        testRunning = true
        Thread({
            try {
                say("Listening for a few seconds with no call...")
                control = SelfTestRunner(HelperCapturePort(c)).control()
                val loud = control.values.any { it.opened && it.rms >= SelfTestVerdict.SILENT_RMS }
                say(
                    if (loud) "Without a call the phone still produced sound on a call source. That cannot be the other person, so the test may end as Unsure."
                    else "Good: with no call the call sources are silent or refused. Now make a call and press step 2.",
                )
            } finally {
                testRunning = false
                changed()
            }
        }, "gb-callwifi-selftest").apply { isDaemon = true; start() }
    }

    /** Step 2: wait for a call (up to a minute), record the sources for a few seconds and say what happened. */
    fun runCallTest() {
        val c = conn
        if (c == null || !helperUp) return say("Start the helper first (scripts/callaudio-enable.sh).")
        if (sharingWith != null || testRunning) return say("Wait until the current test or sharing is finished.")
        testRunning = true
        Thread({
            try {
                var waited = 0
                while (!callActive() && waited < 60) {
                    say("Waiting for a call... start one now (${60 - waited} s left).")
                    Thread.sleep(1000)
                    waited++
                }
                if (!callActive()) return@Thread say("No call within a minute. Nothing was recorded.")
                say("Recording the call source for a few seconds. Ask the other person to talk.")
                val r = SelfTestRunner(HelperCapturePort(c)).duringCall(control)
                verdict = r.verdict
                source = r.source ?: source
                uplink = if (r.uplink == "none") "none" else "telephony_tx"
                prefs?.edit()?.putString("verdict", verdict.wire)?.putString("source", source)?.putString("uplink", uplink)?.apply()
                say(describe(r.verdict, control.isEmpty()))
                pushConfig()
            } finally {
                testRunning = false
                changed()
            }
        }, "gb-callwifi-selftest").apply { isDaemon = true; start() }
    }

    fun describe(v: Verdict, noControl: Boolean): String = when (v) {
        Verdict.Works -> "Works: the other person's voice reaches the helper." + if (noControl) " (No no-call check was run first, so this is less certain.)" else ""
        Verdict.Silent -> "Silent: this phone opens the call source but delivers silence. Samsung may block it. Bluetooth stays the way to hear calls."
        Verdict.Refused -> "Refused: Android denied the capture. This phone does not allow it."
        Verdict.Unsure -> "Unsure: the phone makes the same sound with no call, so this proves nothing. Treat it as not working."
        Verdict.NotRun -> "Not run yet."
    }

    private fun say(s: String) {
        progress = s
        changed()
    }

    // ---- the loop: events from the hub, call state ---------------------------------------------------------

    @Synchronized
    private fun ensureLoop() {
        if (loop?.isAlive == true) return
        loop = Thread({ runLoop() }, "gb-callwifi-loop").apply { isDaemon = true; start() }
    }

    private fun runLoop() {
        var since = 0L
        var lastPush = 0L
        while (enabled) {
            val now = System.currentTimeMillis()
            if (now - lastPush > 5_000) {
                pushConfig()
                lastPush = now
            }
            val mode = callActive()
            if (mode != lastCallMode) {
                lastCallMode = mode
                runCatching { request("transport_callwifi_call") { put("active", mode) } }
            }
            val r = request("transport_callwifi_wait") {
                put("since", since)
                put("timeout_ms", 1000)
            }
            if (r !is CoreOutcome.Ok) {
                Thread.sleep(1000)
                continue
            }
            val o = r.result as? JSONObject ?: continue
            val evs: JSONArray = o.optJSONArray("events") ?: JSONArray()
            for (i in 0 until evs.length()) {
                val e = evs.optJSONObject(i) ?: continue
                since = maxOf(since, e.optLong("seq"))
                handle(e)
            }
            if (o.optBoolean("stopped")) Thread.sleep(1000)
        }
    }

    private fun handle(e: JSONObject) {
        when (e.optString("ev")) {
            "start" -> startCapture(e.optString("computer"))
            "indicator" -> if (e.optBoolean("on")) {
                sharingWith = e.optString("computer").ifEmpty { "your computer" }
                showNotification(sharingWith!!)
                changed()
            }
            "stop" -> sessionEnded()
            "speak" -> if (e.optBoolean("on")) startSpeaking() else stopSpeaking()
        }
    }

    private fun startCapture(computer: String) {
        val c = conn ?: return endSession()
        sharingWith = computer.ifEmpty { "your computer" }
        var n = 0
        c.listener = object : HelperConnection.Listener {
            override fun onPcm(samples: ShortArray, cap: Int) {
                val b = ByteBuffer.allocate(samples.size * 2).order(ByteOrder.LITTLE_ENDIAN)
                for (s in samples) b.putShort(s)
                val pcm = Base64.encodeToString(b.array(), Base64.NO_WRAP)
                request("transport_callwifi_frame") {
                    put("pcm", pcm)
                    put("cap", cap.toLong() and 0xFFFFFFFFL)
                }
                n++
            }
            override fun onStatus(json: String) {
                if (json.contains("\"error\"")) endSession()
            }
            override fun onClosed() = quietListener.onClosed()
        }
        c.command("start", "source" to source, "inject" to "none")
    }

    private fun startSpeaking() {
        val c = conn ?: return
        if (!speakOptIn || uplink == "none") return
        // Restart the capture with the uplink route open; the helper stops the previous capture first.
        c.command("start", "source" to source, "inject" to "telephony_tx")
        speaking = true
        injectThread?.interrupt()
        injectThread = Thread({
            var seq = 0
            try {
                while (speaking && !c.closed) {
                    val r = request("transport_callwifi_pull")
                    val frames = ((r as? CoreOutcome.Ok)?.result as? JSONObject)?.optJSONArray("frames")
                    if (frames != null) {
                        for (i in 0 until frames.length()) {
                            val bytes = Base64.decode(frames.optString(i), Base64.NO_WRAP)
                            if (bytes.size != 640) continue
                            val sh = ShortArray(320)
                            ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).asShortBuffer().get(sh)
                            c.inject(seq++, sh)
                        }
                    }
                    Thread.sleep(20)
                }
            } catch (_: InterruptedException) {
            }
        }, "gb-callwifi-inject").apply { isDaemon = true; start() }
        changed()
    }

    private fun stopSpeaking() {
        if (!speaking) return
        speaking = false
        injectThread?.interrupt()
        injectThread = null
        // Back to listening only: a capture restart without the uplink route.
        conn?.takeIf { sharingWith != null }?.command("start", "source" to source, "inject" to "none")
        changed()
    }

    // ---- the per-call notification -------------------------------------------------------------------------

    private fun showNotification(computer: String) {
        val a = app ?: return
        val nm = a.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, "Call audio sharing", NotificationManager.IMPORTANCE_HIGH))
        val stop = PendingIntent.getBroadcast(
            a, 0, Intent(a, CallWifiStopReceiver::class.java).setAction(ACTION_STOP_SESSION),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val n = Notification.Builder(a, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_link)
            .setContentTitle("Call audio is being shared with $computer")
            .setContentText("Over Wi-Fi, experimental. Tap Stop to end it now.")
            .setOngoing(true)
            .setCategory(Notification.CATEGORY_CALL)
            .addAction(Notification.Action.Builder(null, "Stop", stop).build())
            .build()
        try {
            nm.notify(NOTIFICATION_ID, n)
        } catch (_: SecurityException) {
            // Notifications are denied: the phone cannot show the indicator, so it must not share.
            endSession()
        }
    }

    private fun cancelNotification() {
        app?.getSystemService(NotificationManager::class.java)?.cancel(NOTIFICATION_ID)
    }
}
