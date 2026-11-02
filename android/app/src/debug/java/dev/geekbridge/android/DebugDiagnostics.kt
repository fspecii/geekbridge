// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.util.Log
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import org.json.JSONObject

/*
 * DEBUG BUILDS ONLY (src/debug). Wires the debug diagnostics into the running app without the release code knowing:
 * [DebugBootProvider] runs at process start, [DebugDiagnostics.install] hooks the link observer, the data-event
 * hook and the Diagnostics card through [DebugHooks]. Nothing here exists in a release APK.
 */

/** Starts the diagnostics with the process (the log ring must fill before any link exists). */
class DebugBootProvider : ContentProvider() {
    override fun onCreate(): Boolean {
        context?.applicationContext?.let { DebugDiagnostics.boot(it) }
        return true
    }

    override fun query(u: Uri, p: Array<String>?, s: String?, a: Array<String>?, o: String?): Cursor? = null
    override fun getType(uri: Uri): String? = null
    override fun insert(uri: Uri, values: ContentValues?): Uri? = null
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<String>?) = 0
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<String>?) = 0
}

/** The user's "Send debug log to computer" switch. Default ON in debug builds. */
object DebugSettings {
    private const val FILE = "geekbridge_debug"

    fun streaming(ctx: Context): Boolean =
        ctx.getSharedPreferences(FILE, Context.MODE_PRIVATE).getBoolean("stream", true)

    fun setStreaming(ctx: Context, on: Boolean) {
        ctx.getSharedPreferences(FILE, Context.MODE_PRIVATE).edit().putBoolean("stream", on).apply()
    }
}

object DebugDiagnostics {
    private val main = Handler(Looper.getMainLooper())
    private val core: CoreClient by lazy { CoreClient.native() }
    val ring = LogRing()
    private val linkHistory = EventLog(50)
    private val exceptions = EventLog(20)

    @Volatile private var app: Context? = null
    @Volatile private var streamer: DebugLogStreamer? = null
    @Volatile private var started = false
    private var reader: OwnLogReader? = null
    private var lastPhase: LinkPhase? = null
    private var lastTrusted: Set<Long> = emptySet()
    private var lastEventSeq = 0L
    private var lastCapabilities = ""

    /** Process start (ContentProvider.onCreate): start reading the log; hook the UI once the Application is up. */
    @Synchronized
    fun boot(ctx: Context) {
        if (started) return
        started = true
        app = ctx
        DebugHooks.streaming = DebugSettings.streaming(ctx)
        GbLog.sink = ::record
        val s = DebugLogStreamer(ring, ::send, System::currentTimeMillis).also { it.enabled = DebugHooks.streaming }
        streamer = s
        reader = OwnLogReader(Process.myPid()) { raw -> onLogLine(raw) }.also { it.start() }
        Thread({ pumpLoop(s) }, "geekbridge-debug-send").apply { isDaemon = true; start() }
        // The Application's onCreate (strings, saved state) has not run yet: hook the link on the next main-loop turn.
        main.post { install(ctx) }
    }

    private fun install(ctx: Context) {
        DebugHooks.onDataEvent = { e -> onDataEvent(e) }
        DebugHooks.diagnosticsCard = { activity, ui, root -> addCard(activity, ui, root) }
        try {
            AppState.init(ctx)
            val c = LinkRuntime.controller()
            c.observers += { onLinkChanged(c) }
            onLinkChanged(c)
        } catch (t: RuntimeException) {
            Log.w(GbLog.TAG, "debug diagnostics: could not hook the link (${t.javaClass.simpleName})")
        }
        GbLog.i("debug", "diagnostics started; streaming ${if (DebugHooks.streaming) "ON" else "OFF"}")
    }

    // ---------------------------------------------------------------- log capture

    private fun allowedIps(): Set<String> =
        LinkRuntime.controller().snapshot?.connections.orEmpty().filter { it.trusted }.map { it.peer.ip }.toSet()

    private fun onLogLine(raw: String) {
        val line = DebugRedactor.line(raw, allowedIps())
        ring.add(line)
        // Exceptions the app caught and logged: the class name line only (messages are redacted already).
        if (EXCEPTION.containsMatchIn(line) && (" E " in line || " W " in line)) exceptions.add(line.substringAfter(": ", line))
    }

    private fun record(line: String) {
        if (line.startsWith("exception ")) exceptions.add(line.removePrefix("exception "))
    }

    // ---------------------------------------------------------------- link tracking

    @Synchronized
    private fun onLinkChanged(c: TransportController) {
        val phase = c.phase
        if (phase != lastPhase) {
            linkHistory.add("link phase ${lastPhase ?: "-"} -> $phase")
            GbLog.i("link", "phase ${lastPhase ?: "-"} -> $phase")
            lastPhase = phase
        }
        val snap = c.snapshot
        val trusted = snap?.connections.orEmpty().filter { it.trusted }.map { it.id }.toSet()
        for (id in trusted - lastTrusted) {
            val alias = aliasOf(snap, id)
            linkHistory.add("connected: $alias (connection $id)")
            GbLog.i("link", "connected $alias connection=$id")
        }
        for (id in lastTrusted - trusted) {
            linkHistory.add("disconnected: connection $id (see the next close event for who/why)")
        }
        lastTrusted = trusted
        streamer?.setConnections(
            trusted,
            snap?.connections.orEmpty().filter { it.trusted }.associate { it.id to it.peer.deviceId },
        )
        for (e in c.events) {
            if (e.seq <= lastEventSeq) continue
            lastEventSeq = e.seq
            val who = when (e.kind) {
                "peer_unpaired" -> "the computer unpaired this phone"
                "link_closed" -> "the connection closed"
                "peer_requested_repair" -> "the computer asked to pair again"
                else -> e.kind
            }
            val why = e.code ?: "no code"
            linkHistory.add("event ${e.kind}: $who; reason: $why")
            if (e.kind.contains("clos") || e.kind.contains("unpair") || e.kind.contains("lost") || e.kind.contains("fail")) {
                GbLog.i("link", "close: kind=${e.kind} who=$who why=$why connection=${e.connectionId ?: 0}")
            }
        }
        logCapabilities("link observer")
    }

    private fun aliasOf(snap: TransportSnapshot?, connection: Long): String {
        val id = snap?.connections?.firstOrNull { it.id == connection }?.peer?.deviceId ?: return "unknown computer"
        val n = AppState.pairedPeers.indexOfFirst { it.first == id }
        return "computer ${if (n >= 0) n + 1 else "?"} (${id.take(6)})"
    }

    /** Logs every capability decision with its reason, whenever the decision changes. */
    @Synchronized
    private fun logCapabilities(why: String) {
        val ctx = app ?: return
        val table = try {
            CapabilityReasons.table(DebugStateCollector.inputs(ctx, DebugHooks.streaming))
        } catch (t: RuntimeException) {
            return
        }
        val text = table.joinToString(" | ") { it.text() }
        if (text == lastCapabilities) return
        lastCapabilities = text
        for (line in table) GbLog.i("capability", "${line.text()} (after $why)")
        linkHistory.add("capabilities changed: " + table.filter { it.offered }.joinToString(",") { it.feature }.ifEmpty { "none offered" })
    }

    // ---------------------------------------------------------------- sending

    private fun send(connectionId: Long, kind: String, body: JSONObject): DebugSendResult {
        val request = JSONObject(PhoneDataProtocol.sendRequest(kind, body)).put("connection_id", connectionId).toString()
        return when (val o = core.dataAction(request)) {
            is CoreOutcome.Ok -> DebugSendResult.Sent
            is CoreOutcome.Rejected -> when (o.code) {
                "rate_limited", "busy" -> DebugSendResult.Later
                else -> DebugSendResult.Parked // not_connected / data_disabled / not offered by that computer
            }
            else -> DebugSendResult.Parked
        }
    }

    private fun pumpLoop(s: DebugLogStreamer) {
        while (true) {
            try {
                s.pump()
                Thread.sleep(500)
            } catch (_: InterruptedException) {
                return
            } catch (t: RuntimeException) {
                GbLog.caught("debug-send", t)
            }
        }
    }

    // ---------------------------------------------------------------- state requests

    private fun onDataEvent(e: DataEvent) {
        if (e.kind != DebugKinds.STATE_REQUEST || !DebugHooks.streaming) return
        val id = e.body.optString("requestId", "")
        if (!PhoneDataProtocol.validRequestId(id)) return
        Thread({
            val text = stateText()
            val body = JSONObject().put("requestId", id).put("text", text.take(100_000))
            send(e.connectionId, DebugKinds.STATE, body)
        }, "geekbridge-debug-state").apply { isDaemon = true; start() }
    }

    fun stateText(): String {
        val ctx = app ?: return "not started"
        return try {
            DebugStateText.render(
                DebugStateCollector.model(
                    ctx, DebugHooks.streaming, ring, streamer?.connectionCount() ?: 0,
                    linkHistory.all(), exceptions.all(),
                ),
            )
        } catch (t: RuntimeException) {
            "state dump failed: ${t.javaClass.simpleName}"
        }
    }

    // ---------------------------------------------------------------- the switch

    fun setStreaming(ctx: Context, on: Boolean) {
        val was = DebugHooks.streaming
        DebugSettings.setStreaming(ctx, on)
        DebugHooks.streaming = on
        streamer?.enabled = on
        GbLog.i("debug", "Send debug log to computer: ${if (on) "ON" else "OFF"}")
        worker { core.dataAction(PhoneDataProtocol.configRequest(PhoneDataRuntime.active())) }
        if (on && !was) {
            // The packet types are declared when the link starts: reconnect so the computer learns them.
            val c = LinkRuntime.controller()
            if (c.phase == LinkPhase.Running) {
                c.stop("debug diagnostics switched on")
                restartAttempt(ctx, 0)
            }
        }
        logCapabilities("debug switch")
    }

    private fun restartAttempt(ctx: Context, n: Int) {
        main.postDelayed({
            LinkService.sync(ctx)
            if (n < 4 && LinkRuntime.controller().phase == LinkPhase.Stopped) restartAttempt(ctx, n + 1)
        }, 800)
    }

    private fun worker(block: () -> Unit) {
        Thread(block, "geekbridge-debug-cfg").apply { isDaemon = true; start() }
    }

    // ---------------------------------------------------------------- UI

    private fun addCard(activity: Activity, ui: Ui, root: LinearLayout) {
        val card = ui.card(root, "Debug build")
        ui.body(
            card,
            "This is a debug build. Its app log (names, numbers, addresses and message or clipboard text removed) " +
                "can be sent to your paired computer: geekbridge-ctl phone-log DEVICE_ID.",
            Tone.Muted,
        )
        val sw = ui.check(card, "Send debug log to computer", DebugHooks.streaming)
        sw.setOnCheckedChangeListener { _, on -> setStreaming(activity, on) }
        ui.button(card, "Debug state") { showState(activity) }
    }

    private fun showState(activity: Activity) {
        val text = stateText()
        val tv = TextView(activity).apply {
            this.text = text
            typeface = android.graphics.Typeface.MONOSPACE
            textSize = 11f
            setTextIsSelectable(true)
            setPadding(32, 24, 32, 24)
        }
        AlertDialog.Builder(activity)
            .setTitle("Debug state")
            .setView(ScrollView(activity).apply { addView(tv) })
            .setPositiveButton("Copy") { _, _ ->
                val cm = activity.getSystemService(ClipboardManager::class.java)
                cm?.setPrimaryClip(ClipData.newPlainText("GeekBridge debug state", text))
                Toast.makeText(activity, "Copied (redacted)", Toast.LENGTH_SHORT).show()
            }
            .setNegativeButton("Close", null)
            .show()
    }

    private val EXCEPTION = Regex("""\b\w+(Exception|Error)\b""")
}
