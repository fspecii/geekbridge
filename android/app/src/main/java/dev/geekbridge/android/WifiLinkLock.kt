// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Build

/**
 * When the Wi-Fi radio should be kept out of power-save. Pure so the rule is unit-tested.
 *
 * Wi-Fi power save (strong on One UI) parks the radio between beacons, so an idle TCP link sees dropped
 * or very late packets and resets after a few minutes. The lock is held only while a computer is paired
 * and the link is up, plus a short [LINGER_MS] after a loss so the redial that follows is not delayed by
 * a sleeping radio. With no computer, or a link that is not wanted, nothing is held.
 */
object WifiLockPolicy {
    const val LINGER_MS = 2 * 60_000L

    /** [msSinceLoss] is null while connected or when no link was ever lost. */
    fun wanted(paired: Int, connected: Boolean, msSinceLoss: Long?): Boolean =
        paired > 0 && (connected || (msSinceLoss != null && msSinceLoss in 0 until LINGER_MS))

    /**
     * The lock modes to hold, strongest guarantee first. Neither mode is a promise on every device, so they are
     * combined and the OS applies what it honours (docs/android-power.md):
     *  - `WIFI_MODE_FULL_HIGH_PERF` exists on every API level. It is the only mode meant to work with the screen off
     *    (the holder here is a foreground service). It is deprecated as of API 34 and documented as having no effect
     *    from there on, so on newer phones it is a harmless extra.
     *  - `WIFI_MODE_FULL_LOW_LATENCY` (API 29+) is applied only while the screen is on and the holder is in the
     *    foreground; it is the useful one on API 34+ with the screen on.
     * With the screen off on Android 14 and later there is therefore no lock that keeps the radio out of power save:
     * the link has to survive it (heartbeat, kernel keepalive, redial) and the lock is a screen-on improvement.
     */
    @Suppress("DEPRECATION")
    fun modes(sdk: Int): List<Int> = when {
        sdk >= 29 -> listOf(WifiManager.WIFI_MODE_FULL_HIGH_PERF, WifiManager.WIFI_MODE_FULL_LOW_LATENCY)
        else -> listOf(WifiManager.WIFI_MODE_FULL_HIGH_PERF)
    }
}

/** Holds the [WifiManager.WifiLock]s of [WifiLockPolicy.modes] while [WifiLockPolicy] wants them. Main thread only. */
class WifiLinkLock(context: Context) {
    private val locks: List<WifiManager.WifiLock> = try {
        val wm = context.applicationContext.getSystemService(WifiManager::class.java)
        WifiLockPolicy.modes(Build.VERSION.SDK_INT).mapNotNull { mode ->
            wm?.createWifiLock(mode, "geekbridge-link-$mode")?.apply { setReferenceCounted(false) }
        }
    } catch (_: RuntimeException) {
        emptyList()
    }

    val held: Boolean get() = locks.any { it.isHeld }

    fun set(wanted: Boolean) {
        if (locks.isEmpty()) return
        try {
            if (wanted && !held) {
                locks.forEach { it.acquire() }
                GbLog.i("wifi", "wifi lock acquired (${locks.size} mode(s))")
            } else if (!wanted && held) {
                locks.forEach { if (it.isHeld) it.release() }
                GbLog.i("wifi", "wifi lock released")
            }
        } catch (e: RuntimeException) {
            GbLog.caught("wifi", e) // missing permission or the OS refused: the link works without it
        }
    }
}
