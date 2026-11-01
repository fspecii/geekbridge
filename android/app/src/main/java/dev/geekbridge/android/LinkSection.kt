package dev.geekbridge.android

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.text.InputType
import android.view.View
import android.widget.Button
import android.widget.CompoundButton
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

/**
 * The "Computer link" card: explicit Start/Stop for the Rust phone transport, discovery and manual connect,
 * and the pairing decision dialog. It only renders [TransportController] state; the controller owns the
 * lifecycle. Nothing starts, connects or approves without a tap here.
 */
class LinkSection(
    private val activity: Activity,
    private val ui: Ui,
    private val controller: TransportController,
    root: LinearLayout,
    private val summary: TextView,
    private val pairedRow: TextView,
) {
    private val main = Handler(Looper.getMainLooper())

    private val card = ui.card(root, "Computer link", accent = true)
    private val stateLine: TextView
    private val optListen: CompoundButton
    private val optPort: EditText
    private val optDiscover: CompoundButton
    private val optAnnounce: CompoundButton
    private val optLoopback: CompoundButton?
    private val toggle: Button
    private val infoBox = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
    private val nameValue: TextView
    private val idValue: TextView
    private val listenValue: TextView
    private val discoveryValue: TextView
    private val certText: TextView
    private val connectBox = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
    private val hostField: EditText
    private val portField: EditText
    private val idField: EditText
    private val discoveredBox = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
    private val connectionsBox = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
    private val pairedBox = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
    private val eventsBox = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
    private val noticeText: TextView

    private var dialog: AlertDialog? = null
    private var dialogRequestId = -1L
    private var multicastLock: WifiManager.MulticastLock? = null
    private val signatures = HashMap<LinearLayout, String>()

    private val poller = object : Runnable {
        override fun run() {
            controller.poll()
            main.postDelayed(this, pollDelayMs())
        }
    }

    init {
        ui.body(
            card,
            "Your phone and your computer talk over an encrypted link on your own network (TCP + TLS with " +
                "certificate pinning). Pairing needs your approval of an exact code and certificate. Once paired you " +
                "can share the clipboard, send files and answer calls, and, if you switch them on, share texts, " +
                "contacts and calendar. The link starts when you tap Start and stops when you leave the app.",
            Tone.Muted,
        )
        stateLine = ui.statusLine(card, "Link: stopped.", Tone.Muted)
        toggle = ui.button(card, "Start link", emphasis = Emphasis.Primary) { onToggle() }
        // Everything below is for testing and troubleshooting; the defaults are right for everyday use.
        val dev = ui.expander(card, "Developer options", "Ports, discovery and connecting by address", "link-dev")

        optListen = ui.check(dev, "Accept connections from computers (TCP port below)", checked = true)
        optPort = ui.input(dev, "Listen port (default 1716)", InputType.TYPE_CLASS_NUMBER, 5)
        optDiscover = ui.check(dev, "Look for computers on this network (UDP discovery)")
        optAnnounce = ui.check(dev, "Announce this phone on this network")
        optLoopback = if (BuildConfig.DEBUG) {
            ui.check(dev, "Testing only: allow loopback peers (debug build)")
        } else {
            null
        }

        // Details while running.
        card.addView(infoBox, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        nameValue = ui.row(infoBox, 0, "This phone’s name", "", Tone.Normal)
        idValue = ui.row(infoBox, 1, "Device id", "", Tone.Normal).apply { typeface = android.graphics.Typeface.MONOSPACE; textSize = 12f }
        listenValue = ui.row(infoBox, 2, "Listening", "", Tone.Normal)
        discoveryValue = ui.row(infoBox, 3, "Discovery", "", Tone.Normal)
        ui.body(infoBox, "This phone’s certificate (SHA-256) — the other device shows the same value:", Tone.Muted)
        certText = ui.mono("", 12f).also { infoBox.addView(it, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP)) }

        // Manual connect.
        dev.addView(connectBox, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        ui.divider(connectBox)
        ui.body(connectBox, "Connect to a computer by address. Only private local-network IP addresses are accepted; this does not pair.", Tone.Muted)
        hostField = ui.input(connectBox, "IP address, e.g. 192.168.1.20", InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI, 64)
        portField = ui.input(connectBox, "Port (e.g. 1716)", InputType.TYPE_CLASS_NUMBER, 5)
        idField = ui.input(connectBox, "Device id of that computer", InputType.TYPE_CLASS_TEXT, 64)
        ui.button(connectBox, "Connect") { onConnect() }

        for ((box, title) in listOf(
            discoveredBox to "Found on this network",
            connectionsBox to "Connections",
            pairedBox to "Paired computers",
            eventsBox to "Recent events",
        )) {
            card.addView(box, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
            box.tag = title
        }
        noticeText = ui.body(card, "", Tone.Muted)
        render()
    }

    // ------------------------------------------------------------------ lifecycle (called by MainActivity)

    fun onStart() {
        controller.listener = { render() }
        render()
        controller.poll()
        main.postDelayed(poller, pollDelayMs())
    }

    /**
     * [leavingForeground] is false for a configuration change (rotation): the link keeps running and the
     * recreated Activity reattaches to it. Otherwise the link stops, as documented.
     */
    fun onStop(leavingForeground: Boolean) {
        main.removeCallbacks(poller)
        controller.listener = null
        dismissDialog()
        if (leavingForeground) {
            controller.stop("Link stopped because the app left the foreground. Pending requests were rejected.")
            releaseMulticast()
        }
    }

    private fun pollDelayMs(): Long {
        val s = controller.snapshot
        val active = controller.phase != LinkPhase.Running ||
            s == null || s.prompts.isNotEmpty() || s.connections.any { !it.trusted }
        return if (active) 500L else 2000L
    }

    // ------------------------------------------------------------------ actions

    private fun onToggle() {
        when (controller.phase) {
            LinkPhase.Stopped -> {
                val portText = optPort.text.toString().trim()
                val port = if (portText.isEmpty()) null else portText.toIntOrNull()
                if (optListen.isChecked && portText.isNotEmpty() && (port == null || port !in 0..65535)) {
                    return controller.postNotice("Listen port must be a number from 0 to 65535.", Tone.Warn)
                }
                controller.start(
                    StartOptions(
                        stateDir = activity.filesDir.absolutePath,
                        deviceName = Build.MODEL ?: "Android",
                        listen = optListen.isChecked,
                        listenPort = if (optListen.isChecked) port else null,
                        discoveryListen = optDiscover.isChecked,
                        discoveryAnnounce = optAnnounce.isChecked && optListen.isChecked,
                        allowLoopback = BuildConfig.DEBUG && optLoopback?.isChecked == true,
                    ),
                )
            }
            LinkPhase.Running -> controller.stop()
            else -> Unit
        }
    }

    private fun onConnect() {
        val port = portField.text.toString().trim().toIntOrNull()
        val host = hostField.text.toString().trim()
        val id = idField.text.toString().trim()
        if (host.isEmpty() || port == null || id.isEmpty()) {
            return controller.postNotice("Enter an IP address, a port and the computer’s device id.", Tone.Warn)
        }
        controller.connect(host, port, id)
    }

    // ------------------------------------------------------------------ rendering

    fun render() {
        val phase = controller.phase
        val s = controller.snapshot
        val running = phase == LinkPhase.Running
        val idle = phase == LinkPhase.Stopped

        for (v in listOfNotNull(optListen, optPort, optDiscover, optAnnounce, optLoopback)) v.isEnabled = idle
        optPort.visibility = vis(idle || optListen.isChecked)
        toggle.isEnabled = phase == LinkPhase.Stopped || phase == LinkPhase.Running
        toggle.text = if (running) "Stop link" else "Start link"
        infoBox.visibility = vis(running && s?.local != null)
        connectBox.visibility = vis(running)

        when (phase) {
            LinkPhase.Stopped -> {
                ui.setStatus(stateLine, "Link: stopped. Nothing is listening and no connection is open.", Tone.Muted)
                ui.setStatus(summary, "Connection: link stopped. Nothing is listening, no port is open, and no computer is connected.", Tone.Muted)
                ui.setTone(pairedRow, "start the link to read", Tone.Muted)
            }
            LinkPhase.Starting -> ui.setStatus(stateLine, "Link: starting…", Tone.Muted)
            LinkPhase.Stopping -> ui.setStatus(stateLine, "Link: stopping…", Tone.Muted)
            LinkPhase.Running -> {
                val conns = s?.connections.orEmpty()
                val trusted = conns.count { it.trusted }
                ui.setStatus(stateLine, "Link: running.", Tone.Ok)
                ui.setStatus(
                    summary,
                    "Connection: link running — ${conns.size} open, $trusted trusted.",
                    Tone.Ok,
                )
                val paired = s?.paired.orEmpty()
                ui.setTone(
                    pairedRow,
                    if (s?.pairedError == true) "unreadable" else if (paired.isEmpty()) "none" else paired.size.toString(),
                    if (s?.pairedError == true) Tone.Warn else Tone.Normal,
                )
            }
        }

        if (running && s?.local != null) {
            nameValue.text = s.local.deviceName
            idValue.text = s.local.deviceId
            certText.text = TransportText.group(s.local.certSha256)
            listenValue.text = s.listen?.let { "${it.host}:${it.port}" } ?: "not listening (outbound only)"
            val d = s.discovery
            discoveryValue.text = when {
                d == null -> "off"
                d.error != null -> "problem (${d.error})"
                d.listening && d.announcing -> "listening and announcing (UDP ${d.port})"
                d.listening -> "listening (UDP ${d.port})"
                d.announcing -> "announcing"
                else -> "off"
            }
            syncMulticast(d?.listening == true)
        } else {
            syncMulticast(false)
        }

        renderDiscovered(s)
        renderConnections(s)
        renderPaired(s)
        renderEvents()

        val n = controller.notice
        val extra = if ((s?.undisplayablePrompts ?: 0) > 0) {
            " A pairing request could not be shown exactly and was ignored."
        } else {
            ""
        }
        if (n != null) ui.setTone(noticeText, n.text + extra, n.tone) else noticeText.text = ""
        noticeText.visibility = vis(n != null)

        syncDialog()
    }

    private fun vis(b: Boolean) = if (b) View.VISIBLE else View.GONE

    /** Rebuilds [box] only when what it shows changed, so buttons are not recreated under a finger every poll. */
    private fun fill(box: LinearLayout, signature: String, build: () -> Unit) {
        if (signatures[box] == signature) return
        signatures[box] = signature
        box.removeAllViews()
        if (signature.isEmpty()) {
            box.visibility = View.GONE
            return
        }
        box.visibility = View.VISIBLE
        ui.divider(box)
        box.addView(ui.text(box.tag as String, Tone.Muted, 12f, bold = true))
        build()
    }

    private fun renderDiscovered(s: TransportSnapshot?) {
        val list = s?.discovered.orEmpty()
        val sig = list.joinToString("|") { "${it.deviceId}/${it.name}/${it.ip}/${it.tcpPort}/${it.paired}" }
        fill(discoveredBox, sig) {
            ui.body(
                discoveredBox,
                "Heard on the network, not verified. Connecting only opens an encrypted channel; it does not pair.",
                Tone.Muted,
            )
            list.forEachIndexed { i, d ->
                ui.row(discoveredBox, i, d.name.ifEmpty { "(no name)" }, "${d.ip}:${d.tcpPort}" + if (d.paired) " · paired before" else "", Tone.Muted)
                ui.mono(d.deviceId, 11f, tone = Tone.Muted).also { discoveredBox.addView(it) }
                ui.button(discoveredBox, "Connect to ${d.name.ifEmpty { "this computer" }}") { controller.connect(d.ip, d.tcpPort, d.deviceId) }
            }
        }
    }

    private fun phaseText(c: LinkConnection): Pair<String, Tone> = when {
        c.trusted -> "Paired and trusted · pings received: ${c.pingsReceived}" to Tone.Ok
        c.phase == "parked" -> "Connected over TLS. Not paired." to Tone.Muted
        c.phase == "pairing_request" -> "Pairing request sent — waiting for the other device." to Tone.Muted
        c.phase == "pairing_await" -> "Waiting for the other device to ask to pair (this phone cannot ask on an incoming connection)." to Tone.Muted
        else -> "State: ${c.phase}" to Tone.Muted
    }

    private fun renderConnections(s: TransportSnapshot?) {
        val list = s?.connections.orEmpty()
        val sig = list.joinToString("|") { "${it.id}/${it.phase}/${it.trusted}/${it.peer.name}/${it.peer.certSha256}/${it.pingsReceived}" }
        fill(connectionsBox, sig) {
            for (c in list) {
                val (text, tone) = phaseText(c)
                ui.body(connectionsBox, "${c.peer.name.ifEmpty { "(no name)" }} · ${c.peer.ip} · ${c.role}", Tone.Normal)
                ui.mono(c.peer.deviceId, 11f, tone = Tone.Muted).also { connectionsBox.addView(it) }
                ui.body(connectionsBox, text, tone)
                if (!c.trusted && c.peer.certSha256.isNotEmpty()) {
                    ui.body(connectionsBox, "Their certificate (SHA-256):", Tone.Muted)
                    ui.mono(TransportText.group(c.peer.certSha256), 11f).also { connectionsBox.addView(it) }
                }
                val actions = ArrayList<Pair<String, () -> Unit>>()
                when {
                    c.trusted -> actions += ("Send ping" to { controller.ping(c.id) })
                    c.phase == "parked" -> {
                        actions += ("Pair…" to { controller.pairRequest(c.id, await = false) })
                        actions += ("Wait for their request" to { controller.pairRequest(c.id, await = true) })
                    }
                }
                actions += ("Disconnect" to { controller.disconnect(c.id) })
                ui.buttonRow(connectionsBox, *actions.toTypedArray())
            }
        }
    }

    private fun renderPaired(s: TransportSnapshot?) {
        val list = s?.paired.orEmpty()
        val policy = ShareCoordinator.get(activity).policy
        val sig = list.joinToString("|") { "${it.deviceId}/${it.name}/${it.certSha256}/${policy.autoAccept(it.deviceId)}" }
        fill(pairedBox, sig) {
            for (p in list) {
                ui.body(pairedBox, p.name.ifEmpty { "(no name)" }, Tone.Normal)
                ui.mono(p.deviceId, 11f, tone = Tone.Muted).also { pairedBox.addView(it) }
                ui.mono(TransportText.group(p.certSha256), 11f, tone = Tone.Muted).also { pairedBox.addView(it) }
                ui.check(pairedBox, "Auto-accept files from this computer", checked = policy.autoAccept(p.deviceId)).setOnCheckedChangeListener { _, on ->
                    policy.setAutoAccept(p.deviceId, on)
                }
                ui.button(pairedBox, "Unpair ${p.name.ifEmpty { "this computer" }}…", emphasis = Emphasis.Quiet) { confirmUnpair(p) }
            }
        }
    }

    private fun renderEvents() {
        val recent = controller.events.takeLast(5).reversed()
        val sig = recent.joinToString("|") { "${it.seq}" }
        fill(eventsBox, sig) {
            recent.forEachIndexed { i, e -> ui.listRow(eventsBox, i, TransportText.event(e), TransportText.eventTone(e)) }
        }
    }

    private fun confirmUnpair(p: PairedPeer) {
        AlertDialog.Builder(activity)
            .setTitle("Unpair ${p.name.ifEmpty { "this computer" }}?")
            .setMessage("This removes its stored certificate from this phone. It will have to be paired again, with a new approval, before it is trusted.")
            .setPositiveButton("Unpair") { _, _ -> controller.unpair(p.deviceId) }
            .setNegativeButton("Cancel", null)
            .show()
    }

    // ------------------------------------------------------------------ pairing dialog

    private fun syncDialog() {
        val pending = controller.pendingPrompt()
        val shown = dialog
        if (shown != null && (pending == null || pending.requestId != dialogRequestId)) dismissDialog()
        if (pending != null && dialog == null) showPrompt(pending)
    }

    private fun dismissDialog() {
        dialog?.dismiss()
        dialog = null
        dialogRequestId = -1
    }

    /** Shows exactly [p]; the buttons send back these same values. Not cancellable by a stray tap or Back. */
    private fun showPrompt(p: PairPrompt) {
        val outgoing = p.direction == "outgoing"
        val pad = ui.dp(20)
        val body = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, ui.dp(8), pad, 0)
        }
        body.addView(
            ui.text(
                if (outgoing) "You asked ${p.peerName.ifEmpty { "this device" }} to pair. Compare this code with the one it shows:"
                else "${p.peerName.ifEmpty { "A device" }} asks to pair. Compare this code with the one it shows:",
            ),
        )
        body.addView(ui.mono(p.code, 28f, bold = true).apply { letterSpacing = 0.12f })
        body.addView(ui.text("Their device id", Tone.Muted, 12f))
        body.addView(ui.mono(p.peerDeviceId, 12f))
        body.addView(ui.text("Their certificate (SHA-256) — must match what that device shows for itself", Tone.Muted, 12f))
        body.addView(ui.mono(TransportText.group(p.peerCertSha256), 12f))
        if (p.ownCertSha256.isNotEmpty()) {
            body.addView(ui.text("This phone’s certificate (SHA-256)", Tone.Muted, 12f))
            body.addView(ui.mono(TransportText.group(p.ownCertSha256), 12f))
        }
        body.addView(
            ui.text(
                "Approve only if the code and certificate match on both devices. Approving here is only this phone’s " +
                    "half: pairing also needs the other device to accept. This request expires in about " +
                    "${(p.expiresInMs / 1000).coerceAtLeast(0)} seconds.",
                Tone.Muted,
            ),
        )
        val d = AlertDialog.Builder(activity)
            .setTitle(if (outgoing) "Pair with ${p.peerName.ifEmpty { "this device" }}?" else "Pairing request")
            .setView(ScrollView(activity).apply { addView(body) })
            .setCancelable(false)
            .setPositiveButton("Pair") { _, _ -> finishPrompt(p, true) }
            .setNegativeButton("Decline") { _, _ -> finishPrompt(p, false) }
            .create()
        dialog = d
        dialogRequestId = p.requestId
        d.show()
    }

    private fun finishPrompt(p: PairPrompt, approve: Boolean) {
        dialog = null
        dialogRequestId = -1
        controller.decide(p, approve)
    }

    // ------------------------------------------------------------------ multicast lock (discovery only)

    private fun syncMulticast(wanted: Boolean) {
        if (wanted) {
            if (multicastLock?.isHeld == true) return
            val wifi = activity.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager ?: return
            multicastLock = wifi.createMulticastLock("geekbridge-discovery").apply {
                setReferenceCounted(false)
                try {
                    acquire()
                } catch (_: SecurityException) {
                    // Permission missing: discovery may miss broadcasts, the link itself is unaffected.
                }
            }
        } else {
            releaseMulticast()
        }
    }

    private fun releaseMulticast() {
        multicastLock?.let { if (it.isHeld) it.release() }
        multicastLock = null
    }
}
