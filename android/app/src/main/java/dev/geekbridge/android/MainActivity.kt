// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.CancellationSignal
import android.os.Handler
import android.os.Looper
import android.os.PersistableBundle
import android.provider.Settings
import android.text.Editable
import android.text.InputType
import android.text.TextWatcher
import android.view.View
import android.view.WindowInsets
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.time.ZoneId
import java.util.Locale
import java.util.concurrent.ExecutorService

/**
 * Diagnostics & about: technical details, local previews and the manual "Computer link" for troubleshooting.
 * The everyday app is HomeActivity. Previews are local to this phone; the only networking here is the link to a
 * paired computer, which the user starts explicitly.
 */
class MainActivity : Activity() {
    private lateinit var ui: Ui
    private lateinit var work: WorkRunner
    private val core: CoreClient by lazy { CoreClient.native() } // first touched on the worker thread

    private lateinit var coreStatus: TextView
    private lateinit var coreDetail: TextView

    private lateinit var clipField: EditText
    private lateinit var clipStatus: TextView
    private var clipBusy = false

    private lateinit var smsRecipient: EditText
    private lateinit var smsText: EditText
    private lateinit var smsSubscription: EditText
    private lateinit var smsStatus: TextView
    private var smsBusy = false

    private lateinit var callPermissionRow: TextView
    private lateinit var callStateRow: TextView
    private lateinit var callChangedRow: TextView
    private lateinit var callStatus: TextView
    private lateinit var callAllowButton: Button
    private lateinit var callSettingsButton: Button
    private var callPermission = PermissionUi.NotGranted
    private val callTracker = CallStateTracker()
    private lateinit var callExecutor: ExecutorService
    private lateinit var callMonitor: CallStateMonitor

    private lateinit var link: LinkSection
    private lateinit var updates: UpdateSection
    private lateinit var offers: OfferDialogs
    private lateinit var connSummary: TextView
    private lateinit var pairedRow: TextView

    private val sections = ArrayList<PreviewSection<*>>()
    private lateinit var topScrim: View
    private lateinit var bottomScrim: View

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ActivityTracker.install(this)
        ui = Ui(this)
        val main = Handler(Looper.getMainLooper())
        work = WorkRunner(postToMain = { main.post(it) })
        callExecutor = CallStateExecutor.create()
        callMonitor = CallStateMonitor(
            registrar = TelephonyRegistrar(this),
            executor = callExecutor,
            postToMain = { main.post(it) },
            onState = { onCallState(it) },
        )

        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val scroll = ScrollView(this).apply {
            clipToPadding = false
            isVerticalScrollBarEnabled = false
            addView(root, android.widget.FrameLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
            setOnApplyWindowInsetsListener { v, insets -> applyInsets(v, insets) }
        }
        // Opaque bars behind the status/navigation insets so scrolled content never runs under the clock
        // or the gesture handle (the window is edge-to-edge on Android 15+).
        topScrim = View(this).apply { setBackgroundColor(getColor(R.color.gb_surface)) }
        bottomScrim = View(this).apply { setBackgroundColor(getColor(R.color.gb_surface)) }
        val frame = android.widget.FrameLayout(this).apply {
            addView(scroll, android.widget.FrameLayout.LayoutParams(Ui.MATCH, Ui.MATCH))
            addView(topScrim, android.widget.FrameLayout.LayoutParams(Ui.MATCH, 0, android.view.Gravity.TOP))
            addView(bottomScrim, android.widget.FrameLayout.LayoutParams(Ui.MATCH, 0, android.view.Gravity.BOTTOM))
        }
        setContentView(frame)
        SystemBars.setup(this)

        buildHeader(root)
        buildStatusCard(root)
        buildAboutCard(root)
        link = LinkSection(
            this, ui,
            // The native core is first touched on the transport worker thread, never on the main thread.
            LinkRuntime.controller(),
            root, connSummary, pairedRow,
        )
        buildCallsCard(root)
        buildClipboardCard(root)
        buildMessagesCard(root)
        buildContactsCard(root)
        buildAgendaCard(root)
        updates = UpdateSection(this, ui, root)
        buildPrivacyCard(root)
    }

    override fun onStart() {
        super.onStart()
        sections.forEach { it.sync() }
        syncCalls()
        refreshCore()
        link.onStart()
        updates.onStart()
        offers = OfferDialogs(this, ShareCoordinator.get(this)).also { it.attach() }
        ShareCoordinator.get(this).controller.appVisible = true
    }

    override fun onStop() {
        // Leaving the foreground: stop all provider/JNI work and drop on-screen private data. A rotation is not
        // leaving: the computer link keeps running and the recreated Activity reattaches to it.
        ShareCoordinator.get(this).controller.appVisible = false
        if (::offers.isInitialized) offers.detach()
        link.onStop(leavingForeground = ActivityTracker.leavingApp(this))
        updates.onStop()
        callMonitor.stop()
        work.cancelAll()
        sections.forEach { it.clear() }
        if (clipBusy) {
            clipBusy = false
            ui.setTone(clipStatus, getString(R.string.dev_interrupted), Tone.Warn)
        }
        if (smsBusy) {
            smsBusy = false
            ui.setTone(smsStatus, getString(R.string.dev_interrupted), Tone.Warn)
        }
        super.onStop()
    }

    /** Same flow as the system Share sheet: pick documents, then [ShareActivity] sends them. */
    private fun pickFilesToShare() {
        val pick = Intent(Intent.ACTION_OPEN_DOCUMENT)
            .addCategory(Intent.CATEGORY_OPENABLE).setType("*/*").putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        try {
            @Suppress("DEPRECATION")
            startActivityForResult(pick, REQ_PICK_SHARE)
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(this, getString(R.string.dev_no_file_picker), Toast.LENGTH_SHORT).show()
        }
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_PICK_SHARE || resultCode != RESULT_OK || data == null) return
        val clip = data.clipData
        val send = Intent(this, ShareActivity::class.java).setAction(Intent.ACTION_SEND_MULTIPLE)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        if (clip != null) send.clipData = clip else data.data?.let { send.clipData = ClipData.newRawUri("files", it) }
        startActivity(send)
    }

    override fun onDestroy() {
        updates.onDestroy()
        callMonitor.stop()
        callExecutor.shutdownNow()
        work.shutdown()
        super.onDestroy()
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        val granted = grantResults.isNotEmpty() && grantResults[0] == PackageManager.PERMISSION_GRANTED
        if (requestCode == REQ_PHONE_STATE) {
            callPermission = PermissionLogic.afterRequest(
                granted, shouldShowRequestPermissionRationale(Manifest.permission.READ_PHONE_STATE),
            )
            syncCalls()
            return
        }
        sections.firstOrNull { it.requestCode == requestCode }?.onResult(granted)
    }

    @Suppress("DEPRECATION")
    private fun applyInsets(v: View, insets: WindowInsets): WindowInsets {
        val base = ui.dp(16)
        var l = 0
        var t = 0
        var r = 0
        var b = 0
        if (Build.VERSION.SDK_INT >= 30) {
            val i = insets.getInsets(
                WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout() or WindowInsets.Type.ime(),
            )
            l = i.left; t = i.top; r = i.right; b = i.bottom
        } else {
            l = insets.systemWindowInsetLeft
            t = insets.systemWindowInsetTop
            r = insets.systemWindowInsetRight
            b = insets.systemWindowInsetBottom
        }
        topScrim.layoutParams.height = t
        bottomScrim.layoutParams.height = if (Build.VERSION.SDK_INT >= 30) {
            insets.getInsets(WindowInsets.Type.navigationBars()).bottom
        } else {
            insets.systemWindowInsetBottom
        }
        topScrim.requestLayout()
        bottomScrim.requestLayout()
        // Keep a readable column on wide screens (landscape, tablets).
        val avail = resources.displayMetrics.widthPixels - l - r - 2 * base
        val extra = maxOf(0, (avail - ui.dp(MAX_CONTENT_DP)) / 2)
        v.setPadding(base + l + extra, base + t, base + r + extra, base + b)
        return insets
    }

    // ------------------------------------------------------------------ header & status

    private fun buildHeader(root: LinearLayout) {
        root.addView(DotText(this, getString(R.string.dev_title), maxPitchDp = Tok.DOT_NUMERAL, accentDot = true), LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = ui.dp(8) })
        root.addView(
            ui.text(getString(R.string.dev_intro, BuildConfig.VERSION_NAME), Tone.Muted),
            LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP),
        )
    }

    private fun buildAboutCard(root: LinearLayout) {
        val card = ui.card(root, getString(R.string.about_licence_title))
        ui.row(card, 0, getString(R.string.dev_row_licence), getString(R.string.about_licence_spdx), Tone.Normal)
        ui.body(card, getString(R.string.about_licence_summary), Tone.Muted)
        ui.body(card, getString(R.string.about_thirdparty), Tone.Muted)
        val open = { url: Int ->
            runCatching { startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(getString(url)))) }
        }
        ui.buttonRow(
            card,
            getString(R.string.about_source_button) to { open(R.string.about_source_url); Unit },
            getString(R.string.about_licence_button) to { open(R.string.about_licence_url); Unit },
        )
    }

    private fun buildStatusCard(root: LinearLayout) {
        val card = ui.card(root, getString(R.string.dev_card_status))
        ui.row(card, 0, getString(R.string.dev_row_phone), "${Build.MANUFACTURER} ${Build.MODEL}", Tone.Normal)
        ui.row(card, 1, getString(R.string.dev_row_android), getString(R.string.dev_android_value, Build.VERSION.RELEASE, Build.VERSION.SDK_INT), Tone.Normal)
        pairedRow = ui.row(card, 2, getString(R.string.dev_row_paired), getString(R.string.dev_paired_unknown), Tone.Muted)
        coreStatus = ui.statusLine(card, getString(R.string.dev_core_checking), Tone.Muted)
        coreDetail = ui.body(card, "", Tone.Muted).apply {
            typeface = android.graphics.Typeface.MONOSPACE
            setTextSize(android.util.TypedValue.COMPLEX_UNIT_SP, 12f)
            setTextIsSelectable(false)
        }
        connSummary = ui.statusLine(card, getString(R.string.dev_conn_stopped), Tone.Muted)
        ui.button(card, getString(R.string.dev_recheck_core)) { refreshCore() }
        ui.button(card, getString(R.string.dev_copy_report)) { copyReport() }
        ui.button(card, getString(R.string.report_button)) {
            startActivity(
                Intent(this, ReportActivity::class.java)
                    .putExtra(ReportActivity.EXTRA_CORE, coreStatus.text.toString())
                    .putExtra(ReportActivity.EXTRA_ISSUE, connSummary.text.toString()),
            )
        }
        ui.body(card, getString(R.string.report_hint), Tone.Muted)
        ui.body(card, getString(R.string.dev_copy_report_hint), Tone.Muted)
    }

    private fun copyReport() {
        val report = DiagnosticsReport.build(this, coreStatus.text.toString(), connSummary.text.toString())
        getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText(getString(R.string.dev_report_clip_label), report))
        Toast.makeText(this, getString(R.string.dev_report_copied), Toast.LENGTH_SHORT).show()
    }

    private fun refreshCore() {
        ui.setStatus(coreStatus, getString(R.string.dev_core_checking), Tone.Muted)
        coreDetail.text = ""
        work.submit("core", { core.version() }) { r ->
            when (val o = r.outcome()) {
                is CoreOutcome.Ok -> {
                    val v = CoreProtocol.coreVersionOf(o.result)
                    ui.setStatus(coreStatus, if (v != null) getString(R.string.dev_core_available_version, v) else getString(R.string.dev_core_available), Tone.Ok)
                    coreDetail.text = getString(R.string.core_info, CoreProtocol.summarize(o.result))
                }
                is CoreOutcome.Unavailable -> {
                    ui.setStatus(
                        coreStatus,
                        getString(R.string.dev_core_unavailable, o.reason),
                        Tone.Bad,
                    )
                }
                is CoreOutcome.Rejected -> ui.setStatus(coreStatus, getString(R.string.dev_core_error, o.code), Tone.Warn)
                is CoreOutcome.Malformed -> ui.setStatus(coreStatus, getString(R.string.dev_core_unreadable, o.reason), Tone.Warn)
            }
        }
    }

    private fun buildCallsCard(root: LinearLayout) {
        val card = ui.card(root, getString(R.string.dev_calls_title))
        ui.body(card, getString(R.string.dev_calls_intro))
        callPermissionRow = ui.row(card, 0, getString(R.string.dev_calls_row_access), "", Tone.Muted)
        callStateRow = ui.row(card, 1, getString(R.string.dev_calls_row_state), "", Tone.Muted)
        callChangedRow = ui.row(card, 2, getString(R.string.dev_calls_row_changed), "", Tone.Muted)
        callStatus = ui.body(card, "", Tone.Muted)
        callAllowButton = ui.button(card, getString(R.string.dev_calls_allow), emphasis = Emphasis.Primary) {
            requestPermissions(arrayOf(Manifest.permission.READ_PHONE_STATE), REQ_PHONE_STATE)
        }
        callSettingsButton = ui.button(card, getString(R.string.dev_open_app_settings)) { openAppSettings() }
        ui.body(card, getString(R.string.dev_calls_bluetooth_note))
        ui.button(card, getString(R.string.dev_open_bluetooth)) {
            try {
                startActivity(Intent(Settings.ACTION_BLUETOOTH_SETTINGS))
            } catch (e: ActivityNotFoundException) {
                Toast.makeText(this, getString(R.string.dev_no_bluetooth_settings), Toast.LENGTH_SHORT).show()
            }
        }
        renderCalls(false) // onStart (next) checks the permission and starts observing
    }

    /** Re-checks the permission and starts or stops observing. Runs from onStart and after the permission dialog. */
    private fun syncCalls() {
        val granted = checkSelfPermission(Manifest.permission.READ_PHONE_STATE) == PackageManager.PERMISSION_GRANTED
        callPermission = PermissionLogic.current(granted, callPermission)
        callAllowButton.visibility = visibleIf(callPermission == PermissionUi.NotGranted || callPermission == PermissionUi.Denied)
        callSettingsButton.visibility = visibleIf(callPermission == PermissionUi.Blocked)
        if (granted && !isFinishing && !isDestroyed) {
            val started = callMonitor.start()
            if (started is MonitorStart.Failed) {
                callTracker.reset()
                ui.setTone(callStatus, started.reason(strings), Tone.Bad)
            } else {
                ui.setTone(callStatus, getString(R.string.dev_calls_listening), Tone.Ok)
            }
        } else {
            callMonitor.stop()
            callTracker.reset()
            val msg = when (callPermission) {
                PermissionUi.Denied -> getString(R.string.dev_calls_denied)
                PermissionUi.Blocked -> getString(R.string.dev_calls_blocked)
                else -> getString(R.string.dev_calls_not_allowed)
            }
            ui.setTone(callStatus, msg, if (callPermission == PermissionUi.NotGranted) Tone.Muted else Tone.Warn)
        }
        renderCalls(granted)
    }

    private fun onCallState(state: CallState) {
        callTracker.update(state, System.currentTimeMillis())
        renderCalls(true)
    }

    private fun renderCalls(granted: Boolean) {
        val (permText, permTone) = when (callPermission) {
            PermissionUi.Granted -> getString(R.string.dev_perm_allowed) to Tone.Ok
            PermissionUi.NotGranted -> getString(R.string.dev_perm_not_allowed) to Tone.Muted
            PermissionUi.Denied -> getString(R.string.dev_perm_denied) to Tone.Warn
            PermissionUi.Blocked -> getString(R.string.dev_perm_blocked) to Tone.Warn
        }
        ui.setTone(callPermissionRow, permText, permTone)
        if (!granted) {
            ui.setTone(callStateRow, getString(R.string.dev_calls_off), Tone.Muted)
            ui.setTone(callChangedRow, getString(R.string.dev_calls_none_yet), Tone.Muted)
            return
        }
        val state = callTracker.state
        val tone = when (state) {
            CallState.Ringing -> Tone.Warn
            CallState.OffHook -> Tone.Ok
            else -> Tone.Normal
        }
        ui.setTone(callStateRow, Format.callState(strings, state), tone)
        ui.setTone(callChangedRow, Format.callChangedAt(strings, callTracker.changedAtMillis, ZoneId.systemDefault()), Tone.Normal)
    }

    private fun visibleIf(b: Boolean) = if (b) View.VISIBLE else View.GONE

    // ------------------------------------------------------------------ clipboard

    private fun buildClipboardCard(root: LinearLayout) {
        val card = ui.card(root, getString(R.string.dev_clip_title))
        ui.body(card, getString(R.string.dev_clip_intro), Tone.Muted)
        clipField = ui.input(
            card, getString(R.string.dev_clip_hint), InputType.TYPE_CLASS_TEXT, maxChars = Limits.CLIPBOARD_MAX_BYTES, lines = 4,
        )
        clipStatus = ui.body(card, getString(R.string.dev_clip_not_prepared), Tone.Muted)
        clipField.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, st: Int, c: Int, a: Int) {}
            override fun onTextChanged(s: CharSequence?, st: Int, b: Int, c: Int) {}
            override fun afterTextChanged(s: Editable?) {
                if (!clipBusy) ui.setTone(clipStatus, getString(R.string.dev_clip_not_prepared), Tone.Muted)
            }
        })

        ui.buttonRow(
            card,
            getString(R.string.dev_clip_paste) to { pasteIntoField() },
            getString(R.string.dev_clip_copy) to { copyFromField() },
            getString(R.string.dev_clip_share) to { shareFromField() },
        )
        ui.body(card, getString(R.string.dev_clip_buttons_hint), Tone.Muted)
        ui.button(card, getString(R.string.dev_clip_prepare), emphasis = Emphasis.Primary) { prepareClipboard() }
        ui.button(card, getString(R.string.dev_clip_clear)) {
            clipField.setText("")
            ui.setTone(clipStatus, getString(R.string.dev_clip_cleared), Tone.Muted)
        }
        ui.button(card, getString(R.string.dev_send_files), emphasis = Emphasis.Quiet) { pickFilesToShare() }
    }

    private fun pasteIntoField() {
        val cm = getSystemService(ClipboardManager::class.java)
        val item = cm?.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)
        val text = item?.coerceToText(this)
        when {
            text == null || text.isEmpty() -> ui.setTone(clipStatus, getString(R.string.dev_clip_empty), Tone.Warn)
            text.length > Limits.CLIPBOARD_MAX_BYTES ->
                ui.setTone(clipStatus, getString(R.string.dev_clip_too_big_load), Tone.Warn)
            else -> {
                clipField.setText(text)
                ui.setTone(clipStatus, getString(R.string.dev_clip_pasted), Tone.Muted)
            }
        }
    }

    private fun copyFromField() {
        val text = clipField.text.toString()
        if (text.isEmpty()) return ui.setTone(clipStatus, getString(R.string.dev_clip_nothing_copy), Tone.Warn)
        val clip = ClipData.newPlainText(getString(R.string.dev_clip_label), text)
        clip.description.extras = PersistableBundle().apply {
            // Asks the system to hide the preview in clipboard overlays (honoured on Android 13+).
            putBoolean("android.content.extra.IS_SENSITIVE", true)
        }
        getSystemService(ClipboardManager::class.java).setPrimaryClip(clip)
        ui.setTone(clipStatus, getString(R.string.dev_clip_copied), Tone.Muted)
    }

    private fun shareFromField() {
        val text = clipField.text.toString()
        if (text.isEmpty()) return ui.setTone(clipStatus, getString(R.string.dev_clip_nothing_share), Tone.Warn)
        if (Bounds.utf8Size(text) > Limits.CLIPBOARD_MAX_BYTES) {
            return ui.setTone(clipStatus, getString(R.string.dev_clip_too_big_share), Tone.Warn)
        }
        val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
        try {
            startActivity(Intent.createChooser(send, getString(R.string.dev_clip_share_title)))
        } catch (e: ActivityNotFoundException) {
            ui.setTone(clipStatus, getString(R.string.dev_clip_no_receiver), Tone.Warn)
        }
    }

    private fun prepareClipboard() {
        val text = clipField.text.toString()
        clipBusy = true
        ui.setTone(clipStatus, getString(R.string.dev_validating), Tone.Muted)
        work.submit("clipboard", { core.validateClipboard(text) }) { r ->
            clipBusy = false
            val o = r.outcome()
            ui.setTone(clipStatus, Format.clipboardOutcome(strings, o, Bounds.utf8Size(text)), if (o is CoreOutcome.Ok) Tone.Ok else Tone.Warn)
        }
    }

    // ------------------------------------------------------------------ messages (validation only)

    private fun buildMessagesCard(root: LinearLayout) {
        val card = ui.card(root, getString(R.string.dev_msg_title), accent = true)
        ui.dotBanner(card, getString(R.string.dev_msg_banner))
        val red = getColor(R.color.gb_red)
        val shared = PhoneDataRuntime.active().sms
        ui.row(card, 0, getString(R.string.dev_msg_row_check), getString(R.string.dev_msg_available_offline), Tone.Normal)
        ui.row(card, 1, getString(R.string.dev_msg_row_share), if (shared) getString(R.string.dev_msg_on) else getString(R.string.dev_msg_off_optional), if (shared) Tone.Ok else Tone.Muted)
        ui.body(card, getString(R.string.dev_msg_intro), Tone.Muted)
        smsRecipient = ui.input(card, getString(R.string.dev_msg_recipient), InputType.TYPE_CLASS_PHONE, Limits.SMS_MAX_ADDRESS_CHARS)
        smsText = ui.input(card, getString(R.string.dev_msg_message), InputType.TYPE_CLASS_TEXT, Limits.SMS_MAX_TEXT_CHARS, lines = 3)
        smsSubscription = ui.input(card, getString(R.string.dev_msg_subscription), InputType.TYPE_CLASS_NUMBER, 9)
        smsStatus = ui.body(card, getString(R.string.dev_msg_not_checked), Tone.Muted)
        ui.button(card, getString(R.string.dev_msg_validate), emphasis = Emphasis.Primary, accent = red) { validateSms() }
    }

    private fun validateSms() {
        val subText = smsSubscription.text.toString().trim()
        val sub = if (subText.isEmpty()) null else subText.toIntOrNull()
        if (subText.isNotEmpty() && sub == null) {
            return ui.setTone(smsStatus, getString(R.string.dev_msg_sub_invalid), Tone.Warn)
        }
        val recipient = smsRecipient.text.toString()
        val text = smsText.text.toString()
        smsBusy = true
        ui.setTone(smsStatus, getString(R.string.dev_validating), Tone.Muted)
        work.submit("sms", { core.validateSms(recipient, text, sub) }) { r ->
            smsBusy = false
            val o = r.outcome()
            ui.setTone(smsStatus, Format.smsOutcome(strings, o), if (o is CoreOutcome.Ok) Tone.Ok else Tone.Warn)
        }
    }

    // ------------------------------------------------------------------ contacts & agenda

    private fun buildContactsCard(root: LinearLayout) {
        val card = ui.card(root, getString(R.string.dev_contacts_title))
        ui.body(card, resources.getQuantityString(R.plurals.dev_contacts_intro, LocalProviders.MAX_CONTACTS, LocalProviders.MAX_CONTACTS))
        sections += PreviewSection<ContactPreview>(
            card = card,
            subject = "contacts",
            allowLabel = R.string.dev_contacts_allow,
            shown = R.plurals.dev_contacts_shown,
            shownTruncated = R.plurals.dev_contacts_shown_truncated,
            permission = Manifest.permission.READ_CONTACTS,
            requestCode = REQ_CONTACTS,
            loadLabel = getString(R.string.dev_contacts_show),
            loader = { c, sig -> LocalProviders.loadContacts(c, sig) },
            render = { Format.contactLine(strings, it) },
        )
    }

    private fun buildAgendaCard(root: LinearLayout) {
        val card = ui.card(root, getString(R.string.dev_agenda_title))
        ui.body(card, resources.getQuantityString(R.plurals.dev_agenda_intro, LocalProviders.MAX_EVENTS, LocalProviders.MAX_EVENTS, LocalProviders.AGENDA_DAYS))
        val zone = ZoneId.systemDefault()
        val locale = Locale.getDefault()
        sections += PreviewSection<AgendaEvent>(
            card = card,
            subject = "calendar",
            allowLabel = R.string.dev_agenda_allow,
            shown = R.plurals.dev_events_shown,
            shownTruncated = R.plurals.dev_events_shown_truncated,
            permission = Manifest.permission.READ_CALENDAR,
            requestCode = REQ_CALENDAR,
            loadLabel = getString(R.string.dev_agenda_show),
            loader = { c, sig -> LocalProviders.loadAgenda(c, System.currentTimeMillis(), sig) },
            render = { Format.eventLines(strings, it, zone, locale) },
        )
    }

    private fun buildPrivacyCard(root: LinearLayout) {
        val card = ui.card(root, getString(R.string.dev_privacy_title))
        ui.body(card, getString(R.string.dev_privacy_body))
    }

    /** One permission-gated, bounded, cancellable local preview. */
    private inner class PreviewSection<T>(
        card: LinearLayout,
        private val subject: String,
        private val allowLabel: Int,
        private val shown: Int,
        private val shownTruncated: Int,
        private val permission: String,
        val requestCode: Int,
        loadLabel: String,
        private val loader: (Context, CancellationSignal) -> LoadResult<T>,
        private val render: (T) -> String,
    ) {
        private var state = PermissionUi.NotGranted
        private var showingData = false

        private val status: TextView = ui.body(card, "", Tone.Muted)
        private val allowButton: Button = ui.button(card, getString(allowLabel), emphasis = Emphasis.Primary) {
            requestPermissions(arrayOf(permission), requestCode)
        }
        private val loadButton: Button = ui.button(card, loadLabel) { load() }
        private val settingsButton: Button = ui.button(card, getString(R.string.dev_open_app_settings)) { openAppSettings() }
        private val list = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.VERTICAL }
        private val clearButton: Button = ui.button(card, getString(R.string.dev_clear_preview)) { clear() }

        init {
            card.addView(list, card.indexOfChild(clearButton), LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
            sync()
        }

        fun sync() {
            val granted = checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED
            state = PermissionLogic.current(granted, state)
            allowButton.visibility = visible(state == PermissionUi.NotGranted || state == PermissionUi.Denied)
            settingsButton.visibility = visible(state == PermissionUi.Blocked)
            loadButton.visibility = visible(state == PermissionUi.Granted)
            clearButton.visibility = visible(showingData)
            if (!showingData) {
                when (state) {
                    PermissionUi.NotGranted -> ui.setTone(
                        status, getString(R.string.dev_preview_not_allowed), Tone.Muted,
                    )
                    PermissionUi.Granted -> ui.setTone(
                        status, getString(R.string.dev_preview_allowed), Tone.Ok,
                    )
                    PermissionUi.Denied -> ui.setTone(
                        status, getString(R.string.dev_preview_denied), Tone.Warn,
                    )
                    PermissionUi.Blocked -> ui.setTone(
                        status, getString(R.string.dev_preview_blocked), Tone.Warn,
                    )
                }
            }
        }

        fun onResult(granted: Boolean) {
            state = PermissionLogic.afterRequest(granted, shouldShowRequestPermissionRationale(permission))
            sync()
            if (granted) load()
        }

        private fun load() {
            ui.setTone(status, getString(R.string.dev_reading), Tone.Muted)
            work.submit(
                subject,
                { token ->
                    val signal = CancellationSignal()
                    token.onCancel { signal.cancel() }
                    loader(applicationContext, signal)
                },
            ) { r ->
                when (val res = r.getOrElse { LoadResult.Failed(R.string.dev_reading_failed, it.javaClass.simpleName) }) {
                    is LoadResult.Loaded<T> -> show(res)
                    is LoadResult.Failed -> {
                        showingData = false
                        ui.setTone(status, res.reason(strings), Tone.Bad)
                        sync()
                    }
                    LoadResult.Cancelled -> Unit
                }
            }
        }

        private fun show(res: LoadResult.Loaded<T>) {
            list.removeAllViews()
            showingData = true
            if (res.items.isEmpty()) {
                ui.setTone(status, getString(R.string.dev_preview_empty), Tone.Muted)
            } else {
                ui.setTone(status, resources.getQuantityString(if (res.truncated) shownTruncated else shown, res.items.size, res.items.size), Tone.Muted)
                res.items.forEachIndexed { i, item -> ui.listRow(list, i, render(item)) }
            }
            clearButton.visibility = View.VISIBLE
        }

        fun clear() {
            work.cancel(subject)
            list.removeAllViews()
            showingData = false
            sync()
        }

        private fun visible(b: Boolean) = if (b) View.VISIBLE else View.GONE
    }

    private fun openAppSettings() {
        startActivity(
            Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", packageName, null)),
        )
    }

    private fun <R : CoreOutcome> Result<R>.outcome(): CoreOutcome =
        getOrElse { CoreOutcome.Unavailable("worker failed (${it.javaClass.simpleName})") }

    private companion object {
        const val REQ_CONTACTS = 1
        const val REQ_CALENDAR = 2
        const val REQ_PHONE_STATE = 3
        const val REQ_PICK_SHARE = 40
        const val MAX_CONTENT_DP = 640
    }
}
