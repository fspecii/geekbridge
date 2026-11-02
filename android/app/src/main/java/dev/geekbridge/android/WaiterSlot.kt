// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/**
 * Says whether a wait thread exists, without the race of asking the thread itself: a thread that has decided to
 * stop but has not died yet still looks alive, so a restart request in that window would be lost. The thread
 * decides to leave and gives up its claim in one step, under the same lock the restarter uses.
 */
class WaiterSlot {
    private var claimed = false

    /** True if the caller may start the thread; false if one is already running. */
    @Synchronized
    fun claim(): Boolean {
        if (claimed) return false
        claimed = true
        return true
    }

    /** Called by the thread when it wants to leave. Returns true (and gives up the claim) unless [stillWanted]. */
    @Synchronized
    fun leaveUnless(stillWanted: () -> Boolean): Boolean {
        if (stillWanted()) return false
        claimed = false
        return true
    }
}
