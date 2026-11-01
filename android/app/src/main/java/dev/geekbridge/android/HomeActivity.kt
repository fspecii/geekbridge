package dev.geekbridge.android

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.widget.LinearLayout
import android.widget.Toast

/**
 * The phone app's home: who this phone is paired with and whether it is connected, quick actions, the
 * status of calls / messages / agenda, and settings. Developer details live in [MainActivity] (Diagnostics).
 *
 * Re-checks permissions and battery state on every resume and shows a persistent warning when something
 * that was working has been revoked. The page is rebuilt only when what it shows changes.
 */
class HomeActivity : Activity() {
    private lateinit var kit: UiKit
    private lateinit var screen: Screen
    private lateinit var controller: TransportController
    private val main = Handler(Looper.getMainLooper())
    private var lastSig = ""
    private var offers: OfferDialogs? = null
    private val prompt by lazy { PairPromptDialog(this, kit.ui, controller) }
    private val observer: () -> Unit = { render() }

    private val poller = object : Runnable {
        override fun run() {
            controller.poll()
            main.postDelayed(this, POLL_MS)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppState.init(this)
        ActivityTracker.install(this)
        kit = UiKit(this, Ui(this))
        screen = Screen(this, kit).also { it.install() }
        controller = LinkRuntime.controller()
        if (savedInstanceState == null && !AppState.setupFinished) startActivity(SetupActivity.intent(this))
    }

    override fun onStart() {
        super.onStart()
        controller.observers += observer
        LinkService.sync(this)
        ensureLinkWhileVisible()
        offers = OfferDialogs(this, ShareCoordinator.get(this)).also { it.attach() }
        ShareCoordinator.get(this).controller.appVisible = true
        main.post(poller)
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        // Android lets us read the clipboard now; remember it briefly (nothing is sent by this).
        if (hasFocus) ClipboardRuntime.captureOnFocus(this)
    }

    override fun onResume() {
        super.onResume()
        lastSig = "" // permissions and battery may have changed in system settings
        render()
    }

    override fun onStop() {
        ShareCoordinator.get(this).controller.appVisible = false
        offers?.detach()
        offers = null
        main.removeCallbacks(poller)
        controller.observers -= observer
        prompt.dismiss()
        if (ActivityTracker.leavingApp(this)) controller.stop("Link stopped because the app left the foreground.")
        super.onStop()
    }

    /** Without the background service the link only lives while the app is open. */
    private fun ensureLinkWhileVisible() {
        if (AppState.pairedCount > 0 && !LinkService.isRunning && controller.phase == LinkPhase.Stopped) {
            controller.start(
                LinkRuntime.startOptions(filesDir.absolutePath, Build.MODEL ?: "Android", discoveryListen = false, announce = true),
            )
        }
    }

    // ------------------------------------------------------------------ state

    private fun granted(p: String) = checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED

    private fun inputs() = SetupInputs(
        sdk = Build.VERSION.SDK_INT,
        pairedCount = AppState.pairedCount,
        notifications = status(Manifest.permission.POST_NOTIFICATIONS, Build.VERSION.SDK_INT >= 33),
        contacts = status(Manifest.permission.READ_CONTACTS),
        calendar = status(Manifest.permission.READ_CALENDAR),
        phoneState = status(Manifest.permission.READ_PHONE_STATE),
        answerCalls = status(Manifest.permission.ANSWER_PHONE_CALLS),
        callLog = status(Manifest.permission.READ_CALL_LOG),
        battery = PowerChecks.batteryState(this),
        oemAcknowledged = AppState.oemAcknowledged,
        oem = OemDetector.detect(Build.MANUFACTURER, Build.BRAND),
        wasDone = AppState.wasDone(),
    )

    private fun status(p: String, applies: Boolean = true) = if (!applies) PermissionStatus.Granted else
        PermissionStatusLogic.compute(granted(p), AppState.wasAsked(p), shouldShowRequestPermissionRationale(p))

    // ------------------------------------------------------------------ rendering

    private fun render() {
        prompt.sync()
        val s = controller.snapshot
        val connected = s?.connections.orEmpty().filter { it.trusted }.map { it.peer.name.ifEmpty { getString(R.string.home_computer) } }
        val inp = inputs()
        val steps = SetupModel.steps(inp)
        AppState.rememberDone(steps)
        val warnings = SetupModel.warnings(inp, AppState.setupFinished)
        val hero = HomeModel.hero(
            AppState.pairedNames, AppState.pairedCount, controller.phase, connected, AppState.keepConnected, LinkService.isRunning,
        )
        val sig = listOf(hero, connected, warnings, steps.map { it.status }, AppState.keepConnected, AppState.setupFinished, QuickActions.sendClipboard != null).joinToString("|")
        if (sig == lastSig) return
        lastSig = sig
        build(hero, connected, warnings, steps)
    }

    private fun build(hero: HeroState, connected: List<String>, warnings: List<SetupModel.Warning>, steps: List<StepInfo>) {
        val root = screen.root
        root.removeAllViews()
        fun status(id: StepId) = steps.first { it.id == id }.status

        // Header
        val header = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; gravity = android.view.Gravity.CENTER_VERTICAL }
        header.addView(kit.heading(getString(R.string.app_name), 32f), LinearLayout.LayoutParams(0, Ui.WRAP, 1f))
        val gear = kit.icon(R.drawable.ic_settings, 24, kit.color(R.color.gb_muted)).apply {
            contentDescription = getString(R.string.home_cd_setup)
            isClickable = true
            isFocusable = true
            kit.asButton(this)
            setPadding(kit.dp(10), kit.dp(10), kit.dp(10), kit.dp(10))
            setOnClickListener { startActivity(SetupActivity.intent(this@HomeActivity, review = true)) }
            layoutParams = LinearLayout.LayoutParams(kit.dp(48), kit.dp(48))
        }
        header.addView(gear)
        root.addView(header, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(4) })

        // Warning / unfinished setup
        if (warnings.isNotEmpty() || !AppState.setupFinished) {
            val w = kit.panel(root, 14, kit.withAlpha(kit.ui.toneColor(Tone.Warn), 0x1F), kit.withAlpha(kit.ui.toneColor(Tone.Warn), 0x66))
            (w.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(12)
            val first = warnings.firstOrNull()
            w.addView(kit.text(first?.text ?: getString(R.string.home_finish_setup), 14f, Tone.Warn, bold = true))
            if (warnings.size > 1) w.addView(kit.text(resources.getQuantityString(R.plurals.home_more_to_check, warnings.size - 1, warnings.size - 1), 13f, Tone.Muted).apply { setPadding(0, kit.dp(4), 0, 0) })
            kit.button(w, getString(if (first != null) R.string.home_fix else R.string.home_continue_setup), BtnStyle.Primary) {
                startActivity(SetupActivity.intent(this, step = first?.step))
            }
        }

        // Hero: paired device status
        val heroPanel = kit.panel(root, 18, kit.withAlpha(kit.blue, 0x14), kit.withAlpha(kit.blue, 0x40))
        (heroPanel.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(16)
        val top = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; gravity = android.view.Gravity.CENTER_VERTICAL }
        top.addView(kit.badge(R.drawable.ic_computer, if (hero.tone == Tone.Normal || hero.tone == Tone.Muted) null else hero.tone, 56))
        val col = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        col.addView(kit.text(hero.title, 20f, bold = true))
        col.addView(kit.text(hero.subtitle, 14f, Tone.Muted).apply { setPadding(0, kit.dp(2), 0, 0) })
        top.addView(col, LinearLayout.LayoutParams(0, Ui.WRAP, 1f).apply { marginStart = kit.dp(14) })
        heroPanel.addView(top)
        if (hero.action == HeroAction.Pair) {
            kit.button(heroPanel, getString(R.string.home_pair_computer), BtnStyle.Primary) {
                startActivity(SetupActivity.intent(this, step = StepId.Pair))
            }
        } else if (AppState.pairedNames.isNotEmpty()) {
            heroPanel.addView(
                kit.text(getString(R.string.home_paired_list, AppState.pairedNames.joinToString(", ")), 13f, Tone.Muted).apply { setPadding(0, kit.dp(12), 0, 0) },
            )
        }

        // Quick actions
        kit.label(root, getString(R.string.home_section_quick))
        val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        root.addView(row, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        val paired = AppState.pairedCount
        val sub = HomeModel.tileSubtitle(paired, connected)
        kit.tile(row, R.drawable.ic_file, getString(R.string.home_send_file), sub, paired > 0) { if (paired > 0) sendFile() else needPairing() }
        val clipHook = QuickActions.sendClipboard
        kit.tile(row, R.drawable.ic_clipboard, getString(R.string.home_send_clipboard), if (clipHook == null && paired > 0) getString(R.string.home_opens_clipboard_tools) else sub, paired > 0) { if (paired > 0) sendClipboard() else needPairing() }

        // Feature status
        kit.label(root, getString(R.string.home_section_phone))
        val feats = kit.panel(root, 8)
        featureRow(feats, R.drawable.ic_call, R.string.home_feature_calls, status(StepId.PhoneState), R.string.home_calls_ok, R.string.home_calls_todo, StepId.PhoneState)
        kit.divider(feats)
        featureRow(
            feats, R.drawable.ic_call, R.string.home_answer,
            if (CallControlRuntime.settings.enabled) status(StepId.AnswerCalls) else StepStatus.Todo,
            R.string.home_answer_ok, R.string.home_answer_todo, StepId.AnswerCalls,
        )
        kit.divider(feats)
        kit.row(
            feats, R.drawable.ic_message, getString(R.string.home_messages), getString(R.string.pd_home_row_sub), kit.chip(getString(if (PhoneDataRuntime.active().sms) R.string.pd_state_on else R.string.pd_state_off), if (PhoneDataRuntime.active().sms) Tone.Ok else Tone.Muted), null,
        ) { startActivity(SetupActivity.intent(this, step = StepId.Sms)) }
        kit.divider(feats)
        featureRow(feats, R.drawable.ic_calendar, R.string.home_agenda, status(StepId.Calendar), R.string.home_agenda_ok, R.string.home_agenda_todo, StepId.Calendar)

        // Settings
        kit.label(root, getString(R.string.home_section_settings))
        val set = kit.panel(root, 8)
        kit.row(
            set, R.drawable.ic_computer, getString(R.string.home_stay_connected), getString(R.string.home_stay_connected_sub),
            kit.toggle(AppState.keepConnected) { on ->
                AppState.keepConnected = on
                LinkService.sync(this)
                lastSig = ""
                render()
            },
        )
        ClipboardSettingsUi.addRows(this, kit, set) { lastSig = ""; render() }
        kit.divider(set)
        kit.row(set, R.drawable.ic_check, getString(R.string.home_setup_row), getString(R.string.home_setup_row_sub), null) { startActivity(SetupActivity.intent(this, review = true)) }
        kit.divider(set)
        kit.row(set, R.drawable.ic_settings, getString(R.string.home_diag_row), getString(R.string.home_diag_row_sub, BuildConfig.VERSION_NAME), null) {
            startActivity(Intent(this, MainActivity::class.java))
        }
    }

    private fun featureRow(parent: LinearLayout, icon: Int, title: Int, st: StepStatus, okText: Int, todoText: Int, step: StepId) {
        val ok = st == StepStatus.Done
        kit.row(
            parent, icon, getString(title), getString(if (ok) okText else todoText),
            kit.chip(getString(if (ok) R.string.home_chip_ready else R.string.home_chip_allow), if (ok) Tone.Ok else Tone.Warn), if (ok) Tone.Ok else null,
        ) { startActivity(SetupActivity.intent(this, step = step)) }
    }

    // ------------------------------------------------------------------ quick actions

    private fun sendClipboard() {
        val hook = QuickActions.sendClipboard
        if (hook != null) {
            if (hook.isAvailable(this)) hook.launch(this) else toast(hook.unavailableReason(this) ?: getString(R.string.home_not_available_now))
        } else {
            startActivity(Intent(this, MainActivity::class.java)) // clipboard tools live in Diagnostics for now
        }
    }

    private fun sendFile() {
        val hook = QuickActions.sendFile
        if (hook != null) {
            if (hook.isAvailable(this)) hook.launch(this) else toast(hook.unavailableReason(this) ?: getString(R.string.home_not_available_now))
            return
        }
        // Same flow as the system Share sheet: pick documents, then ShareActivity sends them.
        val pick = Intent(Intent.ACTION_OPEN_DOCUMENT)
            .addCategory(Intent.CATEGORY_OPENABLE).setType("*/*").putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        try {
            @Suppress("DEPRECATION")
            startActivityForResult(pick, REQ_PICK)
        } catch (_: ActivityNotFoundException) {
            toast(getString(R.string.home_no_file_picker))
        }
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_PICK || resultCode != RESULT_OK || data == null) return
        val clip = data.clipData
        val send = Intent(this, ShareActivity::class.java).setAction(Intent.ACTION_SEND_MULTIPLE)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        if (clip != null) send.clipData = clip else data.data?.let { send.clipData = ClipData.newRawUri("files", it) }
        startActivity(send)
    }

    /** The tiles look dimmed without a computer; tapping one explains why and goes straight to pairing. */
    private fun needPairing() {
        toast(getString(R.string.home_pair_first_toast))
        startActivity(SetupActivity.intent(this, step = StepId.Pair))
    }

    private fun toast(s: String) = Toast.makeText(this, s, Toast.LENGTH_SHORT).show()

    private companion object {
        const val POLL_MS = 2000L
        const val REQ_PICK = 71
    }
}
