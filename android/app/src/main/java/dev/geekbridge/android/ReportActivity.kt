// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast

/**
 * Diagnostics & about > "Report a problem". Shows exactly what the prefilled GitHub issue link carries (the same
 * redacted text as "Copy report") and opens it in the browser only when asked. This app makes no network request
 * for it: the browser does. With no browser, the report goes to the clipboard and the plain link is shown.
 */
class ReportActivity : Activity() {
    private lateinit var kit: UiKit
    private lateinit var screen: Screen
    private lateinit var link: ReportLink.Link
    private var fallbackShown = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppState.init(this)
        ActivityTracker.install(this)
        kit = UiKit(this, Ui(this))
        screen = Screen(this, kit).also { it.install() }
        link = buildLink()
        fallbackShown = savedInstanceState?.getBoolean(STATE_FALLBACK) == true
        render()
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putBoolean(STATE_FALLBACK, fallbackShown)
    }

    private fun buildLink(): ReportLink.Link {
        val report = DiagnosticsReport.build(this, intent.getStringExtra(EXTRA_CORE).orEmpty(), intent.getStringExtra(EXTRA_ISSUE))
        val repo = ReportLink.repoOrDefault(BuildConfig.RELEASE_REPO, "fspecii/geekbridge")
        val phone = "${Build.MANUFACTURER} ${Build.MODEL}, Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})"
        return ReportLink.build(
            repo,
            ReportLink.DEFAULT_TITLE,
            listOf(
                ReportLink.Names.FIELD_VERSION to "phone ${BuildConfig.VERSION_NAME} (code ${BuildConfig.VERSION_CODE}); desktop: geekbridge --version",
                ReportLink.Names.FIELD_PHONE to phone,
            ),
            report,
        )
    }

    private fun render() {
        val root = screen.root
        root.removeAllViews()
        root.addView(
            DotText(this, getString(R.string.report_title), maxPitchDp = Tok.DOT_NUMERAL, maxLines = 3, accentDot = true),
            LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(8); bottomMargin = kit.dp(8) },
        )
        root.addView(kit.text(getString(R.string.report_nothing_sent), 14f, Tone.Muted))

        val card = kit.ui.card(root, getString(R.string.report_preview_title))
        // Selectable monospace text: what the user reads is the exact string put into the link.
        card.addView(
            kit.ui.mono(link.preview(), 12f).apply {
                setTextIsSelectable(true)
                contentDescription = getString(R.string.report_preview_cd)
            },
        )
        if (link.truncated) card.addView(kit.text(getString(R.string.report_truncated), 12f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })

        if (fallbackShown) {
            val fb = kit.ui.card(root, getString(R.string.report_no_browser_title), accent = true)
            fb.addView(kit.text(getString(R.string.report_no_browser_body), 14f))
            fb.addView(kit.ui.mono(link.url, 11f).apply { setTextIsSelectable(true) })
        }

        val actions = kit.panel(root)
        kit.button(actions, getString(R.string.report_open), BtnStyle.Primary) { openInBrowser() }
        kit.button(actions, getString(R.string.report_copy), BtnStyle.Tonal) { copy(); toast(R.string.report_copied) }
        kit.button(actions, getString(R.string.report_close), BtnStyle.Plain) { finish() }
        screen.announcePage(getString(R.string.report_title))
        screen.requestInsets()
    }

    private fun copy() {
        getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText(getString(R.string.dev_report_clip_label), link.preview()))
    }

    private fun toast(res: Int) = Toast.makeText(this, res, Toast.LENGTH_SHORT).show()

    private fun openInBrowser() {
        val open = Intent(Intent.ACTION_VIEW, Uri.parse(link.url)).addCategory(Intent.CATEGORY_BROWSABLE)
        try {
            startActivity(open)
        } catch (_: ActivityNotFoundException) {
            copy()
            fallbackShown = true
            toast(R.string.report_copied)
            render()
        }
    }

    companion object {
        const val EXTRA_CORE = "core_state"
        const val EXTRA_ISSUE = "last_issue"
        private const val STATE_FALLBACK = "fallback"
    }
}
