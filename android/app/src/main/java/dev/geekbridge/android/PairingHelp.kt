// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import java.net.ConnectException
import java.net.Inet4Address
import java.net.InetSocketAddress
import java.net.NoRouteToHostException
import java.net.Socket
import java.net.SocketTimeoutException

/** A friendly sentence and one thing to try. Mirrors `bridge_core::pairing_help` on the computer. */
data class Explained(val headline: String, val advice: String)

object PairingHelp {
    /** Plain-language meaning of a core event ([kind] + [code]); [viaQr] when a scanned code was in use. */
    fun explain(s: Strings, kind: String?, code: String?, viaQr: Boolean = false): Explained {
        fun e(h: Int, a: Int) = Explained(s.get(h), s.get(a))
        return when {
            code == "qr_certificate_mismatch" || code == "pin_mismatch" || code == "pairing_binding_changed" ->
                e(R.string.help_not_scanned_h, R.string.help_not_scanned_a)
            code == "identity_mismatch" -> e(R.string.help_different_device_h, R.string.help_different_device_a)
            code == "clock_skew" -> e(R.string.help_clock_h, R.string.help_clock_a)
            code == "pairing_rejected_locally" -> e(R.string.help_declined_local_h, R.string.help_declined_local_a)
            code == "pairing_rejected_by_peer" && viaQr -> e(R.string.help_qr_rejected_h, R.string.help_qr_rejected_a)
            code == "pairing_rejected_by_peer" -> e(R.string.help_declined_peer_h, R.string.help_declined_peer_a)
            code == "pairing_timeout" -> e(R.string.help_pair_timeout_h, R.string.help_pair_timeout_a)
            code == "timeout" || code == "connect_timeout" || kind == "connect_failed" && code == null ->
                e(R.string.help_no_answer_h, R.string.help_no_answer_a)
            code == "address_rejected" || code == "network_unreachable" ->
                e(R.string.help_other_network_h, R.string.help_other_network_a)
            code == "link_closed" || code == "peer_closed" || code == "park_timeout" || code == "io_error" ->
                e(R.string.help_closed_h, R.string.help_closed_a)
            code == "tls_failed" -> e(R.string.help_tls_h, R.string.help_tls_a)
            else -> e(R.string.help_default_h, R.string.help_default_a)
        }
    }
}

enum class Probe { NotTried, Connected, Refused, TimedOut, NoRoute, Other }

data class NetStep(val label: String, val ok: Boolean, val detail: String)
data class NetReport(val ok: Boolean, val headline: String, val steps: List<NetStep>, val advice: List<String>)

/** What was measured about this phone's network and the computer. */
data class NetFacts(
    val wifi: Boolean,
    val ownIp: String?,
    val vpn: Boolean,
    val peerIp: String?,
    val probe: Probe,
)

/** The "Test connection" check. [report] is pure; [probe] and [facts] touch the network. */
object NetCheck {
    /** Same /24 for IPv4, which is how nearly every home network is laid out; anything else is not judged. */
    fun sameSubnet(a: String?, b: String?): Boolean {
        if (a == null || b == null) return true
        val x = a.split('.'); val y = b.split('.')
        if (x.size != 4 || y.size != 4) return true
        return x.take(3) == y.take(3)
    }

    fun report(str: Strings, f: NetFacts): NetReport {
        val wifi = str.get(R.string.help_adv_wifi)
        val guest = str.get(R.string.help_adv_guest)
        val vpn = str.get(R.string.help_adv_vpn)
        val firewall = str.get(R.string.help_adv_firewall)
        val openApp = str.get(R.string.help_adv_open_app)
        val steps = ArrayList<NetStep>()
        val advice = LinkedHashSet<String>()
        steps += NetStep(
            str.get(R.string.help_step_phone_wifi), f.wifi,
            if (f.wifi) (f.ownIp ?: str.get(R.string.help_step_connected)) else str.get(R.string.help_step_no_wifi),
        )
        val different = f.peerIp != null && f.ownIp != null && !sameSubnet(f.ownIp, f.peerIp)
        if (f.peerIp != null) {
            steps += NetStep(
                str.get(R.string.help_step_same_network), !different,
                if (different) str.get(R.string.help_step_not_next, f.peerIp, f.ownIp ?: "")
                else str.get(R.string.help_step_computer_at, f.peerIp),
            )
            steps += NetStep(
                str.get(R.string.help_step_answers), f.probe == Probe.Connected,
                str.get(
                    when (f.probe) {
                        Probe.Connected -> R.string.help_probe_connected
                        Probe.Refused -> R.string.help_probe_refused
                        Probe.TimedOut -> R.string.help_probe_timed_out
                        Probe.NoRoute -> R.string.help_probe_no_route
                        Probe.NotTried -> R.string.help_probe_not_tried
                        Probe.Other -> R.string.help_probe_other
                    },
                ),
            )
        } else {
            steps += NetStep(str.get(R.string.help_step_found), false, str.get(R.string.help_step_not_found))
        }
        if (f.vpn) steps += NetStep(str.get(R.string.help_step_no_vpn), false, str.get(R.string.help_step_vpn_active))
        val ok = f.wifi && f.probe == Probe.Connected
        val headline: String
        when {
            !f.wifi -> {
                headline = str.get(R.string.help_head_no_wifi)
                advice += str.get(R.string.help_adv_connect_phone)
            }
            f.peerIp == null -> {
                headline = str.get(R.string.help_head_not_found)
                advice += openApp; advice += wifi; advice += guest
                if (f.vpn) advice += vpn
                advice += firewall
            }
            ok -> headline = str.get(R.string.help_head_ok)
            f.probe == Probe.Refused -> {
                headline = str.get(R.string.help_head_refused)
                advice += openApp
                advice += str.get(R.string.help_adv_start_service)
            }
            different || f.probe == Probe.NoRoute -> {
                headline = str.get(R.string.help_head_different)
                advice += wifi; advice += guest
                if (f.vpn) advice += vpn
            }
            else -> {
                headline = str.get(R.string.help_head_no_answer)
                advice += firewall; advice += guest
                if (f.vpn) advice += vpn
                advice += wifi; advice += openApp
            }
        }
        return NetReport(ok, headline, steps, advice.toList())
    }

    /** Dial [host]:[port] with a plain TCP connect. Blocking: call off the main thread. */
    fun probe(host: String, port: Int, timeoutMs: Int = 4000): Probe = try {
        Socket().use { it.connect(InetSocketAddress(host, port), timeoutMs) }
        Probe.Connected
    } catch (_: SocketTimeoutException) {
        Probe.TimedOut
    } catch (e: ConnectException) {
        // "ECONNREFUSED" is a real answer from the computer; "ENETUNREACH" / "EHOSTUNREACH" are not.
        val m = e.message.orEmpty().lowercase()
        when {
            "refused" in m -> Probe.Refused
            "unreachable" in m || "no route" in m -> Probe.NoRoute
            "timed out" in m -> Probe.TimedOut
            else -> Probe.Other
        }
    } catch (_: NoRouteToHostException) {
        Probe.NoRoute
    } catch (_: java.io.IOException) {
        Probe.Other
    }

    /** Wi-Fi/VPN state and this phone's IPv4 address. Needs only ACCESS_NETWORK_STATE. */
    fun localFacts(ctx: Context): Triple<Boolean, String?, Boolean> {
        val cm = ctx.getSystemService(ConnectivityManager::class.java) ?: return Triple(false, null, false)
        val net = cm.activeNetwork
        val caps = net?.let { cm.getNetworkCapabilities(it) }
        val vpn = caps?.hasTransport(NetworkCapabilities.TRANSPORT_VPN) == true
        val lan = caps != null && (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) ||
            caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) || vpn)
        val ip = net?.let { cm.getLinkProperties(it) }?.linkAddresses
            ?.map { it.address }
            ?.firstOrNull { it is Inet4Address && !it.isLoopbackAddress }
            ?.hostAddress
        return Triple(lan, ip, vpn)
    }

    /** Runs the whole check for the computer at [host]:[port] (or none) on a worker thread. */
    fun run(ctx: Context, host: String?, port: Int, onDone: (NetReport) -> Unit) {
        val app = ctx.applicationContext
        val main = android.os.Handler(android.os.Looper.getMainLooper())
        Thread({
            val (wifi, ip, vpn) = localFacts(app)
            val probe = if (host != null) probe(host, port) else Probe.NotTried
            val r = report(app.strings, NetFacts(wifi, ip, vpn, host, probe))
            main.post { onDone(r) }
        }, "geekbridge-nettest").apply { isDaemon = true }.start()
    }
}
