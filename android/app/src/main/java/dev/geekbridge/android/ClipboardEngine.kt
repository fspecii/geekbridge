package dev.geekbridge.android

/** Everything the engine needs from Android. Implemented by `AndroidClipboardHost`; faked in tests. */
interface ClipboardHost {
    /** Keyguard locked, or the screen is off. */
    fun isLocked(): Boolean

    /** Put an item from the computer on the Android clipboard (allowed in the background). */
    fun write(item: ClipEvent.Incoming)

    /** Show "your computer wants your clipboard: tap to send". Must not reveal any content. */
    fun askForTap()
    fun dismissTapRequest()

    /**
     * Run [block] with a readable descriptor of the snapshot's image, a private copy no larger than
     * [maxBytes]. Null if the image can't be read or is too large. The descriptor is closed afterwards.
     */
    fun <T> withImageFd(snapshot: ClipSnapshot, maxBytes: Long, block: (Int) -> T): T?
}

/** Result shown to the user after a tap or an explicit send. */
data class SendResult(val sent: Boolean, val reason: String?) {
    val message: String get() = if (sent) "Sent to your computer." else ClipboardText.refusal(reason ?: "")
}

/**
 * The phone side of clipboard sync, with no Android types and no threads of its own: callers (the wait
 * loop, the activities) call in, and it calls [core] to answer. Nothing is ever sent unless a computer
 * asked and the [PullGate] allows it, or the user explicitly sent something.
 */
class ClipboardEngine(
    private val core: (String) -> CoreOutcome,
    private val host: ClipboardHost,
    private val settings: () -> ClipSettings,
    private val clock: () -> Long,
) {
    private class Pending(val pull: ClipEvent.Pull, val expiresAtMs: Long)

    /** The last thing this app was allowed to look at (memory only, trusted briefly). */
    private var cache: ClipSnapshot? = null
    private var pending: Pending? = null

    /** Text we put on the clipboard for the computer, so we never send it back as if the user copied it. */
    private var lastApplied: String? = null

    /** Called when the app has focus and looked at the clipboard. Does not send anything. */
    @Synchronized
    fun capture(snapshot: ClipSnapshot?) {
        if (!settings().enabled) {
            cache = null
            return
        }
        cache = if (snapshot?.text != null && snapshot.text == lastApplied) null else snapshot
    }

    @Synchronized
    fun onEvents(events: List<ClipEvent>) {
        for (e in events) when (e) {
            is ClipEvent.Pull -> handlePull(e)
            is ClipEvent.Incoming -> handleIncoming(e)
        }
    }

    private fun handleIncoming(e: ClipEvent.Incoming) {
        if (!settings().enabled) return
        lastApplied = e.text
        cache = null
        host.write(e)
    }

    private fun handlePull(p: ClipEvent.Pull) {
        val s = settings()
        val v = PullGate.decide(s.enabled, host.isLocked(), cache, false, clock(), s.windowMs, p.maxAgeMs)
        when {
            v is Verdict.Send -> answer(p, v.snapshot)
            v is Verdict.Refuse && v.reason == "needs_gesture" -> {
                // Keep the question for a short while: a tap on the notification answers it.
                pending = Pending(p, clock() + PENDING_MS)
                host.askForTap()
            }
            v is Verdict.Refuse -> refuse(p, v.reason)
        }
    }

    /**
     * The user tapped something that gave this app focus and it read the clipboard ([snapshot], possibly
     * null if empty). Answers the waiting pull if there is one; otherwise it is an explicit send.
     */
    @Synchronized
    fun onGesture(snapshot: ClipSnapshot?): SendResult {
        val s = settings()
        val waiting = pending?.takeIf { it.expiresAtMs > clock() }?.pull
        pending = null
        host.dismissTapRequest()
        cache = snapshot
        if (waiting != null) {
            // Same rule as [capture]: text the computer itself put here is not the user's copy to hand back.
            val own = snapshot?.text != null && snapshot.text == lastApplied
            val v = PullGate.decide(s.enabled, host.isLocked(), if (own) null else snapshot, true, clock(), s.windowMs, waiting.maxAgeMs)
            return when (v) {
                is Verdict.Send -> SendResult(answer(waiting, v.snapshot), null)
                is Verdict.Refuse -> {
                    refuse(waiting, v.reason)
                    SendResult(false, v.reason)
                }
            }
        }
        return sendExplicit(snapshot)
    }

    /** "Send to computer" on selected text: the text is the user's explicit choice. */
    @Synchronized
    fun sendText(text: String): SendResult {
        val kind = ClipClassifier.classify(text, null, null) ?: return SendResult(false, "empty")
        val now = clock()
        return sendExplicit(ClipSnapshot(kind, text, null, null, null, now, now, sensitive = false))
    }

    private fun sendExplicit(snapshot: ClipSnapshot?): SendResult {
        val s = settings()
        return when (val v = PullGate.decideExplicit(s.enabled, host.isLocked(), snapshot)) {
            is Verdict.Send -> SendResult(deliver(v.snapshot, null, null), null)
            is Verdict.Refuse -> SendResult(false, v.reason)
        }.let { if (!it.sent && it.reason == null) SendResult(false, "not_connected") else it }
    }

    private fun answer(p: ClipEvent.Pull, snap: ClipSnapshot): Boolean {
        val ok = deliver(snap, p.requestId, p.connectionId)
        if (!ok) refuse(p, "too_large")
        return ok
    }

    private fun refuse(p: ClipEvent.Pull, reason: String) {
        core(ClipboardProtocol.sendNoneRequest(p.requestId, p.connectionId, reason))
    }

    /** True if the core accepted the item for sending. */
    private fun deliver(snap: ClipSnapshot, requestId: String?, connectionId: Long?): Boolean {
        val age = (clock() - snap.copiedAtMs).coerceAtLeast(0)
        val outcome = if (snap.kind == ClipKind.Image) {
            host.withImageFd(snap, settings().maxImageBytes) { fd ->
                core(ClipboardProtocol.sendImageRequest(fd, requestId, connectionId, age))
            }
        } else {
            val text = snap.text ?: return false
            core(ClipboardProtocol.sendTextRequest(snap.kind, text, snap.html, requestId, connectionId, age))
        }
        return outcome is CoreOutcome.Ok
    }

    /** The link stopped or the app is shutting down: forget everything held in memory. */
    @Synchronized
    fun reset() {
        cache = null
        pending = null
        lastApplied = null
        host.dismissTapRequest()
    }

    companion object {
        /** How long a pull waits for the user's tap. The computer gives up on its own at 90 s. */
        const val PENDING_MS = 80_000L
    }
}
