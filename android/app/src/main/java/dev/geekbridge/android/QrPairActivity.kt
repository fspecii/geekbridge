// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipboardManager
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.MediaStore
import android.text.InputType
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Pair by QR code, without a camera permission. The computer shows a QR code that carries a
 * `geekbridge://pair?...` link. It can arrive here three ways: the phone's camera app or Google Lens opens the
 * link (the manifest intent filter), a scanner app returns it, or the user pastes it. GeekBridge never opens the camera.
 *
 * The link pins the computer's certificate. The core refuses to send the one-time token unless the certificate
 * on the live connection is the pinned one, and this phone still asks "Pair with X?" before trusting it.
 */
class QrPairActivity : Activity() {
    private lateinit var kit: UiKit
    private lateinit var screen: Screen
    private lateinit var controller: TransportController
    private lateinit var prompt: PairPromptDialog
    private val main = Handler(Looper.getMainLooper())

    private sealed interface Mode {
        data object Entry : Mode
        /** A link was handed to the core and its answer has not arrived: no form yet, so no stray taps. */
        data object Reading : Mode
        /** [scanned]: the user scanned it just now with this app's own camera screen, so the page can say so. */
        data class Confirm(val target: QrTarget, val scanned: Boolean = false) : Mode
        data class Connecting(val target: QrTarget) : Mode
        data class Failed(val why: Explained, val host: String?, val port: Int) : Mode
    }

    private var mode: Mode = Mode.Entry
    private var plan: QrPlan? = null
    private var report: NetReport? = null
    private var testing = false
    private var linkField: android.widget.EditText? = null
    private var message: String? = null
    private var linkPending = false
    private val gate = QrLinkGate()

    private val ticker = object : Runnable {
        override fun run() {
            tick()
            main.postDelayed(this, 700L)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppState.init(this)
        ActivityTracker.install(this)
        kit = UiKit(this, Ui(this))
        screen = Screen(this, kit).also { it.install() }
        controller = LinkRuntime.controller()
        prompt = PairPromptDialog(this, kit.ui, controller)
        // A link from outside (camera app, browser, another app) must be confirmed. The link itself (it holds a
        // one-time token) is never stored in saved state; only a flag that its confirmation was still waiting. If
        // the system recreates the activity at that moment (first launch from a cold start, a permission grant, a
        // theme change), the launch intent still carries the link and is read again, so the confirm page comes back.
        // Opened from the setup wizard's "Scan QR code from my computer": go straight to the camera.
        if (savedInstanceState == null && intent.getBooleanExtra(EXTRA_SCAN, false) && !intent.hasLink()) openScanner()
        if (QrReplay.shouldHandle(savedInstanceState?.let { true } ?: false, savedInstanceState?.getBoolean(KEY_WAITING, false) ?: false)) handleIntent(intent)
        render()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        // A link from outside must not take over a pairing that is already running.
        if (mode is Mode.Connecting) return
        handleIntent(intent)
        render()
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putBoolean(KEY_WAITING, linkPending || mode is Mode.Confirm)
    }

    override fun onStart() {
        super.onStart()
        main.post(ticker)
    }

    override fun onStop() {
        main.removeCallbacks(ticker)
        prompt.dismiss()
        super.onStop()
    }

    // Back while "Connecting..." is Cancel: drop the attempt (its steps stop, nothing more is dialled or sent, a half-open
    // connection to that computer is closed) and show the page the user came from. Elsewhere Back closes the page.
    // Android 13+ delivers Back through the callback, which is registered only while connecting (so the system's own
    // back animation still plays on the other pages); older versions use onBackPressed.
    private val backCallback = android.window.OnBackInvokedCallback { cancelAttempt() }
    private var backRegistered = false

    private fun syncBackCallback() {
        if (Build.VERSION.SDK_INT < 33) return
        val want = mode is Mode.Connecting
        if (want == backRegistered) return
        backRegistered = want
        if (want) onBackInvokedDispatcher.registerOnBackInvokedCallback(android.window.OnBackInvokedDispatcher.PRIORITY_DEFAULT, backCallback)
        else onBackInvokedDispatcher.unregisterOnBackInvokedCallback(backCallback)
    }

    @android.annotation.SuppressLint("GestureBackNavigation")
    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        if (mode is Mode.Connecting) cancelAttempt() else {
            @Suppress("DEPRECATION")
            super.onBackPressed()
        }
    }

    private fun cancelAttempt() {
        val m = mode
        gate.supersede()
        plan = null
        prompt.dismiss()
        if (m is Mode.Connecting) {
            controller.snapshot?.connections.orEmpty()
                .filter { it.peer.deviceId == m.target.deviceId && !it.trusted }
                .forEach { controller.disconnect(it.id) }
        }
        mode = Mode.Entry
        render()
    }

    private fun handleIntent(i: Intent?) {
        val text = QrProtocol.linkText(i?.dataString) ?: return
        linkPending = true
        mode = Mode.Reading
        parse(text, confirm = true)
    }

    // ------------------------------------------------------------------ input

    private fun Intent.hasLink() = QrProtocol.linkText(dataString) != null

    private fun parse(text: String, confirm: Boolean, scanned: Boolean = false) {
        message = null
        val ticket = gate.start()
        controller.raw(QrProtocol.parseRequest(text)) { o ->
            // The user may have cancelled, or a newer link replaced this one, while the core was reading it.
            if (!gate.accept(ticket)) return@raw
            linkPending = false
            when (o) {
                is CoreOutcome.Ok -> {
                    val t = QrProtocol.parseTarget(o.result)
                    if (t == null) fail(getString(R.string.qr_err_unreadable)) else if (confirm) { mode = Mode.Confirm(t, scanned); render() } else begin(t)
                }
                is CoreOutcome.Rejected -> fail(QrProtocol.parseError(strings, o.code))
                else -> fail(getString(R.string.qr_err_core_missing))
            }
        }
    }

    private fun fail(text: String) {
        message = text
        mode = Mode.Entry
        render()
    }

    /** The in-app camera screen ([QrScanActivity]): the lightest way to scan, with no scanner app needed. */
    @Suppress("DEPRECATION")
    private fun openScanner() {
        startActivityForResult(Intent(this, QrScanActivity::class.java), REQ_CAMERA)
    }

    @Suppress("DEPRECATION")
    private fun scanWithApp() {
        val i = Intent("com.google.zxing.client.android.SCAN").putExtra("SCAN_MODE", "QR_CODE_MODE")
        try {
            startActivityForResult(i, REQ_SCAN)
        } catch (_: ActivityNotFoundException) {
            message = getString(R.string.qr_scan_app_missing)
            render()
        }
    }

    private fun openCamera() {
        try {
            startActivity(Intent(MediaStore.INTENT_ACTION_STILL_IMAGE_CAMERA))
        } catch (_: ActivityNotFoundException) {
            message = getString(R.string.qr_scan_app_missing)
            render()
        }
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        when (requestCode) {
            REQ_SCAN -> if (resultCode == RESULT_OK) {
                // Any installed app can answer this intent, so the result is confirmed like a link from outside.
                val text = QrProtocol.linkText(data?.getStringExtra("SCAN_RESULT"))
                if (text != null) { linkPending = true; mode = Mode.Reading; render(); parse(text, confirm = QrConfirmPolicy.required(QrSource.ScannerApp)) } else fail(QrProtocol.parseError(strings, "qr_not_geekbridge"))
            }
            REQ_CAMERA -> if (resultCode == RESULT_OK) {
                // Scanned on purpose, but the page still names the computer and asks before anything connects.
                val text = QrProtocol.linkText(data?.getStringExtra(QrScanActivity.EXTRA_LINK))
                if (text != null) { linkPending = true; mode = Mode.Reading; render(); parse(text, confirm = true, scanned = true) }
                else fail(QrProtocol.parseError(strings, "qr_not_geekbridge"))
            }
        }
    }

    // ------------------------------------------------------------------ pairing

    private fun begin(t: QrTarget) {
        // One tap starts pairing: a second tap on a stale Continue (or a late callback) must not restart it.
        if (!QrContinue.mayBegin(mode is Mode.Confirm || mode is Mode.Entry, mode is Mode.Connecting)) return
        gate.supersede()
        plan = QrPlan(t)
        report = null
        mode = Mode.Connecting(t)
        startLinkIfStopped()
        render()
    }

    /** The scan is self-contained: this phone only dials out, so discovery stays off. */
    private fun startLinkIfStopped() {
        if (controller.phase != LinkPhase.Stopped) return
        AppState.startPairingGrace()
        controller.start(
            LinkRuntime.startOptions(
                stateDir = filesDir.absolutePath,
                deviceName = DeviceName.current(this),
                discoveryListen = false,
                announce = false,
            ),
        )
        LinkService.sync(this)
    }

    private fun tick() {
        val m = mode
        if (m !is Mode.Connecting) return
        // Forgetting the last computer stops the link (E2E-3): start it again to carry on with this scan.
        if (controller.phase == LinkPhase.Stopped) startLinkIfStopped()
        if (controller.phase != LinkPhase.Running) return
        controller.poll()
        prompt.sync()
        when (val step = plan?.next(controller.snapshot, SystemClock.elapsedRealtime())) {
            is QrStep.Dial -> controller.connect(step.address.host, step.address.port, m.target.deviceId)
            is QrStep.Pair -> controller.qrPairRequest(step.connectionId, m.target.token, m.target.certSha256) { code ->
                if (code != null) failed(m.target, PairingHelp.explain(strings, null, code, viaQr = true))
            }
            QrStep.Paired -> {
                prompt.dismiss()
                LinkService.sync(this)
                // Land on Home, which shows the success. The name is the one the user just approved.
                val name = QrConfirmText.safeName(m.target.name).ifEmpty { getString(R.string.home_computer) }
                startActivity(HomeActivity.pairedIntent(this, name))
                finish()
            }
            is QrStep.Failed -> {
                val e = controller.events.lastOrNull { it.kind == "pairing_failed" || it.kind == "connect_failed" }
                val why = when (step.why) {
                    QrFailure.NoAddressAnswered -> PairingHelp.explain(strings, "connect_failed", e?.code ?: "timeout", viaQr = true)
                    QrFailure.ConnectionLost -> PairingHelp.explain(strings, e?.kind, e?.code, viaQr = true)
                }
                failed(m.target, why)
            }
            is QrStep.Forget -> controller.unpair(step.deviceId)
            QrStep.Waiting, null -> {}
        }
    }

    private fun failed(t: QrTarget, why: Explained) {
        if (mode is Mode.Failed) return
        prompt.dismiss()
        val a = t.addresses.firstOrNull()
        mode = Mode.Failed(why, a?.host, a?.port ?: 1716)
        render()
    }

    // ------------------------------------------------------------------ views

    private fun render() {
        val root = screen.root
        root.removeAllViews()
        linkField = null
        root.addView(DotText(this, getString(R.string.qr_title), maxPitchDp = Tok.DOT_NUMERAL, maxLines = 3, accentDot = true), LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(8); bottomMargin = kit.dp(16) })
        when (val m = mode) {
            Mode.Entry -> entry(root)
            Mode.Reading -> reading(root)
            is Mode.Confirm -> confirm(root, m.target, m.scanned)
            is Mode.Connecting -> connecting(root, m.target)
            is Mode.Failed -> failedView(root, m)
        }
        syncBackCallback()
        screen.requestInsets()
    }

    private fun reading(root: LinearLayout) {
        kit.panel(root).addView(kit.text(getString(R.string.qr_reading), 15f))
    }

    private fun entry(root: LinearLayout) {
        val card = kit.panel(root)
        card.addView(kit.text(getString(R.string.qr_intro), 15f))
        message?.let { card.addView(kit.text(it, 14f, Tone.Warn).apply { setPadding(0, kit.dp(10), 0, 0) }) }
        kit.button(card, getString(R.string.qr_scan_in_app), BtnStyle.Primary) { openScanner() }
        kit.button(card, getString(R.string.qr_scan_app), BtnStyle.Tonal) { scanWithApp() }
        kit.button(card, getString(R.string.qr_camera), BtnStyle.Tonal) { openCamera() }
        card.addView(kit.text(getString(R.string.qr_camera_hint), 12f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, kit.dp(10)) })
        kit.divider(card)
        card.addView(kit.text(getString(R.string.qr_paste_header), 14f, bold = true).apply { setPadding(0, kit.dp(10), 0, 0) })
        val field = kit.ui.input(card, getString(R.string.qr_link_hint), InputType.TYPE_TEXT_VARIATION_URI, QrProtocol.MAX_TEXT_CHARS, lines = 3)
        linkField = field
        kit.button(card, getString(R.string.qr_paste), BtnStyle.Tonal) {
            val cm = getSystemService(ClipboardManager::class.java)
            val t = cm?.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(this)
            if (t != null) field.setText(t.take(QrProtocol.MAX_TEXT_CHARS))
        }
        kit.button(card, getString(R.string.qr_continue), BtnStyle.Primary) {
            val text = QrProtocol.linkText(field.text)
            if (text != null) parse(text, confirm = QrConfirmPolicy.required(QrSource.Pasted)) else fail(QrProtocol.parseError(strings, "qr_not_geekbridge"))
        }
        kit.button(root, getString(R.string.qr_close), BtnStyle.Plain) { finish() }
    }

    private fun confirm(root: LinearLayout, t: QrTarget, scanned: Boolean) {
        QrConfirmView.add(
            kit, root, t, scanned, onContinue = { begin(t) },
            onCancel = { gate.supersede(); mode = Mode.Entry; render() },
        )
    }

    private fun connecting(root: LinearLayout, t: QrTarget) {
        val card = kit.panel(root)
        card.addView(kit.text(getString(R.string.qr_connecting, Bidi.isolate(QrConfirmText.safeName(t.name).ifEmpty { getString(R.string.pair_this_computer) })), 18f, bold = true))
        card.addView(kit.text(getString(R.string.qr_connecting_hint), 14f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })
        kit.button(card, getString(R.string.qr_cancel), BtnStyle.Plain) {
            cancelAttempt()
        }
    }

    private fun failedView(root: LinearLayout, m: Mode.Failed) {
        val card = kit.panel(root)
        card.addView(kit.text(m.why.headline, 18f, Tone.Bad, bold = true))
        card.addView(kit.text(m.why.advice, 15f).apply { setPadding(0, kit.dp(8), 0, kit.dp(4)) })
        kit.button(card, getString(if (testing) R.string.qr_testing else R.string.qr_test), BtnStyle.Tonal, enabled = !testing) {
            testing = true
            report = null
            render()
            NetCheck.run(this, m.host, m.port) { r -> testing = false; report = r; render() }
        }
        report?.let { ReportView.add(kit, card, it) }
        kit.button(card, getString(R.string.qr_try_again), BtnStyle.Primary) { mode = Mode.Entry; report = null; render() }
        kit.button(root, getString(R.string.qr_close), BtnStyle.Plain) { finish() }
    }

    companion object {
        private const val KEY_WAITING = "link_waiting"
        private const val REQ_SCAN = 81
        private const val REQ_CAMERA = 83
        /** Set by the setup wizard: open the camera screen at once. */
        const val EXTRA_SCAN = "scan"

        fun scanIntent(ctx: android.content.Context): Intent = Intent(ctx, QrPairActivity::class.java).putExtra(EXTRA_SCAN, true)
    }
}

/** The "Link to <computer>?" page before anything connects (also drawn by the debug design gallery). */
object QrConfirmView {
    fun add(kit: UiKit, root: LinearLayout, t: QrTarget, scanned: Boolean, onContinue: () -> Unit, onCancel: () -> Unit) {
        val c = kit.context
        val name = QrConfirmText.safeName(t.name).ifEmpty { c.getString(R.string.pair_this_computer) }
        val card = kit.panel(root)
        // The name and address come from whoever made the link: say so, and show the part that cannot be faked.
        if (scanned) {
            card.addView(kit.cardTitle(c.getString(R.string.qr_scan_confirm_title, Bidi.isolate(name)), 20f))
            card.addView(kit.text(c.getString(R.string.qr_scan_confirm_body), 15f).apply { setPadding(0, kit.dp(8), 0, kit.dp(8)) })
        } else {
            card.addView(kit.cardTitle(c.getString(R.string.qr_confirm_title2), 20f))
            card.addView(kit.text(c.getString(R.string.qr_confirm_body2), 15f).apply { setPadding(0, kit.dp(8), 0, kit.dp(8)) })
        }
        card.addView(kit.text(c.getString(R.string.qr_confirm_name_label), 12f, Tone.Muted))
        card.addView(kit.text(c.getString(R.string.qr_confirm_name_unverified, Bidi.isolate(name), c.getString(R.string.qr_confirm_unverified)), 16f, Tone.Warn, bold = true))
        card.addView(kit.text(c.getString(R.string.qr_confirm_addr_label), 12f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })
        card.addView(kit.ui.mono(QrConfirmText.address(t), 13f))
        card.addView(kit.text(c.getString(R.string.qr_confirm_fp_label), 12f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })
        card.addView(kit.ui.mono(QrConfirmText.fingerprintPrefix(t.certSha256), 16f, bold = true))
        card.addView(kit.text(c.getString(R.string.qr_confirm_fp_hint), 12f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, kit.dp(6)) })
        kit.button(card, c.getString(R.string.qr_continue), BtnStyle.Primary) { onContinue() }
        kit.button(card, c.getString(R.string.qr_cancel), BtnStyle.Plain) { onCancel() }
    }
}

/** The "what to do next" card shown right after a first pairing. */
object NextSteps {
    fun card(ctx: android.content.Context, kit: UiKit, parent: LinearLayout, onHome: () -> Unit, onFile: () -> Unit) {
        val card = kit.panel(parent)
        card.addView(kit.text(ctx.getString(R.string.qr_next_title), 16f, bold = true))
        card.addView(kit.text(ctx.getString(R.string.list_numbered, 1, ctx.getString(R.string.qr_next_clipboard)), 14f).apply { setPadding(0, kit.dp(8), 0, 0) })
        kit.button(card, ctx.getString(R.string.qr_next_home), BtnStyle.Tonal) { onHome() }
        card.addView(kit.text(ctx.getString(R.string.list_numbered, 2, ctx.getString(R.string.qr_next_file)), 14f).apply { setPadding(0, kit.dp(8), 0, 0) })
        kit.button(card, ctx.getString(R.string.qr_next_file_btn), BtnStyle.Tonal) { onFile() }
    }
}

/** Renders a [NetReport] as a headline, one line per check and a short list of things to try. */
object ReportView {
    fun add(kit: UiKit, parent: LinearLayout, r: NetReport) {
        parent.addView(kit.text(r.headline, 16f, if (r.ok) Tone.Ok else Tone.Warn, bold = true).apply { setPadding(0, kit.dp(12), 0, kit.dp(4)) })
        for (s in r.steps) {
            parent.addView(kit.text(kit.context.getString(if (s.ok) R.string.net_step_ok else R.string.net_step_bad, s.label, s.detail), 13f, if (s.ok) Tone.Muted else Tone.Warn))
        }
        for (a in r.advice) parent.addView(kit.text(kit.context.getString(R.string.list_bullet, a), 13f).apply { setPadding(0, kit.dp(4), 0, 0) })
    }
}
