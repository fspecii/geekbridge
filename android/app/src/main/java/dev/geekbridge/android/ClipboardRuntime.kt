// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.annotation.SuppressLint
import android.content.Context
import android.content.SharedPreferences
import java.util.concurrent.Executors

/**
 * Process-wide wiring of clipboard sync: the user's settings, the [ClipboardEngine], and the one thread
 * that waits for the computer. That thread parks inside the native core until something arrives (no timer,
 * no polling); it exists only while the transport runs and at most one wakeup per ~50 s keeps the call
 * alive. Everything is idle when the setting is off.
 */
// Only the application context is held, never an Activity.
@SuppressLint("StaticFieldLeak")
object ClipboardRuntime {
    private const val FILE = "geekbridge_clipboard"

    @Volatile private var prefs: SharedPreferences? = null
    @Volatile private var app: Context? = null
    @Volatile var settings: ClipSettings = ClipSettings()
        private set
    @Volatile private var engine: ClipboardEngine? = null

    /** The last refusal (or null after a send), for the line on Home. Memory only; never any content. */
    @Volatile var lastOutcome: ClipOutcome? = null
        private set

    /** Whether the Send clipboard tile is in Quick Settings. Android has no query for it, so the tile reports it. */
    var tileAdded: Boolean
        get() = prefs?.getBoolean("tile_added", false) == true
        set(v) { prefs?.edit()?.putBoolean("tile_added", v)?.apply() }
    private val waiter = WaiterSlot()

    /** Whether the running link declared clipboard in its identity (decided when the transport started). */
    @Volatile private var declaredEnabled = false
    @Volatile private var linkUp = false
    private val main = android.os.Handler(android.os.Looper.getMainLooper())
    private val core: CoreClient by lazy { CoreClient.native() }

    /** Native calls that answer the computer or push settings, off the main thread, in order. */
    private val sender = Executors.newSingleThreadExecutor { r -> Thread(r, "geekbridge-clip-send").apply { isDaemon = true } }

    /** Idempotent; call from every entry point that touches clipboard sync. */
    @Synchronized
    fun init(ctx: Context) {
        if (app != null) return
        val a = ctx.applicationContext
        app = a
        val p = a.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        prefs = p
        settings = ClipSettings(
            enabled = p.getBoolean("enabled", false),
            windowMs = p.getLong("window_ms", ClipboardLimits.DEFAULT_WINDOW_MS),
            maxImageBytes = p.getLong("max_image", ClipboardLimits.DEFAULT_MAX_IMAGE_BYTES),
        ).normalized()
        engine = ClipboardEngine(
            core = { core.clipboardAction(it) },
            host = AndroidClipboardHost(a),
            settings = { settings },
            clock = System::currentTimeMillis,
            onOutcome = { lastOutcome = ClipOutcome(it, System.currentTimeMillis()) },
        )
    }

    fun engine(): ClipboardEngine? = engine

    /** Runs [block] on the sender thread (JNI must not run on the main thread). */
    fun async(block: () -> Unit) {
        sender.execute {
            try {
                block()
            } catch (_: RuntimeException) {
                // A failed clipboard action must never take the process down.
            }
        }
    }

    fun update(transform: (ClipSettings) -> ClipSettings) {
        val next = transform(settings).normalized()
        settings = next
        prefs?.edit()
            ?.putBoolean("enabled", next.enabled)
            ?.putLong("window_ms", next.windowMs)
            ?.putLong("max_image", next.maxImageBytes)
            ?.apply()
        if (!next.enabled) engine?.reset()
        async { core.clipboardAction(ClipboardProtocol.configRequest(next.enabled, next.maxImageBytes)) }
        if (CapabilityRedeclare.clipboardNeedsReconnect(declaredEnabled, next.enabled, linkUp)) reconnectToDeclare(0, first = true)
    }

    /** The settings the transport starts with; remembers whether clipboard goes into the identity packet. */
    fun startSettings(): ClipSettings = settings.also { declaredEnabled = it.enabled }

    /** Capabilities are declared in the identity packet at connect time: a link started with clipboard off must reconnect. */
    private fun reconnectToDeclare(attempt: Int, first: Boolean = false) {
        val a = app ?: return
        main.post {
            if (first) {
                GbLog.i("capability", "clipboard switched on: reconnecting to re-declare")
                LinkRuntime.controller().stop(R.string.clip_reconnecting)
            }
            main.postDelayed({
                LinkService.sync(a)
                if (attempt < 4 && LinkRuntime.controller().phase == LinkPhase.Stopped) reconnectToDeclare(attempt + 1)
            }, 800)
        }
    }

    /** The app has window focus: remember what is on the clipboard (nothing is sent). */
    fun captureOnFocus(ctx: Context) {
        init(ctx)
        if (!settings.enabled) return
        val clip = AndroidClipboardHost.grab(ctx)
        val e = engine
        val appCtx = ctx.applicationContext
        // Provider access (getType) happens on the worker, never on the focus callback.
        async { e?.capture(clip?.let { AndroidClipboardHost.snapshot(appCtx, it) }) }
    }

    /** Called (main thread) whenever the transport state changes. */
    @Synchronized
    fun onLinkChanged(running: Boolean) {
        linkUp = running
        val e = engine ?: return
        if (!running) {
            e.reset()
            return
        }
        if (settings.enabled) startWaiter(e)
    }

    /** A setting was switched on while the link is already up. */
    @Synchronized
    fun ensureWaiting() {
        val e = engine ?: return
        if (settings.enabled) startWaiter(e)
    }

    private fun startWaiter(e: ClipboardEngine) {
        if (!waiter.claim()) return
        // The native core starts with clipboard sync off: after the process was killed the saved "On" must be pushed again,
        // or the UI reads On while the core ignores the computer (soak finding, E2E-6). Once per waiter, not per refresh.
        val s = settings
        async { core.clipboardAction(ClipboardProtocol.configRequest(s.enabled, s.maxImageBytes)) }
        // Whatever ends the loop (even an exception), the claim is given back so the next link change can restart it.
        Thread({ try { waitLoop(e) } finally { leave() } }, "geekbridge-clip-wait").apply { isDaemon = true; start() }
    }

    private fun waitLoop(e: ClipboardEngine) {
        var since = 0L
        while (!Thread.currentThread().isInterrupted) {
            val o = core.clipboardAction(ClipboardProtocol.waitRequest(since))
            if (o !is CoreOutcome.Ok) return leave() // not running: restarted when the link comes back
            val r = ClipboardProtocol.parseWait(o.result) ?: return leave()
            if (r.stopped) return leave()
            since = maxOf(since, r.lastSeq) // acknowledges what was just returned
            if (r.events.isNotEmpty()) {
                try {
                    e.onEvents(r.events)
                } catch (_: RuntimeException) {
                    // One bad item must not end the loop.
                }
            }
            if (waiter.leaveUnless { settings.enabled }) return
        }
    }

    private fun leave() { waiter.leaveUnless { false } }
}
