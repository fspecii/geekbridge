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
 * Single-screen companion. Everything shown is either local to this phone (device info, contacts and
 * calendar previews, the text the user typed) or an honest statement that a feature is unavailable.
 * The only networking is the optional "Computer link" (a ping-only development transport the user starts
 * explicitly); nothing else is synced, so there are no messages, calls or clipboard results from a computer.
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

        buildHeader(root)
        buildStatusCard(root)
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
            ui.setTone(clipStatus, "Interrupted when the app left the foreground. Run it again.", Tone.Warn)
        }
        if (smsBusy) {
            smsBusy = false
            ui.setTone(smsStatus, "Interrupted when the app left the foreground. Run it again.", Tone.Warn)
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
            Toast.makeText(this, "No file picker is available.", Toast.LENGTH_SHORT).show()
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
        root.addView(ui.title("Diagnostics"), LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        root.addView(
            ui.text(
                "GeekBridge ${BuildConfig.VERSION_NAME}. Technical details, the manual computer link and local " +
                    "previews, for troubleshooting and support. The everyday app is on the home screen.",
                Tone.Muted,
            ),
            LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP),
        )
    }

    private fun buildStatusCard(root: LinearLayout) {
        val card = ui.card(root, "This phone and connection")
        ui.row(card, 0, "Phone", "${Build.MANUFACTURER} ${Build.MODEL}", Tone.Normal)
        ui.row(card, 1, "Android", "${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})", Tone.Normal)
        pairedRow = ui.row(card, 2, "Paired computers", "start the link to read", Tone.Muted)
        coreStatus = ui.statusLine(card, "Native core: checking\u2026", Tone.Muted)
        coreDetail = ui.body(card, "", Tone.Muted).apply {
            typeface = android.graphics.Typeface.MONOSPACE
            setTextSize(android.util.TypedValue.COMPLEX_UNIT_SP, 12f)
            setTextIsSelectable(false)
        }
        connSummary = ui.statusLine(card, "Connection: link stopped.", Tone.Muted)
        ui.button(card, "Recheck native core") { refreshCore() }
    }

    private fun refreshCore() {
        ui.setStatus(coreStatus, "Native core: checking…", Tone.Muted)
        coreDetail.text = ""
        work.submit("core", { core.version() }) { r ->
            when (val o = r.outcome()) {
                is CoreOutcome.Ok -> {
                    val v = CoreProtocol.coreVersionOf(o.result)
                    ui.setStatus(coreStatus, "Native core: available" + (if (v != null) " (version $v)" else ""), Tone.Ok)
                    coreDetail.text = "Core info: " + CoreProtocol.summarize(o.result)
                }
                is CoreOutcome.Unavailable -> {
                    ui.setStatus(
                        coreStatus,
                        "Native core: UNAVAILABLE — ${o.reason}. SMS and clipboard validation are off; local previews still work.",
                        Tone.Bad,
                    )
                }
                is CoreOutcome.Rejected -> ui.setStatus(coreStatus, "Native core: answered with error ${o.code}.", Tone.Warn)
                is CoreOutcome.Malformed -> ui.setStatus(coreStatus, "Native core: unreadable response (${o.reason}).", Tone.Warn)
            }
        }
    }

    private fun buildCallsCard(root: LinearLayout) {
        val card = ui.card(root, "Calls")
        ui.body(
            card,
            "Shows whether this phone is idle, ringing or in a call, after you allow it. Only that coarse state " +
                "is read: no phone numbers, call log or contacts are requested, shown or stored, so an incoming " +
                "call is just \"Ringing\". Nothing is sent to a computer: the development link only carries a ping.",
        )
        callPermissionRow = ui.row(card, 0, "Call state access", "", Tone.Muted)
        callStateRow = ui.row(card, 1, "Phone state", "", Tone.Muted)
        callChangedRow = ui.row(card, 2, "Last change seen", "", Tone.Muted)
        callStatus = ui.body(card, "", Tone.Muted)
        callAllowButton = ui.button(card, "Allow call state", emphasis = Emphasis.Primary) {
            requestPermissions(arrayOf(Manifest.permission.READ_PHONE_STATE), REQ_PHONE_STATE)
        }
        callSettingsButton = ui.button(card, "Open app settings") { openAppSettings() }
        ui.body(
            card,
            "Call audio is carried by your phone's regular Bluetooth hands-free (HFP) link to the computer, " +
                "managed by Android. This app cannot capture or inject cellular call audio and does not try to. " +
                "To use the phone as a headset for the computer, pair the two in Bluetooth settings.",
        )
        ui.button(card, "Open Bluetooth settings") {
            try {
                startActivity(Intent(Settings.ACTION_BLUETOOTH_SETTINGS))
            } catch (e: ActivityNotFoundException) {
                Toast.makeText(this, "No Bluetooth settings screen found.", Toast.LENGTH_SHORT).show()
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
                ui.setTone(callStatus, started.reason, Tone.Bad)
            } else {
                ui.setTone(callStatus, "Listening for call state changes while this screen is open.", Tone.Ok)
            }
        } else {
            callMonitor.stop()
            callTracker.reset()
            val msg = when (callPermission) {
                PermissionUi.Denied -> "Access denied. The feature stays off; you can ask again."
                PermissionUi.Blocked -> "Access denied and Android will not ask again. Enable it in app settings."
                else -> "Access not allowed yet. Nothing is observed until you allow it."
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
            PermissionUi.Granted -> "allowed" to Tone.Ok
            PermissionUi.NotGranted -> "not allowed yet" to Tone.Muted
            PermissionUi.Denied -> "denied" to Tone.Warn
            PermissionUi.Blocked -> "denied \u00b7 use app settings" to Tone.Warn
        }
        ui.setTone(callPermissionRow, permText, permTone)
        if (!granted) {
            ui.setTone(callStateRow, "off", Tone.Muted)
            ui.setTone(callChangedRow, "none yet", Tone.Muted)
            return
        }
        val state = callTracker.state
        val tone = when (state) {
            CallState.Ringing -> Tone.Warn
            CallState.OffHook -> Tone.Ok
            else -> Tone.Normal
        }
        ui.setTone(callStateRow, Format.callState(state), tone)
        ui.setTone(callChangedRow, Format.callChangedAt(callTracker.changedAtMillis, ZoneId.systemDefault()), Tone.Normal)
    }

    private fun visibleIf(b: Boolean) = if (b) View.VISIBLE else View.GONE

    // ------------------------------------------------------------------ clipboard

    private fun buildClipboardCard(root: LinearLayout) {
        val card = ui.card(root, "Clipboard \u00b7 manual only")
        ui.body(
            card,
            "The app never reads the clipboard in the background or on its own. Only the buttons below " +
                "touch it, and only while this screen is open. Android 12+ shows its own notice when text is pasted.",
            Tone.Muted,
        )
        clipField = ui.input(
            card, "Text to prepare", InputType.TYPE_CLASS_TEXT, maxChars = Limits.CLIPBOARD_MAX_BYTES, lines = 4,
        )
        clipStatus = ui.body(card, "Not prepared.", Tone.Muted)
        clipField.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, st: Int, c: Int, a: Int) {}
            override fun onTextChanged(s: CharSequence?, st: Int, b: Int, c: Int) {}
            override fun afterTextChanged(s: Editable?) {
                if (!clipBusy) ui.setTone(clipStatus, "Not prepared.", Tone.Muted)
            }
        })

        ui.buttonRow(
            card,
            "Paste" to { pasteIntoField() },
            "Copy" to { copyFromField() },
            "Share" to { shareFromField() },
        )
        ui.body(
            card,
            "Paste reads the Android clipboard once, when tapped. Copy puts the field text on it. " +
                "Share opens the Android share sheet.",
            Tone.Muted,
        )
        ui.button(card, "Prepare: validate with native core", emphasis = Emphasis.Primary) { prepareClipboard() }
        ui.button(card, "Clear field") {
            clipField.setText("")
            ui.setTone(clipStatus, "Cleared.", Tone.Muted)
        }
        ui.button(card, "Send files to computer…", emphasis = Emphasis.Quiet) { pickFilesToShare() }
    }

    private fun pasteIntoField() {
        val cm = getSystemService(ClipboardManager::class.java)
        val item = cm?.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)
        val text = item?.coerceToText(this)
        when {
            text == null || text.isEmpty() -> ui.setTone(clipStatus, "The Android clipboard has no text.", Tone.Warn)
            text.length > Limits.CLIPBOARD_MAX_BYTES ->
                ui.setTone(clipStatus, "Clipboard text is over 64 KiB; not loaded.", Tone.Warn)
            else -> {
                clipField.setText(text)
                ui.setTone(clipStatus, "Pasted into the field. Not prepared.", Tone.Muted)
            }
        }
    }

    private fun copyFromField() {
        val text = clipField.text.toString()
        if (text.isEmpty()) return ui.setTone(clipStatus, "Nothing to copy.", Tone.Warn)
        val clip = ClipData.newPlainText("GeekBridge", text)
        clip.description.extras = PersistableBundle().apply {
            // Asks the system to hide the preview in clipboard overlays (honoured on Android 13+).
            putBoolean("android.content.extra.IS_SENSITIVE", true)
        }
        getSystemService(ClipboardManager::class.java).setPrimaryClip(clip)
        ui.setTone(clipStatus, "Copied. Other apps on this phone can read the clipboard.", Tone.Muted)
    }

    private fun shareFromField() {
        val text = clipField.text.toString()
        if (text.isEmpty()) return ui.setTone(clipStatus, "Nothing to share.", Tone.Warn)
        if (Bounds.utf8Size(text) > Limits.CLIPBOARD_MAX_BYTES) {
            return ui.setTone(clipStatus, "Text is over 64 KiB.", Tone.Warn)
        }
        val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
        try {
            startActivity(Intent.createChooser(send, "Share text"))
        } catch (e: ActivityNotFoundException) {
            ui.setTone(clipStatus, "No app can receive shared text.", Tone.Warn)
        }
    }

    private fun prepareClipboard() {
        val text = clipField.text.toString()
        clipBusy = true
        ui.setTone(clipStatus, "Validating with the native core…", Tone.Muted)
        work.submit("clipboard", { core.validateClipboard(text) }) { r ->
            clipBusy = false
            val o = r.outcome()
            ui.setTone(clipStatus, Format.clipboardOutcome(o, Bounds.utf8Size(text)), if (o is CoreOutcome.Ok) Tone.Ok else Tone.Warn)
        }
    }

    // ------------------------------------------------------------------ messages (validation only)

    private fun buildMessagesCard(root: LinearLayout) {
        val card = ui.card(root, "Messages \u00b7 validate only", accent = true)
        ui.dotBanner(card, "MESSAGES")
        val red = getColor(R.color.gb_red)
        ui.row(card, 0, "Check a draft against the shared rules", "available offline", Tone.Normal)
        ui.row(card, 1, "Read messages on this phone", "off \u00b7 no permission", Tone.Muted)
        ui.row(card, 2, "Send a text from the computer", "off \u00b7 not implemented", Tone.Muted)
        ui.row(card, 3, "Sync conversations", "off \u00b7 not implemented", Tone.Muted)
        ui.body(
            card,
            "Checks a draft against the shared message rules in the native core. It does not send, read " +
                "or store any SMS, and this app holds no SMS permission. Reading or sending SMS for a computer " +
                "would need READ_SMS/SEND_SMS, which Android restricts: stores generally allow them only for the " +
                "default SMS app, and sideloaded installs on Android 13+ may need \"Allow restricted settings\". " +
                "A denial at the runtime prompt is final until you change it in app settings. " +
                "Nothing here ever sends automatically.",
            Tone.Muted,
        )
        smsRecipient = ui.input(card, "Recipient number", InputType.TYPE_CLASS_PHONE, Limits.SMS_MAX_ADDRESS_CHARS)
        smsText = ui.input(card, "Message", InputType.TYPE_CLASS_TEXT, Limits.SMS_MAX_TEXT_CHARS, lines = 3)
        smsSubscription = ui.input(card, "SIM subscription id (optional)", InputType.TYPE_CLASS_NUMBER, 9)
        smsStatus = ui.body(card, "Not checked.", Tone.Muted)
        ui.button(card, "Validate draft (nothing is sent)", emphasis = Emphasis.Primary, accent = red) { validateSms() }
        ui.button(card, "Send from computer (unavailable)", enabled = false, emphasis = Emphasis.Quiet) {}
    }

    private fun validateSms() {
        val subText = smsSubscription.text.toString().trim()
        val sub = if (subText.isEmpty()) null else subText.toIntOrNull()
        if (subText.isNotEmpty() && sub == null) {
            return ui.setTone(smsStatus, "Subscription id must be a whole number.", Tone.Warn)
        }
        val recipient = smsRecipient.text.toString()
        val text = smsText.text.toString()
        smsBusy = true
        ui.setTone(smsStatus, "Validating with the native core…", Tone.Muted)
        work.submit("sms", { core.validateSms(recipient, text, sub) }) { r ->
            smsBusy = false
            val o = r.outcome()
            ui.setTone(smsStatus, Format.smsOutcome(o), if (o is CoreOutcome.Ok) Tone.Ok else Tone.Warn)
        }
    }

    // ------------------------------------------------------------------ contacts & agenda

    private fun buildContactsCard(root: LinearLayout) {
        val card = ui.card(root, "Contacts \u00b7 preview")
        ui.body(
            card,
            "Reads the first ${LocalProviders.MAX_CONTACTS} contacts with a phone number, on this phone only, " +
                "after you allow access. Not copied, stored or sent.",
        )
        sections += PreviewSection<ContactPreview>(
            card = card,
            subject = "contacts",
            permission = Manifest.permission.READ_CONTACTS,
            requestCode = REQ_CONTACTS,
            noun = "contacts",
            loadLabel = "Show contacts preview",
            loader = { c, sig -> LocalProviders.loadContacts(c, sig) },
            render = { Format.contactLine(it) },
        )
    }

    private fun buildAgendaCard(root: LinearLayout) {
        val card = ui.card(root, "Agenda \u00b7 preview")
        ui.body(
            card,
            "Reads up to ${LocalProviders.MAX_EVENTS} calendar events in the next ${LocalProviders.AGENDA_DAYS} days " +
                "from Android's calendar provider, after you allow access. Titles and locations only.",
        )
        val zone = ZoneId.systemDefault()
        val locale = Locale.getDefault()
        sections += PreviewSection<AgendaEvent>(
            card = card,
            subject = "calendar",
            permission = Manifest.permission.READ_CALENDAR,
            requestCode = REQ_CALENDAR,
            noun = "events",
            loadLabel = "Show agenda preview",
            loader = { c, sig -> LocalProviders.loadAgenda(c, System.currentTimeMillis(), sig) },
            render = { Format.eventLines(it, zone, locale) },
        )
    }

    private fun buildPrivacyCard(root: LinearLayout) {
        val card = ui.card(root, "Privacy")
        ui.body(
            card,
            "The internet permission is used by the optional computer link (starts when you tap Start, stops when you " +
                "leave the app) and by update checks, which only read GitHub's public release list when the app opens (at most " +
                "every 12 hours, can be switched off) or when you tap Check for updates. There is no background service and " +
                "nothing listens for connections. Updates are installed only after a SHA-256 check and your confirmation in " +
                "Android's installer. It never asks for " +
                "accessibility, root or ADB access. Contacts, calendar, clipboard and message text are not " +
                "logged or saved, and previews are cleared when you leave the app.",
        )
    }

    /** One permission-gated, bounded, cancellable local preview. */
    private inner class PreviewSection<T>(
        card: LinearLayout,
        private val subject: String,
        private val permission: String,
        val requestCode: Int,
        private val noun: String,
        loadLabel: String,
        private val loader: (Context, CancellationSignal) -> LoadResult<T>,
        private val render: (T) -> String,
    ) {
        private var state = PermissionUi.NotGranted
        private var showingData = false

        private val status: TextView = ui.body(card, "", Tone.Muted)
        private val allowButton: Button = ui.button(card, "Allow $subject access", emphasis = Emphasis.Primary) {
            requestPermissions(arrayOf(permission), requestCode)
        }
        private val loadButton: Button = ui.button(card, loadLabel) { load() }
        private val settingsButton: Button = ui.button(card, "Open app settings") { openAppSettings() }
        private val list = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.VERTICAL }
        private val clearButton: Button = ui.button(card, "Clear preview") { clear() }

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
                        status, "Access not allowed yet. Nothing is read until you allow it.", Tone.Muted,
                    )
                    PermissionUi.Granted -> ui.setTone(
                        status, "Access allowed. Tap to read a preview from this phone.", Tone.Ok,
                    )
                    PermissionUi.Denied -> ui.setTone(
                        status, "Access denied. The feature stays off; you can ask again.", Tone.Warn,
                    )
                    PermissionUi.Blocked -> ui.setTone(
                        status, "Access denied and Android will not ask again. Enable it in app settings.", Tone.Warn,
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
            ui.setTone(status, "Reading…", Tone.Muted)
            work.submit(
                subject,
                { token ->
                    val signal = CancellationSignal()
                    token.onCancel { signal.cancel() }
                    loader(applicationContext, signal)
                },
            ) { r ->
                when (val res = r.getOrElse { LoadResult.Failed("Reading failed (${it.javaClass.simpleName}).") }) {
                    is LoadResult.Loaded<T> -> show(res)
                    is LoadResult.Failed -> {
                        showingData = false
                        ui.setTone(status, res.reason, Tone.Bad)
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
                ui.setTone(status, "Access allowed, but there is nothing to show.", Tone.Muted)
            } else {
                ui.setTone(status, Format.truncationNote(res.items.size, res.truncated, noun), Tone.Muted)
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
