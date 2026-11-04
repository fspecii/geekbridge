// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import android.app.Activity
import android.app.AlertDialog
import android.content.ActivityNotFoundException
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.widget.LinearLayout
import dev.geekbridge.android.ActivityTracker
import dev.geekbridge.android.AppState
import dev.geekbridge.android.BtnStyle
import dev.geekbridge.android.GrantFeature
import dev.geekbridge.android.GrantsUi
import dev.geekbridge.android.R
import dev.geekbridge.android.Screen
import dev.geekbridge.android.Tone
import dev.geekbridge.android.Ui
import dev.geekbridge.android.UiKit

/*
 * DEBUG BUILDS ONLY, EXPERIMENT. Settings > Experimental > "Call audio over Wi-Fi (ADB helper)".
 * Explains what it does and what it risks, guides Developer options and the enable script, runs the self-test,
 * and holds the switches: the main one, the per-computer allow, and the separate "Speak (experimental)".
 */
class CallWifiActivity : Activity() {
    private lateinit var kit: UiKit
    private lateinit var screen: Screen
    private val main = Handler(Looper.getMainLooper())
    private val refresh = { main.post { render() }; Unit }
    private val tick = object : Runnable {
        override fun run() {
            render()
            main.postDelayed(this, 1500)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppState.init(this)
        ActivityTracker.install(this)
        CallWifiRuntime.init(this)
        kit = UiKit(this, Ui(this))
        screen = Screen(this, kit).also { it.install() }
        render()
    }

    override fun onResume() {
        super.onResume()
        CallWifiRuntime.addListener(refresh)
        main.postDelayed(tick, 1500)
    }

    override fun onPause() {
        super.onPause()
        CallWifiRuntime.removeListener(refresh)
        main.removeCallbacks(tick)
    }

    private fun render() {
        val root: LinearLayout = screen.root
        root.removeAllViews()
        val rt = CallWifiRuntime
        root.addView(kit.heading(getString(R.string.cw_title), 24f))
        root.addView(kit.text(getString(R.string.cw_lead), 14f, Tone.Muted))

        val warn = kit.ui.card(root, getString(R.string.cw_risks_title), accent = true)
        warn.addView(kit.text(getString(R.string.cw_risks), 14f))

        // 1. The main switch.
        val sw = kit.panel(root, 8)
        val toggle = kit.toggle(rt.enabled) { on -> if (on) confirmEnable() else rt.setEnabled(false) }
        toggle.contentDescription = getString(R.string.cw_switch)
        kit.row(sw, R.drawable.ic_call, getString(R.string.cw_switch), getString(R.string.cw_switch_sub), toggle)

        if (!rt.enabled) {
            screen.announcePage(getString(R.string.cw_title))
            screen.requestInsets()
            return
        }

        // 2. Helper: guide + status.
        val helper = kit.ui.card(root, getString(R.string.cw_helper_title))
        helper.addView(kit.text(getString(R.string.cw_helper_steps), 14f))
        helper.addView(
            kit.text(
                if (rt.helperUp) getString(R.string.cw_helper_up) else getString(R.string.cw_helper_down),
                14f, if (rt.helperUp) Tone.Ok else Tone.Warn,
            ).apply { setPadding(0, kit.dp(8), 0, 0) },
        )
        kit.button(helper, getString(R.string.cw_open_dev), BtnStyle.Tonal) {
            try {
                startActivity(Intent(Settings.ACTION_APPLICATION_DEVELOPMENT_SETTINGS))
            } catch (_: ActivityNotFoundException) {
                startActivity(Intent(Settings.ACTION_SETTINGS))
            }
        }
        helper.addView(kit.text(getString(R.string.cw_inapp_later), 12f, Tone.Muted).apply { setPadding(0, kit.dp(8), 0, 0) })

        // 3. Self-test.
        val test = kit.ui.card(root, getString(R.string.cw_test_title))
        test.addView(kit.text(getString(R.string.cw_test_body), 14f))
        kit.button(test, getString(R.string.cw_test_control), BtnStyle.Tonal, rt.helperUp) { rt.runControl() }
        kit.button(test, getString(R.string.cw_test_call), BtnStyle.Primary, rt.helperUp) { rt.runCallTest() }
        if (rt.progress.isNotEmpty()) {
            test.addView(kit.text(rt.progress, 14f, Tone.Normal).apply { setPadding(0, kit.dp(8), 0, 0) })
        }
        val v = rt.verdict
        test.addView(
            kit.text(
                getString(R.string.cw_last_result, rt.describe(v, false)),
                13f,
                when (v) { Verdict.Works -> Tone.Ok; Verdict.NotRun -> Tone.Muted; else -> Tone.Warn },
            ).apply { setPadding(0, kit.dp(8), 0, 0) },
        )

        // 3b. Speaking: beeps through each uplink route; the other person counts them.
        val st3 = kit.ui.card(root, getString(R.string.cw_speaktest_title))
        st3.addView(kit.text(getString(R.string.cw_speaktest_body), 14f))
        kit.button(st3, getString(R.string.cw_speaktest_run), BtnStyle.Tonal, rt.helperUp) { rt.runSpeakTest() }
        if (rt.uplinkSummary.isNotEmpty()) {
            st3.addView(kit.text(rt.uplinkSummary, 14f, Tone.Normal).apply { setPadding(0, kit.dp(8), 0, 0) })
        }
        if (rt.uplinkTest.isNotEmpty()) {
            for (n in 1..3) kit.button(st3, getString(R.string.cw_speaktest_heard, n), BtnStyle.Tonal) { rt.heard(n) }
            kit.button(st3, getString(R.string.cw_speaktest_none), BtnStyle.Tonal) { rt.heard(0) }
        }

        // 4. Which computers.
        val who = kit.ui.card(root, getString(R.string.cw_who_title))
        who.addView(kit.text(getString(R.string.cw_who_body), 14f))
        GrantsUi.add(this, kit, who, GrantFeature.CallAudio) { render() }

        // 5. Speak back: a separate opt-in.
        val speak = kit.panel(root, 8)
        val canSpeak = v == Verdict.Works && UplinkRoutes.valid(rt.uplink)
        val st = kit.toggle(rt.speakOptIn && canSpeak) { on -> rt.setSpeak(on) }
        st.isEnabled = canSpeak
        st.contentDescription = getString(R.string.cw_speak)
        kit.row(
            speak, R.drawable.ic_person, getString(R.string.cw_speak),
            getString(if (canSpeak) R.string.cw_speak_sub else R.string.cw_speak_unavailable), st,
        )

        // 6. A live session.
        rt.sharingWith?.let { who2 ->
            val live = kit.ui.card(root, getString(R.string.cw_live_title), accent = true)
            live.addView(kit.text(getString(R.string.cw_live, who2), 14f))
            if (rt.speaking) live.addView(kit.text(getString(R.string.cw_live_speaking), 14f, Tone.Warn))
            if (rt.injectNote.isNotEmpty()) live.addView(kit.text(rt.injectNote, 12f, Tone.Muted))
            kit.button(live, getString(R.string.cw_stop), BtnStyle.Primary) { rt.endSession() }
        }
        screen.announcePage(getString(R.string.cw_title))
        screen.requestInsets()
    }

    private fun confirmEnable() {
        AlertDialog.Builder(this)
            .setTitle(R.string.cw_confirm_title)
            .setMessage(R.string.cw_confirm_body)
            .setPositiveButton(R.string.cw_confirm_yes) { _, _ -> CallWifiRuntime.setEnabled(true); render() }
            .setNegativeButton(android.R.string.cancel) { _, _ -> render() }
            .setOnCancelListener { render() }
            .show()
    }
}
