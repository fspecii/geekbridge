package dev.geekbridge.android

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
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
import android.widget.Toast

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
        data class Confirm(val target: QrTarget) : Mode
        data class Connecting(val target: QrTarget) : Mode
        data class Done(val name: String) : Mode
        data class Failed(val why: Explained, val host: String?, val port: Int) : Mode
    }

    private var mode: Mode = Mode.Entry
    private var plan: QrPlan? = null
    private var report: NetReport? = null
    private var testing = false
    private var linkField: android.widget.EditText? = null
    private var message: String? = null

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
        // A link from outside (camera app, browser, another app) must be confirmed; nothing is stored in
        // saved state, so a rotation or process death simply returns to the start.
        if (savedInstanceState == null) handleIntent(intent)
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

    override fun onStart() {
        super.onStart()
        main.post(ticker)
    }

    override fun onStop() {
        main.removeCallbacks(ticker)
        prompt.dismiss()
        super.onStop()
    }

    private fun handleIntent(i: Intent?) {
        val text = QrProtocol.linkText(i?.dataString) ?: return
        parse(text, confirm = true)
    }

    // ------------------------------------------------------------------ input

    private fun parse(text: String, confirm: Boolean) {
        message = null
        controller.raw(QrProtocol.parseRequest(text)) { o ->
            when (o) {
                is CoreOutcome.Ok -> {
                    val t = QrProtocol.parseTarget(o.result)
                    if (t == null) fail("The pairing code could not be read.") else if (confirm) { mode = Mode.Confirm(t); render() } else begin(t)
                }
                is CoreOutcome.Rejected -> fail(QrProtocol.parseError(o.code))
                else -> fail("GeekBridge's network core is not available on this phone.")
            }
        }
    }

    private fun fail(text: String) {
        message = text
        mode = Mode.Entry
        render()
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
                // The user just scanned it on purpose: no extra confirmation step, the final "Pair?" still applies.
                val text = QrProtocol.linkText(data?.getStringExtra("SCAN_RESULT"))
                if (text != null) parse(text, confirm = false) else fail(QrProtocol.parseError("qr_not_geekbridge"))
            }
            REQ_PICK -> if (resultCode == RESULT_OK && data != null) {
                val clip = data.clipData
                val send = Intent(this, ShareActivity::class.java).setAction(Intent.ACTION_SEND_MULTIPLE)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                if (clip != null) send.clipData = clip else data.data?.let { send.clipData = ClipData.newRawUri("files", it) }
                startActivity(send)
            }
        }
    }

    // ------------------------------------------------------------------ pairing

    private fun begin(t: QrTarget) {
        plan = QrPlan(t)
        report = null
        mode = Mode.Connecting(t)
        if (controller.phase == LinkPhase.Stopped) {
            // The scan is self-contained: this phone only dials out, so discovery stays off.
            controller.start(
                LinkRuntime.startOptions(
                    stateDir = filesDir.absolutePath,
                    deviceName = Build.MODEL ?: "Android",
                    discoveryListen = false,
                    announce = false,
                ),
            )
        }
        render()
    }

    private fun tick() {
        val m = mode
        if (m !is Mode.Connecting) return
        if (controller.phase != LinkPhase.Running) return
        controller.poll()
        prompt.sync()
        when (val step = plan?.next(controller.snapshot, SystemClock.elapsedRealtime())) {
            is QrStep.Dial -> controller.connect(step.address.host, step.address.port, m.target.deviceId)
            is QrStep.Pair -> controller.qrPairRequest(step.connectionId, m.target.token, m.target.certSha256) { code ->
                if (code != null) failed(m.target, PairingHelp.explain(null, code, viaQr = true))
            }
            QrStep.Paired -> {
                prompt.dismiss()
                LinkService.sync(this)
                mode = Mode.Done(m.target.name.ifEmpty { getString(R.string.home_computer) })
                render()
            }
            is QrStep.Failed -> {
                val e = controller.events.lastOrNull { it.kind == "pairing_failed" || it.kind == "connect_failed" }
                val why = when (step.why) {
                    QrFailure.NoAddressAnswered -> PairingHelp.explain("connect_failed", e?.code ?: "timeout", viaQr = true)
                    QrFailure.ConnectionLost -> PairingHelp.explain(e?.kind, e?.code, viaQr = true)
                }
                failed(m.target, why)
            }
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
            is Mode.Confirm -> confirm(root, m.target)
            is Mode.Connecting -> connecting(root, m.target)
            is Mode.Done -> done(root, m.name)
            is Mode.Failed -> failedView(root, m)
        }
        screen.requestInsets()
    }

    private fun entry(root: LinearLayout) {
        val card = kit.panel(root)
        card.addView(kit.text(getString(R.string.qr_intro), 15f))
        message?.let { card.addView(kit.text(it, 14f, Tone.Warn).apply { setPadding(0, kit.dp(10), 0, 0) }) }
        kit.button(card, getString(R.string.qr_scan_app), BtnStyle.Primary) { scanWithApp() }
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
            if (text != null) parse(text, confirm = false) else fail(QrProtocol.parseError("qr_not_geekbridge"))
        }
        kit.button(root, getString(R.string.qr_close), BtnStyle.Plain) { finish() }
    }

    private fun confirm(root: LinearLayout, t: QrTarget) {
        val name = QrConfirmText.safeName(t.name).ifEmpty { getString(R.string.pair_this_computer) }
        val card = kit.panel(root)
        // The name and address come from whoever made the link: say so, and show the part that cannot be faked.
        card.addView(kit.text(getString(R.string.qr_confirm_title2), 20f, bold = true))
        card.addView(kit.text(getString(R.string.qr_confirm_body2), 15f).apply { setPadding(0, kit.dp(8), 0, kit.dp(8)) })
        card.addView(kit.text(getString(R.string.qr_confirm_name_label), 12f, Tone.Muted))
        card.addView(kit.text("“$name”  ·  " + getString(R.string.qr_confirm_unverified), 16f, Tone.Warn, bold = true))
        card.addView(kit.text(getString(R.string.qr_confirm_addr_label), 12f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })
        card.addView(kit.ui.mono(QrConfirmText.address(t), 13f))
        card.addView(kit.text(getString(R.string.qr_confirm_fp_label), 12f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })
        card.addView(kit.ui.mono(QrConfirmText.fingerprintPrefix(t.certSha256), 16f, bold = true))
        card.addView(kit.text(getString(R.string.qr_confirm_fp_hint), 12f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, kit.dp(6)) })
        kit.button(card, getString(R.string.qr_continue), BtnStyle.Primary) { begin(t) }
        kit.button(card, getString(R.string.qr_cancel), BtnStyle.Plain) { mode = Mode.Entry; render() }
    }

    private fun connecting(root: LinearLayout, t: QrTarget) {
        val card = kit.panel(root)
        card.addView(kit.text(getString(R.string.qr_connecting, t.name.ifEmpty { getString(R.string.pair_this_computer) }), 18f, bold = true))
        card.addView(kit.text(getString(R.string.qr_connecting_hint), 14f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })
        kit.button(card, getString(R.string.qr_cancel), BtnStyle.Plain) {
            plan = null
            mode = Mode.Entry
            render()
        }
    }

    private fun done(root: LinearLayout, name: String) {
        val card = kit.panel(root)
        card.addView(kit.text(getString(R.string.qr_done_title, name), 20f, Tone.Ok, bold = true))
        NextSteps.card(this, kit, root, onHome = {
            startActivity(Intent(this, HomeActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP))
            finish()
        }, onFile = { pickFile() })
        kit.button(root, getString(R.string.qr_next_done), BtnStyle.Primary) { finish() }
    }

    private fun pickFile() {
        val pick = Intent(Intent.ACTION_OPEN_DOCUMENT)
            .addCategory(Intent.CATEGORY_OPENABLE).setType("*/*").putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        try {
            @Suppress("DEPRECATION")
            startActivityForResult(pick, REQ_PICK)
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(this, R.string.qr_no_picker, Toast.LENGTH_SHORT).show()
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

    private companion object {
        const val REQ_SCAN = 81
        const val REQ_PICK = 82
    }
}

/** The "what to do next" card shown right after a first pairing. */
object NextSteps {
    fun card(ctx: android.content.Context, kit: UiKit, parent: LinearLayout, onHome: () -> Unit, onFile: () -> Unit) {
        val card = kit.panel(parent)
        card.addView(kit.text(ctx.getString(R.string.qr_next_title), 16f, bold = true))
        card.addView(kit.text("1. " + ctx.getString(R.string.qr_next_clipboard), 14f).apply { setPadding(0, kit.dp(8), 0, 0) })
        kit.button(card, ctx.getString(R.string.qr_next_home), BtnStyle.Tonal) { onHome() }
        card.addView(kit.text("2. " + ctx.getString(R.string.qr_next_file), 14f).apply { setPadding(0, kit.dp(8), 0, 0) })
        kit.button(card, ctx.getString(R.string.qr_next_file_btn), BtnStyle.Tonal) { onFile() }
    }
}

/** Renders a [NetReport] as a headline, one line per check and a short list of things to try. */
object ReportView {
    fun add(kit: UiKit, parent: LinearLayout, r: NetReport) {
        parent.addView(kit.text(r.headline, 16f, if (r.ok) Tone.Ok else Tone.Warn, bold = true).apply { setPadding(0, kit.dp(12), 0, kit.dp(4)) })
        for (s in r.steps) {
            parent.addView(kit.text((if (s.ok) "✔  " else "✖  ") + s.label + " – " + s.detail, 13f, if (s.ok) Tone.Muted else Tone.Warn))
        }
        for (a in r.advice) parent.addView(kit.text("• $a", 13f).apply { setPadding(0, kit.dp(4), 0, 0) })
    }
}
