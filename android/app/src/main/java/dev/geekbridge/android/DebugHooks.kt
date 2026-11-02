// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.widget.LinearLayout

/**
 * Extension points that only the debug build's `DebugDiagnostics` (src/debug) fills in. In a release build
 * nothing ever sets them, so they stay false/null and no diagnostics code, packet name or screen exists.
 */
object DebugHooks {
    /** Declare the debug packet types when the transport starts (the user's "Send debug log to computer" switch). */
    @JvmField @Volatile var streaming = false

    /** A `geekbridge.debug.*` request from the paired computer (called on the data-wait thread). */
    @JvmField @Volatile var onDataEvent: ((DataEvent) -> Unit)? = null

    /** Adds the debug card to the Diagnostics screen. */
    @JvmField @Volatile var diagnosticsCard: ((Activity, Ui, LinearLayout) -> Unit)? = null
}
