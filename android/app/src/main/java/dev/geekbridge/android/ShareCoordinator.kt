// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Context
import android.content.SharedPreferences
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.provider.OpenableColumns
import org.json.JSONObject
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

data class ReceivedText(val id: Long, val kind: TransferKind, val text: String, val from: String)

/** A shared file opened up front: its descriptor plus the name and size the provider reported. */
class OpenedShare(val pfd: android.os.ParcelFileDescriptor, val name: String?, val size: Long) : AutoCloseable {
    override fun close() = pfd.close()
}

/** One file (or text) of the current outgoing share and how it is going. */
class ShareItem(val label: String) {
    var transferId: Long? = null
    var seen = false
    var startedAtMs = 0L
    var error: String? = null
    var inFlight = false

    /** A text or link the core handed to the connection: the computer sends no answer, so this is all that is known. */
    var handedOver = false

    /** The file descriptor opened up front; null for text items. */
    var source: EarlyOpen<OpenedShare>? = null
}

/**
 * Process-wide glue between the Rust transfers (polled through [TransportController]) and Android: it drives the
 * outgoing queue, saves incoming files through MediaStore, and raises notifications. It outlives any Activity,
 * so a share keeps going if the user leaves the screen, and it asks the controller to keep the link up until
 * every transfer has finished.
 */
class ShareCoordinator private constructor(app: Context) {
    private val ctx = app.applicationContext
    private val main = Handler(Looper.getMainLooper())
    private val io: ExecutorService = Executors.newSingleThreadExecutor { r -> Thread(r, "geekbridge-share").apply { isDaemon = true } }
    private val prefetch: ExecutorService = Executors.newSingleThreadExecutor { r -> Thread(r, "geekbridge-share-open").apply { isDaemon = true } }
    private val core: CoreClient by lazy { CoreClient.native() }
    val controller: TransportController = TransportController.get({ core.transportAction(it) }, { main.post(it) })
    val notifications = ShareNotifications(ctx)
    val policy = ReceivePolicy(object : PolicyStore {
        private val prefs: SharedPreferences = ctx.getSharedPreferences("share_policy", Context.MODE_PRIVATE)
        override fun getBoolean(key: String) = prefs.getBoolean(key, false)
        override fun putBoolean(key: String, value: Boolean) { prefs.edit().putBoolean(key, value).apply() }
    })

    /**
     * Screens that want to hear about offers, received text or the session, by owner. A single callback slot was
     * clobbered when the Share screen stopped, so Home no longer showed incoming-file dialogs (E2E-5).
     */
    private val listeners = LinkedHashMap<Any, () -> Unit>()

    fun setListener(owner: Any, listener: () -> Unit) { listeners[owner] = listener }
    fun clearListener(owner: Any) { listeners.remove(owner) }
    private fun onChange() { for (l in listeners.values.toList()) l() }

    private val answered = RecentIds(RECENT_IDS)
    private val saved = HashMap<Long, SavedFile>()
    private val finished = RecentIds(RECENT_IDS)
    private val handledText = RecentIds(RECENT_IDS)
    val texts = ArrayList<ReceivedText>()

    // --- outgoing session ---
    private val queue = ArrayDeque<Pair<Uri, ShareItem>>()
    var items: List<ShareItem> = emptyList()
        private set
    private var sessionConn = -1L
    private var sessionHeld = false
    var sessionActive = false
        private set

    private val ticker = object : Runnable {
        override fun run() {
            ticking = false
            if (controller.phase == LinkPhase.Running && controller.needsLink()) {
                controller.poll()
                schedule()
            }
        }
    }
    private var ticking = false

    init {
        controller.observers += { onSnapshot() }
    }

    private fun schedule() {
        if (ticking) return
        ticking = true
        main.postDelayed(ticker, 700)
    }

    // ------------------------------------------------------------------ snapshot handling

    private fun onSnapshot() {
        val transfers = controller.snapshot?.transfers.orEmpty()
        for (t in transfers) {
            when {
                t.direction == TransferDirection.Incoming && t.kind == TransferKind.File -> incomingFile(t)
                t.direction == TransferDirection.Incoming -> incomingText(t)
            }
        }
        for (i in items) if (!i.seen && i.transferId != null && transfers.any { it.id == i.transferId }) i.seen = true
        pump()
        outgoingNotification(transfers)
        if (controller.needsLink()) schedule()
        // The background service owns the link while it runs: the system Share chooser covers Home (E2E-4).
        else if (!controller.appVisible && !LinkService.isRunning && controller.phase == LinkPhase.Running) {
            controller.stop(R.string.share_link_stopped_done)
        }
        onChange()
    }

    /** Offers waiting for the user (not auto-accepted, not answered). */
    fun offers(): List<Transfer> = controller.snapshot?.transfers.orEmpty().filter {
        it.direction == TransferDirection.Incoming && it.kind == TransferKind.File &&
            it.state == TransferState.Offered && it.id !in answered
    }

    private fun incomingFile(t: Transfer) {
        when (t.state) {
            TransferState.Offered -> if (t.id !in answered) {
                if (policy.autoAccept(t.deviceId)) {
                    answered += t.id
                    accept(t)
                } else if (finished.add(-t.id - 1)) { // first sighting: raise the notification once
                    notifications.offer(t)
                }
            }
            TransferState.Done -> if (finished.add(t.id)) {
                notifications.clearOffer(t.id)
                val s = saved.remove(t.id)
                if (s != null) io.execute {
                    try {
                        ReceivedFiles.publish(ctx, s)
                        main.post { notifications.received(t.id, s.name, s.uri) }
                    } catch (_: Exception) {
                        main.post { notifications.failed(t.id, ctx.getString(R.string.share_err_finish_saving, s.name)) }
                    }
                }
                dismiss(t.id)
            }
            TransferState.Failed, TransferState.Cancelled, TransferState.Declined -> if (finished.add(t.id)) {
                notifications.clearOffer(t.id)
                saved.remove(t.id)?.let { s -> io.execute { runCatching { ReceivedFiles.discard(ctx, s) } } }
                if (t.state == TransferState.Failed) notifications.failed(t.id, ctx.getString(R.string.share_failed_named, t.name, ShareText.failure(t.code, ctx.strings)))
                dismiss(t.id)
            }
            else -> Unit
        }
    }

    private fun incomingText(t: Transfer) {
        if (t.state != TransferState.Done || !handledText.add(t.id)) return
        val text = t.text ?: return
        val item = ReceivedText(t.id, t.kind, text, t.peerName)
        texts += item
        while (texts.size > 8) texts.removeAt(0)
        notifications.text(item)
        dismiss(t.id)
    }

    private fun dismiss(id: Long) = controller.raw(ShareProtocol.dismiss(id)) { }

    // ------------------------------------------------------------------ user decisions

    fun answer(id: Long, accept: Boolean) {
        main.post {
            val t = controller.snapshot?.transfers?.firstOrNull { it.id == id }
            if (t == null || t.state != TransferState.Offered || !answered.add(id)) return@post
            notifications.clearOffer(id)
            if (accept) accept(t) else controller.raw(ShareProtocol.decline(id)) { onChange() }
            onChange()
        }
    }

    private fun accept(t: Transfer) {
        io.execute {
            val file = try {
                ReceivedFiles.create(ctx, t.name)
            } catch (_: Exception) {
                main.post {
                    controller.raw(ShareProtocol.decline(t.id)) { }
                    notifications.failed(t.id, ctx.getString(R.string.share_err_create_file, t.name))
                }
                return@execute
            }
            main.post {
                controller.raw(ShareProtocol.accept(t.id, file.pfd.fd)) { o ->
                    runCatching { file.pfd.close() }
                    if (o is CoreOutcome.Ok) {
                        saved[t.id] = file
                        controller.poll()
                    } else {
                        io.execute { runCatching { ReceivedFiles.discard(ctx, file) } }
                        notifications.failed(t.id, ctx.getString(R.string.share_err_start_receive, t.name))
                    }
                }
            }
        }
    }

    fun cancel(id: Long) {
        main.post { controller.raw(ShareProtocol.cancel(id)) { controller.poll() } }
    }

    fun copyText(id: Long) {
        main.post { texts.firstOrNull { it.id == id }?.let { ShareNotifications.copyToClipboard(ctx, it.text) } }
    }

    // ------------------------------------------------------------------ outgoing session

    /**
     * A new share was opened: results of the last session, once it has ended, must not stand in for it. They stay on
     * screen only for the Share screen that ran them (this object outlives it); otherwise every later share in this
     * process would just show the old result and never send.
     */
    fun filesOpened(): Boolean = items.all { it.source?.isSettled() != false }

    fun forgetFinishedSession() {
        if (!sessionActive) items = emptyList()
    }

    /** Starts sending [uris] (content:// only, checked by the caller) to [connectionId]. */
    fun sendFiles(connectionId: Long, uris: List<Uri>) {
        // A second share while one is running joins it (same computer) instead of being dropped without a word.
        val joining = sessionActive && connectionId == sessionConn
        if (sessionActive && !joining) return
        if (!joining) beginSession(connectionId)
        val list = uris.take(ShareLimits.MAX_FILES).map { uri ->
            uri to ShareItem(uri.lastPathSegment?.let { s -> NameRules.sanitize(s) } ?: ctx.getString(R.string.share_what_file)).also { item ->
                item.source = EarlyOpen { openShared(uri) }
            }
        }
        items = (if (joining) items else emptyList()) + list.map { it.second }
        if (!joining) queue.clear()
        queue.addAll(list)
        // Open every file now, while the Share screen's temporary read grant is certain to be alive (Photos, Gallery,
        // Chrome): a later file of a long queue would otherwise be opened after the screen is gone.
        prefetch.execute { for ((_, item) in list) item.source?.prefetch() }
        pump()
        onChange()
    }

    fun sendText(connectionId: Long, kind: TransferKind, text: String) {
        val joining = sessionActive && connectionId == sessionConn
        if (sessionActive && !joining) return
        if (!joining) beginSession(connectionId)
        val item = ShareItem(ctx.getString(if (kind == TransferKind.Url) R.string.share_what_link else R.string.share_what_text)).also { it.inFlight = true }
        items = (if (joining) items else emptyList()) + item
        controller.raw(ShareProtocol.sendText(connectionId, kind, text)) { o ->
            item.inFlight = false
            if (o !is CoreOutcome.Ok) item.error = describe(o) else item.handedOver = true
            endIfDone()
            onChange()
        }
        onChange()
    }

    private fun beginSession(conn: Long) {
        sessionActive = true
        sessionConn = conn
        AppState.sharing = true
        LinkService.sync(ctx) // the Share screen is in front: Android lets a foreground service start now
        if (!sessionHeld) { controller.hold(); sessionHeld = true }
        schedule()
    }

    private fun describe(o: CoreOutcome): String = when (o) {
        is CoreOutcome.Rejected -> ShareText.error(o.code, ctx.strings)
        is CoreOutcome.Unavailable -> ctx.getString(R.string.share_err_core_unavailable)
        else -> ctx.getString(R.string.share_err_core_unreadable)
    }

    private fun pump() {
        if (!sessionActive) return
        val busy = items.any { it.inFlight }
        val active = controller.snapshot?.transfers.orEmpty().count { it.direction == TransferDirection.Outgoing && !it.state.finished }
        if (!busy && queue.isNotEmpty() && active < MAX_ACTIVE) {
            val (uri, item) = queue.removeFirst()
            item.inFlight = true
            io.execute { prepareAndSend(uri, item) }
        }
        endIfDone()
    }

    /** Reads the provider's name and size, then opens the descriptor; both need the temporary read grant. */
    private fun openShared(uri: Uri): OpenedShare {
        var name: String? = null
        var size = -1L
        ctx.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { c ->
            if (c.moveToFirst()) {
                c.getColumnIndex(OpenableColumns.DISPLAY_NAME).takeIf { it >= 0 && !c.isNull(it) }?.let { name = c.getString(it) }
                c.getColumnIndex(OpenableColumns.SIZE).takeIf { it >= 0 && !c.isNull(it) }?.let { size = c.getLong(it) }
            }
        }
        val pfd = ctx.contentResolver.openFileDescriptor(uri, "r") ?: throw java.io.IOException("not readable")
        return OpenedShare(pfd, name, if (size <= 0) pfd.statSize else size)
    }

    private fun prepareAndSend(uri: Uri, item: ShareItem) {
        try {
            val opened = item.source?.take() ?: openShared(uri)
            val pfd = opened.pfd
            var size = opened.size
            val name = opened.name
            val clean = NameRules.sanitize(name ?: uri.lastPathSegment)
            if (size <= 0) {
                pfd.close()
                main.post { fail(item, ctx.getString(if (size == 0L) R.string.share_err_empty else R.string.share_err_no_size)) }
                return
            }
            main.post {
                controller.raw(ShareProtocol.sendFile(sessionConn, pfd.fd, clean, size, null)) { o ->
                    item.inFlight = false
                    when {
                        o is CoreOutcome.Rejected && o.code == "busy" -> {
                            item.source?.giveBack(opened) ?: runCatching { pfd.close() } // keep it open: the grant may be gone by the retry
                            queue.addFirst(uri to item)
                        }
                        else -> {
                            runCatching { pfd.close() } // the core duplicated the descriptor during the call
                            if (o is CoreOutcome.Ok) {
                                item.transferId = (o.result as? JSONObject)?.optLong("transfer_id", -1)?.takeIf { it >= 0 }
                                item.startedAtMs = android.os.SystemClock.elapsedRealtime()
                            } else item.error = describe(o)
                        }
                    }
                    controller.poll()
                    pump()
                    onChange()
                }
            }
        } catch (e: Exception) {
            main.post { fail(item, ctx.getString(R.string.share_err_open, e.javaClass.simpleName)) }
        }
    }

    private fun fail(item: ShareItem, why: String) {
        item.inFlight = false
        item.error = why
        pump()
        onChange()
    }

    private fun endIfDone() {
        if (!sessionActive || queue.isNotEmpty() || items.any { it.inFlight }) return
        val ts = controller.snapshot?.transfers.orEmpty()
        val now = android.os.SystemClock.elapsedRealtime()
        val open = items.any { i -> ShareSessionLogic.itemOpen(i.transferId, ts.firstOrNull { it.id == i.transferId }, i.seen, now - i.startedAtMs) }
        if (open) return
        sessionActive = false
        AppState.sharing = false
        LinkService.sync(ctx)
        if (sessionHeld) { controller.release(); sessionHeld = false }
        notifications.cancel(ShareNotifications.ID_PROGRESS)
    }

    /** Per-item text for the share screen. */
    fun itemLine(i: ShareItem): Pair<String, Tone> {
        i.error?.let { return ctx.getString(R.string.share_item_error, i.label, it) to Tone.Bad }
        val t = i.transferId?.let { id -> controller.snapshot?.transfers?.firstOrNull { it.id == id } }
        if (t != null) {
            val tone = when (t.state) {
                TransferState.Done -> Tone.Ok
                TransferState.Failed -> Tone.Bad
                TransferState.Cancelled, TransferState.Declined -> Tone.Warn
                else -> Tone.Normal
            }
            return ProgressText.line(t, ctx.strings) to tone
        }
        return when {
            i.handedOver -> ctx.getString(R.string.share_item_text_sent, i.label) to Tone.Normal
            i.inFlight -> ctx.getString(R.string.share_item_starting, i.label) to Tone.Muted
            i.transferId != null -> ctx.getString(R.string.share_item_done, i.label) to Tone.Ok // already pruned from the snapshot
            else -> ctx.getString(R.string.share_item_queued, i.label) to Tone.Muted
        }
    }

    private fun outgoingNotification(ts: List<Transfer>) {
        val out = ts.filter { it.direction == TransferDirection.Outgoing && !it.state.finished }
        if (out.isEmpty()) return notifications.cancel(ShareNotifications.ID_PROGRESS)
        val total = out.sumOf { it.size ?: 0 }
        val sent = out.sumOf { it.bytes }
        val pct = if (total > 0 && out.all { it.size != null }) (sent.coerceAtMost(total) * 100 / total).toInt() else null
        notifications.progress(ctx.resources.getQuantityString(R.plurals.share_progress_summary, out.size, out.size, ProgressText.bytes(sent, ctx.strings)), pct, out.first().id)
    }

    companion object {
        const val MAX_ACTIVE = 3

        /** How many transfer ids each bookkeeping set remembers; older ones are long gone from the snapshot. */
        const val RECENT_IDS = 512

        @Volatile private var instance: ShareCoordinator? = null
        fun get(ctx: Context): ShareCoordinator =
            instance ?: synchronized(this) { instance ?: ShareCoordinator(ctx.applicationContext).also { instance = it } }
    }
}
