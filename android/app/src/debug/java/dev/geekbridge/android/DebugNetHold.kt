// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.provider.Settings

/**
 * DEBUG BUILDS ONLY (src/debug). Holds one request for the cellular network so that Android keeps it FOREGROUND.
 *
 * In the emulator every connection redirected with `adb emu redir` (the e2e suites' way to reach the phone) arrives on the
 * guest's mobile-data interface, not on Wi-Fi. Android destroys the sockets of a network when it loses FOREGROUND, which
 * happens whenever the last app that asked for the cellular network releases its request (docs/reports/e2e-link-drops.md),
 * and the link drops with `conn_aborted`. A request held here keeps the capability, so the link lives. Armed with
 * `adb shell settings put global gb_hold_cellular 1` (read at process start) or the DebugControlReceiver `hold_cellular`.
 * It does not change what the product does: no release code references it.
 */
object DebugNetHold {
    private const val SETTING = "gb_hold_cellular"
    private var callback: ConnectivityManager.NetworkCallback? = null

    /** Process start: hold the request when the setting is on. */
    fun bootIfArmed(ctx: Context) {
        val armed = try { Settings.Global.getInt(ctx.contentResolver, SETTING, 0) == 1 } catch (_: RuntimeException) { false }
        if (armed) set(ctx, true)
    }

    @Synchronized
    fun set(ctx: Context, on: Boolean) {
        val cm = ctx.applicationContext.getSystemService(ConnectivityManager::class.java) ?: return
        val held = callback
        if (!on) {
            if (held != null) try { cm.unregisterNetworkCallback(held) } catch (_: RuntimeException) {}
            callback = null
            GbLog.i("debug", "cellular request released")
            return
        }
        if (held != null) return
        val request = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_CELLULAR)
            .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .build()
        val cb = object : ConnectivityManager.NetworkCallback() {}
        try {
            cm.requestNetwork(request, cb)
            callback = cb
            GbLog.i("debug", "cellular request held")
        } catch (t: RuntimeException) {
            GbLog.caught("debug-nethold", t)
        }
    }
}
