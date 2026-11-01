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
    private var request: ShareRequest = ShareRequest.Invalid("There is nothing to send.")
    private var uris: List<Uri> = emptyList()
    private val connectTried = HashSet<String>()
    private var asked = false
    private var holding = false
    private var multicast: WifiManager.MulticastLock? = null
    private val main = Handler(Looper.getMainLooper())

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ui = Ui(this)
        coordinator = ShareCoordinator.get(this)
        parse(intent)
        val pad = ui.dp(20)
        box = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(pad, pad, pad, pad) }
        setContentView(ScrollView(this).apply { addView(box) })
        if (savedInstanceState == null && request !is ShareRequest.Invalid) {
            coordinator.controller.hold()
            holding = true
            startLinkIfNeeded()
        }
        render()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        if (!coordinator.sessionActive) { parse(intent); render() }
    }

    override fun onStart() {
        super.onStart()
        coordinator.onChange = { onState() }
        coordinator.controller.poll()
    }

    override fun onStop() {
        coordinator.onChange = null
        releaseMulticast()
        super.onStop()
    }

    override fun onDestroy() {
        if (holding && !isChangingConfigurations) coordinator.controller.release()
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
        uris = streams.filter { it.scheme == "content" }.distinct()
        request = if (i.action != Intent.ACTION_SEND && i.action != Intent.ACTION_SEND_MULTIPLE) {
            ShareRequest.Invalid("Nothing was shared.")
        } else {
            ShareIntentLogic.classify(uris.map { it.toString() }, i.getCharSequenceExtra(Intent.EXTRA_TEXT))
        }
        if (request is ShareRequest.Files) uris = uris.take(ShareLimits.MAX_FILES)
    }

    private fun startLinkIfNeeded() {
        val c = coordinator.controller
        if (c.phase == LinkPhase.Stopped) {
            c.start(StartOptions(filesDir.absolutePath, Build.MODEL ?: "Android", true, null, true, false, false))
        }
    }

    private fun onState() {
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
        box.addView(ui.text("Send to computer", sizeSp = 22f, bold = true))
        val req = request
        if (req is ShareRequest.Invalid) {
            box.addView(ui.text(req.reason, Tone.Warn))
            ui.button(box, "Close") { finish() }
            return
        }
        box.addView(
            ui.text(
                when (req) {
                    is ShareRequest.Files -> "${req.uris.size} file(s)" + if (req.truncated) " (only the first ${ShareLimits.MAX_FILES} are sent)" else ""
                    is ShareRequest.Text -> if (req.kind == TransferKind.Url) "A link: " + Bounds.clip(req.text, 80) else "Text: " + Bounds.clip(req.text, 80)
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
                box.addView(ui.text("Starting the link…", Tone.Muted))
            c.phase != LinkPhase.Running -> {
                box.addView(ui.text("The link is not running.", Tone.Warn))
                ui.button(box, "Start link", emphasis = Emphasis.Primary) { startLinkIfNeeded() }
            }
            targets.isEmpty() -> noComputer()
            else -> {
                val choice = TargetPicker.autoChoice(targets)
                if (choice?.connectionId != null) {
                    send(choice)
                    return
                }
                box.addView(ui.text("Choose a paired computer:", Tone.Muted))
                for (t in targets) {
                    val ready = t.connectionId != null
                    val label = t.name.ifEmpty { "(no name)" } +
                        if (ready) "" else if (t.reachableAt != null) " — connecting…" else " — not found on this network"
                    ui.button(box, label, enabled = ready) { send(t) }
                }
                ui.body(box, "A computer that is not listed as connected must be on the same Wi-Fi with GeekBridge running.", Tone.Muted)
                ui.button(box, "Open GeekBridge", emphasis = Emphasis.Quiet) { openMain() }
            }
        }
        ui.button(box, "Cancel", emphasis = Emphasis.Quiet) { finish() }
    }

    private fun noComputer() {
        box.addView(ui.text("No paired computer yet. Open GeekBridge, connect to your computer and pair it, then share again.", Tone.Warn))
        ui.button(box, "Open GeekBridge", emphasis = Emphasis.Primary) { openMain() }
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
        }
        val active = coordinator.controller.snapshot?.transfers.orEmpty().filter { it.direction == TransferDirection.Outgoing && !it.state.finished }
        if (coordinator.sessionActive) {
            box.addView(ui.text("Keep going in the background; progress is also in the notification.", Tone.Muted))
            if (active.isNotEmpty()) ui.button(box, "Cancel transfers", emphasis = Emphasis.Quiet) { active.forEach { coordinator.cancel(it.id) } }
        }
        ui.button(box, if (coordinator.sessionActive) "Hide" else "Done", emphasis = Emphasis.Primary) { finish() }
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
