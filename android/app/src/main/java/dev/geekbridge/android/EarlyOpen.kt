// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * One shared file opened while the Share screen (and with it Android's temporary read grant for the content
 * Uri) is still alive. The grant of an ACTION_SEND is tied to the receiving activity, so a queued file opened
 * only after the user left would fail with a SecurityException; an already open descriptor keeps working.
 * [take] hands the open handle out exactly once (opening it now if nobody did); [giveBack] returns it when the core
 * answered "busy"; [discard] closes whatever is still held. Thread-safe: the prefetch and the sender both call it.
 */
class EarlyOpen<T : AutoCloseable>(private val open: () -> T) {
    private var held: T? = null
    private var discarded = false
    @Volatile private var settled = false

    /** True once the descriptor was opened, or opening it was tried (and failed), or it was taken: the read grant no longer matters. */
    fun isSettled(): Boolean = settled

    /** Opens ahead of time; failures are left for [take] to report when the file is really needed. */
    fun prefetch() {
        try { synchronized(this) { if (held == null && !discarded) held = open() } } catch (_: Exception) { } finally { settled = true }
    }

    @Synchronized
    fun take(): T {
        settled = true
        val h = held
        if (h != null) { held = null; return h }
        check(!discarded) { "discarded" }
        return open()
    }

    @Synchronized
    fun giveBack(handle: T) {
        if (discarded || held != null) handle.close() else held = handle
    }

    @Synchronized
    fun discard() {
        discarded = true
        held?.let { try { it.close() } catch (_: Exception) { } }
        held = null
    }
}

/** Concurrent prefetch workers: enough that one slow provider cannot hold up the rest, few enough to stay bounded. */
const val PREFETCH_THREADS = 4

/** The pool the Share flow opens its files on: [PREFETCH_THREADS] daemon threads named geekbridge-share-open-N. */
fun newPrefetchPool(): ExecutorService {
    val n = java.util.concurrent.atomic.AtomicInteger()
    return Executors.newFixedThreadPool(PREFETCH_THREADS) { r -> Thread(r, "geekbridge-share-open-${n.incrementAndGet()}").apply { isDaemon = true } }
}

/**
 * Submits one [EarlyOpen.prefetch] per item so a slow content provider only delays its own file; with one thread
 * the later files queued behind it and could miss the revoke of the temporary read grant. Parallelism narrows that
 * window but does not remove it, and a failed open is not lost: [EarlyOpen.take] reports it per file. Memory is
 * bounded by ShareLimits.MAX_FILES items and at most [PREFETCH_THREADS] opens in flight.
 */
fun prefetchAll(items: List<EarlyOpen<*>?>, executor: java.util.concurrent.Executor) {
    for (source in items) if (source != null) executor.execute { source.prefetch() }
}

/** True only when every source has settled (null = a text item with nothing to open). */
fun allSettled(items: List<EarlyOpen<*>?>): Boolean = items.all { it?.isSettled() != false }
