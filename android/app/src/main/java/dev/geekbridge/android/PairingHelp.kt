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
    fun explain(kind: String?, code: String?, viaQr: Boolean = false): Explained = when {
        code == "qr_certificate_mismatch" || code == "pin_mismatch" || code == "pairing_binding_changed" ->
            Explained(
                "This is not the computer you scanned.",
                "Nothing was paired. Check that you are on your own Wi-Fi, then scan the code again.",
            )
        code == "identity_mismatch" ->
            Explained("A different device answered at that address.", "Nothing was paired. Check the Wi-Fi and scan again.")
        code == "clock_skew" ->
            Explained("The clocks on the two devices disagree.", "Turn on automatic date and time on the phone and the computer.")
        code == "pairing_rejected_locally" ->
            Explained("Pairing was declined on this phone.", "Start again and tap Pair when you are ready.")
        code == "pairing_rejected_by_peer" && viaQr ->
            Explained(
                "The computer did not accept the QR code.",
                "It may have expired or been used already. Show a new QR code on the computer and scan it again.",
            )
        code == "pairing_rejected_by_peer" ->
            Explained("The computer declined the pairing.", "Start again and confirm on both devices.")
        code == "pairing_timeout" ->
            Explained("Pairing timed out before it was confirmed.", "Start again and confirm on both devices within half a minute.")
        code == "timeout" || code == "connect_timeout" || kind == "connect_failed" && code == null ->
            Explained(
                "The computer did not answer.",
                "Same Wi-Fi? Guest Wi-Fi and VPNs block this. Allow GeekBridge through the computer's firewall (TCP and UDP 1716-1764).",
            )
        code == "address_rejected" || code == "network_unreachable" ->
            Explained("The computer is on a different network.", "Put both devices on the same Wi-Fi and switch off VPNs.")
        code == "link_closed" || code == "peer_closed" || code == "park_timeout" || code == "io_error" ->
            Explained("The connection closed before pairing finished.", "Keep both apps open and the screens on, then try again. Still failing? Tap Test connection to check the Wi-Fi.")
        code == "tls_failed" ->
            Explained("A secure connection could not be set up.", "Try again. If it keeps failing, restart GeekBridge on the computer.")
        else -> Explained("Pairing did not work.", "Tap \"Test connection\" for a step-by-step check.")
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
    private const val WIFI = "Make sure the phone and the computer are on the same Wi-Fi network (not a guest network)."
    private const val GUEST = "Guest and public Wi-Fi often block devices from talking to each other (\"client isolation\"). Use your main Wi-Fi."
    private const val VPN = "Turn off any VPN on the phone and on the computer while pairing."
    private const val FIREWALL = "Allow GeekBridge through the computer's firewall on your home network (TCP and UDP 1716-1764)."
    private const val OPEN_APP = "Open GeekBridge on the computer and keep both screens on while pairing."

    /** Same /24 for IPv4, which is how nearly every home network is laid out; anything else is not judged. */
    fun sameSubnet(a: String?, b: String?): Boolean {
        if (a == null || b == null) return true
        val x = a.split('.'); val y = b.split('.')
        if (x.size != 4 || y.size != 4) return true
        return x.take(3) == y.take(3)
    }

    fun report(f: NetFacts): NetReport {
        val steps = ArrayList<NetStep>()
        val advice = LinkedHashSet<String>()
        steps += NetStep("Phone is on Wi-Fi or Ethernet", f.wifi, if (f.wifi) (f.ownIp ?: "Connected") else "No Wi-Fi connection found")
        val different = f.peerIp != null && f.ownIp != null && !sameSubnet(f.ownIp, f.peerIp)
        if (f.peerIp != null) {
            steps += NetStep(
                "Computer is on the same network", !different,
                if (different) "The computer is at ${f.peerIp}, which is not next to this phone (${f.ownIp})" else "Computer at ${f.peerIp}",
            )
            steps += NetStep(
                "Computer answers", f.probe == Probe.Connected,
                when (f.probe) {
                    Probe.Connected -> "Connected"
                    Probe.Refused -> "The computer refused: GeekBridge is not running there"
                    Probe.TimedOut -> "No answer"
                    Probe.NoRoute -> "No route to the computer"
                    Probe.NotTried -> "Not tried"
                    Probe.Other -> "Could not connect"
                },
            )
        } else {
            steps += NetStep("Computer has been found", false, "No computer announced itself yet")
        }
        if (f.vpn) steps += NetStep("No VPN in the way", false, "A VPN is active on this phone")
        val ok = f.wifi && f.probe == Probe.Connected
        val headline: String
        when {
            !f.wifi -> { headline = "This phone is not on Wi-Fi"; advice += "Connect the phone to the same Wi-Fi as the computer." }
            f.peerIp == null -> {
                headline = "No computer has been found yet"
                advice += OPEN_APP; advice += WIFI; advice += GUEST
                if (f.vpn) advice += VPN
                advice += FIREWALL
            }
            ok -> headline = "Your computer is reachable - you can pair now"
            f.probe == Probe.Refused -> {
                headline = "The computer is reachable, but GeekBridge is not listening on it"
                advice += OPEN_APP
                advice += "Start the GeekBridge service on the computer (it starts at login once installed)."
            }
            different || f.probe == Probe.NoRoute -> {
                headline = "The computer seems to be on a different network"
                advice += WIFI; advice += GUEST
                if (f.vpn) advice += VPN
            }
            else -> {
                headline = "The computer does not answer"
                advice += FIREWALL; advice += GUEST
                if (f.vpn) advice += VPN
                advice += WIFI; advice += OPEN_APP
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
            val r = report(NetFacts(wifi, ip, vpn, host, probe))
            main.post { onDone(r) }
        }, "geekbridge-nettest").apply { isDaemon = true }.start()
    }
}
