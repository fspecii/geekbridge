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
    private var waiter: Thread? = null
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
        engine?.capture(AndroidClipboardHost.read(ctx))
    }

    /** Called (main thread) whenever the transport state changes. */
    @Synchronized
    fun onLinkChanged(running: Boolean) {
        val e = engine ?: return
        if (!running) {
            e.reset()
            return
        }
        if (!settings.enabled || waiter?.isAlive == true) return
        waiter = Thread({ waitLoop(e) }, "geekbridge-clip-wait").apply { isDaemon = true; start() }
    }

    /** A setting was switched on while the link is already up. */
    @Synchronized
    fun ensureWaiting() {
        val e = engine ?: return
        if (settings.enabled && waiter?.isAlive != true) {
            waiter = Thread({ waitLoop(e) }, "geekbridge-clip-wait").apply { isDaemon = true; start() }
        }
    }

    private fun waitLoop(e: ClipboardEngine) {
        var since = 0L
        while (!Thread.currentThread().isInterrupted) {
            val o = core.clipboardAction(ClipboardProtocol.waitRequest(since))
            if (o !is CoreOutcome.Ok) return // not running: restarted when the link comes back
            val r = ClipboardProtocol.parseWait(o.result) ?: return
            if (r.stopped) return
            since = maxOf(since, r.lastSeq) // acknowledges what was just returned
            if (r.events.isNotEmpty()) {
                try {
                    e.onEvents(r.events)
                } catch (_: RuntimeException) {
                    // One bad item must not end the loop.
                }
            }
            if (!settings.enabled) return
        }
    }
}
