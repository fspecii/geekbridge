// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.util.concurrent.Executor

enum class LinkPhase { Stopped, Starting, Running, Stopping }

data class Notice(val text: String, val tone: Tone)

/**
 * Owns the lifecycle and state of the Rust phone transport for the whole process (it outlives an Activity so
 * rotation does not restart the link). All state is read and written on the main thread; every native call
 * runs on [execute] (one thread, so calls are serialised) and its result comes back through [postToMain].
 *
 * Nothing here starts by itself: [start] is only called from an explicit user action, and nothing here
 * decides pairing: [decide] forwards exactly the prompt the user was shown.
 */
class TransportController(
    private val client: (String) -> CoreOutcome,
    private val execute: (Runnable) -> Unit,
    private val postToMain: (Runnable) -> Unit,
    private val strings: Strings,
) {
    var phase: LinkPhase = LinkPhase.Stopped
        private set
    var snapshot: TransportSnapshot? = null
        private set
    var notice: Notice? = null
        private set

    /** Recent advisory events, newest last. Cleared on start. */
    val events = ArrayList<LinkEvent>()

    /** Request ids the user already answered; they are never offered again while the core still lists them. */
    private val answered = HashSet<Long>()
    private var lastSeq = 0L
    private var epoch = 0
    private var polling = false
    private var pollAgain = false

    /** Called on the main thread after any state change. */
    var listener: (() -> Unit)? = null

    /** Extra observers (share coordinator); called after [listener] on the main thread. */
    val observers = ArrayList<() -> Unit>()

    /** Number of share sessions that need the link to stay up even when no screen is visible. */
    var holds = 0
        private set
    /**
     * True while any screen of the app is started. The per-activity flag alone is not enough: moving
     * Home -> Diagnostics runs the new screen's onStart before the old one's onStop, which cleared the flag and
     * made [ShareCoordinator] stop a link that had just been started (docs/reports/e2e-findings.md, E2E-1).
     */
    var appVisible = false
        get() = field || ActivityTracker.startedCount > 0

    fun hold() { holds++ }
    fun release() { if (holds > 0) holds-- }

    /** True while a share needs the link: a session hold, or any transfer that has not finished. */
    fun needsLink(): Boolean = holds > 0 || snapshot?.transfers?.any { !it.state.finished } == true

    /** Runs an arbitrary core request on the worker; the outcome arrives on the main thread. */
    fun raw(request: String, thenMain: (CoreOutcome) -> Unit) = run(request, thenMain)

    private fun changed() {
        listener?.invoke()
        for (o in observers.toList()) o()
    }

    private fun say(text: String, tone: Tone) { notice = Notice(text, tone) }

    /** A local, UI-side message (e.g. a field was not filled in). */
    fun postNotice(text: String, tone: Tone) { say(text, tone); changed() }

    private fun rejected(o: CoreOutcome, verb: Int): Notice {
        val v = strings.get(verb)
        return when (o) {
            is CoreOutcome.Rejected ->
                Notice(strings.get(R.string.notice_rejected, v, TransportText.error(strings, o.code)), Tone.Warn)
            is CoreOutcome.Unavailable -> Notice(strings.get(R.string.notice_core_unavailable, v, o.reason), Tone.Bad)
            is CoreOutcome.Malformed -> Notice(strings.get(R.string.notice_core_unreadable, v, o.reason), Tone.Bad)
            is CoreOutcome.Ok -> Notice(strings.get(R.string.notice_unexpected_success, v), Tone.Warn)
        }
    }

    private fun say(id: Int, tone: Tone) = say(strings.get(id), tone)
    private fun notice(id: Int, tone: Tone = Tone.Muted) = Notice(strings.get(id), tone)

    /** Runs [request] on the worker thread and hands the outcome back on the main thread. */
    private fun run(request: String, thenMain: (CoreOutcome) -> Unit) {
        execute(Runnable {
            val o = try {
                client(request)
            } catch (e: RuntimeException) {
                CoreOutcome.Unavailable("worker failed (${e.javaClass.simpleName})")
            }
            postToMain(Runnable { thenMain(o) })
        })
    }

    fun start(options: StartOptions) {
        if (phase != LinkPhase.Stopped) return
        phase = LinkPhase.Starting
        epoch++
        val mine = epoch
        events.clear()
        answered.clear()
        lastSeq = 0
        snapshot = null
        say(R.string.notice_starting, Tone.Muted)
        changed()
        run(TransportProtocol.startRequest(options)) { o ->
            if (mine != epoch) return@run
            when (o) {
                is CoreOutcome.Ok -> {
                    phase = LinkPhase.Running
                    say(R.string.notice_running, Tone.Ok)
                    changed()
                    poll()
                }
                else -> {
                    GbLog.i("link", "start failed: ${o.javaClass.simpleName}")
                    phase = LinkPhase.Stopped
                    notice = rejected(o, R.string.verb_could_not_start)
                    changed()
                }
            }
        }
    }

    /** Idempotent. Used by the Stop button and when the app leaves the foreground. */
    fun stop(reasonRes: Int) = stop(strings.get(reasonRes))

    fun stop(reason: String? = null) {
        if (phase == LinkPhase.Stopped || phase == LinkPhase.Stopping) return
        // Leaving the foreground (reason != null) must not cut a running share; an explicit Stop still does.
        if (reason != null && needsLink()) return
        GbLog.i("link", "close: requested by this phone (${reason ?: "explicit stop"}), shares holding the link: $holds")
        phase = LinkPhase.Stopping
        epoch++
        val mine = epoch
        snapshot = null
        say(R.string.notice_stopping, Tone.Muted)
        changed()
        run(TransportProtocol.stopRequest()) { o ->
            if (mine != epoch) return@run
            phase = LinkPhase.Stopped
            notice = when (o) {
                is CoreOutcome.Ok -> Notice(reason ?: strings.get(R.string.notice_stopped), Tone.Muted)
                else -> rejected(o, R.string.verb_stop)
            }
            changed()
        }
    }

    /** Fetches one status snapshot; ignored if one is in flight or the link is not running. */
    fun poll() {
        if (phase != LinkPhase.Running || polling) return
        polling = true
        val mine = epoch
        run(TransportProtocol.statusRequest(lastSeq)) { o ->
            polling = false
            if (mine != epoch || phase != LinkPhase.Running) return@run
            when (o) {
                is CoreOutcome.Ok -> {
                    val s = TransportProtocol.parseSnapshot(o.result)
                    if (s == null) {
                        say(R.string.notice_status_unreadable, Tone.Warn)
                    } else {
                        apply(s)
                    }
                }
                else -> notice = rejected(o, R.string.verb_status)
            }
            changed()
            if (pollAgain) {
                pollAgain = false
                poll()
            }
        }
    }

    /** The core has events up to [seq]. A snapshot already in flight may predate them, so one more follows it. */
    fun eventsUpTo(seq: Long) {
        if (seq <= lastSeq) return
        if (polling) pollAgain = true else poll()
    }

    private fun apply(s: TransportSnapshot) {
        if (!s.isRunning) {
            // The core says it is not running (e.g. it was stopped underneath us): reflect that.
            phase = LinkPhase.Stopped
            snapshot = null
            say(R.string.notice_transport_not_running, Tone.Warn)
            return
        }
        snapshot = s
        for (e in s.events) {
            if (e.seq > lastSeq) {
                events += e
                GbLog.i("link", "event ${e.kind} code=${e.code ?: "-"}") // reason a link ended / was redialed
                if (TransportText.isSecurity(e.code) || e.kind == "peer_requested_repair") {
                    say(TransportText.event(strings, e), Tone.Bad)
                } else if (e.kind == "peer_unpaired") {
                    say(TransportText.event(strings, e), Tone.Warn)
                }
            }
        }
        while (events.size > MAX_EVENTS) events.removeAt(0)
        if (s.lastEventSeq > lastSeq) lastSeq = s.lastEventSeq
        answered.retainAll(s.prompts.map { it.requestId }.toSet())
    }

    /** The first prompt that can be shown exactly and has not been answered yet. */
    fun pendingPrompt(): PairPrompt? = snapshot?.prompts?.firstOrNull { it.requestId !in answered }

    private fun command(request: String, verb: Int, success: (CoreOutcome.Ok) -> Notice?) {
        if (phase != LinkPhase.Running) return
        val mine = epoch
        run(request) { o ->
            if (mine != epoch) return@run
            if (o is CoreOutcome.Ok) success(o)?.let { notice = it } else notice = rejected(o, verb)
            changed()
            poll()
        }
    }

    fun connect(host: String, port: Int, deviceId: String) = command(
        TransportProtocol.connectRequest(host.trim(), port, deviceId.trim()), R.string.verb_connect,
    ) { notice(R.string.notice_connecting) }

    fun pairRequest(connectionId: Long, await: Boolean) = command(
        TransportProtocol.pairRequest(connectionId, await), R.string.verb_pair,
    ) {
        notice(if (await) R.string.notice_pair_await else R.string.notice_pair_sent)
    }

    /**
     * Pair on a parked connection with a scanned QR code. The core checks the connection's certificate against the
     * pinned one *before* the token leaves the phone; [done] gets null on success or the core's error code.
     */
    fun qrPairRequest(connectionId: Long, token: String, certSha256: String, done: (String?) -> Unit) {
        if (phase != LinkPhase.Running) return done("not_running")
        val mine = epoch
        run(QrProtocol.pairRequest(connectionId, token, certSha256)) { o ->
            if (mine != epoch) return@run
            if (o is CoreOutcome.Ok) {
                notice = notice(R.string.notice_qr_checking)
                done(null)
            } else {
                notice = rejected(o, R.string.verb_pair)
                done((o as? CoreOutcome.Rejected)?.code ?: "unavailable")
            }
            changed()
            poll()
        }
    }

    /**
     * Sends the user's choice for exactly [prompt], as it was displayed. "Approved" is only this phone's half:
     * the result is reported as waiting until a later status shows the connection trusted.
     */
    fun decide(prompt: PairPrompt, approve: Boolean) {
        if (phase != LinkPhase.Running) return
        answered += prompt.requestId
        command(TransportProtocol.decisionRequest(prompt, approve), if (approve) R.string.verb_approve else R.string.verb_decline) {
            if (approve) {
                notice(R.string.notice_approved)
            } else {
                notice(R.string.notice_declined)
            }
        }
    }

    /** Applies a saved phone name to the running link: new connections and announcements use it. */
    fun rename(name: String) = command(TransportProtocol.setNameRequest(name), R.string.verb_rename) {
        notice(R.string.notice_renamed)
    }

    fun unpair(deviceId: String) = command(TransportProtocol.unpairRequest(deviceId), R.string.verb_unpair) {
        notice(R.string.notice_unpaired)
    }

    fun disconnect(connectionId: Long) = command(TransportProtocol.disconnectRequest(connectionId), R.string.verb_disconnect) {
        notice(R.string.notice_disconnected)
    }

    fun ping(connectionId: Long) = command(TransportProtocol.pingRequest(connectionId), R.string.verb_ping) {
        notice(R.string.notice_ping_queued)
    }

    /** Fire and forget: a failure only means the heartbeat keeps its normal cadence. */
    fun setScreen(on: Boolean) = run(TransportProtocol.screenRequest(on)) { }

    /** Fire and forget: pings every link now (a Doze maintenance window is open; the timer may not run again soon). */
    fun heartbeatNow() = run(TransportProtocol.heartbeatNowRequest()) { }

    /** Fire and forget: the `bind_wifi` kill switch ([AppState.bindWifi]); the core also marks a bad bind itself. */
    fun setWifiBindEnabled(on: Boolean) = run(TransportProtocol.wifiBindEnabledRequest(on)) { }

    /** Fire and forget: dials and announcements leave from this Wi-Fi address (null: unbound). See [WifiBindPolicy]. */
    fun setWifiBind(ip: String?) = run(TransportProtocol.wifiBindRequest(ip)) { }

    companion object {
        const val MAX_EVENTS = 20

        /** The process-wide instance, created on first use with its own worker thread. */
        @Volatile private var instance: TransportController? = null

        fun get(client: (String) -> CoreOutcome, postToMain: (Runnable) -> Unit): TransportController =
            instance ?: synchronized(this) {
                instance ?: TransportController(
                    client = client,
                    execute = java.util.concurrent.Executors.newSingleThreadExecutor { r ->
                        Thread(r, "geekbridge-transport").apply { isDaemon = true }
                    }::execute,
                    postToMain = postToMain,
                    strings = AppStrings,
                ).also { instance = it }
            }
    }
}
