// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build

/**
 * The text behind Diagnostics -> "Copy report". It holds versions, capability states and counts only:
 * no computer or contact names, phone numbers, IP addresses, certificates, clipboard or file content.
 */
object DiagnosticsReport {
    private val PERMISSIONS = listOf(
        "contacts" to Manifest.permission.READ_CONTACTS,
        "calendar" to Manifest.permission.READ_CALENDAR,
        "phone state" to Manifest.permission.READ_PHONE_STATE,
        "answer calls" to Manifest.permission.ANSWER_PHONE_CALLS,
        "notifications" to Manifest.permission.POST_NOTIFICATIONS,
    )

    fun build(ctx: Context, coreState: String, lastIssue: String?): String {
        fun granted(permission: String) =
            ctx.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED
        val battery = PowerChecks.batteryState(ctx).name
        val oem = OemDetector.detect(Build.MANUFACTURER, Build.BRAND).label
        return buildString {
            appendLine("GeekBridge Android support report (no names, numbers, addresses or content)")
            appendLine("app: ${BuildConfig.VERSION_NAME} (code ${BuildConfig.VERSION_CODE})")
            appendLine("phone: ${Build.MANUFACTURER} ${Build.MODEL}, Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})")
            appendLine("licence: GPL-3.0-or-later (source: https://github.com/fspecii/geekbridge)")
            appendLine("vendor skin: $oem")
            appendLine("native core: ${scrub(coreState)}")
            appendLine("paired computers: ${AppState.pairedCount}")
            appendLine("stay connected: ${AppState.keepConnected}")
            appendLine("background service: ${if (LinkService.isRunning) "running" else "not running"}${AppState.serviceError?.let { " (last start failed: " + scrub(it) + ")" }.orEmpty()}")
            appendLine("setup finished: ${AppState.setupFinished}")
            appendLine("saved data: ${StateMigrations.diagnostics()?.let(::scrub) ?: "ok"}${BackedBlob.lastNote?.let { " / " + scrub(it) }.orEmpty()}")
            appendLine("battery: $battery")
            appendLine("permissions: " + PERMISSIONS.joinToString(", ") { (label, p) -> "$label=" + if (granted(p)) "granted" else "no" })
            appendLine("last issue: ${lastIssue?.let(::scrub) ?: "none"}")
            appendLine("last crash: " + (CrashNote.read(ctx)?.let { "\n" + it.trimEnd() } ?: "none"))
        }
    }

    /** Drops anything that looks like an address, key or phone number and bounds the length. */
    fun scrub(text: String): String =
        text.split(Regex("\\s+")).filter { it.isNotEmpty() }.take(40).joinToString(" ") { word ->
            val digits = word.count { it.isDigit() }
            val hex = word.count { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }
            val ipLike = word.count { it == '.' } == 3 && digits >= 4 || word.count { it == ':' } >= 2 && hex >= 4
            val keyLike = word.length >= 16 && hex * 10 >= word.length * 9
            if (ipLike || keyLike || digits >= 6) "<redacted>" else word
        }.take(160)
}
