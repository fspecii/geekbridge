// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.content.Context
import android.content.Intent
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
    /** Set when a computer got paired while this panel was open: shows the "what to do next" card. */
    private var showNext = false
    private var netReport: NetReport? = null
    private var testing = false
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
        if (stopLink) controller.stop(R.string.link_stop_foreground)
    }

    private fun start() {
        pendingDevice = null
        // The user will look at the other computer to compare the code: keep the link alive if this screen is left.
        AppState.startPairingGrace()
        controller.start(
            LinkRuntime.startOptions(
                stateDir = activity.filesDir.absolutePath,
                deviceName = DeviceName.current(activity),
                discoveryListen = true,
                announce = true,
            ),
        )
        LinkService.sync(activity) // after the start, so the service finds the link already running
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
            if (AppState.pairedCount > lastPaired) showNext = true
            lastPaired = AppState.pairedCount
            main.post { onPairedChanged() }
        }
        prompt.sync()

        val sig = listOf(
            phase, s?.discovered?.joinToString { "${it.deviceId}${it.ip}${it.paired}" },
            s?.connections?.joinToString { "${it.id}${it.phase}${it.trusted}" },
            s?.paired?.joinToString { "${it.deviceId}${it.name}" }, controller.notice?.text, pendingDevice, AppState.pairedNames,
            showNext, testing, netReport?.headline,
        ).joinToString("|")
        if (sig == lastSig) return
        lastSig = sig
        container.removeAllViews()
        build(phase, s)
    }

    private fun build(phase: LinkPhase, s: TransportSnapshot?) {
        val names = AppState.pairedNames
        if (names.isNotEmpty()) {
            val peers = s?.paired.orEmpty()
            for ((i, n) in names.withIndex()) {
                // Unlink is right on the row once the core has listed the computer; the chip stands in until then.
                val peer = peers.getOrNull(i)?.takeIf { it.name == n } ?: peers.firstOrNull { it.name == n }
                val trailing = if (peer != null) kit.pill(activity.getString(R.string.link_unpair), BtnStyle.Plain) { UnlinkDialog.show(activity, controller, peer) }
                else kit.chip(activity.getString(R.string.pair_chip_paired), Tone.Ok)
                kit.row(container, R.drawable.ic_computer, n, activity.getString(R.string.pair_paired_and_trusted), trailing, Tone.Ok)
            }
            container.addView(kit.text(activity.getString(R.string.pair_manage_hint), 13f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, kit.dp(6)) })
        }
        if (showNext && names.isNotEmpty()) {
            NextSteps.card(activity, kit, container, onHome = {
                activity.startActivity(Intent(activity, HomeActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP))
            }, onFile = {
                activity.startActivity(Intent(activity, HomeActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP))
            })
            kit.button(container, activity.getString(R.string.qr_next_done), BtnStyle.Plain) { showNext = false; render() }
        }
        // No code needed: scan the QR code the computer shows.
        kit.button(container, activity.getString(R.string.pair_scan_qr), if (names.isEmpty()) BtnStyle.Primary else BtnStyle.Tonal) {
            activity.startActivity(QrPairActivity.scanIntent(activity))
        }
        // No computer app yet: get the download page to the computer.
        kit.button(container, activity.getString(R.string.pair_dont_have_computer_app), BtnStyle.Plain) {
            activity.startActivity(Intent(activity, ComputerLinkActivity::class.java))
        }
        when (phase) {
            LinkPhase.Stopped -> {
                container.addView(
                    kit.text(
                        activity.getString(if (names.isEmpty()) R.string.pair_intro_first else R.string.pair_intro_more),
                        14f, Tone.Muted,
                    ).apply { setPadding(0, kit.dp(8), 0, 0) },
                )
                kit.button(container, activity.getString(R.string.pair_look), if (names.isEmpty()) BtnStyle.Tonal else BtnStyle.Plain) { start() }
            }
            LinkPhase.Starting, LinkPhase.Stopping ->
                container.addView(kit.text(activity.getString(R.string.pair_working), 14f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })
            LinkPhase.Running -> {
                val found = s?.discovered.orEmpty().filter { !it.paired }
                val conns = s?.connections.orEmpty().filter { !it.trusted }
                if (found.isEmpty() && conns.isEmpty()) {
                    container.addView(kit.text(activity.getString(R.string.pair_looking), 14f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })
                }
                for (d in found) {
                    if (conns.any { it.peer.deviceId == d.deviceId }) continue
                    val busy = pendingDevice == d.deviceId
                    kit.row(
                        container, R.drawable.ic_computer, d.name.ifEmpty { activity.getString(R.string.home_computer) }, d.ip,
                        kit.pill(activity.getString(if (busy) R.string.pair_connecting else R.string.pair_action), BtnStyle.Tonal, !busy) {
                            pendingDevice = d.deviceId
                            controller.connect(d.ip, d.tcpPort, d.deviceId)
                            render()
                        },
                    )
                }
                for (c in conns) {
                    val name = c.peer.name.ifEmpty { activity.getString(R.string.home_computer) }
                    val text = when (c.phase) {
                        "parked" -> activity.getString(R.string.pair_phase_parked)
                        "pairing_request" -> activity.getString(R.string.pair_phase_request)
                        "pairing_await" -> activity.getString(R.string.pair_phase_await)
                        else -> activity.getString(R.string.pair_working)
                    }
                    kit.row(container, R.drawable.ic_computer, name, text, null)
                    if (c.phase == "parked") {
                        kit.button(container, activity.getString(R.string.pair_with, name), BtnStyle.Primary) { controller.pairRequest(c.id, await = false) }
                        kit.button(container, activity.getString(R.string.pair_wait_for, name), BtnStyle.Tonal) { controller.pairRequest(c.id, await = true) }
                    }
                }
                val target = found.firstOrNull() ?: s?.discovered.orEmpty().firstOrNull()
                kit.button(container, activity.getString(if (testing) R.string.qr_testing else R.string.pair_test_connection), BtnStyle.Tonal, enabled = !testing) {
                    testing = true
                    netReport = null
                    render()
                    NetCheck.run(activity, target?.ip, target?.tcpPort ?: 1716) { r -> testing = false; netReport = r; render() }
                }
                netReport?.let { ReportView.add(kit, container, it) }
                kit.button(container, activity.getString(R.string.pair_stop_looking), BtnStyle.Plain) { AppState.endPairingGrace(); controller.stop(); LinkService.sync(activity) }
            }
        }
        val n = controller.notice
        // A "waiting for the other device" note is only true while a pairing is actually in progress.
        val inProgress = pendingDevice != null || s?.connections.orEmpty().any { !it.trusted }
        if (n != null && phase != LinkPhase.Stopped && (n.tone != Tone.Muted || inProgress || names.isEmpty())) {
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
