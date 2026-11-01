package dev.geekbridge.android

import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException

/** Cooperative cancellation that can also forward to a platform handle (e.g. CancellationSignal). */
class CancelToken {
    private val lock = Any()
    private var cancelled = false
    private var listener: (() -> Unit)? = null

    val isCancelled: Boolean get() = synchronized(lock) { cancelled }

    fun cancel() {
        val l = synchronized(lock) {
            if (cancelled) return
            cancelled = true
            listener.also { listener = null }
        }
        l?.invoke()
    }

    /** Runs [action] on cancel, or immediately if already cancelled. */
    fun onCancel(action: () -> Unit) {
        val runNow = synchronized(lock) {
            if (cancelled) true else {
                listener = action
                false
            }
        }
        if (runNow) action()
    }
}

/**
 * Runs blocking work (provider queries, JNI calls) off the main thread, one task per named slot.
 * Submitting to a busy slot cancels the previous task; [cancelAll] and [shutdown] are for lifecycle
 * callbacks. Results are delivered through [postToMain] only if the task was not cancelled meanwhile.
 */
class WorkRunner(
    private val postToMain: (Runnable) -> Unit,
    private val executor: ExecutorService = Executors.newSingleThreadExecutor { r ->
        Thread(r, "geekbridge-work").apply { isDaemon = true }
    },
) {
    private val running = HashMap<String, CancelToken>()

    fun <R> submit(slot: String, work: (CancelToken) -> R, deliver: (Result<R>) -> Unit) {
        val token = CancelToken()
        val previous = synchronized(running) { running.put(slot, token) }
        previous?.cancel()
        try {
            executor.execute {
                if (token.isCancelled) return@execute
                val result = runCatching { work(token) }
                if (token.isCancelled) return@execute
                postToMain(
                    Runnable {
                        synchronized(running) { if (running[slot] === token) running.remove(slot) }
                        if (!token.isCancelled) deliver(result)
                    },
                )
            }
        } catch (e: RejectedExecutionException) {
            token.cancel()
        }
    }

    fun cancel(slot: String) {
        synchronized(running) { running.remove(slot) }?.cancel()
    }

    fun cancelAll() {
        val tokens = synchronized(running) { running.values.toList().also { running.clear() } }
        tokens.forEach { it.cancel() }
    }

    fun shutdown() {
        cancelAll()
        executor.shutdownNow()
    }
}
