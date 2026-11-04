// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Debug builds only: flips the phone's opt-in switches without driving the UI, for scripts/e2e-debug-diag.sh.
 *
 *   adb shell am broadcast -n dev.geekbridge.android/.DebugControlReceiver -a dev.geekbridge.debug.SET \
 *       --es what sms|contacts|calendar|calendar_edit|calls|stream|bind_wifi --ez on true
 *   adb shell am broadcast ... --es what grant --es computer DEVICE_ID --es feature sms --ez on true
 *
 * `call_audio_experiment` arms the hidden VOICE_DOWNLINK probe and `call_audio_probe` runs it for 5 s (CallAudioProbe).
 * `grant` is the per-computer switch (ComputerGrantsRuntime); the others are the app-wide switches.
 */
class DebugControlReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val what = intent.getStringExtra("what") ?: return
        val on = intent.getBooleanExtra("on", false)
        AppState.init(context)
        // The runtimes expect the main thread (this is it).
        when (what) {
            "sms" -> PhoneDataRuntime.setSms(on)
            "contacts" -> PhoneDataRuntime.setContacts(on)
            "calendar" -> PhoneDataRuntime.setCalendar(on)
            "calendar_edit" -> PhoneDataRuntime.setCalendarEdit(on)
            "calls" -> CallControlRuntime.setEnabled(on)
            "stream" -> DebugDiagnostics.setStreaming(context, on)
            "call_audio_experiment" -> CallAudioProbe.setEnabled(context, on)
            "call_audio_probe" -> CallAudioProbe.runAsync(context)
            "bind_wifi" -> DebugDiagnostics.setBindWifi(on)
            "grant" -> {
                val computer = intent.getStringExtra("computer") ?: return
                val feature = GrantFeature.entries.firstOrNull { it.key == intent.getStringExtra("feature") } ?: return
                ComputerGrantsRuntime.allow(computer, feature, on)
            }
            else -> return
        }
        GbLog.i("debug", "control: $what=$on")
    }
}
