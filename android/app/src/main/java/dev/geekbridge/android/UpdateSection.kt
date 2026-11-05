// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
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
    checkerOverride: UpdateChecker? = null,
) {
    private var checker: UpdateChecker = checkerOverride ?: UpdateChecker.forBuild(activity)
    private val fixedChecker = checkerOverride != null
    private var channel = UpdateChecker.channelOf(activity)
    private val main = Handler(Looper.getMainLooper())
    private val prefs = activity.getSharedPreferences("updates", Context.MODE_PRIVATE)
    private val worker: ExecutorService = Executors.newSingleThreadExecutor { r -> Thread(r, "geekbridge-update").apply { isDaemon = true } }

    private val card = ui.card(root, activity.getString(R.string.upd_title))
    private val status: TextView
    private val notes: TextView
    private val channelLine: TextView
    private val channelButtons: List<Button>
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
            activity.getString(R.string.upd_intro, BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE),
            Tone.Muted,
        )
        status = ui.statusLine(card, activity.getString(R.string.upd_not_checked), Tone.Muted)
        notes = ui.body(card, "", Tone.Muted).apply { visibility = android.view.View.GONE }
        channelLine = ui.body(card, "", Tone.Muted)
        channelButtons = ui.buttonRow(
            card,
            activity.getString(R.string.upd_channel_stable) to { chooseChannel(UpdateChannel.Stable) },
            activity.getString(R.string.upd_channel_dev) to { chooseChannel(UpdateChannel.Dev) },
        )
        autoCheck = ui.check(card, activity.getString(R.string.upd_auto), checked = prefs.getBoolean(KEY_AUTO, true))
        autoCheck.setOnCheckedChangeListener { _, on -> prefs.edit().putBoolean(KEY_AUTO, on).apply() }
        checkButton = ui.button(card, activity.getString(R.string.upd_check)) { startCheck(manual = true) }
        downloadButton = ui.button(card, activity.getString(R.string.upd_download), emphasis = Emphasis.Primary) { startDownload() }
        installButton = ui.button(card, activity.getString(R.string.upd_install), emphasis = Emphasis.Primary) { startInstall() }
        pageButton = ui.button(card, activity.getString(R.string.upd_open_page)) { openPage() }
        render()
    }

    fun onStart() {
        cancelled = false
        val now = System.currentTimeMillis()
        if (autoCheck.isChecked && !busy && UpdateModel.autoCheckDue(prefs.getLong(KEY_LAST_CHECK, 0), now) &&
            UpdateModel.attemptDue(prefs.getLong(KEY_LAST_ATTEMPT, 0), now)
        ) {
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

    private fun channelName(c: UpdateChannel) =
        activity.getString(if (c == UpdateChannel.Dev) R.string.upd_channel_dev else R.string.upd_channel_stable)

    private fun chooseChannel(target: UpdateChannel) {
        if (busy || target == channel) return
        if (target == UpdateChannel.Dev && !BuildConfig.DEBUG) {
            // A release build is signed with the release key; dev builds are debug-signed. Say so before switching.
            channelWarning(activity) { applyChannel(target) }.show()
        } else {
            applyChannel(target)
        }
    }

    /** Shows the card as it looks in a given state, without a network check (the debug design gallery). */
    internal fun preview(o: UpdateOffer?, isVerified: Boolean, text: String, tone: Tone, dev: Boolean) {
        channel = if (dev) UpdateChannel.Dev else UpdateChannel.Stable
        offer = o
        verified = isVerified
        say(text, tone)
        render()
    }

    private fun applyChannel(target: UpdateChannel) {
        channel = target
        prefs.edit().putString(UpdateChecker.KEY_CHANNEL, target.id).putLong(KEY_LAST_CHECK, 0).apply()
        if (!fixedChecker) checker = UpdateChecker.forBuild(activity)
        UpdateFileProvider.dir(activity).listFiles()?.forEach { it.delete() }
        offer = null
        verified = false
        say(activity.getString(R.string.upd_not_checked), Tone.Muted)
        render()
        startCheck(manual = true)
    }

    private fun render() {
        channelLine.text = activity.getString(R.string.upd_channel_line, channelName(channel))
        channelButtons.forEachIndexed { i, b ->
            val which = if (i == 1) UpdateChannel.Dev else UpdateChannel.Stable
            val name = channelName(which)
            b.isEnabled = !busy
            ui.markSelected(b, which == channel, name, activity.getString(R.string.upd_channel_selected, name))
        }
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
        say(activity.getString(if (manual) R.string.upd_checking_github else R.string.upd_checking), Tone.Muted)
        render()
        val sfx = if (channel == UpdateChannel.Dev) "_dev" else ""
        val cache = prefs.getString(KEY_ETAG + sfx, null)?.let { etag ->
            prefs.getString(KEY_BODY + sfx, null)?.let { UpdateChecker.Cache(etag, it, prefs.getString(KEY_EXTRA + sfx, null)) }
        }
        val checking = checker
        worker.execute {
            prefs.edit().putLong(KEY_LAST_ATTEMPT, System.currentTimeMillis()).apply()
            val result = checking.check(cache) { fresh ->
                prefs.edit().putString(KEY_ETAG + sfx, fresh.etag).putString(KEY_BODY + sfx, fresh.body.takeIf { it.length <= 128 * 1024 })
                    .putString(KEY_EXTRA + sfx, fresh.extra?.takeIf { it.length <= 16 * 1024 }).apply()
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
                    say(activity.getString(R.string.upd_available_no_sha, o.version), Tone.Warn)
                } else if (o.dev) {
                    say(activity.getString(R.string.upd_available_dev, o.version, BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE), Tone.Ok)
                } else {
                    say(activity.getString(R.string.upd_available, o.version, BuildConfig.VERSION_NAME), Tone.Ok)
                }
            }
            is UpdateCheckResult.UpToDate -> {
                offer = null
                say(
                    if (channel == UpdateChannel.Dev) {
                        activity.getString(R.string.upd_up_to_date_dev, BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE)
                    } else {
                        activity.getString(R.string.upd_up_to_date, BuildConfig.VERSION_NAME)
                    },
                    Tone.Ok,
                )
            }
            is UpdateCheckResult.NoPublicRelease -> {
                offer = null
                say(UpdateCheckResult.NoPublicRelease.MESSAGE.text(activity.strings), Tone.Muted)
            }
            is UpdateCheckResult.Failed -> say(result.reason.text(activity.strings), Tone.Warn)
        }
        render()
    }

    private fun startDownload() {
        val o = offer ?: return
        if (busy) return
        busy = true
        cancelled = false
        say(activity.getString(R.string.upd_downloading_name, o.apkName), Tone.Muted)
        render()
        val dir = UpdateFileProvider.dir(activity)
        worker.execute {
            val outcome = try {
                checker.download(o, dir) { done, total ->
                    if (cancelled) throw IOException("cancelled")
                    if (done == total || done % (256 * 1024) < 32 * 1024) {
                        main.post { say(activity.getString(R.string.upd_downloading_pct, (done * 100 / total).coerceIn(0, 100)), Tone.Muted) }
                    }
                }
                signerProblem(dir)
            } catch (e: UpdateChecker.UpdateException) {
                e.text.text(activity.strings)
            }
            main.post {
                busy = false
                if (outcome == null) {
                    verified = true
                    say(activity.getString(R.string.upd_verified, o.version), Tone.Ok)
                } else {
                    say(outcome, Tone.Warn)
                }
                render()
            }
        }
    }

    /** Non-null (user text) when the verified APK is signed by a different key than the installed app; the file is deleted. */
    private fun signerProblem(dir: File?): String? {
        val file = dir?.let { File(it, UpdateChecker.UPDATE_FILE_NAME) } ?: return null
        val verdict = SignerCheck.check(SignerCheck.AndroidSource(activity.packageManager, activity.packageName), file.path)
        val text = if (verdict is SignerCheck.Verdict.Mismatch && channel == UpdateChannel.Dev) {
            // The dev APK is signed with the developer's debug key: say what that means for this installation.
            activity.getString(if (BuildConfig.DEBUG) R.string.upd_dev_signer_debug else R.string.upd_dev_signer_release)
        } else {
            SignerCheck.message(activity.strings, verdict, BuildConfig.DEBUG)
        } ?: return null
        file.delete()
        return text
    }

    private fun startInstall() {
        val o = offer ?: return
        val expected = o.sha256 ?: return
        if (busy) return
        val file = UpdateFileProvider.file(activity)
        if (!activity.packageManager.canRequestPackageInstalls()) {
            say(activity.getString(R.string.upd_need_permission), Tone.Warn)
            try {
                activity.startActivity(
                    Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}")),
                )
            } catch (_: ActivityNotFoundException) {
                say(activity.getString(R.string.upd_need_permission_manual), Tone.Warn)
            }
            return
        }
        busy = true
        render()
        worker.execute {
            // Re-check the file the installer will read, right before handing it over.
            val ok = file.isFile && sha256Of(file) == expected.lowercase()
            val signer = if (ok) signerProblem(file.parentFile) else null
            main.post {
                busy = false
                if (signer != null) {
                    file.delete()
                    verified = false
                    say(signer, Tone.Warn)
                } else if (!ok) {
                    file.delete()
                    verified = false
                    say(activity.getString(R.string.upd_file_changed), Tone.Warn)
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
            say(activity.getString(R.string.upd_installer_open), Tone.Ok)
        } catch (_: ActivityNotFoundException) {
            say(activity.getString(R.string.upd_no_installer), Tone.Warn)
        }
    }

    private fun openPage() {
        val url = offer?.releaseUrl?.takeIf { UpdateModel.urlAllowed(it) } ?: return
        try {
            activity.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
        } catch (_: ActivityNotFoundException) {
            say(activity.getString(R.string.upd_no_browser), Tone.Warn)
        }
    }

    companion object {
        /** The question before switching a release build to the dev channel (dev builds are debug-signed). Not shown yet. */
        internal fun channelWarning(activity: Activity, onOk: () -> Unit): android.app.AlertDialog =
            android.app.AlertDialog.Builder(activity)
                .setTitle(R.string.upd_channel_warn_title)
                .setMessage(R.string.upd_channel_warn)
                .setPositiveButton(R.string.upd_channel_warn_ok) { _, _ -> onOk() }
                .setNegativeButton(R.string.upd_channel_warn_cancel, null)
                .create()

        private const val KEY_AUTO = "auto_check"
        private const val KEY_LAST_CHECK = "last_check_ms"
        private const val KEY_LAST_ATTEMPT = "last_attempt_ms"
        private const val KEY_ETAG = "etag"
        private const val KEY_BODY = "body"
        private const val KEY_EXTRA = "extra"

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
