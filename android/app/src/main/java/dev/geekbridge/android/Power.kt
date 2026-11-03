// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.app.ActivityManager
import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings

/** Whether Android may freeze or kill the background connection to the computer. */
enum class BatteryState {
    /** Excluded from battery optimisation: the connection can stay up. */
    Unrestricted,

    /** Subject to Doze / app standby: the connection can be delayed or dropped. */
    Optimised,

    /** The user put the app in "Restricted" background mode: the connection will not run in the background. */
    BackgroundRestricted,
}

/** Pure classification of the power state, so every combination is unit-tested. */
object PowerRules {
    /** `UsageStatsManager.STANDBY_BUCKET_RESTRICTED`: the bucket Android (or `am set-standby-bucket`) gives an app it throttles hardest. */
    const val BUCKET_RESTRICTED = 45

    /**
     * [backgroundRestricted] is the user's "Restricted" background mode (`RUN_ANY_IN_BACKGROUND` denied). A
     * Doze-exempt app is never held back by its standby bucket, so the bucket only counts for an app that is
     * not exempt ([standbyBucket] is null when unknown).
     */
    fun classify(backgroundRestricted: Boolean, ignoringOptimisations: Boolean, standbyBucket: Int?): BatteryState = when {
        backgroundRestricted -> BatteryState.BackgroundRestricted
        ignoringOptimisations -> BatteryState.Unrestricted
        standbyBucket != null && standbyBucket >= BUCKET_RESTRICTED -> BatteryState.BackgroundRestricted
        else -> BatteryState.Optimised
    }
}

/** Doze: the heartbeat timer's clock stops while the CPU sleeps, so a maintenance window is used to ping at once. */
object DozeRules {
    /** [idle] is `PowerManager.isDeviceIdleMode` after a change broadcast: false is a maintenance window or the end of Doze. */
    fun pingNow(idle: Boolean): Boolean = !idle
}

object PowerChecks {
    fun batteryState(ctx: Context): BatteryState {
        val am = ctx.getSystemService(ActivityManager::class.java)
        val restricted = Build.VERSION.SDK_INT >= 28 && am?.isBackgroundRestricted == true
        val pm = ctx.getSystemService(PowerManager::class.java)
        val ignoring = pm?.isIgnoringBatteryOptimizations(ctx.packageName) == true
        val bucket = if (Build.VERSION.SDK_INT >= 28) {
            try { ctx.getSystemService(android.app.usage.UsageStatsManager::class.java)?.appStandbyBucket } catch (_: RuntimeException) { null }
        } else null
        return PowerRules.classify(restricted, ignoring, bucket)
    }

    /**
     * The per-app battery screen ("Unrestricted / Optimized / Restricted"), where a restriction is lifted. Android 14+
     * has an intent for it; earlier versions and some ROMs do not, so this ends on the app details page.
     */
    fun openBatteryUsage(activity: Activity): LaunchResult = OemLauncher.launch(
        listOf(IntentSpec("android.settings.APP_BATTERY_SETTINGS", data = "package:${activity.packageName}")),
        { tryStart(activity, it) },
        { openAppSettings(activity) },
    )

    /**
     * Asks Android to exclude this app from battery optimisation. Falls back to the list of optimisation
     * settings and finally to this app's settings page, so the user always lands somewhere useful.
     */
    fun requestUnrestricted(activity: Activity): LaunchResult {
        val pkg = activity.packageName
        val attempts = listOf(
            IntentSpec(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, data = "package:$pkg"),
            IntentSpec(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS),
        )
        return OemLauncher.launch(attempts, { tryStart(activity, it) }, { openAppSettings(activity) })
    }

    fun openAppSettings(activity: Activity) {
        try {
            activity.startActivity(
                Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", activity.packageName, null)),
            )
        } catch (_: RuntimeException) {
            // No settings app, or a ROM that refuses the intent: nothing more to fall back to.
        }
    }

    fun openOemSettings(activity: Activity, oem: Oem): LaunchResult =
        OemLauncher.launch(OemIntents.candidates(oem), { tryStart(activity, it) }, { openAppSettings(activity) })

    private fun tryStart(activity: Activity, spec: IntentSpec): Boolean {
        val intent = Intent().apply {
            spec.action?.let { action = it }
            spec.pkg?.let { pkg -> spec.cls?.let { component = ComponentName(pkg, it) } }
            spec.data?.let { data = Uri.parse(it) }
            // Several OEM settings screens read the package they should highlight from these extras.
            putExtra("package_name", activity.packageName)
            putExtra("packageName", activity.packageName)
        }
        return try {
            activity.startActivity(intent)
            true
        } catch (_: ActivityNotFoundException) {
            false
        } catch (_: RuntimeException) {
            false // SecurityException (not exported / permission-protected on this ROM version) or a ROM-specific crash
        }
    }
}

enum class Oem(val label: String, val hasExtraSwitch: Boolean = true) {
    Xiaomi("Xiaomi / Redmi / POCO"),
    Samsung("Samsung"),
    Huawei("Huawei / Honor"),
    OnePlus("OnePlus"),
    Oppo("OPPO / Realme"),
    Vivo("vivo / iQOO"),

    /** Near-stock Android: the system battery exemption is the only switch, so there is no vendor screen to open. */
    Nothing("Nothing / CMF", hasExtraSwitch = false),

    /** Pixel and other Google-built phones: no vendor layer, but the standby bucket and Adaptive Battery apply. */
    Pixel("Pixel / Google", hasExtraSwitch = false),
    Other("your phone"),
}

object OemDetector {
    fun detect(manufacturer: String?, brand: String? = null): Oem {
        val m = "${manufacturer.orEmpty()} ${brand.orEmpty()}".lowercase()
        return when {
            "xiaomi" in m || "redmi" in m || "poco" in m -> Oem.Xiaomi
            "samsung" in m -> Oem.Samsung
            "huawei" in m || "honor" in m -> Oem.Huawei
            "oneplus" in m -> Oem.OnePlus
            "oppo" in m || "realme" in m -> Oem.Oppo
            "vivo" in m || "iqoo" in m -> Oem.Vivo
            "nothing" in m || "cmf" in m -> Oem.Nothing
            "google" in m || "pixel" in m -> Oem.Pixel
            else -> Oem.Other
        }
    }
}

/** One settings screen to try. [pkg]/[cls] are an explicit component; [action]/[data] a plain intent. */
data class IntentSpec(val action: String? = null, val pkg: String? = null, val cls: String? = null, val data: String? = null)

enum class LaunchResult { Opened, OpenedFallback }

object OemLauncher {
    /** Tries each candidate in order; if none opens, runs [fallback] (app settings). Pure so it is unit-tested. */
    fun launch(candidates: List<IntentSpec>, tryStart: (IntentSpec) -> Boolean, fallback: () -> Unit): LaunchResult {
        for (c in candidates) if (tryStart(c)) return LaunchResult.Opened
        fallback()
        return LaunchResult.OpenedFallback
    }
}

/**
 * Known vendor autostart / background-activity screens. They are undocumented and vary by ROM version, so each
 * one is only a best-effort attempt: a missing or protected one is skipped, then the app settings page opens.
 */
object OemIntents {
    fun candidates(oem: Oem): List<IntentSpec> = when (oem) {
        Oem.Xiaomi -> listOf(
            IntentSpec(pkg = "com.miui.securitycenter", cls = "com.miui.permcenter.autostart.AutoStartManagementActivity"),
            IntentSpec(pkg = "com.miui.powerkeeper", cls = "com.miui.powerkeeper.ui.HiddenAppsConfigActivity"),
        )
        Oem.Samsung -> listOf(
            IntentSpec(pkg = "com.samsung.android.lool", cls = "com.samsung.android.sm.battery.ui.BatteryActivity"),
            IntentSpec(pkg = "com.samsung.android.lool", cls = "com.samsung.android.sm.ui.battery.BatteryActivity"),
            IntentSpec(pkg = "com.samsung.android.sm_cn", cls = "com.samsung.android.sm.ui.battery.BatteryActivity"),
        )
        Oem.Huawei -> listOf(
            IntentSpec(pkg = "com.huawei.systemmanager", cls = "com.huawei.systemmanager.startupmgr.ui.StartupNormalAppListActivity"),
            IntentSpec(pkg = "com.huawei.systemmanager", cls = "com.huawei.systemmanager.optimize.process.ProtectActivity"),
        )
        Oem.OnePlus -> listOf(
            IntentSpec(pkg = "com.oneplus.security", cls = "com.oneplus.security.chainlaunch.view.ChainLaunchAppListActivity"),
            IntentSpec(pkg = "com.coloros.safecenter", cls = "com.coloros.safecenter.startupapp.StartupAppListActivity"),
        )
        Oem.Oppo -> listOf(
            IntentSpec(pkg = "com.coloros.safecenter", cls = "com.coloros.safecenter.startupapp.StartupAppListActivity"),
            IntentSpec(pkg = "com.oppo.safe", cls = "com.oppo.safe.permission.startup.StartupAppListActivity"),
        )
        Oem.Vivo -> listOf(
            IntentSpec(pkg = "com.vivo.permissionmanager", cls = "com.vivo.permissionmanager.activity.BgStartUpManagerActivity"),
            IntentSpec(pkg = "com.iqoo.secure", cls = "com.iqoo.secure.ui.phoneoptimize.AddWhiteListActivity"),
        )
        Oem.Nothing, Oem.Pixel, Oem.Other -> emptyList()
    }
}

/** Plain-language steps for the settings the app cannot change or detect itself (string-array resources). */
object OemGuides {
    /** The string-array of steps for [oem], or null when there is no guide. Resolve with `resources.getStringArray`. */
    fun stepsRes(oem: Oem): Int? = when (oem) {
        Oem.Xiaomi -> R.array.power_steps_xiaomi
        Oem.Samsung -> R.array.power_steps_samsung
        Oem.Huawei -> R.array.power_steps_huawei
        Oem.OnePlus -> R.array.power_steps_oneplus
        Oem.Oppo -> R.array.power_steps_oppo
        Oem.Vivo -> R.array.power_steps_vivo
        Oem.Nothing -> R.array.power_steps_nothing
        Oem.Pixel -> R.array.power_steps_pixel
        Oem.Other -> null
    }
}
