// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject

/** What the phone reports about its battery (stock KDE Connect `kdeconnect.battery` fields). */
data class BatteryReading(val percent: Int, val charging: Boolean, val low: Boolean) {
    fun body(): JSONObject = JSONObject().put("currentCharge", percent).put("isCharging", charging).put("thresholdEvent", if (low) 1 else 0)
}

object BatteryRules {
    const val KIND = "kdeconnect.battery"
    const val REQUEST_KIND = "kdeconnect.battery.request"
    /** Android's own default for "low battery". */
    const val LOW_PERCENT = 15

    /** From the extras of the sticky ACTION_BATTERY_CHANGED intent; null when the level is not known. */
    fun reading(level: Int, scale: Int, plugged: Int): BatteryReading? {
        if (level < 0 || scale <= 0) return null
        val percent = (level.toLong() * 100 / scale).coerceIn(0L, 100L).toInt()
        val charging = plugged != 0
        return BatteryReading(percent, charging, low = !charging && percent <= LOW_PERCENT)
    }
}

/**
 * Sends the battery state to the computers allowed to have it (the native core enforces which, and sends nothing
 * when none is connected). Event driven only: the system's battery broadcast, a computer's request, a link coming
 * up or a computer being allowed. There is no timer that polls and no wake lock. Pure Kotlin so it is tested without
 * Android.
 *
 * A change of charging state or of the low flag goes out at once; a plain percentage change at most once per [gapMs],
 * with one trailing send so the last value is not lost. An unchanged reading is never sent again.
 */
class BatteryReporter(
    private val read: () -> BatteryReading?,
    private val core: (String) -> CoreOutcome,
    private val enabled: () -> Boolean,
    private val clock: () -> Long = System::currentTimeMillis,
    /** Runs [action] once after [delayMs] on some other thread. */
    private val later: (delayMs: Long, action: () -> Unit) -> Unit,
    private val gapMs: Long = 60_000,
    /** One short line per send or refusal (no level, no names): a reading that never arrives is told apart from one that was never sent. */
    private val note: (String) -> Unit = {},
) {
    private var last: BatteryReading? = null
    private var lastAt = Long.MIN_VALUE / 2
    private var pending = false

    /** New link session or the feature was switched: forget what was sent. */
    @Synchronized
    fun reset() {
        last = null
        lastAt = Long.MIN_VALUE / 2
    }

    /** The system reported a battery change. */
    @Synchronized
    fun onChanged() {
        if (!enabled()) return
        val r = read() ?: return
        val prev = last
        if (r == prev) return
        val now = clock()
        val urgent = prev == null || r.charging != prev.charging || r.low != prev.low
        val wait = gapMs - (now - lastAt)
        if (urgent || wait <= 0) {
            push(r, null)
        } else if (!pending) {
            pending = true
            later(wait) { synchronized(this) { pending = false }; onChanged() }
        }
    }

    /** A computer asked for the state (or was just allowed): answer it now, whatever was sent before. */
    @Synchronized
    fun sendNow(connectionId: Long? = null) {
        if (!enabled()) { note("asked for the battery, not sent: sharing is off or the link is not running"); return }
        val r = read() ?: run { note("asked for the battery, not sent: the system gave no level"); return }
        push(r, connectionId)
    }

    private fun push(r: BatteryReading, connectionId: Long?) {
        val o = core(PhoneDataProtocol.sendRequest(BatteryRules.KIND, r.body(), connectionId))
        note(
            "${if (connectionId == null) "sent to all allowed computers" else "reply on connection $connectionId"}: " +
                when (o) {
                    is CoreOutcome.Ok -> "queued"
                    is CoreOutcome.Rejected -> "refused (${o.code})"
                    else -> "core unavailable"
                },
        )
        if (o is CoreOutcome.Ok) {
            // A reply to one computer says nothing about the others: only a broadcast updates the memory.
            if (connectionId == null) {
                last = r
                lastAt = clock()
            }
        } else if (o is CoreOutcome.Rejected && o.code == "rate_limited" && !pending) {
            pending = true
            later(5_000) { synchronized(this) { pending = false }; onChanged() }
        }
        // not_connected / data_disabled: nothing to tell anyone; the next connection asks or is told at link up.
    }
}
