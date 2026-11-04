// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.Manifest
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.content.Context
import android.content.pm.PackageManager
import android.os.Handler
import android.os.Looper

/**
 * What the phone sees on the Bluetooth hands-free profile, for the "Call audio on your computer" part of the
 * Answer calls step. The phone is the audio gateway; the computer is the hands-free unit. Android gives apps no
 * call audio, so the only way for the voice to reach the computer is the computer being a connected
 * hands-free device of this phone (docs/call-audio-options.md).
 */
enum class HfpPhase {
    /** This phone has no Bluetooth radio. */
    NoBluetooth,

    /** Android 12+ and BLUETOOTH_CONNECT is not granted: reading connected devices is not allowed yet. */
    NeedsPermission,

    /** Bluetooth is switched off on the phone. */
    BluetoothOff,

    /** Bluetooth is on and no hands-free device is connected. */
    NoneConnected,

    /** At least one hands-free device (headset, car, or a computer) is connected. */
    Connected,

    /** Android did not answer in time. */
    Unknown,
}

data class HfpView(val phase: HfpPhase, val names: List<String> = emptyList())

object CallAudioModel {
    /** BLUETOOTH_CONNECT is a runtime permission from Android 12 (API 31); before that, install-time BLUETOOTH covers it. */
    fun needsRuntimePermission(sdk: Int): Boolean = sdk >= 31

    /**
     * The state to show before the profile proxy has answered, or null when it must be asked.
     * No scanning is involved at any point: only already connected devices are listed.
     */
    fun preflight(hasBluetooth: Boolean, sdk: Int, permissionGranted: Boolean, enabled: Boolean): HfpPhase? = when {
        !hasBluetooth -> HfpPhase.NoBluetooth
        needsRuntimePermission(sdk) && !permissionGranted -> HfpPhase.NeedsPermission
        !enabled -> HfpPhase.BluetoothOff
        else -> null
    }

    /** Names are shown on the phone's own screen only; blank names become a generic label by the caller. */
    fun result(connected: List<String?>?): HfpView = when {
        connected == null -> HfpView(HfpPhase.Unknown)
        connected.isEmpty() -> HfpView(HfpPhase.NoneConnected)
        else -> HfpView(HfpPhase.Connected, connected.map { it?.trim().orEmpty() })
    }
}

object CallAudioBluetooth {
    private const val TIMEOUT_MS = 3000L

    fun hasPermission(ctx: Context): Boolean =
        !CallAudioModel.needsRuntimePermission(android.os.Build.VERSION.SDK_INT) ||
            ctx.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED

    /** Calls [done] once, on the main thread. Never scans, never pairs, never connects. */
    fun read(ctx: Context, done: (HfpView) -> Unit) {
        val app = ctx.applicationContext
        val adapter = app.getSystemService(BluetoothManager::class.java)?.adapter
        val sdk = android.os.Build.VERSION.SDK_INT
        val early = CallAudioModel.preflight(
            hasBluetooth = adapter != null && app.packageManager.hasSystemFeature(PackageManager.FEATURE_BLUETOOTH),
            sdk = sdk,
            permissionGranted = hasPermission(app),
            enabled = adapter?.isEnabled == true,
        )
        if (early != null || adapter == null) {
            done(HfpView(early ?: HfpPhase.NoBluetooth))
            return
        }
        val main = Handler(Looper.getMainLooper())
        var finished = false
        fun finish(v: HfpView) {
            if (finished) return
            finished = true
            done(v)
        }
        val listener = object : BluetoothProfile.ServiceListener {
            override fun onServiceConnected(profile: Int, proxy: BluetoothProfile) {
                val names = try {
                    proxy.connectedDevices.map { d -> try { d.name } catch (_: SecurityException) { null } }
                } catch (_: SecurityException) {
                    null
                }
                try { adapter.closeProfileProxy(profile, proxy) } catch (_: RuntimeException) {}
                finish(CallAudioModel.result(names))
            }

            override fun onServiceDisconnected(profile: Int) {}
        }
        val asked = try {
            adapter.getProfileProxy(app, listener, BluetoothProfile.HEADSET)
        } catch (_: SecurityException) {
            false
        }
        if (!asked) finish(HfpView(HfpPhase.Unknown)) else main.postDelayed({ finish(HfpView(HfpPhase.Unknown)) }, TIMEOUT_MS)
    }
}
