package dev.geekbridge.android

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.widget.Button
import android.widget.CompoundButton
import android.widget.LinearLayout
import android.widget.TextView
import java.io.File
import java.io.IOException
import java.security.MessageDigest
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * The "App updates" card. A check runs when the app opens (at most every 12 hours, switch-off-able) and when
 * the user taps "Check for updates"; the only traffic is GET requests to GitHub's release API. A download
 * starts only on a tap, is SHA-256 verified against the release's `SHA256SUMS`, and is then handed to the
 * system installer (which asks the user to confirm). Nothing installs silently.
 */
class UpdateSection(
    private val activity: Activity,
    private val ui: Ui,
    root: LinearLayout,
    private val checker: UpdateChecker = UpdateChecker.forBuild(activity),
) {
    private val main = Handler(Looper.getMainLooper())
    private val prefs = activity.getSharedPreferences("updates", Context.MODE_PRIVATE)
    private val worker: ExecutorService = Executors.newSingleThreadExecutor { r -> Thread(r, "geekbridge-update").apply { isDaemon = true } }

    private val card = ui.card(root, "App updates")
    private val status: TextView
    private val notes: TextView
    private val autoCheck: CompoundButton
    private val checkButton: Button
    private val downloadButton: Button
    private val installButton: Button
    private val pageButton: Button

    private var offer: UpdateOffer? = null
    private var verified = false
    private var busy = false
    @Volatile private var cancelled = false

    init {
        ui.body(
            card,
            "GeekBridge ${BuildConfig.VERSION_NAME} (build ${BuildConfig.VERSION_CODE}). Checking only asks GitHub for the " +
                "latest public release of this project; nothing about you or this phone is sent. Android always asks you to " +
                "confirm an install, and an update only installs over the same signing key.",
            Tone.Muted,
        )
        status = ui.statusLine(card, "Not checked yet.", Tone.Muted)
        notes = ui.body(card, "", Tone.Muted).apply { visibility = android.view.View.GONE }
        autoCheck = ui.check(card, "Check when the app opens (at most every 12 hours)", checked = prefs.getBoolean(KEY_AUTO, true))
        autoCheck.setOnCheckedChangeListener { _, on -> prefs.edit().putBoolean(KEY_AUTO, on).apply() }
        checkButton = ui.button(card, "Check for updates") { startCheck(manual = true) }
        downloadButton = ui.button(card, "Download and verify", emphasis = Emphasis.Primary) { startDownload() }
        installButton = ui.button(card, "Install update", emphasis = Emphasis.Primary) { startInstall() }
        pageButton = ui.button(card, "Open release page") { openPage() }
        render()
    }

    fun onStart() {
        cancelled = false
        if (autoCheck.isChecked && !busy && UpdateModel.autoCheckDue(prefs.getLong(KEY_LAST_CHECK, 0), System.currentTimeMillis())) {
            startCheck(manual = false)
        }
    }

    /** Leaving the foreground abandons a running download (no background service) and drops the partial file. */
    fun onStop() {
        cancelled = true
    }

    fun onDestroy() {
        cancelled = true
        worker.shutdownNow()
    }

    private fun render() {
        val o = offer
        val canDownload = o != null && o.sha256 != null && !verified
        downloadButton.visibility = if (canDownload) android.view.View.VISIBLE else android.view.View.GONE
        installButton.visibility = if (verified) android.view.View.VISIBLE else android.view.View.GONE
        pageButton.visibility = if (o != null && o.releaseUrl.isNotEmpty()) android.view.View.VISIBLE else android.view.View.GONE
        checkButton.isEnabled = !busy
        downloadButton.isEnabled = !busy
        installButton.isEnabled = !busy
        if (o != null && o.notes.isNotEmpty()) {
            notes.text = o.notes
            notes.visibility = android.view.View.VISIBLE
        } else {
            notes.visibility = android.view.View.GONE
        }
    }

    private fun say(text: String, tone: Tone) {
        ui.setStatus(status, text, tone)
    }

    private fun startCheck(manual: Boolean) {
        if (busy) return
        busy = true
        verified = false
        say(if (manual) "Checking GitHub…" else "Checking for updates…", Tone.Muted)
        render()
        val cache = prefs.getString(KEY_ETAG, null)?.let { etag -> prefs.getString(KEY_BODY, null)?.let { UpdateChecker.Cache(etag, it) } }
        worker.execute {
            val result = checker.check(cache) { fresh ->
                prefs.edit().putString(KEY_ETAG, fresh.etag).putString(KEY_BODY, fresh.body.takeIf { it.length <= 128 * 1024 }).apply()
            }
            if (result !is UpdateCheckResult.Failed) prefs.edit().putLong(KEY_LAST_CHECK, System.currentTimeMillis()).apply()
            main.post { onCheckDone(result) }
        }
    }

    private fun onCheckDone(result: UpdateCheckResult) {
        busy = false
        when (result) {
            is UpdateCheckResult.Available -> {
                offer = result.offer
                val o = result.offer
                if (o.sha256 == null) {
                    say("Version ${o.version} is available, but the release publishes no SHA-256 for the app, so it can't be downloaded here.", Tone.Warn)
                } else {
                    say("Version ${o.version} is available (you have ${BuildConfig.VERSION_NAME}).", Tone.Ok)
                }
            }
            is UpdateCheckResult.UpToDate -> {
                offer = null
                say("GeekBridge ${BuildConfig.VERSION_NAME} is up to date.", Tone.Ok)
            }
            is UpdateCheckResult.Failed -> say(result.reason, Tone.Warn)
        }
        render()
    }

    private fun startDownload() {
        val o = offer ?: return
        if (busy) return
        busy = true
        cancelled = false
        say("Downloading ${o.apkName}…", Tone.Muted)
        render()
        val dir = UpdateFileProvider.dir(activity)
        worker.execute {
            val outcome = try {
                checker.download(o, dir) { done, total ->
                    if (cancelled) throw IOException("cancelled")
                    if (done == total || done % (256 * 1024) < 32 * 1024) {
                        main.post { say("Downloading… ${(done * 100 / total).coerceIn(0, 100)}%", Tone.Muted) }
                    }
                }
                null
            } catch (e: UpdateChecker.UpdateException) {
                e.message ?: "The download failed."
            }
            main.post {
                busy = false
                if (outcome == null) {
                    verified = true
                    say("Version ${o.version} downloaded and verified against the published SHA-256. Tap Install to continue.", Tone.Ok)
                } else {
                    say(outcome, Tone.Warn)
                }
                render()
            }
        }
    }

    private fun startInstall() {
        val o = offer ?: return
        val expected = o.sha256 ?: return
        if (busy) return
        val file = UpdateFileProvider.file(activity)
        if (Build.VERSION.SDK_INT >= 26 && !activity.packageManager.canRequestPackageInstalls()) {
            say("Android needs your permission first: allow GeekBridge to install apps on the next screen, then come back and tap Install again.", Tone.Warn)
            try {
                activity.startActivity(
                    Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}")),
                )
            } catch (_: ActivityNotFoundException) {
                say("Open Settings > Apps > GeekBridge > Install unknown apps, allow it, then tap Install again.", Tone.Warn)
            }
            return
        }
        busy = true
        render()
        worker.execute {
            // Re-check the file the installer will read, right before handing it over.
            val ok = file.isFile && sha256Of(file) == expected.lowercase()
            main.post {
                busy = false
                if (!ok) {
                    file.delete()
                    verified = false
                    say("The downloaded file changed or disappeared. Download it again.", Tone.Warn)
                } else {
                    handOff()
                }
                render()
            }
        }
    }

    private fun handOff() {
        val intent = Intent(Intent.ACTION_VIEW)
            .setDataAndType(UpdateFileProvider.uri(activity), UpdateFileProvider.APK_MIME)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        try {
            activity.startActivity(intent)
            say("Android's installer is open. Confirm there to finish the update.", Tone.Ok)
        } catch (_: ActivityNotFoundException) {
            say("No package installer was found on this phone.", Tone.Warn)
        }
    }

    private fun openPage() {
        val url = offer?.releaseUrl?.takeIf { UpdateModel.urlAllowed(it) } ?: return
        try {
            activity.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
        } catch (_: ActivityNotFoundException) {
            say("No browser was found to open the release page.", Tone.Warn)
        }
    }

    companion object {
        private const val KEY_AUTO = "auto_check"
        private const val KEY_LAST_CHECK = "last_check_ms"
        private const val KEY_ETAG = "etag"
        private const val KEY_BODY = "body"

        fun sha256Of(file: File): String {
            val md = MessageDigest.getInstance("SHA-256")
            file.inputStream().use { input ->
                val buf = ByteArray(64 * 1024)
                while (true) {
                    val n = input.read(buf)
                    if (n < 0) break
                    md.update(buf, 0, n)
                }
            }
            return md.digest().joinToString("") { "%02x".format(it) }
        }
    }
}
