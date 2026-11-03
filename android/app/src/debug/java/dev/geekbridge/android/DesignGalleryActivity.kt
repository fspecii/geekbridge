// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.os.Bundle
import android.widget.LinearLayout
import java.util.Calendar

/**
 * Debug-only design gallery: every dot-matrix component with synthetic data, for screenshots and for checking
 * light/dark, font scale, RTL and landscape without a paired computer. Not part of the release APK.
 * `adb shell am start -n dev.geekbridge.android/.DesignGalleryActivity --es page calendar|status|controls`
 */
class DesignGalleryActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val kit = UiKit(this, Ui(this))
        val screen = Screen(this, kit).also { it.install() }
        val root = screen.root
        when (intent.getStringExtra("page")) {
            "calendar" -> calendar(kit, root)
            "controls" -> controls(kit, root)
            "shareoff" -> share(kit, root, on = false)
            "shareon" -> share(kit, root, on = true)
            "dial" -> dial(kit, root)
            else -> status(kit, root)
        }
    }

    private fun status(kit: UiKit, root: LinearLayout) {
        root.addView(kit.dotHeader("GeekBridge"))
        for (h in listOf(
            HeroView("Online", 1f, "Connected to Desk", "Ready to send and receive.", true, "Sending 2 of 2 · 60%", 0.6f),
            HeroView("Waiting", 0.5f, "Waiting for Desk", "Open GeekBridge on your computer, on the same Wi-Fi network.", false),
            HeroView("Offline", 0f, "Not connected", "Staying connected in the background is off.", false),
        )) {
            val holder = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
            root.addView(holder, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(12) })
            kit.heroTile(holder, h)
        }
        kit.label(root, "On this phone")
        val g = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        root.addView(g, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        kit.widgetTile(g, R.drawable.ic_call, "Calls", "Ready", true) {}
        kit.widgetTile(g, R.drawable.ic_message, "Messages", "Off", false) {}
    }

    private fun calendar(kit: UiKit, root: LinearLayout) {
        root.addView(kit.dotHeader("Calendar"))
        val now = Calendar.getInstance()
        val grid = DotCalendar(kit).apply {
            bind(
                now.get(Calendar.YEAR), now.get(Calendar.MONTH),
                eventCounts = mapOf(3 to 1, 6 to 2, 12 to 1, 18 to 3, 24 to 1, 27 to 1),
                today = now.get(Calendar.DAY_OF_MONTH), selected = 18, onSelect = {},
            )
        }
        val card = kit.monthCard(root, now.get(Calendar.YEAR), now.get(Calendar.MONTH), now.get(Calendar.DAY_OF_MONTH), grid)
        (card.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(12)
        kit.label(root, "Agenda")
        val a = kit.agendaCard(
            root, now.get(Calendar.DAY_OF_MONTH), "Nov",
            listOf(AgendaEntry("Lunch at Miyamos Cafe", "2:00 PM"), AgendaEntry("Meeting with Ali", "3:30 PM", "Room 4"), AgendaEntry("Afternoon tea at the Royal Mirage", "4:00 PM")),
            "No events", add = {},
        )
        a.contentDescription = null
    }

    private fun note(kit: UiKit, c: LinearLayout, id: Int, tone: Tone, size: Float = 13.5f, vararg args: Any) {
        c.addView(kit.text(getString(id, *args), size, tone).apply { setPadding(0, kit.dp(6), 0, 0) })
    }

    /** The battery and notification switches of the "Share with computer" step, switched off and switched on without access. */
    private fun share(kit: UiKit, root: LinearLayout, on: Boolean) {
        root.addView(DotText(this, getString(R.string.setup_title_sms), maxPitchDp = Tok.DOT_NUMERAL, maxLines = 3))
        val c = kit.panel(root, 16).also { (it.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(16) }
        kit.row(c, R.drawable.ic_battery, getString(R.string.pd_battery), getString(R.string.pd_battery_sub) + "\n" + getString(if (on) R.string.pd_state_on else R.string.pd_state_off), kit.toggle(on) {})
        if (on) {
            note(kit, c, R.string.grant_allowed, Tone.Muted, 13f, "Desk")
            kit.button(c, getString(R.string.grant_choose), BtnStyle.Tonal) {}
        }
        kit.divider(c)
        kit.row(c, R.drawable.ic_message, getString(R.string.pd_notifications), getString(R.string.pd_notifications_sub), kit.toggle(on) {})
        if (!on) {
            note(kit, c, R.string.mirror_state_turn_on, Tone.Muted)
        } else {
            note(kit, c, R.string.mirror_state_access, Tone.Warn)
            kit.button(c, getString(R.string.mirror_open_access), BtnStyle.Tonal) {}
            kit.button(c, getString(R.string.setup_open_app_settings), BtnStyle.Plain) {}
            note(kit, c, R.string.mirror_restricted_help, Tone.Muted, 13f)
            kit.button(c, getString(R.string.mirror_pick_apps) + " · " + getString(R.string.mirror_apps_none), BtnStyle.Tonal) {}
            note(kit, c, R.string.mirror_honest, Tone.Muted, 13f)
        }
    }

    /** The calls step: answer permission, the call-status switch and the dial and confirm-dial rows. */
    private fun dial(kit: UiKit, root: LinearLayout) {
        root.addView(DotText(this, getString(R.string.setup_title_answercalls), maxPitchDp = Tok.DOT_NUMERAL, maxLines = 3))
        val c = kit.panel(root, 16).also { (it.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(16) }
        kit.row(c, R.drawable.ic_call, getString(R.string.setup_calls_toggle), getString(R.string.setup_calls_toggle_sub), kit.toggle(true) {})
        kit.divider(c)
        c.addView(kit.text(getString(R.string.setup_dial_title), 16f, bold = true).apply { setPadding(0, kit.dp(14), 0, 0) })
        note(kit, c, R.string.setup_dial_off, Tone.Muted)
        kit.button(c, getString(R.string.setup_dial_allow), BtnStyle.Tonal) {}
        kit.row(c, R.drawable.ic_call, getString(R.string.setup_dial_confirm_toggle), getString(R.string.setup_dial_confirm_toggle_sub), kit.toggle(true) {})
    }

    private fun controls(kit: UiKit, root: LinearLayout) {
        root.addView(kit.dotHeader("Setup"))
        root.addView(DotStepper(this).apply { set(listOf(DotStep.Done, DotStep.Done, DotStep.Todo, DotStep.Attention, DotStep.Todo, DotStep.Todo, DotStep.Todo), 2, "Step 3 of 7") })
        val c = kit.panel(root, 16).also { (it.layoutParams as LinearLayout.LayoutParams).topMargin = kit.dp(16) }
        kit.row(c, R.drawable.ic_bell, "Notifications", "Quiet notice while connected", kit.chip("Done", Tone.Ok))
        kit.divider(c)
        kit.row(c, R.drawable.ic_call, "Phone state", "Show ringing and in-call", kit.chip("Denied", Tone.Warn))
        kit.divider(c)
        kit.row(c, R.drawable.ic_computer, "Stay connected", "Keep reachable", kit.toggle(true) {})
        kit.divider(c)
        kit.row(c, R.drawable.ic_battery, "Battery", "Allow unrestricted", kit.toggle(false) {})
        kit.button(c, "Allow", BtnStyle.Primary) {}
        kit.button(c, "Open app settings", BtnStyle.Tonal) {}
        kit.button(c, "Not now", BtnStyle.Plain) {}
        c.addView(DotProgress(this).apply { set(0.42f, "Transfer 42 percent") }, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(16) })
        kit.ui.input(c, "Name", android.text.InputType.TYPE_CLASS_TEXT, 40)
    }
}
