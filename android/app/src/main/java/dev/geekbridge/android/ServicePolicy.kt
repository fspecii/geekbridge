// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/** When the background connection service should hold sockets. Pure so the rule is unit-tested. */
object ServicePolicy {
    /**
     * Never without a paired computer: an unpaired phone has nobody to stay connected to. The one exception is a
     * pairing in progress ([pairing]): the user is on the other computer approving a code, so the phone screen may
     * lock or the app may leave the foreground before it is paired, and the link must survive that.
     */
    fun shouldRun(pairedCount: Int, keepConnected: Boolean, pairing: Boolean = false): Boolean =
        (pairedCount > 0 && keepConnected) || pairing

    /** How long a pairing may keep the link alive in the background (code compare on the computer takes a minute or two). */
    const val PAIRING_GRACE_MS = 3 * 60_000L

    /** Network changes arrive in bursts; one restart per burst, and not on the very first callback. */
    fun shouldRestartOnNetwork(previousNetworkId: Long?, newNetworkId: Long): Boolean =
        previousNetworkId != null && previousNetworkId != newNetworkId

    /**
     * What a Wi-Fi network becoming available means for the link. The service watches the Wi-Fi network itself, not the
     * default network: One UI moves the default between Wi-Fi and mobile data on its own ("Switch to mobile data", Wi-Fi
     * re-validation), the Wi-Fi link to the computer survives that (an existing socket stays on the network it was made
     * on), and restarting the transport for it closed a healthy link on purpose and then redialed over mobile data,
     * where the computer is not reachable. Another Wi-Fi network (a new association: other access point, other SSID,
     * Wi-Fi back after a loss) restarts; the first one seen since the service started only nudges (announce and redial
     * now: the phone may have been off Wi-Fi with every redial spent).
     */
    fun onWifiAvailable(previousWifiId: Long?, newWifiId: Long): NetworkAction = when {
        shouldRestartOnNetwork(previousWifiId, newWifiId) -> NetworkAction.Restart
        previousWifiId == null -> NetworkAction.Nudge
        else -> NetworkAction.None
    }

    /**
     * The same network can change address without a new Network object (DHCP renewal, Wi-Fi roam to an AP on another
     * subnet, VPN toggle). Compared on IPv4 only: IPv6 privacy addresses rotate on their own and must not restart the link.
     * [previous] null means this is the first report for the network, which is never a change.
     */
    fun addressesChanged(previous: Set<String>?, current: Set<String>): Boolean = previous != null && previous != current

    const val RESTART_DEBOUNCE_MS = 3_000L

    /** When to look again at an unfinished pairing's grace: just after it ends; null when nothing is waiting on it. */
    fun graceCheckDelayMs(pairedCount: Int, graceLeftMs: Long): Long? =
        if (pairedCount == 0 && graceLeftMs > 0) graceLeftMs + 500 else null

    /**
     * Runs [promote] (startForeground). On Android 12+ a restart from the background may be refused with an
     * exception; the service then has no right to keep running, so [stop] is called instead of crashing.
     * Returns true if it is now a foreground service.
     */
    fun promoteOrStop(promote: () -> Unit, stop: () -> Unit): Boolean = try {
        promote()
        true
    } catch (_: RuntimeException) {
        stop()
        false
    }
}

/** See [ServicePolicy.onWifiAvailable]. */
enum class NetworkAction { None, Nudge, Restart }
