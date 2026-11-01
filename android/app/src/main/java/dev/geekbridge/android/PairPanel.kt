package dev.geekbridge.android

import android.app.Activity
import android.content.Context
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView

/**
 * "Pair with your computer" for the setup wizard: look for computers on the network, pair with one, see who is
 * paired. Nothing starts until the user taps "Look for computers". Rendering only; [TransportController] owns state.
 */
class PairPanel(
    private val activity: Activity,
    private val kit: UiKit,
    private val controller: TransportController,
    private var container: LinearLayout,
    private val onPairedChanged: () -> Unit,
) {
    private val ui = kit.ui
    private val main = Handler(Looper.getMainLooper())
    private val prompt = PairPromptDialog(activity, ui, controller)
    private var multicast: WifiManager.MulticastLock? = null
    private var pendingDevice: String? = null
    private var lastSig = ""
    private var lastPaired = AppState.pairedCount
    private val observer: () -> Unit = { render() }
    private var attached = false

    private val poller = object : Runnable {
        override fun run() {
            controller.poll()
            main.postDelayed(this, if (controller.phase == LinkPhase.Running) 1000L else 500L)
        }
    }

    /** The wizard rebuilds its page on every state change; point the panel at the new container. */
    fun rebind(newContainer: LinearLayout) {
        container = newContainer
        lastSig = ""
        render()
    }

    fun attach() {
        if (attached) return
        attached = true
        controller.observers += observer
        main.post(poller)
        render()
    }

    /** Pauses everything. The link is only stopped if the app is really leaving and no service owns it. */
    fun detach(stopLink: Boolean) {
        if (!attached) return
        attached = false
        controller.observers -= observer
        main.removeCallbacks(poller)
        prompt.dismiss()
        releaseMulticast()
        if (stopLink) controller.stop("Link stopped because the app left the foreground.")
    }

    private fun start() {
        pendingDevice = null
        controller.start(
            LinkRuntime.startOptions(
                stateDir = activity.filesDir.absolutePath,
                deviceName = Build.MODEL ?: "Android",
                discoveryListen = true,
                announce = true,
            ),
        )
    }

    fun render() {
        val s = controller.snapshot
        val phase = controller.phase
        syncMulticast(phase == LinkPhase.Running && s?.discovery?.listening == true)

        // Turn "Pair" on a found computer into the actual request once the secure channel is up.
        pendingDevice?.let { id ->
            s?.connections?.firstOrNull { it.peer.deviceId == id && it.phase == "parked" }?.let {
                pendingDevice = null
                controller.pairRequest(it.id, await = false)
            }
        }
        if (AppState.pairedCount != lastPaired) {
            lastPaired = AppState.pairedCount
            main.post { onPairedChanged() }
        }
        prompt.sync()

        val sig = listOf(
            phase, s?.discovered?.joinToString { "${it.deviceId}${it.ip}${it.paired}" },
            s?.connections?.joinToString { "${it.id}${it.phase}${it.trusted}" },
            s?.paired?.joinToString { it.deviceId }, controller.notice?.text, pendingDevice, AppState.pairedNames,
        ).joinToString("|")
        if (sig == lastSig) return
        lastSig = sig
        container.removeAllViews()
        build(phase, s)
    }

    private fun build(phase: LinkPhase, s: TransportSnapshot?) {
        val names = AppState.pairedNames
        if (names.isNotEmpty()) {
            for (n in names) {
                kit.row(container, R.drawable.ic_computer, n, "Paired and trusted", kit.chip("Paired", Tone.Ok), Tone.Ok)
            }
        }
        when (phase) {
            LinkPhase.Stopped -> {
                container.addView(
                    kit.text(
                        if (names.isEmpty()) "Open GeekBridge or KDE Connect on your computer, on the same Wi-Fi network as this phone, then look for it."
                        else "To add another computer, open GeekBridge on it, on the same Wi-Fi network, then look for it.",
                        14f, Tone.Muted,
                    ).apply { setPadding(0, kit.dp(8), 0, 0) },
                )
                kit.button(container, "Look for computers", BtnStyle.Primary) { start() }
            }
            LinkPhase.Starting, LinkPhase.Stopping ->
                container.addView(kit.text("Working…", 14f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })
            LinkPhase.Running -> {
                val found = s?.discovered.orEmpty().filter { !it.paired }
                val conns = s?.connections.orEmpty().filter { !it.trusted }
                if (found.isEmpty() && conns.isEmpty()) {
                    container.addView(kit.text("Looking for computers on this network… keep GeekBridge open on the computer.", 14f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })
                }
                for (d in found) {
                    if (conns.any { it.peer.deviceId == d.deviceId }) continue
                    val busy = pendingDevice == d.deviceId
                    kit.row(
                        container, R.drawable.ic_computer, d.name.ifEmpty { "Computer" }, d.ip,
                        kit.pill(if (busy) "Connecting…" else "Pair", BtnStyle.Tonal, !busy) {
                            pendingDevice = d.deviceId
                            controller.connect(d.ip, d.tcpPort, d.deviceId)
                            render()
                        },
                    )
                }
                for (c in conns) {
                    val name = c.peer.name.ifEmpty { "Computer" }
                    val text = when (c.phase) {
                        "parked" -> "Connected. Not paired yet."
                        "pairing_request" -> "Waiting for the computer to answer…"
                        "pairing_await" -> "Waiting for the computer to ask to pair…"
                        else -> "Working…"
                    }
                    kit.row(container, R.drawable.ic_computer, name, text, null)
                    if (c.phase == "parked") {
                        kit.button(container, "Pair with $name", BtnStyle.Primary) { controller.pairRequest(c.id, await = false) }
                        kit.button(container, "Wait for $name to ask", BtnStyle.Tonal) { controller.pairRequest(c.id, await = true) }
                    }
                }
                kit.button(container, "Stop looking", BtnStyle.Plain) { controller.stop() }
            }
        }
        val n = controller.notice
        if (n != null && phase != LinkPhase.Stopped) {
            container.addView(kit.text(n.text, 13f, n.tone).apply { setPadding(0, kit.dp(10), 0, 0) })
        } else if (n != null && n.tone != Tone.Muted) {
            container.addView(kit.text(n.text, 13f, n.tone).apply { setPadding(0, kit.dp(10), 0, 0) })
        }
    }

    private fun syncMulticast(wanted: Boolean) {
        if (!wanted) return releaseMulticast()
        if (multicast?.isHeld == true) return
        val wifi = activity.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager ?: return
        multicast = wifi.createMulticastLock("geekbridge-discovery").apply {
            setReferenceCounted(false)
            try { acquire() } catch (_: SecurityException) { /* discovery may miss broadcasts; the link is unaffected */ }
        }
    }

    private fun releaseMulticast() {
        multicast?.let { if (it.isHeld) it.release() }
        multicast = null
    }
}
