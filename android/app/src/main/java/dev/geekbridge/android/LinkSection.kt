// SPDX-License-Identifier: GPL-3.0-or-later
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

    private val card = ui.card(root, activity.getString(R.string.link_title), accent = true)
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
        ui.body(card, activity.getString(R.string.link_intro), Tone.Muted)
        stateLine = ui.statusLine(card, activity.getString(R.string.link_state_stopped_short), Tone.Muted)
        toggle = ui.button(card, activity.getString(R.string.link_start), emphasis = Emphasis.Primary) { onToggle() }
        // Everything below is for testing and troubleshooting; the defaults are right for everyday use.
        val dev = ui.expander(card, activity.getString(R.string.link_dev_title), activity.getString(R.string.link_dev_sub), "link-dev")

        optListen = ui.check(dev, activity.getString(R.string.link_opt_listen), checked = true)
        optPort = ui.input(dev, activity.getString(R.string.link_opt_port), InputType.TYPE_CLASS_NUMBER, 5)
        optDiscover = ui.check(dev, activity.getString(R.string.link_opt_discover))
        optAnnounce = ui.check(dev, activity.getString(R.string.link_opt_announce))
        optLoopback = if (BuildConfig.DEBUG) {
            ui.check(dev, activity.getString(R.string.link_opt_loopback))
        } else {
            null
        }

        // Details while running.
        card.addView(infoBox, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        nameValue = ui.row(infoBox, 0, activity.getString(R.string.link_row_name), "", Tone.Normal)
        idValue = ui.row(infoBox, 1, activity.getString(R.string.link_row_id), "", Tone.Normal).apply { typeface = android.graphics.Typeface.MONOSPACE; textSize = 12f }
        listenValue = ui.row(infoBox, 2, activity.getString(R.string.link_row_listen), "", Tone.Normal)
        discoveryValue = ui.row(infoBox, 3, activity.getString(R.string.link_row_discovery), "", Tone.Normal)
        ui.body(infoBox, activity.getString(R.string.link_own_cert), Tone.Muted)
        certText = ui.mono("", 12f).also { infoBox.addView(it, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP)) }

        // Manual connect.
        dev.addView(connectBox, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        ui.divider(connectBox)
        ui.body(connectBox, activity.getString(R.string.link_connect_intro), Tone.Muted)
        hostField = ui.input(connectBox, activity.getString(R.string.link_hint_host), InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI, 64)
        portField = ui.input(connectBox, activity.getString(R.string.link_hint_port), InputType.TYPE_CLASS_NUMBER, 5)
        idField = ui.input(connectBox, activity.getString(R.string.link_hint_device_id), InputType.TYPE_CLASS_TEXT, 64)
        ui.button(connectBox, activity.getString(R.string.link_connect)) { onConnect() }

        for ((box, title) in listOf(
            discoveredBox to activity.getString(R.string.link_sec_discovered),
            connectionsBox to activity.getString(R.string.link_sec_connections),
            pairedBox to activity.getString(R.string.link_sec_paired),
            eventsBox to activity.getString(R.string.link_sec_events),
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
            controller.stop(R.string.notice_left_foreground)
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
                    return controller.postNotice(activity.getString(R.string.link_port_range), Tone.Warn)
                }
                controller.start(
                    StartOptions(
                        stateDir = activity.filesDir.absolutePath,
                        deviceName = DeviceName.current(activity),
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
            return controller.postNotice(activity.getString(R.string.link_enter_connect), Tone.Warn)
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
        toggle.text = if (running) activity.getString(R.string.link_stop) else activity.getString(R.string.link_start)
        infoBox.visibility = vis(running && s?.local != null)
        connectBox.visibility = vis(running)

        when (phase) {
            LinkPhase.Stopped -> {
                ui.setStatus(stateLine, activity.getString(R.string.link_state_stopped), Tone.Muted)
                ui.setStatus(summary, activity.getString(R.string.link_summary_stopped), Tone.Muted)
                ui.setTone(pairedRow, activity.getString(R.string.link_paired_none_read), Tone.Muted)
            }
            LinkPhase.Starting -> ui.setStatus(stateLine, activity.getString(R.string.link_state_starting), Tone.Muted)
            LinkPhase.Stopping -> ui.setStatus(stateLine, activity.getString(R.string.link_state_stopping), Tone.Muted)
            LinkPhase.Running -> {
                val conns = s?.connections.orEmpty()
                val trusted = conns.count { it.trusted }
                ui.setStatus(stateLine, activity.getString(R.string.link_state_running), Tone.Ok)
                ui.setStatus(
                    summary,
                    activity.getString(R.string.link_summary_running, conns.size, trusted),
                    Tone.Ok,
                )
                val paired = s?.paired.orEmpty()
                ui.setTone(
                    pairedRow,
                    if (s?.pairedError == true) activity.getString(R.string.link_paired_unreadable) else if (paired.isEmpty()) activity.getString(R.string.link_paired_none) else paired.size.toString(),
                    if (s?.pairedError == true) Tone.Warn else Tone.Normal,
                )
            }
        }

        if (running && s?.local != null) {
            nameValue.text = s.local.deviceName
            idValue.text = s.local.deviceId
            certText.text = TransportText.group(s.local.certSha256)
            listenValue.text = s.listen?.let { "${it.host}:${it.port}" } ?: activity.getString(R.string.link_listen_none)
            val d = s.discovery
            discoveryValue.text = when {
                d == null -> activity.getString(R.string.link_disc_off)
                d.error != null -> activity.getString(R.string.link_disc_problem, d.error)
                d.listening && d.announcing -> activity.getString(R.string.link_disc_both, d.port.toString())
                d.listening -> activity.getString(R.string.link_disc_listening, d.port.toString())
                d.announcing -> activity.getString(R.string.link_disc_announcing)
                else -> activity.getString(R.string.link_disc_off)
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
        val ignored = (s?.undisplayablePrompts ?: 0) > 0
        if (n != null) {
            ui.setTone(noticeText, if (ignored) activity.getString(R.string.link_notice_with_ignored, n.text) else n.text, n.tone)
        } else {
            noticeText.text = ""
        }
        noticeText.visibility = vis(n != null)

        syncDialog()
    }

    private fun noName() = activity.getString(R.string.link_no_name)

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
            ui.body(discoveredBox, activity.getString(R.string.link_disc_intro), Tone.Muted)
            list.forEachIndexed { i, d ->
                ui.row(discoveredBox, i, Bidi.isolate(d.name.ifEmpty { noName() }), "${d.ip}:${d.tcpPort}".let { if (d.paired) activity.getString(R.string.link_disc_paired_before, it) else it }, Tone.Muted)
                ui.mono(d.deviceId, 11f, tone = Tone.Muted).also { discoveredBox.addView(it) }
                ui.button(discoveredBox, activity.getString(R.string.link_connect_to, Bidi.isolate(d.name.ifEmpty { activity.getString(R.string.link_this_computer) }))) { controller.connect(d.ip, d.tcpPort, d.deviceId) }
            }
        }
    }

    private fun phaseText(c: LinkConnection): Pair<String, Tone> = when {
        c.trusted -> activity.getString(R.string.link_phase_trusted, c.pingsReceived) to Tone.Ok
        c.phase == "parked" -> activity.getString(R.string.link_phase_parked) to Tone.Muted
        c.phase == "pairing_request" -> activity.getString(R.string.link_phase_pairing_request) to Tone.Muted
        c.phase == "pairing_await" -> activity.getString(R.string.link_phase_pairing_await) to Tone.Muted
        else -> activity.getString(R.string.link_phase_other, c.phase) to Tone.Muted
    }

    private fun renderConnections(s: TransportSnapshot?) {
        val list = s?.connections.orEmpty()
        val sig = list.joinToString("|") { "${it.id}/${it.phase}/${it.trusted}/${it.peer.name}/${it.peer.certSha256}/${it.pingsReceived}" }
        fill(connectionsBox, sig) {
            for (c in list) {
                val (text, tone) = phaseText(c)
                ui.body(connectionsBox, activity.getString(R.string.link_conn_head, Bidi.isolate(c.peer.name.ifEmpty { noName() }), c.peer.ip, c.role), Tone.Normal)
                ui.mono(c.peer.deviceId, 11f, tone = Tone.Muted).also { connectionsBox.addView(it) }
                ui.body(connectionsBox, text, tone)
                if (!c.trusted && c.peer.certSha256.isNotEmpty()) {
                    ui.body(connectionsBox, activity.getString(R.string.link_their_cert), Tone.Muted)
                    ui.mono(TransportText.group(c.peer.certSha256), 11f).also { connectionsBox.addView(it) }
                }
                val actions = ArrayList<Pair<String, () -> Unit>>()
                when {
                    c.trusted -> actions += (activity.getString(R.string.link_send_ping) to { controller.ping(c.id) })
                    c.phase == "parked" -> {
                        actions += (activity.getString(R.string.link_pair_ellipsis) to { controller.pairRequest(c.id, await = false) })
                        actions += (activity.getString(R.string.link_wait_for_request) to { controller.pairRequest(c.id, await = true) })
                    }
                }
                actions += (activity.getString(R.string.link_disconnect) to { controller.disconnect(c.id) })
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
                ui.body(pairedBox, Bidi.isolate(p.name.ifEmpty { noName() }), Tone.Normal)
                ui.mono(p.deviceId, 11f, tone = Tone.Muted).also { pairedBox.addView(it) }
                ui.mono(TransportText.group(p.certSha256), 11f, tone = Tone.Muted).also { pairedBox.addView(it) }
                ui.check(pairedBox, activity.getString(R.string.link_auto_accept), checked = policy.autoAccept(p.deviceId)).setOnCheckedChangeListener { _, on ->
                    policy.setAutoAccept(p.deviceId, on)
                }
                ui.button(pairedBox, activity.getString(R.string.link_unpair_button, Bidi.isolate(p.name.ifEmpty { activity.getString(R.string.link_this_computer) })), emphasis = Emphasis.Quiet) { confirmUnpair(p) }
            }
        }
    }

    private fun renderEvents() {
        val recent = controller.events.takeLast(5).reversed()
        val sig = recent.joinToString("|") { "${it.seq}" }
        fill(eventsBox, sig) {
            recent.forEachIndexed { i, e -> ui.listRow(eventsBox, i, TransportText.event(activity.strings, e), TransportText.eventTone(e)) }
        }
    }

    private fun confirmUnpair(p: PairedPeer) {
        AlertDialog.Builder(activity)
            .setTitle(activity.getString(R.string.link_unpair_title, Bidi.isolate(p.name.ifEmpty { activity.getString(R.string.link_this_computer) })))
            .setMessage(activity.getString(R.string.link_unpair_message))
            .setPositiveButton(R.string.link_unpair) { _, _ -> controller.unpair(p.deviceId) }
            .setNegativeButton(R.string.link_cancel, null)
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
        val secs = (p.expiresInMs / 1000).coerceAtLeast(0).toInt()
        val pad = ui.dp(20)
        val body = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, ui.dp(8), pad, 0)
        }
        body.addView(
            ui.text(
                if (outgoing) activity.getString(R.string.link_prompt_outgoing, Bidi.isolate(p.peerName.ifEmpty { activity.getString(R.string.link_this_device) }))
                else activity.getString(R.string.link_prompt_incoming, Bidi.isolate(p.peerName.ifEmpty { activity.getString(R.string.link_a_device) })),
            ),
        )
        body.addView(ui.mono(p.code, 28f, bold = true).apply { letterSpacing = 0.12f })
        body.addView(ui.text(activity.getString(R.string.link_their_device_id), Tone.Muted, 12f))
        body.addView(ui.mono(p.peerDeviceId, 12f))
        body.addView(ui.text(activity.getString(R.string.link_their_cert_must_match), Tone.Muted, 12f))
        body.addView(ui.mono(TransportText.group(p.peerCertSha256), 12f))
        if (p.ownCertSha256.isNotEmpty()) {
            body.addView(ui.text(activity.getString(R.string.link_own_cert_short), Tone.Muted, 12f))
            body.addView(ui.mono(TransportText.group(p.ownCertSha256), 12f))
        }
        body.addView(
            ui.text(
                activity.resources.getQuantityString(
                    R.plurals.link_prompt_warning, secs, secs,
                ),
                Tone.Muted,
            ),
        )
        val d = AlertDialog.Builder(activity)
            .setTitle(if (outgoing) activity.getString(R.string.link_prompt_title_outgoing, Bidi.isolate(p.peerName.ifEmpty { activity.getString(R.string.link_this_device) })) else activity.getString(R.string.link_prompt_title_incoming))
            .setView(ScrollView(activity).apply { addView(body) })
            .setCancelable(false)
            .setPositiveButton(R.string.link_prompt_pair) { _, _ -> finishPrompt(p, true) }
            .setNegativeButton(R.string.link_prompt_decline) { _, _ -> finishPrompt(p, false) }
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
