// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/**
 * Which local address the Rust core should dial and announce from. One UI's "Switch to mobile data" makes mobile data
 * the default network while Wi-Fi is still connected, and a socket that is not bound follows the default network, so
 * a redial or an announcement would leave over mobile data, where the computer is not. Binding the core's sockets to
 * the Wi-Fi IPv4 address keeps them on Wi-Fi (`ConnectivityManager.bindProcessToNetwork` would capture every socket of
 * the process, the share and update checks included). Pure so the rule is unit-tested.
 */
object WifiBindPolicy {
    private val v4 = Regex("""(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})""")

    /** A unicast IPv4 address worth leaving from: not 0.0.0.0, loopback, link-local (no DHCP), multicast or broadcast. */
    fun usable(ip: String): Boolean {
        val m = v4.matchEntire(ip) ?: return false
        val o = m.groupValues.drop(1).map { it.toInt() }
        if (o.any { it > 255 }) return false
        return !(o[0] == 0 || o[0] == 127 || o[0] >= 224 || (o[0] == 169 && o[1] == 254))
    }

    /**
     * [perNetwork]: the IPv4 addresses of each connected Wi-Fi network. Exactly one usable address overall: bind to it.
     * None, or more than one (two Wi-Fi networks at once, a network with several addresses): stay unbound, because a
     * wrong guess would pin the link to a network the computer is not on.
     */
    fun choose(perNetwork: Collection<List<String>>): String? =
        perNetwork.flatten().filter(::usable).distinct().singleOrNull()

    /** Tell the core only when the answer changed, and always once per service instance (the core outlives it). */
    fun shouldPush(first: Boolean, last: String?, now: String?): Boolean = first || last != now
}
