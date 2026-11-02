package dev.geekbridge.android

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

/**
 * The "Send to computer" entry in the system Share sheet. It only gathers what the user shared (content://
 * Uris and/or text), lets them pick a paired computer, and hands the work to [ShareCoordinator]. File bytes are
 * never read here: the native core streams from a duplicated file descriptor.
 */
class ShareActivity : Activity() {
    private lateinit var ui: Ui
    private lateinit var coordinator: ShareCoordinator
    private lateinit var box: LinearLayout
    private var request: ShareRequest = ShareRequest.Invalid("")
    private var uris: List<Uri> = emptyList()
    private val connectTried = HashSet<String>()
    private var asked = false
    private var restartedAfterStop = false
    private lateinit var hold: ShareHold
    private var multicast: WifiManager.MulticastLock? = null
    private val main = Handler(Looper.getMainLooper())

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ui = Ui(this)
        coordinator = ShareCoordinator.get(this)
        // After a rotation the previous instance hands over its hold; a fresh start makes a new one.
        hold = lastNonConfigurationInstance as? ShareHold
            ?: ShareHold({ coordinator.controller.hold() }, { coordinator.controller.release() })
        parse(intent)
        val pad = ui.dp(20)
        box = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(pad, pad, pad, pad) }
        setContentView(ScrollView(this).apply { addView(box) })
        if (savedInstanceState == null && request !is ShareRequest.Invalid) {
            coordinator.forgetFinishedSession()
            hold.take()
            startLinkIfNeeded()
        }
        render()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        if (!coordinator.sessionActive) {
            parse(intent)
            if (request !is ShareRequest.Invalid) coordinator.forgetFinishedSession()
            render()
        }
    }

    override fun onStart() {
        super.onStart()
        coordinator.setListener(this) { onState() }
        coordinator.controller.poll()
    }

    override fun onStop() {
        coordinator.clearListener(this)
        releaseMulticast()
        super.onStop()
    }

    @Deprecated("Deprecated in Java")
    override fun onRetainNonConfigurationInstance(): Any = hold

    override fun onDestroy() {
        if (!isChangingConfigurations) hold.giveBack()
        super.onDestroy()
    }

    private fun parse(i: Intent) {
        val streams = ArrayList<Uri>()
        if (i.action == Intent.ACTION_SEND_MULTIPLE) {
            @Suppress("DEPRECATION")
            i.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)?.let { streams += it }
        } else if (i.action == Intent.ACTION_SEND) {
            @Suppress("DEPRECATION")
            (i.getParcelableExtra<Uri>(Intent.EXTRA_STREAM))?.let { streams += it }
        }
        i.clipData?.let { cd -> for (n in 0 until minOf(cd.itemCount, 200)) cd.getItemAt(n).uri?.let { streams += it } }
        // Only content:// Uris: a file:// Uri from another app could name this app's own private files.
        uris = streams.filter { it.scheme == "content" && !ShareIntentLogic.isBlockedAuthority(it.authority, packageName) }.distinct()
        request = if (i.action != Intent.ACTION_SEND && i.action != Intent.ACTION_SEND_MULTIPLE) {
            ShareRequest.Invalid(getString(R.string.share_nothing_shared))
        } else {
            ShareIntentLogic.classify(uris.map { it.toString() }, i.getCharSequenceExtra(Intent.EXTRA_TEXT))
        }
        if (request is ShareRequest.Files) uris = uris.take(ShareLimits.MAX_FILES)
    }

    /** What is about to be sent, by the names the provider reports (first few), not just a count. */
    private fun fileSummary(count: Int): String {
        val names = uris.take(5).map { displayName(it) }
        val more = if (count > names.size) "\n" + resources.getQuantityString(R.plurals.share_files_more, count - names.size, count - names.size) else ""
        return resources.getQuantityString(R.plurals.share_files_header, count, count) + "\n" + names.joinToString("\n") + more
    }

    /** Names the provider reported, filled in off the main thread: a provider (Photos, Drive) can block for seconds. */
    private val names = HashMap<Uri, String>()
    private var resolving = false

    private fun displayName(uri: Uri): String {
        val cached = names[uri]
        if (cached == null) resolveNamesSoon()
        val raw = cached?.takeIf { it.isNotEmpty() }
        val name = (raw ?: uri.lastPathSegment ?: getString(R.string.share_unnamed_file)).filter { !Character.isISOControl(it) }
        return Bounds.clip(name, 80)
    }

    private fun resolveNamesSoon() {
        if (resolving) return
        resolving = true
        val todo = uris.take(5).filter { it !in names }
        Thread({
            val found = todo.mapNotNull { uri ->
                try {
                    contentResolver.query(uri, arrayOf(android.provider.OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
                        if (c.moveToFirst() && !c.isNull(0)) uri to c.getString(0) else null
                    }
                } catch (_: Exception) {
                    null
                }
            }
            main.post {
                if (isDestroyed) return@post
                names.putAll(found)
                for (u in todo) names.putIfAbsent(u, u.lastPathSegment ?: "") // never ask twice
                resolving = false
                render()
            }
        }, "geekbridge-share-names").apply { isDaemon = true; start() }
    }

    private fun startLinkIfNeeded() {
        val c = coordinator.controller
        if (c.phase == LinkPhase.Stopped) {
            c.start(StartOptions(filesDir.absolutePath, Build.MODEL ?: "Android", true, null, true, false, false))
        }
    }

    private fun onState() {
        // The link can be mid-stop when this screen opens (Home leaving the foreground); start it once it has stopped.
        val phase = coordinator.controller.phase
        if (phase == LinkPhase.Stopped && !restartedAfterStop && request !is ShareRequest.Invalid && !coordinator.sessionActive) {
            restartedAfterStop = true
            startLinkIfNeeded()
        }
        autoConnect()
        syncMulticast(coordinator.controller.snapshot?.discovery?.listening == true)
        render()
    }

    /** A paired computer seen on the network but not connected: connect (TLS only; trust is the pinned certificate). */
    private fun autoConnect() {
        val s = coordinator.controller.snapshot ?: return
        if (coordinator.controller.phase != LinkPhase.Running) return
        for (t in TargetPicker.targets(s)) {
            val seen = t.reachableAt ?: continue
            if (t.connectionId == null && connectTried.add(t.deviceId)) {
                coordinator.controller.connect(seen.ip, seen.tcpPort, t.deviceId)
            }
        }
    }

    private fun render() {
        box.removeAllViews()
        // The dialog window already shows "Send to computer" as its title (and TalkBack reads it); no second heading.
        box.addView(DotText(this, getString(R.string.share_dot_title), maxPitchDp = 5f, maxLines = 1, accentDot = true, decorative = true))
        val req = request
        if (req is ShareRequest.Invalid) {
            box.addView(ui.text(req.reason.ifEmpty { getString(R.string.share_nothing) }, Tone.Warn))
            ui.button(box, getString(R.string.share_close)) { finish() }
            return
        }
        box.addView(
            ui.text(
                when (req) {
                    is ShareRequest.Files -> fileSummary(req.uris.size) + if (req.truncated) resources.getQuantityString(R.plurals.share_first_only, ShareLimits.MAX_FILES, ShareLimits.MAX_FILES) else ""
                    is ShareRequest.Text -> getString(if (req.kind == TransferKind.Url) R.string.share_link_prefix else R.string.share_text_prefix, Bounds.clip(req.text, 80))
                    else -> ""
                },
                Tone.Muted,
            ),
        )
        if (coordinator.sessionActive || coordinator.items.isNotEmpty()) return renderSession()

        val c = coordinator.controller
        val targets = TargetPicker.targets(c.snapshot)
        when {
            c.phase == LinkPhase.Starting || c.snapshot == null && c.phase == LinkPhase.Running ->
                box.addView(ui.text(getString(R.string.share_starting), Tone.Muted))
            c.phase != LinkPhase.Running -> {
                box.addView(ui.text(getString(R.string.share_link_off), Tone.Warn))
                ui.button(box, getString(R.string.share_start_link), emphasis = Emphasis.Primary) { startLinkIfNeeded() }
            }
            targets.isEmpty() -> noComputer()
            else -> {
                val choice = TargetPicker.autoChoice(targets)
                if (choice?.connectionId != null) {
                    send(choice)
                    return
                }
                box.addView(ui.text(getString(R.string.share_choose), Tone.Muted))
                for (t in targets) {
                    val ready = t.connectionId != null
                    val label = t.name.ifEmpty { getString(R.string.share_no_name) } +
                        if (ready) "" else getString(if (t.reachableAt != null) R.string.share_connecting_suffix else R.string.share_not_found_suffix)
                    ui.button(box, label, enabled = ready) { send(t) }
                }
                ui.body(box, getString(R.string.share_not_listed_help), Tone.Muted)
                ui.button(box, getString(R.string.share_open_app), emphasis = Emphasis.Quiet) { openMain() }
            }
        }
        ui.button(box, getString(R.string.share_cancel), emphasis = Emphasis.Quiet) { finish() }
    }

    private fun noComputer() {
        box.addView(ui.text(getString(R.string.share_no_computer), Tone.Warn))
        ui.button(box, getString(R.string.share_open_app), emphasis = Emphasis.Primary) { openMain() }
    }

    private fun openMain() {
        startActivity(Intent(this, MainActivity::class.java))
        finish()
    }

    private fun send(t: ShareTarget) {
        val conn = t.connectionId ?: return
        askNotificationPermission()
        when (val r = request) {
            is ShareRequest.Files -> coordinator.sendFiles(conn, uris)
            is ShareRequest.Text -> coordinator.sendText(conn, r.kind, r.text)
            is ShareRequest.Invalid -> return
        }
        render()
    }

    private fun renderSession() {
        for (i in coordinator.items) {
            val (text, tone) = coordinator.itemLine(i)
            box.addView(ui.text(text, tone))
            // Dotted progress for a file that is moving (the line above already says the percentage in words).
            val t = i.transferId?.let { id -> coordinator.controller.snapshot?.transfers?.firstOrNull { it.id == id } }
            val pct = t?.takeIf { it.state == TransferState.Active }?.let { ProgressText.percent(it) }
            if (pct != null) {
                box.addView(
                    DotProgress(this).apply { set(pct / 100f, text) },
                    LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = ui.dp(8); bottomMargin = ui.dp(4) },
                )
            }
        }
        val active = coordinator.controller.snapshot?.transfers.orEmpty().filter { it.direction == TransferDirection.Outgoing && !it.state.finished }
        if (coordinator.sessionActive) {
            box.addView(ui.text(getString(R.string.share_background_note), Tone.Muted))
            if (active.isNotEmpty()) ui.button(box, getString(R.string.share_cancel_transfers), emphasis = Emphasis.Quiet) { active.forEach { coordinator.cancel(it.id) } }
        }
        ui.button(box, getString(if (coordinator.sessionActive) R.string.share_hide else R.string.share_done), emphasis = Emphasis.Primary) { finish() }
    }

    private fun askNotificationPermission() {
        if (Build.VERSION.SDK_INT >= 33 && !asked &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            asked = true
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIFY)
        }
    }

    private fun syncMulticast(wanted: Boolean) {
        if (!wanted) return releaseMulticast()
        if (multicast?.isHeld == true) return
        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager ?: return
        multicast = wifi.createMulticastLock("geekbridge-share").apply {
            setReferenceCounted(false)
            try { acquire() } catch (_: SecurityException) { /* discovery may miss broadcasts */ }
        }
    }

    private fun releaseMulticast() {
        multicast?.let { if (it.isHeld) it.release() }
        multicast = null
    }

    companion object {
        private const val REQ_NOTIFY = 41
    }
}
