// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/**
 * One hold on the link, owned by a share screen. It outlives a rotation: the activity returns it from
 * `onRetainNonConfigurationInstance` and the next instance picks it up, so the hold is released exactly once,
 * when the screen is really finished (not when it is only recreated).
 */
class ShareHold(private val acquire: () -> Unit, private val release: () -> Unit) {
    var held = false
        private set

    /** Takes the hold unless this token already has it. */
    fun take() {
        if (held) return
        acquire()
        held = true
    }

    /** Gives the hold back if it is held. Safe to call twice. */
    fun giveBack() {
        if (!held) return
        held = false
        release()
    }
}
