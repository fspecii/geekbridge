package dev.geekbridge.android

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.view.View
import android.widget.LinearLayout
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher

/**
 * Setup wizard and permission master. One step per page (pair, notifications, contacts, calendar, phone
 * state, SMS, battery) with a clear state, a primary action and recovery links, and an overview page that
 * lists every step. Everything is recomputed on every resume, so a permission or battery exemption that is
 * revoked later shows up here and as a warning on the home screen.
 */
class SetupActivity : Activity() {
    private lateinit var kit: UiKit
    private lateinit var screen: Screen
    private var steps: List<StepInfo> = emptyList()
    private var page = 0
    private val oem = OemDetector.detect(Build.MANUFACTURER, Build.BRAND)
    private var pairPanel: PairPanel? = null
    private var pairBox: LinearLayout? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppState.init(this)
        ActivityTracker.install(this)
        kit = UiKit(this, Ui(this))
        screen = Screen(this, kit).also { it.install() }
        screen.footer.visibility = View.VISIBLE
        steps = SetupModel.steps(inputs())
        page = when {
            savedInstanceState != null -> savedInstanceState.getInt(KEY_PAGE, 0)
            intent.getStringExtra(EXTRA_STEP) != null ->
                steps.indexOfFirst { it.id.name == intent.getStringExtra(EXTRA_STEP) }.coerceAtLeast(0)
            intent.getBooleanExtra(EXTRA_REVIEW, false) -> steps.size
            else -> SetupModel.firstOpen(steps)?.let { f -> steps.indexOfFirst { it.id == f.id } } ?: steps.size
        }
    }

    // Predictive back: on the first page the system's own back-to-home animation plays; the callback is only
    // registered while Back means "previous page".
    private val backCallback = OnBackInvokedCallback { goBack() }
    private var backRegistered = false

    private fun syncBackCallback() {
        if (Build.VERSION.SDK_INT < 33) return
        val want = page > 0
        if (want == backRegistered) return
        backRegistered = want
        if (want) onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, backCallback)
        else onBackInvokedDispatcher.unregisterOnBackInvokedCallback(backCallback)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putInt(KEY_PAGE, page)
    }

    override fun onResume() {
        super.onResume()
        render()
    }

    override fun onStop() {
        pairPanel?.detach(stopLink = ActivityTracker.leavingApp(this))
        super.onStop()
    }

    // Android 13+ delivers Back through the callback registered in onCreate; this serves older versions.
    @android.annotation.SuppressLint("GestureBackNavigation")
    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        @Suppress("DEPRECATION")
        if (Build.VERSION.SDK_INT >= 33) super.onBackPressed() else goBack()
    }

    private fun goBack() {
        if (page > 0) { page--; render() } else finish()
    }

    // ------------------------------------------------------------------ state

    private fun perm(p: String): PermissionStatus = PermissionStatusLogic.compute(
        granted = checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED,
        askedBefore = AppState.wasAsked(p),
        shouldShowRationale = shouldShowRequestPermissionRationale(p),
    )

    private fun inputs() = SetupInputs(
        sdk = Build.VERSION.SDK_INT,
        pairedCount = AppState.pairedCount,
        notifications = if (Build.VERSION.SDK_INT >= 33) perm(Manifest.permission.POST_NOTIFICATIONS) else PermissionStatus.Granted,
        contacts = perm(Manifest.permission.READ_CONTACTS),
        calendar = perm(Manifest.permission.READ_CALENDAR),
        phoneState = perm(Manifest.permission.READ_PHONE_STATE),
        answerCalls = perm(Manifest.permission.ANSWER_PHONE_CALLS),
        callLog = perm(Manifest.permission.READ_CALL_LOG),
        battery = PowerChecks.batteryState(this),
        oemAcknowledged = AppState.oemAcknowledged,
        oem = oem,
        wasDone = AppState.wasDone(),
    )

    private fun refresh() {
        steps = SetupModel.steps(inputs())
        AppState.rememberDone(steps)
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        CallControlRuntime.onPermissionsChanged()
        render()
    }

    private fun request(p: String) {
        AppState.markAsked(p)
        requestPermissions(arrayOf(p), page)
    }

    // ------------------------------------------------------------------ rendering

    private fun render() {
        refresh()
        // Re-render keeps the pairing panel alive while it stays on the pair page.
        val onPair = page < steps.size && steps[page].id == StepId.Pair
        if (!onPair) { pairPanel?.detach(stopLink = false); pairPanel = null; pairBox = null }

        val root = screen.root
        root.removeAllViews()
        root.addView(header())
        if (page >= steps.size) overview(root) else stepPage(root, steps[page])
        footer()
        syncBackCallback()
        screen.announcePage(
            if (page >= steps.size) getString(R.string.setup_overview_pane)
            else getString(R.string.setup_step_pane, page + 1, steps.size, SetupModel.title(steps[page].id)),
        )
        screen.requestInsets()
        screen.scroll.scrollTo(0, 0)
    }

    private fun header(): View {
        val box = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val overview = page >= steps.size
        box.addView(
            kit.text(if (overview) getString(R.string.setup_overview) else getString(R.string.setup_step_of, page + 1, steps.size), 13f, Tone.Muted, bold = true),
        )
        val bars = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        for ((i, s) in steps.withIndex()) {
            val c = when {
                i == page -> kit.blue
                s.status == StepStatus.Done -> getColor(R.color.gb_ok)
                else -> getColor(R.color.gb_line)
            }
            val bar = View(this).apply { background = kit.shape(c, 3) }
            bars.addView(bar, LinearLayout.LayoutParams(0, kit.dp(5), 1f).apply { if (i > 0) marginStart = kit.dp(4) })
        }
        box.addView(bars, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(8) })
        return box
    }

    private fun stepPage(root: LinearLayout, s: StepInfo) {
        val (icon, why) = copy(s.id)
        val title = SetupModel.title(s.id)
        val head = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; gravity = android.view.Gravity.START }
        head.addView(kit.badge(icon, tone(s.status), 64).apply { (layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(24) })
        head.addView(kit.heading(title, 28f).apply { setPadding(0, kit.dp(16), 0, 0) })
        val chip = kit.chip(chipText(s), tone(s.status))
        head.addView(chip, LinearLayout.LayoutParams(Ui.WRAP, Ui.WRAP).apply { topMargin = kit.dp(10) })
        root.addView(head)
        root.addView(kit.text(why, 15.5f, Tone.Muted).apply { setPadding(0, kit.dp(14), 0, 0) })
        root.addView(kit.text(s.detail, 14f, tone(s.status).takeIf { it != Tone.Muted } ?: Tone.Normal, bold = true).apply { setPadding(0, kit.dp(14), 0, 0) })

        val panel = kit.panel(root, 16).also { (it.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(18) }
        when (s.id) {
            StepId.Pair -> pairStep(panel)
            StepId.Sms -> {
                kit.button(panel, getString(R.string.setup_open_app_settings), BtnStyle.Tonal) { PowerChecks.openAppSettings(this) }
                panel.addView(kit.text(getString(R.string.setup_sms_restricted), 13f, Tone.Muted).apply { setPadding(0, kit.dp(10), 0, 0) })
            }
            StepId.Battery -> batteryStep(panel, s)
            StepId.AnswerCalls -> answerCallsStep(panel, s)
            else -> permissionStep(panel, s)
        }
    }

    private fun permissionStep(panel: LinearLayout, s: StepInfo) {
        val p = SetupModel.permissionFor(s.id)
        when (s.status) {
            StepStatus.Done -> panel.addView(kit.text(getString(R.string.setup_all_set), 14f, Tone.Ok))
            StepStatus.Blocked -> {
                panel.addView(kit.text(getString(R.string.setup_blocked, SetupModel.title(s.id)), 14f, Tone.Muted))
                kit.button(panel, getString(R.string.setup_open_app_settings), BtnStyle.Primary) { PowerChecks.openAppSettings(this) }
            }
            else -> {
                if (p != null) kit.button(panel, getString(if (s.status == StepStatus.Denied) R.string.setup_ask_again else R.string.setup_allow), BtnStyle.Primary) { request(p) }
                if (s.status == StepStatus.Denied) kit.button(panel, getString(R.string.setup_open_app_settings), BtnStyle.Plain) { PowerChecks.openAppSettings(this) }
                panel.addView(kit.text(getString(R.string.setup_optional_note), 13f, Tone.Muted).apply { setPadding(0, kit.dp(10), 0, 0) })
            }
        }
    }

    /** The permission, plus the opt-in switch: nothing about calls leaves the phone until it is on. */
    private fun answerCallsStep(panel: LinearLayout, s: StepInfo) {
        permissionStep(panel, s)
        kit.divider(panel)
        kit.row(
            panel, R.drawable.ic_call, getString(R.string.setup_calls_toggle),
            getString(R.string.setup_calls_toggle_sub),
            kit.toggle(CallControlRuntime.settings.enabled) { on ->
                CallControlRuntime.setEnabled(on)
                CallControlRuntime.onPermissionsChanged()
            },
        )
        panel.addView(
            kit.text(
                getString(R.string.setup_calls_audio_note),
                13f, Tone.Muted,
            ).apply { setPadding(0, kit.dp(10), 0, 0) },
        )
    }

    private fun pairStep(panel: LinearLayout) {
        val controller = LinkRuntime.controller()
        val box = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        panel.addView(box, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        pairBox = box
        val existing = pairPanel
        if (existing == null) {
            pairPanel = PairPanel(this, kit, controller, box) { LinkService.sync(this); render() }.also { it.attach() }
        } else {
            existing.attach()
            existing.rebind(box)
        }
    }

    private fun batteryStep(panel: LinearLayout, s: StepInfo) {
        val state = PowerChecks.batteryState(this)
        when (state) {
            BatteryState.Optimised -> {
                panel.addView(kit.text(getString(R.string.setup_battery_optimised), 14f, Tone.Muted))
                kit.button(panel, getString(R.string.setup_battery_allow), BtnStyle.Primary) { PowerChecks.requestUnrestricted(this) }
            }
            BatteryState.BackgroundRestricted -> {
                panel.addView(kit.text(getString(R.string.setup_battery_restricted), 14f, Tone.Muted))
                kit.button(panel, getString(R.string.setup_open_app_settings), BtnStyle.Primary) { PowerChecks.openAppSettings(this) }
            }
            BatteryState.Unrestricted ->
                panel.addView(kit.text(getString(R.string.setup_battery_ok), 14f, Tone.Ok))
        }
        if (oem != Oem.Other) {
            kit.divider(panel)
            panel.addView(kit.text(getString(R.string.setup_oem_title, oem.label), 16f, bold = true).apply { setPadding(0, kit.dp(14), 0, 0) })
            panel.addView(kit.text(getString(R.string.setup_oem_intro), 13.5f, Tone.Muted).apply { setPadding(0, kit.dp(4), 0, 0) })
            OemGuides.steps(oem).forEachIndexed { i, line ->
                panel.addView(kit.text("${i + 1}.  $line", 14f).apply { setPadding(0, kit.dp(8), 0, 0) })
            }
            kit.button(panel, getString(R.string.setup_oem_open, oem.label.substringBefore(' ')), BtnStyle.Tonal) {
                if (PowerChecks.openOemSettings(this, oem) == LaunchResult.OpenedFallback) {
                    android.widget.Toast.makeText(this, getString(R.string.setup_oem_fallback), android.widget.Toast.LENGTH_LONG).show()
                }
            }
            if (!AppState.oemAcknowledged) {
                kit.button(panel, getString(R.string.setup_oem_done), BtnStyle.Plain) { AppState.oemAcknowledged = true; render() }
            } else {
                panel.addView(kit.text(getString(R.string.setup_oem_marked), 13f, Tone.Muted).apply { setPadding(0, kit.dp(10), 0, 0) })
                kit.button(panel, getString(R.string.setup_oem_undo), BtnStyle.Plain) { AppState.oemAcknowledged = false; render() }
            }
        } else if (state != BatteryState.Unrestricted) {
            kit.button(panel, getString(R.string.setup_open_app_settings), BtnStyle.Plain) { PowerChecks.openAppSettings(this) }
        }
    }

    private fun overview(root: LinearLayout) {
        root.addView(kit.heading(getString(R.string.setup_overview_title)).apply { setPadding(0, kit.dp(24), 0, 0) })
        val open = steps.count { it.required && it.status != StepStatus.Done }
        root.addView(
            kit.text(
                if (open == 0) getString(R.string.setup_all_in_place) else resources.getQuantityString(R.plurals.setup_steps_need_attention, open, open),
                15f, if (open == 0) Tone.Ok else Tone.Muted,
            ).apply { setPadding(0, kit.dp(6), 0, kit.dp(14)) },
        )
        val panel = kit.panel(root, 8)
        steps.forEachIndexed { i, s ->
            if (i > 0) kit.divider(panel)
            kit.row(
                panel, copy(s.id).first, SetupModel.title(s.id), s.detail,
                kit.chip(chipText(s), tone(s.status)), tone(s.status),
            ) { page = i; render() }
        }
    }

    private fun footer() {
        val f = screen.footer
        f.removeAllViews()
        val last = page >= steps.size
        val back = kit.pill(getString(R.string.setup_back), BtnStyle.Plain) { goBack() }
        back.visibility = if (page == 0) View.INVISIBLE else View.VISIBLE
        f.addView(back, LinearLayout.LayoutParams(0, Ui.WRAP, 1f))
        val s = steps.getOrNull(page)
        val done = s == null || s.status == StepStatus.Done || s.status == StepStatus.Info
        val label = when {
            last -> getString(R.string.setup_finish)
            page == steps.size - 1 -> getString(R.string.setup_overview)
            done -> getString(R.string.setup_next)
            else -> getString(R.string.setup_skip)
        }
        val next = kit.pill(label, if (last || done) BtnStyle.Primary else BtnStyle.Tonal) {
            if (last) finishSetup() else { page++; render() }
        }
        f.addView(next, LinearLayout.LayoutParams(0, Ui.WRAP, 1f).apply { marginStart = kit.dp(12) })
    }

    private fun finishSetup() {
        AppState.setupFinished = true
        LinkService.sync(this)
        finish()
    }

    private fun tone(s: StepStatus) = when (s) {
        StepStatus.Done -> Tone.Ok
        StepStatus.Todo, StepStatus.Info -> Tone.Muted
        StepStatus.Denied -> Tone.Warn
        StepStatus.Blocked -> Tone.Bad
    }

    private fun chipText(s: StepInfo) = when (s.status) {
        StepStatus.Done -> getString(R.string.chip_done)
        StepStatus.Todo -> getString(if (s.required) R.string.chip_todo else R.string.chip_optional)
        StepStatus.Denied -> getString(R.string.chip_denied)
        StepStatus.Blocked -> getString(R.string.chip_blocked)
        StepStatus.Info -> getString(R.string.chip_info)
    }

    private fun copy(id: StepId): Pair<Int, String> = when (id) {
        StepId.Pair -> R.drawable.ic_computer to getString(R.string.setup_why_pair)
        StepId.Notifications -> R.drawable.ic_bell to getString(R.string.setup_why_notifications)
        StepId.Contacts -> R.drawable.ic_person to getString(R.string.setup_why_contacts)
        StepId.Calendar -> R.drawable.ic_calendar to getString(R.string.setup_why_calendar)
        StepId.PhoneState -> R.drawable.ic_call to getString(R.string.setup_why_phone_state)
        StepId.AnswerCalls -> R.drawable.ic_call to getString(R.string.setup_why_answer)
        StepId.CallerNumber -> R.drawable.ic_person to getString(R.string.setup_why_caller)
        StepId.Sms -> R.drawable.ic_message to getString(R.string.setup_why_sms)
        StepId.Battery -> R.drawable.ic_battery to getString(R.string.setup_why_battery)
    }

    companion object {
        private const val KEY_PAGE = "page"
        const val EXTRA_REVIEW = "review"
        const val EXTRA_STEP = "step"

        fun intent(ctx: Context, review: Boolean = false, step: StepId? = null): Intent =
            Intent(ctx, SetupActivity::class.java)
                .putExtra(EXTRA_REVIEW, review)
                .also { i -> step?.let { i.putExtra(EXTRA_STEP, it.name) } }
    }
}
