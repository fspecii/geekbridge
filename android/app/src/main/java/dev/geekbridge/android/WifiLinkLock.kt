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

    /** Low latency from Android 10; the older high-performance mode before. */
    @Suppress("DEPRECATION")
    fun mode(sdk: Int): Int =
        if (sdk >= 29) WifiManager.WIFI_MODE_FULL_LOW_LATENCY else WifiManager.WIFI_MODE_FULL_HIGH_PERF
}

/** Holds one [WifiManager.WifiLock] while [WifiLockPolicy] wants it. Main thread only. */
class WifiLinkLock(context: Context) {
    private val lock: WifiManager.WifiLock? = try {
        context.applicationContext.getSystemService(WifiManager::class.java)
            ?.createWifiLock(WifiLockPolicy.mode(Build.VERSION.SDK_INT), "geekbridge-link")
            ?.apply { setReferenceCounted(false) }
    } catch (_: RuntimeException) {
        null
    }

    val held: Boolean get() = lock?.isHeld == true

    fun set(wanted: Boolean) {
        val l = lock ?: return
        try {
            if (wanted && !l.isHeld) {
                l.acquire()
                GbLog.i("wifi", "wifi lock acquired")
            } else if (!wanted && l.isHeld) {
                l.release()
                GbLog.i("wifi", "wifi lock released")
            }
        } catch (e: RuntimeException) {
            GbLog.caught("wifi", e) // missing permission or the OS refused: the link works without it
        }
    }
}
