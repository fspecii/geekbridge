// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.os.Bundle
import android.widget.LinearLayout
import android.widget.Toast

/**
 * "Don't have GeekBridge on your computer yet?" Three ways to get the download page to the computer, none of
 * which needs a network call or an account from this app: hand the link to any app the user picks (mail, chat,
 * notes), a mail draft, or a short address to type. See [ComputerInvite] for what the text contains.
 */
class ComputerLinkActivity : Activity() {
    private lateinit var kit: UiKit
    private lateinit var screen: Screen

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppState.init(this)
        ActivityTracker.install(this)
        kit = UiKit(this, Ui(this))
        screen = Screen(this, kit).also { it.install() }
        val root = screen.root
        root.addView(DotText(this, getString(R.string.invite_title), maxPitchDp = Tok.DOT_NUMERAL, maxLines = 3, accentDot = true), LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(8); bottomMargin = kit.dp(12) })
        root.addView(kit.text(getString(R.string.invite_why), 15f, Tone.Muted))

        val send = kit.panel(root).also { (it.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(16) }
        send.addView(kit.text(getString(R.string.invite_send_title), 17f, bold = true))
        send.addView(kit.text(getString(R.string.invite_send_body), 14f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, 0) })
        kit.button(send, getString(R.string.invite_send), BtnStyle.Primary) { share() }
        kit.button(send, getString(R.string.invite_mail), BtnStyle.Tonal) { mail() }

        val typed = kit.panel(root).also { (it.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(12) }
        typed.addView(kit.text(getString(R.string.invite_type_title), 17f, bold = true))
        typed.addView(kit.text(getString(R.string.invite_type_body), 14f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, kit.dp(6)) })
        typed.addView(kit.ui.mono(ComputerInvite.SHORT_URL, 16f, bold = true))
        kit.button(typed, getString(R.string.invite_copy), BtnStyle.Tonal) { copy() }

        val steps = kit.panel(root).also { (it.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(12) }
        steps.addView(kit.text(getString(R.string.invite_steps_title), 17f, bold = true))
        ComputerInvite.lines(strings).forEachIndexed { i, line ->
            steps.addView(kit.text(getString(R.string.list_numbered, i + 1, line), 14f).apply { setPadding(0, kit.dp(6), 0, 0) })
        }
        kit.button(root, getString(R.string.qr_close), BtnStyle.Plain) { finish() }
        screen.requestInsets()
    }

    private fun share() {
        val send = Intent(Intent.ACTION_SEND)
            .setType("text/plain")
            .putExtra(Intent.EXTRA_SUBJECT, ComputerInvite.subject(strings))
            .putExtra(Intent.EXTRA_TEXT, ComputerInvite.body(strings))
        try {
            startActivity(Intent.createChooser(send, getString(R.string.invite_chooser)))
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(this, R.string.invite_no_app, Toast.LENGTH_LONG).show()
        }
    }

    private fun mail() {
        try {
            startActivity(Intent(Intent.ACTION_SENDTO, ComputerInvite.mailto(strings)))
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(this, R.string.invite_no_mail, Toast.LENGTH_LONG).show()
        }
    }

    private fun copy() {
        val cm = getSystemService(ClipboardManager::class.java)
        cm?.setPrimaryClip(ClipData.newPlainText(getString(R.string.invite_title), ComputerInvite.RELEASES_URL))
        Toast.makeText(this, R.string.invite_copied, Toast.LENGTH_SHORT).show()
    }
}
