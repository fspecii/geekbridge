// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/*
 * DEBUG BUILDS ONLY, EXPERIMENT. `scripts/callaudio-enable.sh` asks the app where the helper should connect (the app listens on
 * loopback, the helper connects out; an app cannot connect to a shell socket under SELinux). Only the shell may send it (the manifest requires the DUMP permission, which adb holds).
 * Result codes: 1 accepted, 2 refused because the Experimental switch is off, 0 not understood. Also adb-only test
 * hooks (ENABLE, SELFTEST, STATUS) for the emulator e2e script.
 */
class CallWifiReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        CallWifiRuntime.init(context)
        when (intent.action) {
            // The enable script asks where the helper should connect: data is "port=NNNN token=XXXX".
            "dev.geekbridge.callwifi.OPEN" -> {
                val r = CallWifiRuntime.openListener()
                if (r == null) {
                    resultCode = 2
                } else {
                    resultCode = 1
                    resultData = "port=${r.first} token=${r.second}"
                }
            }
            // Test hooks for scripts/e2e/call-audio-emulator.sh (adb only, like DebugControlReceiver).
            "dev.geekbridge.callwifi.ENABLE" -> {
                CallWifiRuntime.setEnabled(intent.getBooleanExtra("on", true))
                resultCode = 1
            }
            "dev.geekbridge.callwifi.SELFTEST" -> {
                if (intent.getStringExtra("step") == "call") CallWifiRuntime.runCallTest() else CallWifiRuntime.runControl()
                resultCode = 1
            }
            // Linked-emulator e2e only: allow (or withdraw) the experiment for one paired computer, as the Settings row does.
            "dev.geekbridge.callwifi.GRANT" -> {
                val device = intent.getStringExtra("device").orEmpty()
                if (device.isEmpty()) {
                    resultCode = 0
                } else {
                    dev.geekbridge.android.ComputerGrantsRuntime.init(context)
                    dev.geekbridge.android.ComputerGrantsRuntime.allow(device, dev.geekbridge.android.GrantFeature.CallAudio, intent.getBooleanExtra("on", true))
                    resultCode = 1
                }
            }
            // Linked-emulator e2e only: stand in for a finished self-test, because the emulator's call sources cannot
            // give a real verdict (they sound with no call). Source TONE is the helper's synthetic far party.
            "dev.geekbridge.callwifi.CONFIG" -> {
                CallWifiRuntime.setTestConfig(intent.getStringExtra("verdict"), intent.getStringExtra("source"))
                resultCode = 1
            }
            "dev.geekbridge.callwifi.STATUS" -> {
                // Numbers and words only, never audio or names.
                resultCode = 1
                resultData = "{\"enabled\":${CallWifiRuntime.enabled},\"helper\":${CallWifiRuntime.helperUp}," +
                    "\"verdict\":\"${CallWifiRuntime.verdict.wire}\",\"uplink\":\"${CallWifiRuntime.uplink.take(24)}\"," +
                    "\"progress\":\"${CallWifiRuntime.progress.filter { it.isLetterOrDigit() || it == ' ' || it == '.' }.take(120)}\",\"sharing\":${CallWifiRuntime.sharingWith != null},\"speaking\":${CallWifiRuntime.speaking}}"
            }
            "dev.geekbridge.callwifi.STOP" -> {
                CallWifiRuntime.endSession()
                CallWifiRuntime.disconnect()
                resultCode = 1
            }
        }
    }
}

/** The Stop button of the "Call audio is being shared" notification. Not exported: only the app's own PendingIntent. */
class CallWifiStopReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        CallWifiRuntime.init(context)
        CallWifiRuntime.endSession()
    }
}
