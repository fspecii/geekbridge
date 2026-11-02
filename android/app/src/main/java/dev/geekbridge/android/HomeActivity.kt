// SPDX-License-Identifier: GPL-3.0-or-later
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
        PhoneDataRuntime.resync() // a permission granted in Settings must be re-declared to the computer without a reconnect by hand
        render()
    }

    override fun onStop() {
        ShareCoordinator.get(this).controller.appVisible = false
        offers?.detach()
        offers = null
        main.removeCallbacks(poller)
        controller.observers -= observer
        prompt.dismiss()
        if (ActivityTracker.leavingApp(this)) controller.stop(R.string.link_stop_foreground)
        super.onStop()
    }

    /** Without the background service the link only lives while the app is open. */
    private fun ensureLinkWhileVisible() {
        if (AppState.pairedCount > 0 && !LinkService.isRunning && controller.phase == LinkPhase.Stopped) {
            controller.start(
                LinkRuntime.startOptions(filesDir.absolutePath, DeviceName.current(this), discoveryListen = false, announce = true),
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
        val steps = SetupModel.steps(strings, inp)
        AppState.rememberDone(steps)
        val warnings = SetupModel.warnings(strings, inp, AppState.setupFinished)
        val hero = HomeModel.hero(
            strings, AppState.pairedNames, AppState.pairedCount, controller.phase, connected, AppState.keepConnected, LinkService.isRunning,
        )
        val glance = TransferGlanceModel.of(s?.transfers.orEmpty())
        val clipLine = clipboardLine(connected.isNotEmpty())
        val sig = listOf(WhatsNewRuntime.pending() != null, clipLine.text, hero, connected, warnings, steps.map { it.status }, AppState.keepConnected, AppState.setupFinished, QuickActions.sendClipboard != null, glance, SmsReadinessAndroid.evaluate(this)).joinToString("|")
        if (sig == lastSig) return
        lastSig = sig
        // The whole screen is rebuilt on a state change; put TalkBack back where the user was.
        val focus = A11yLabels.focusPath(screen.root)
        build(hero, connected, warnings, steps, glance, clipLine)
        A11yLabels.restoreFocus(screen.root, focus)
    }

    private fun build(hero: HeroState, connected: List<String>, warnings: List<SetupModel.Warning>, steps: List<StepInfo>, glance: TransferGlance?, clipLine: ClipboardState.Line) {
        val root = screen.root
        root.removeAllViews()
        fun status(id: StepId) = steps.first { it.id == id }.status

        // Header: dot-matrix wordmark + settings
        val gear = kit.iconButton(R.drawable.ic_settings, getString(R.string.home_cd_setup)) {
            startActivity(SetupActivity.intent(this@HomeActivity, review = true))
        }
        root.addView(kit.dotHeader(getString(R.string.app_name), gear), LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(4) })

        // Shown once after an update, until dismissed
        WhatsNewRuntime.pending()?.let { whatsNewCard(root, it) }

        // Warning / unfinished setup
        if (warnings.isNotEmpty() || !AppState.setupFinished) {
            val w = kit.panel(root, 16, kit.withAlpha(kit.ui.toneColor(Tone.Warn), 0x1A), kit.withAlpha(kit.ui.toneColor(Tone.Warn), 0x55))
            (w.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(12)
            val first = warnings.firstOrNull()
            val msg = first?.text ?: getString(R.string.home_finish_setup)
            w.addView(kit.text(msg, 14f, Tone.Warn, bold = true).apply { contentDescription = getString(R.string.home_cd_warning, msg) })
            if (warnings.size > 1) w.addView(kit.text(resources.getQuantityString(R.plurals.home_more_to_check, warnings.size - 1, warnings.size - 1), 13f, Tone.Muted).apply { setPadding(0, kit.dp(4), 0, 0) })
            kit.button(w, getString(if (first != null) R.string.home_fix else R.string.home_continue_setup), BtnStyle.Primary) {
                startActivity(SetupActivity.intent(this, step = first?.step))
            }
        }

        // Hero: the link as a big dotted tile (state word, link dots, sentence, live transfer progress)
        val word = HomeModel.word(AppState.pairedCount, controller.phase, connected, AppState.keepConnected, LinkService.isRunning)
        val wordText = getString(
            when (word) {
                LinkWord.Online -> R.string.word_online
                LinkWord.Waiting -> R.string.word_waiting
                LinkWord.Linking -> R.string.word_linking
                LinkWord.Offline -> R.string.word_offline
                LinkWord.Unpaired -> R.string.word_unpaired
            },
        )
        val tLabel = glance?.let { g ->
            when {
                g.percent == null -> getString(R.string.hero_sending_plain, g.count)
                g.outgoing == g.count -> getString(R.string.hero_sending, g.count, g.count, g.percent)
                g.outgoing == 0 -> getString(R.string.hero_receiving, g.count, g.percent)
                else -> getString(R.string.hero_sending_plain, g.count)
            }
        }
        val heroHolder = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        root.addView(heroHolder, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(12) })
        val tile = kit.heroTile(
            heroHolder,
            HeroView(wordText, word.level, hero.title, hero.subtitle, word == LinkWord.Online, tLabel, glance?.percent?.let { it / 100f }),
        )
        // The link state changed while the screen is open: say so once (the rebuilt tile is not a live region).
        if (lastWord != null && lastWord != word) {
            @Suppress("DEPRECATION")
            tile.announceForAccessibility(tile.contentDescription)
        }
        lastWord = word
        if (hero.action == HeroAction.Pair) {
            kit.button(tile, getString(R.string.home_pair_computer), BtnStyle.Plain) { startActivity(SetupActivity.intent(this, step = StepId.Pair)) }
                .apply { setTextColor(getColor(R.color.gb_on_hero)); background = kit.shape(0, Tok.R_PILL, getColor(R.color.gb_on_hero)) }
        } else if (AppState.pairedNames.isNotEmpty()) {
            tile.addView(
                kit.text(getString(R.string.home_paired_list, AppState.pairedNames.joinToString(", ")), 13f, Tone.Muted).apply {
                    setTextColor(getColor(R.color.gb_hero_muted)); setPadding(0, kit.dp(12), 0, 0)
                },
            )
        }

        // Quick actions
        kit.label(root, getString(R.string.home_section_quick))
        val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        root.addView(row, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        val paired = AppState.pairedCount
        val sub = HomeModel.tileSubtitle(strings, paired, connected)
        kit.tile(row, R.drawable.ic_file, getString(R.string.home_send_file), sub, paired > 0) { if (paired > 0) sendFile() else needPairing() }
        val clipHook = QuickActions.sendClipboard
        kit.tile(row, R.drawable.ic_clipboard, getString(R.string.home_send_clipboard), if (clipHook == null && paired > 0) getString(R.string.home_opens_clipboard_tools) else sub, paired > 0) { if (paired > 0) sendClipboard() else needPairing() }

        // One line: what clipboard sync is doing and what to do next
        root.addView(
            kit.text(clipLine.text, 13f, clipLine.tone).apply {
                setPadding(kit.dp(4), kit.dp(8), kit.dp(4), 0)
                if (clipLine.kind == ClipboardState.Kind.ReadyNoTile || clipLine.kind == ClipboardState.Kind.NoNotifications) {
                    setOnClickListener { clipboardLineAction(clipLine.kind) }
                }
            },
        )

        // Widget tiles for what this phone shares
        kit.label(root, getString(R.string.home_section_phone))
        val calls = status(StepId.PhoneState) == StepStatus.Done
        val smsState = SmsReadinessAndroid.evaluate(this)
        val sms = smsState.ready
        val contactsOk = status(StepId.Contacts) == StepStatus.Done
        val calOk = status(StepId.Calendar) == StepStatus.Done
        val grid1 = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        root.addView(grid1, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        kit.widgetTile(grid1, R.drawable.ic_call, getString(R.string.home_feature_calls), getString(if (calls) R.string.word_ready else R.string.word_allow), calls) {
            startActivity(SetupActivity.intent(this, step = StepId.PhoneState))
        }
        kit.widgetTile(grid1, R.drawable.ic_message, getString(R.string.home_messages), SmsReadinessLogic.tileWord(smsState, strings), sms) {
            startActivity(SetupActivity.intent(this, step = StepId.Sms))
        }
        val grid2 = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        root.addView(grid2, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(12) })
        kit.widgetTile(grid2, R.drawable.ic_person, getString(R.string.home_contacts), getString(if (contactsOk) R.string.word_ready else R.string.word_allow), contactsOk) {
            openFeature(QuickActions.contacts, StepId.Contacts, contactsOk)
        }
        kit.widgetTile(grid2, R.drawable.ic_calendar, getString(R.string.home_agenda), getString(if (calOk) R.string.word_ready else R.string.word_allow), calOk) {
            openFeature(QuickActions.calendar, StepId.Calendar, calOk)
        }
        val ans = kit.panel(root, 8).also { (it.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(12) }
        featureRow(
            ans, R.drawable.ic_call, R.string.home_answer,
            if (CallControlRuntime.settings.enabled) status(StepId.AnswerCalls) else StepStatus.Todo,
            R.string.home_answer_ok, R.string.home_answer_todo, StepId.AnswerCalls,
        )
        if (CallControlRuntime.settings.enabled) {
            val numberOk = status(StepId.CallerNumber) == StepStatus.Done
            root.addView(
                kit.text(getString(if (numberOk) R.string.home_caller_shown else R.string.home_caller_hidden), 13f, if (numberOk) Tone.Muted else Tone.Warn).apply {
                    setPadding(kit.dp(4), kit.dp(8), kit.dp(4), 0)
                    if (!numberOk) setOnClickListener { startActivity(SetupActivity.intent(this@HomeActivity, step = StepId.CallerNumber)) }
                },
            )
        }

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
        kit.divider(set)
        kit.row(set, R.drawable.ic_person, getString(R.string.home_name_row), DeviceName.current(this), null) { chooseName() }
        ClipboardSettingsUi.addRows(this, kit, set) { lastSig = ""; render() }
        kit.divider(set)
        kit.row(set, R.drawable.ic_check, getString(R.string.home_setup_row), getString(R.string.home_setup_row_sub), null) { startActivity(SetupActivity.intent(this, review = true)) }
        kit.divider(set)
        kit.row(set, R.drawable.ic_settings, getString(R.string.home_diag_row), getString(R.string.home_diag_row_sub, BuildConfig.VERSION_NAME), null) {
            startActivity(Intent(this, MainActivity::class.java))
        }
    }

    /** "Updated to X.Y.Z": a few highlights, a link to the release page, and Dismiss (which remembers the version). */
    private fun whatsNewCard(root: LinearLayout, notes: WhatsNew) {
        val card = kit.panel(root, 16)
        (card.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(12)
        card.addView(kit.text(getString(R.string.whatsnew_title, notes.version), 17f, bold = true).apply {
            if (Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
        })
        for (line in notes.highlights) {
            card.addView(kit.text("•  $line", 14f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, 0) })
        }
        kit.button(card, getString(R.string.whatsnew_full_notes), BtnStyle.Tonal) {
            val url = WhatsNewModel.releaseUrl(BuildConfig.RELEASE_REPO, notes.version)
            try {
                if (url == null || !UpdateModel.urlAllowed(url)) throw ActivityNotFoundException()
                startActivity(Intent(Intent.ACTION_VIEW, android.net.Uri.parse(url)))
            } catch (_: ActivityNotFoundException) {
                toast(getString(R.string.whatsnew_no_browser))
            }
        }
        kit.button(card, getString(R.string.whatsnew_dismiss), BtnStyle.Plain) {
            WhatsNewRuntime.dismiss()
            lastSig = ""
            render()
        }
    }

    /** "This phone's name": what computers show for this phone. Applied to the running link without re-pairing. */
    private fun chooseName() {
        val input = android.widget.EditText(this).apply {
            setText(DeviceName.current(this@HomeActivity))
            setSingleLine()
            filters = arrayOf(android.text.InputFilter.LengthFilter(DeviceName.MAX_CHARS))
            setSelectAllOnFocus(true)
            contentDescription = getString(R.string.home_name_row)
        }
        val box = LinearLayout(this).apply {
            setPadding(kit.dp(24), kit.dp(8), kit.dp(24), 0)
            addView(input, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        }
        val dialog = android.app.AlertDialog.Builder(this)
            .setTitle(getString(R.string.home_name_row))
            .setMessage(getString(R.string.home_name_help))
            .setView(box)
            .setPositiveButton(getString(R.string.home_name_save), null)
            .setNegativeButton(android.R.string.cancel, null)
            .create()
        dialog.setOnShowListener {
            dialog.getButton(android.app.AlertDialog.BUTTON_POSITIVE).setOnClickListener {
                when (val c = DeviceName.check(input.text.toString())) {
                    is DeviceName.Check.Bad -> input.error = c.reason(strings)
                    is DeviceName.Check.Ok -> {
                        val saved = DeviceName.save(this, c.name) ?: return@setOnClickListener
                        controller.rename(saved)
                        dialog.dismiss()
                        lastSig = ""
                        render()
                    }
                }
            }
        }
        dialog.show()
    }

    /** Opens a feature screen when it exists; otherwise (or while its permission is missing) the setup step for it. */
    private fun openFeature(hook: QuickActionHandler?, step: StepId, granted: Boolean) {
        if (hook != null && granted && hook.isAvailable(this)) hook.launch(this)
        else startActivity(SetupActivity.intent(this, step = step))
    }

    private fun featureRow(parent: LinearLayout, icon: Int, title: Int, st: StepStatus, okText: Int, todoText: Int, step: StepId) {
        val ok = st == StepStatus.Done
        kit.row(
            parent, icon, getString(title), getString(if (ok) okText else todoText),
            kit.chip(getString(if (ok) R.string.home_chip_ready else R.string.home_chip_allow), if (ok) Tone.Ok else Tone.Warn), if (ok) Tone.Ok else null,
        ) { startActivity(SetupActivity.intent(this, step = step)) }
    }

    // ------------------------------------------------------------------ quick actions

    private fun clipboardLine(connected: Boolean): ClipboardState.Line {
        ClipboardRuntime.init(this)
        return ClipboardState.line(
            strings, ClipboardRuntime.settings.enabled, connected, AndroidClipboardHost.canAskForTap(this),
            ClipboardRuntime.tileAdded, Build.VERSION.SDK_INT >= 33, ClipboardRuntime.lastOutcome, System.currentTimeMillis(),
        )
    }

    private fun clipboardLineAction(kind: ClipboardState.Kind) {
        when {
            kind == ClipboardState.Kind.NoNotifications ->
                startActivity(Intent(android.provider.Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(android.provider.Settings.EXTRA_APP_PACKAGE, packageName))
            Build.VERSION.SDK_INT >= 33 -> {
                val bar = getSystemService(android.app.StatusBarManager::class.java) ?: return
                bar.requestAddTileService(
                    android.content.ComponentName(this, ClipboardTileService::class.java), getString(R.string.tile_label),
                    android.graphics.drawable.Icon.createWithResource(this, R.drawable.ic_clipboard), mainExecutor,
                ) { result ->
                    val added = result == android.app.StatusBarManager.TILE_ADD_REQUEST_RESULT_TILE_ADDED ||
                        result == android.app.StatusBarManager.TILE_ADD_REQUEST_RESULT_TILE_ALREADY_ADDED
                    if (added) ClipboardRuntime.tileAdded = true
                    lastSig = ""; render()
                }
            }
        }
    }

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

    private var lastWord: LinkWord? = null

    private companion object {
        const val POLL_MS = 2000L
        const val REQ_PICK = 71
    }
}
