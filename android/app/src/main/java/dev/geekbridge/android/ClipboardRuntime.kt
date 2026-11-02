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
    private val waiter = WaiterSlot()
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
