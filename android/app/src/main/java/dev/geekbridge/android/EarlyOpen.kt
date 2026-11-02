// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

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

    /** Opens ahead of time; failures are left for [take] to report when the file is really needed. */
    fun prefetch() {
        try { synchronized(this) { if (held == null && !discarded) held = open() } } catch (_: Exception) { }
    }

    @Synchronized
    fun take(): T {
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
