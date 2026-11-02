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
) {
    private var handle: AutoCloseable? = null
    private var generation = 0

    val isRunning: Boolean get() = handle != null

    fun start(): MonitorStart {
        if (handle != null) return MonitorStart.AlreadyRunning
        val gen = ++generation
        return try {
            handle = registrar.register(executor) { raw ->
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
