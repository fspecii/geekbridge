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
     * made [ShareCoordinator] stop a link that had just been started (docs/e2e-findings.md, E2E-1).
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

    private fun rejected(o: CoreOutcome, verb: String): Notice = when (o) {
        is CoreOutcome.Rejected -> Notice("$verb: ${TransportText.error(o.code)}", Tone.Warn)
        is CoreOutcome.Unavailable -> Notice("$verb: native core unavailable (${o.reason}).", Tone.Bad)
        is CoreOutcome.Malformed -> Notice("$verb: unreadable native response (${o.reason}).", Tone.Bad)
        is CoreOutcome.Ok -> Notice("$verb: unexpected success.", Tone.Warn)
    }

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
        say("Starting the link…", Tone.Muted)
        changed()
        run(TransportProtocol.startRequest(options)) { o ->
            if (mine != epoch) return@run
            when (o) {
                is CoreOutcome.Ok -> {
                    phase = LinkPhase.Running
                    say("Link running. Nothing is paired or trusted until you approve a pairing.", Tone.Ok)
                    changed()
                    poll()
                }
                else -> {
                    phase = LinkPhase.Stopped
                    notice = rejected(o, "Could not start")
                    changed()
                }
            }
        }
    }

    /** Idempotent. Used by the Stop button and when the app leaves the foreground. */
    fun stop(reason: String? = null) {
        if (phase == LinkPhase.Stopped || phase == LinkPhase.Stopping) return
        // Leaving the foreground (reason != null) must not cut a running share; an explicit Stop still does.
        if (reason != null && needsLink()) return
        phase = LinkPhase.Stopping
        epoch++
        val mine = epoch
        snapshot = null
        say("Stopping the link…", Tone.Muted)
        changed()
        run(TransportProtocol.stopRequest()) { o ->
            if (mine != epoch) return@run
            phase = LinkPhase.Stopped
            notice = when (o) {
                is CoreOutcome.Ok -> Notice(
                    reason ?: "Link stopped. All connections and pending requests were closed.", Tone.Muted,
                )
                else -> rejected(o, "Stop")
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
                        say("Status: unreadable native response.", Tone.Warn)
                    } else {
                        apply(s)
                    }
                }
                else -> notice = rejected(o, "Status")
            }
            changed()
        }
    }

    private fun apply(s: TransportSnapshot) {
        if (!s.isRunning) {
            // The core says it is not running (e.g. it was stopped underneath us): reflect that.
            phase = LinkPhase.Stopped
            snapshot = null
            say("The native transport is not running.", Tone.Warn)
            return
        }
        snapshot = s
        for (e in s.events) {
            if (e.seq > lastSeq) {
                events += e
                if (TransportText.isSecurity(e.code) || e.kind == "peer_requested_repair") {
                    say(TransportText.event(e), Tone.Bad)
                }
            }
        }
        while (events.size > MAX_EVENTS) events.removeAt(0)
        if (s.lastEventSeq > lastSeq) lastSeq = s.lastEventSeq
        answered.retainAll(s.prompts.map { it.requestId }.toSet())
    }

    /** The first prompt that can be shown exactly and has not been answered yet. */
    fun pendingPrompt(): PairPrompt? = snapshot?.prompts?.firstOrNull { it.requestId !in answered }

    private fun command(request: String, verb: String, success: (CoreOutcome.Ok) -> Notice?) {
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
        TransportProtocol.connectRequest(host.trim(), port, deviceId.trim()), "Connect",
    ) { Notice("Connecting… (TLS handshake only; nothing is paired yet).", Tone.Muted) }

    fun pairRequest(connectionId: Long, await: Boolean) = command(
        TransportProtocol.pairRequest(connectionId, await), "Pair",
    ) {
        Notice(
            if (await) "Waiting for the other device to ask to pair…" else "Pairing request sent. Waiting for the code…",
            Tone.Muted,
        )
    }

    /**
     * Sends the user's choice for exactly [prompt], as it was displayed. "Approved" is only this phone's half:
     * the result is reported as waiting until a later status shows the connection trusted.
     */
    fun decide(prompt: PairPrompt, approve: Boolean) {
        if (phase != LinkPhase.Running) return
        answered += prompt.requestId
        command(TransportProtocol.decisionRequest(prompt, approve), if (approve) "Approve" else "Decline") {
            if (approve) {
                Notice("Approved on this phone. Waiting for the other device; not paired until it also accepts.", Tone.Muted)
            } else {
                Notice("Pairing declined. Nothing was trusted.", Tone.Muted)
            }
        }
    }

    fun unpair(deviceId: String) = command(TransportProtocol.unpairRequest(deviceId), "Unpair") {
        Notice("Unpaired. The stored certificate for that device was removed from this phone.", Tone.Muted)
    }

    fun disconnect(connectionId: Long) = command(TransportProtocol.disconnectRequest(connectionId), "Disconnect") {
        Notice("Disconnected. Pairing state was not changed.", Tone.Muted)
    }

    fun ping(connectionId: Long) = command(TransportProtocol.pingRequest(connectionId), "Ping") {
        Notice("Ping queued. It is the only message this build can send.", Tone.Muted)
    }

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
                ).also { instance = it }
            }
    }
}
