// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ExecutorService
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

/**
 * Coarse cellular call state, deliberately the only thing this app learns about calls. There is no
 * number, no contact, no direction and no duration: those need READ_CALL_LOG / READ_PHONE_NUMBERS,
 * which the app does not hold and does not request.
 */
enum class CallState { Idle, Ringing, OffHook }

/** Pure helpers (no Android types) so they run in JVM tests. */
object CallStateMapping {
    // Values of TelephonyManager.CALL_STATE_IDLE / _RINGING / _OFFHOOK; fixed since API 1.
    const val RAW_IDLE = 0
    const val RAW_RINGING = 1
    const val RAW_OFFHOOK = 2

    /** Unknown values map to null and are ignored rather than guessed. */
    fun fromRaw(raw: Int): CallState? = when (raw) {
        RAW_IDLE -> CallState.Idle
        RAW_RINGING -> CallState.Ringing
        RAW_OFFHOOK -> CallState.OffHook
        else -> null
    }
}

/** Which platform API observes call state on a given SDK level. */
enum class ListenerStrategy {
    /** API 31+: `TelephonyManager.registerTelephonyCallback(executor, callback)`. */
    TelephonyCallback,

    /** API 29-30: `PhoneStateListener(executor)`, deprecated from 31. */
    ListenerWithExecutor,

    /** API 26-28: `PhoneStateListener()` bound to the creating (main) thread's Looper. */
    ListenerOnMainLooper;

    companion object {
        fun forSdk(sdk: Int): ListenerStrategy = when {
            sdk >= 31 -> TelephonyCallback
            sdk >= 29 -> ListenerWithExecutor
            else -> ListenerOnMainLooper
        }
    }
}

/** Remembers the current state and when it last changed; repeated reports of the same state are not changes. */
class CallStateTracker {
    var state: CallState? = null
        private set
    var changedAtMillis: Long? = null
        private set

    /** Returns true if this report changed what is shown. */
    fun update(newState: CallState, nowMillis: Long): Boolean {
        if (newState == state) return false
        state = newState
        changedAtMillis = nowMillis
        return true
    }

    fun reset() {
        state = null
        changedAtMillis = null
    }
}

/**
 * Combines several sources that each report a raw state (the default subscription's callback, one callback per
 * active SIM, the phone-state broadcast). Ringing wins over off-hook, which wins over idle: the merged state is idle
 * only when every source that has reported says idle.
 */
class CallSourceMerger {
    private val states = LinkedHashMap<String, Int>()
    private var last: Int? = null

    private fun compute(): Int? {
        if (states.isEmpty()) return null
        return when {
            CallStateMapping.RAW_RINGING in states.values -> CallStateMapping.RAW_RINGING
            CallStateMapping.RAW_OFFHOOK in states.values -> CallStateMapping.RAW_OFFHOOK
            else -> CallStateMapping.RAW_IDLE
        }
    }

    /** Records [raw] (unknown values are ignored) and returns the merged state if it changed, else null. */
    @Synchronized fun update(source: String, raw: Int): Int? {
        if (CallStateMapping.fromRaw(raw) == null) return null
        states[source] = raw
        return changed()
    }

    /** A source went away (a SIM was removed); its last state no longer counts. */
    @Synchronized fun remove(source: String): Int? {
        if (states.remove(source) == null) return null
        return changed()
    }

    @Synchronized fun merged(): Int? = compute()

    private fun changed(): Int? {
        val now = compute()
        if (now == last) return null
        last = now
        return now
    }
}

/** A place raw states come from. [onSource] gets (source name, raw state); [onGone] says a source was dropped. */
fun interface CallSourceRegistrar {
    fun register(
        executor: java.util.concurrent.Executor,
        onSource: (String, Int) -> Unit,
        onGone: (String) -> Unit,
    ): AutoCloseable
}

/** Adapts several sources to the single-callback [CallStateRegistrar] the monitor uses, merging them. */
class MergingCallRegistrar(
    private val sources: CallSourceRegistrar,
    private val onSourceSeen: (String, Int) -> Unit = { _, _ -> },
) : CallStateRegistrar {
    override fun register(executor: java.util.concurrent.Executor, onRaw: (Int) -> Unit): AutoCloseable {
        val merger = CallSourceMerger()
        return sources.register(
            executor,
            { source, raw ->
                onSourceSeen(source, raw)
                merger.update(source, raw)?.let(onRaw)
            },
            { source -> merger.remove(source)?.let(onRaw) },
        )
    }
}

/** One active SIM as the diagnostics see it: slot and whether it is an eSIM. No number, no ICCID. */
data class ActiveSim(val subId: Int, val slot: Int, val embedded: Boolean) {
    val sourceName: String get() = "sub$subId/slot$slot"
}

object SimText {
    /** Value of `ACTION_PHONE_STATE_CHANGED`'s state extra to a raw state, or null. */
    fun fromBroadcast(extra: String?): Int? = when (extra) {
        "IDLE" -> CallStateMapping.RAW_IDLE
        "RINGING" -> CallStateMapping.RAW_RINGING
        "OFFHOOK" -> CallStateMapping.RAW_OFFHOOK
        else -> null
    }

    fun summary(sims: List<ActiveSim>, defaultVoiceSubId: Int?): String {
        val list = sims.joinToString(", ") { "sub${it.subId} slot ${it.slot} ${if (it.embedded) "eSIM" else "physical"}" }
        val def = defaultVoiceSubId?.takeIf { it >= 0 }?.let { id ->
            val m = sims.firstOrNull { it.subId == id }
            "sub$id" + (m?.let { " (slot ${it.slot})" } ?: " (not among the active ones)")
        } ?: "none"
        return "active subscriptions: ${sims.size}${if (list.isEmpty()) "" else " [$list]"}; default voice subscription: $def"
    }
}

/** Registers a platform listener; closing the returned handle unregisters it. */
fun interface CallStateRegistrar {
    /** [onRaw] may be invoked on any thread (normally [executor]) with a raw TelephonyManager call state. */
    fun register(executor: java.util.concurrent.Executor, onRaw: (Int) -> Unit): AutoCloseable
}

object CallStateExecutor {
    const val QUEUE_CAPACITY = 8

    /**
     * One daemon thread and a bounded queue. When the queue is full the oldest pending report is dropped,
     * because only the newest call state matters.
     */
    fun create(): ExecutorService = ThreadPoolExecutor(
        1, 1, 0L, TimeUnit.MILLISECONDS,
        ArrayBlockingQueue(QUEUE_CAPACITY),
        { r -> Thread(r, "geekbridge-call-state").apply { isDaemon = true } },
        ThreadPoolExecutor.DiscardOldestPolicy(),
    )
}

sealed interface MonitorStart {
    data object Started : MonitorStart
    data object AlreadyRunning : MonitorStart
    data class Failed(val reasonId: Int, val detail: String? = null) : MonitorStart {
        /** Reason code for the debug log and state dump: the exception class only, never its message. */
        val code: String get() = if (detail == null) "security_exception" else "runtime_exception:$detail"

        fun reason(s: Strings): String = if (detail == null) s.get(reasonId) else s.get(reasonId, detail)
    }
}

/**
 * Listens for coarse call state between [start] and [stop]. Call both from the main thread (onStart/onStop).
 * [stop] unregisters the platform listener and invalidates anything still queued, so no late report is
 * delivered afterwards and nothing keeps a reference to the activity.
 */
class CallStateMonitor(
    private val registrar: CallStateRegistrar,
    private val executor: java.util.concurrent.Executor,
    private val postToMain: (Runnable) -> Unit,
    private val onState: (CallState) -> Unit,
    /** Called on the registered executor for every platform report, before it is mapped or queued (diagnostics only). */
    private val onRawSeen: (Int) -> Unit = {},
) {
    private var handle: AutoCloseable? = null
    private var generation = 0

    val isRunning: Boolean get() = handle != null

    fun start(): MonitorStart {
        if (handle != null) return MonitorStart.AlreadyRunning
        val gen = ++generation
        return try {
            handle = registrar.register(executor) { raw ->
                onRawSeen(raw)
                val s = CallStateMapping.fromRaw(raw) ?: return@register
                postToMain(Runnable { if (gen == generation && handle != null) onState(s) })
            }
            MonitorStart.Started
        } catch (e: SecurityException) {
            generation++
            MonitorStart.Failed(R.string.call_state_refused)
        } catch (e: RuntimeException) {
            generation++
            MonitorStart.Failed(R.string.call_state_unavailable, e.javaClass.simpleName)
        }
    }

    fun stop() {
        generation++
        val h = handle ?: return
        handle = null
        try {
            h.close()
        } catch (_: RuntimeException) {
            // Nothing left to release; the listener is already dropped from our side.
        }
    }
}

/**
 * What the call-state listener has done, for the debug log and the phone-state dump: whether it is registered
 * and if not why not (a reason code), how many platform callbacks arrived, the last one and when, and what the
 * core said to the last report. Codes and counts only: never a number, name or address.
 */
class CallListenerLedger(
    private val clock: () -> Long = System::currentTimeMillis,
    private val log: (String) -> Unit = { GbLog.i("calls", it) },
) {
    private var registeredAt: Long? = null
    private var strategy: ListenerStrategy? = null
    private var notRegisteredCode: String? = "never_attempted"
    private var attemptAt: Long? = null
    private var callbacks = 0
    private var lastRaw: Int? = null
    private var lastRawAt: Long? = null
    private var delivered = 0
    private var lastDelivered: CallState? = null
    private var lastDeliveredAt: Long? = null
    private var lastReport: String? = null
    private var lastReportAt: Long? = null
    private val bySource = LinkedHashMap<String, String>()
    private var subscriptions: String? = null

    /** One source (default callback, a SIM's callback, the broadcast) reported [raw]. */
    @Synchronized fun source(name: String, raw: Int) {
        bySource[name] = "${rawName(raw)} at ${java.text.SimpleDateFormat("HH:mm:ss", java.util.Locale.US).format(java.util.Date(clock()))}"
        log("source $name reported ${rawName(raw)}")
    }

    @Synchronized fun sourceGone(name: String) {
        bySource.remove(name)
        log("source $name removed")
    }

    /** The active SIM set was (re)read. */
    @Synchronized fun subscriptions(summary: String) {
        if (summary != subscriptions) log(summary)
        subscriptions = summary
    }

    /** A source could not be registered; [code] is a reason code. */
    @Synchronized fun sourceFailed(name: String, code: String) {
        log("source $name not registered: $code")
    }

    @Synchronized fun registered(by: ListenerStrategy) {
        val now = clock()
        registeredAt = now; attemptAt = now; strategy = by; notRegisteredCode = null
        log("listener registered via ${by.name}")
    }

    @Synchronized fun notRegistered(code: String) {
        val now = clock()
        if (registeredAt != null || notRegisteredCode != code) log("listener not registered: $code")
        registeredAt = null; strategy = null; notRegisteredCode = code; attemptAt = now
    }

    /** The platform called back (any thread). */
    @Synchronized fun raw(raw: Int) {
        callbacks++
        lastRaw = raw
        lastRawAt = clock()
        log("callback #$callbacks merged=${rawName(raw)}")
    }

    /** The state reached the app's main thread and was handed to the engine. */
    @Synchronized fun delivered(state: CallState) {
        delivered++
        lastDelivered = state
        lastDeliveredAt = clock()
        log("state ${state.name.lowercase()} handed to the call engine")
    }

    /** The engine reported to the core: [outcome] is "sent", "unchanged", "off" or a core error code. */
    @Synchronized fun reported(outcome: String) {
        lastReport = outcome
        lastReportAt = clock()
        log("report to core: $outcome")
    }

    @Synchronized fun isRegistered(): Boolean = registeredAt != null

    /** When the last registration attempt happened, for the retry spacing. */
    @Synchronized fun lastAttemptAt(): Long? = attemptAt

    @Synchronized fun lines(): List<String> {
        val fmt = java.text.SimpleDateFormat("MM-dd HH:mm:ss", java.util.Locale.US)
        fun at(t: Long?) = t?.let { fmt.format(java.util.Date(it)) } ?: "never"
        return buildList {
            add(
                if (registeredAt != null) "call state listener: registered via ${strategy?.name} since ${at(registeredAt)}"
                else "call state listener: not registered ($notRegisteredCode); last attempt ${at(attemptAt)}",
            )
            add(subscriptions ?: "active subscriptions: not read yet")
            add("sources: " + bySource.entries.joinToString("; ") { "${it.key}=${it.value}" }.ifEmpty { "none yet" })
            add("merged state changes since the app started: $callbacks; last ${lastRaw?.let(::rawName) ?: "none"} at ${at(lastRawAt)}")
            add("handed to the call engine: $delivered; last ${lastDelivered?.name?.lowercase() ?: "none"} at ${at(lastDeliveredAt)}")
            add("last report to the core: ${lastReport ?: "none"} at ${at(lastReportAt)}")
        }
    }

    companion object {
        fun rawName(raw: Int): String = when (raw) {
            CallStateMapping.RAW_IDLE -> "idle"
            CallStateMapping.RAW_RINGING -> "ringing"
            CallStateMapping.RAW_OFFHOOK -> "offhook"
            else -> "unknown($raw)"
        }
    }
}

/** When a listener that is wanted but not running is tried again (a failed or missed registration must not be final). */
object ListenerRetry {
    const val SPACING_MS = 30_000L

    fun due(lastAttemptAtMs: Long?, nowMs: Long): Boolean = lastAttemptAtMs == null || nowMs - lastAttemptAtMs >= SPACING_MS
}
