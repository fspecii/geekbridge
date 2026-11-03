// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.Manifest
import android.app.Activity
import android.content.pm.PackageManager
import android.content.pm.PermissionInfo
import android.os.Build

/** The SMS permissions as the user knows them; Android and Samsung can grant each one separately. */
enum class SmsPerm(val permission: String, val label: Int) {
    Read(Manifest.permission.READ_SMS, R.string.sms_perm_read),
    Send(Manifest.permission.SEND_SMS, R.string.sms_perm_send),
    Receive(Manifest.permission.RECEIVE_SMS, R.string.sms_perm_receive),
}

/** What stands between the user and "the computer can use Messages", in the order they must be fixed. */
enum class SmsStep { Ready, TurnOn, Allow, OpenSettings, AllowRestricted, PairComputer, ChooseComputer }

data class SmsReadiness(val step: SmsStep, val missing: List<SmsPerm> = emptyList()) {
    val ready: Boolean get() = step == SmsStep.Ready
}

/** Pure decision table behind the Messages tile and the Messages step; tested on the JVM. */
object SmsReadinessLogic {
    /**
     * [status] is the Android state of each permission. [restrictedCapable] says Android treats SMS as a hard-restricted
     * permission for this install (Android 13+, sideloaded), so a permanent denial means "Allow restricted settings".
     * [pairedCount] / [allowedCount] are the paired computers and how many of them may use Messages.
     */
    fun evaluate(
        switchOn: Boolean,
        status: Map<SmsPerm, PermissionStatus>,
        restrictedCapable: Boolean,
        pairedCount: Int,
        allowedCount: Int,
    ): SmsReadiness {
        if (!switchOn) return SmsReadiness(SmsStep.TurnOn)
        val missing = SmsPerm.entries.filter { status[it] != PermissionStatus.Granted }
        if (missing.isNotEmpty()) {
            val blocked = missing.any { status[it] == PermissionStatus.Blocked }
            val step = when {
                !blocked -> SmsStep.Allow
                restrictedCapable -> SmsStep.AllowRestricted
                else -> SmsStep.OpenSettings
            }
            return SmsReadiness(step, missing)
        }
        return when {
            pairedCount == 0 -> SmsReadiness(SmsStep.PairComputer)
            allowedCount == 0 -> SmsReadiness(SmsStep.ChooseComputer)
            else -> SmsReadiness(SmsStep.Ready)
        }
    }

    fun message(r: SmsReadiness, s: Strings): String {
        val list = r.missing.joinToString(s.get(R.string.list_separator)) { s.get(it.label) }
        return when (r.step) {
            SmsStep.Ready -> s.get(R.string.sms_state_ready)
            SmsStep.TurnOn -> s.get(R.string.sms_state_turn_on)
            SmsStep.Allow -> s.get(R.string.sms_state_allow, list)
            SmsStep.OpenSettings -> s.get(R.string.sms_state_settings, list)
            SmsStep.AllowRestricted -> s.get(R.string.sms_state_restricted, list)
            SmsStep.PairComputer -> s.get(R.string.sms_state_pair)
            SmsStep.ChooseComputer -> s.get(R.string.sms_state_choose)
        }
    }

    /** Short word for the Home tile. */
    fun tileWord(r: SmsReadiness, s: Strings): String = s.get(
        when (r.step) {
            SmsStep.Ready -> R.string.word_on
            SmsStep.TurnOn -> R.string.word_off
            else -> R.string.word_needs_setup
        },
    )
}

/** The SIM chooser of the Messages step; its permission (READ_PHONE_STATE) is separate from the three SMS ones. */
enum class SimChoiceState { NotNeeded, Ask, Blocked, Granted }

object SmsSimChoice {
    /**
     * Only a phone with two or more SIM slots has a choice to make, and only once Messages itself works. Without
     * [phoneState] the phone reports no SIMs, the computer hides the chooser and every text uses the default SIM.
     */
    fun state(switchOn: Boolean, smsPermsGranted: Boolean, simSlots: Int, phoneState: PermissionStatus): SimChoiceState = when {
        !switchOn || !smsPermsGranted || simSlots < 2 -> SimChoiceState.NotNeeded
        phoneState == PermissionStatus.Granted -> SimChoiceState.Granted
        phoneState == PermissionStatus.Blocked -> SimChoiceState.Blocked
        else -> SimChoiceState.Ask
    }

    fun message(state: SimChoiceState, s: Strings): String = when (state) {
        SimChoiceState.Ask -> s.get(R.string.sms_sim_ask)
        SimChoiceState.Blocked -> s.get(R.string.sms_sim_blocked)
        else -> ""
    }
}

/** Whether "Allow restricted settings" can be what blocks SMS; pure so the wording choice is tested. */
object SmsRestriction {
    /** Stores Android trusts: an app installed by one is never restricted. Everything else (adb, a file manager, a browser) is. */
    private val trustedStores = setOf("com.android.vending", "com.sec.android.app.samsungapps")

    fun likely(sdk: Int, permissionIsHardRestricted: Boolean, installer: String?): Boolean =
        sdk >= 33 && permissionIsHardRestricted && installer !in trustedStores
}

/** Reads the real phone state for [SmsReadinessLogic]. */
object SmsReadinessAndroid {
    fun status(a: Activity, p: SmsPerm): PermissionStatus = PermissionStatusLogic.compute(
        granted = a.checkSelfPermission(p.permission) == PackageManager.PERMISSION_GRANTED,
        askedBefore = AppState.wasAsked(p.permission),
        shouldShowRationale = a.shouldShowRequestPermissionRationale(p.permission),
    )

    /** Android 13+ treats SMS as hard-restricted for apps not installed by a store until "Allow restricted settings". */
    fun restrictedCapable(a: Activity): Boolean {
        val flagged = try {
            (a.packageManager.getPermissionInfo(Manifest.permission.READ_SMS, 0).flags and PermissionInfo.FLAG_HARD_RESTRICTED) != 0
        } catch (_: PackageManager.NameNotFoundException) {
            false
        }
        val installer = try {
            if (Build.VERSION.SDK_INT >= 30) a.packageManager.getInstallSourceInfo(a.packageName).installingPackageName else null
        } catch (_: PackageManager.NameNotFoundException) {
            null
        }
        return SmsRestriction.likely(Build.VERSION.SDK_INT, flagged, installer)
    }

    /** SIM slots, which Android tells any app without a permission. */
    fun simSlots(a: Activity): Int = try {
        val t = a.getSystemService(android.telephony.TelephonyManager::class.java)
        if (t == null) 0 else if (Build.VERSION.SDK_INT >= 30) t.activeModemCount else @Suppress("DEPRECATION") t.phoneCount
    } catch (_: RuntimeException) {
        0
    }

    fun simChoice(a: Activity): SimChoiceState = SmsSimChoice.state(
        switchOn = PhoneDataRuntime.settings.sms,
        smsPermsGranted = SmsPerm.entries.all { status(a, it) == PermissionStatus.Granted },
        simSlots = simSlots(a),
        phoneState = PermissionStatusLogic.compute(
            granted = a.checkSelfPermission(Manifest.permission.READ_PHONE_STATE) == PackageManager.PERMISSION_GRANTED,
            askedBefore = AppState.wasAsked(Manifest.permission.READ_PHONE_STATE),
            shouldShowRationale = a.shouldShowRequestPermissionRationale(Manifest.permission.READ_PHONE_STATE),
        ),
    )

    fun evaluate(a: Activity): SmsReadiness {
        ComputerGrantsRuntime.init(a)
        val peers = AppState.pairedPeers
        val allowed = ComputerGrantsRuntime.grants.devicesFor(GrantFeature.Sms)
        return SmsReadinessLogic.evaluate(
            switchOn = PhoneDataRuntime.settings.sms,
            status = SmsPerm.entries.associateWith { status(a, it) },
            restrictedCapable = restrictedCapable(a),
            pairedCount = peers.size,
            allowedCount = peers.count { it.first in allowed },
        )
    }
}

/** Capabilities are declared in the identity packet at connect time, so any change while linked needs a reconnect. */
object CapabilityRedeclare {
    fun needed(declared: ActiveFeatures, now: ActiveFeatures) = now != declared

    enum class Action { None, Reconnect, Observe }

    /**
     * What to do when a permission or setting changed. A link that is not up yet needs nothing now, because
     * [driftAtLinkUp] is checked the moment it comes up; a link that is up reconnects when what it declared differs.
     */
    fun onChange(declared: ActiveFeatures, now: ActiveFeatures, linkRunning: Boolean): Action = when {
        !linkRunning -> Action.None
        needed(declared, now) -> Action.Reconnect
        else -> Action.Observe
    }

    /**
     * The link came up with whatever was declared when it started; if the user answered a permission dialog in
     * between, the computer was told the old set and must hear the new one.
     */
    fun driftAtLinkUp(declared: ActiveFeatures, now: ActiveFeatures) = needed(declared, now)

    /** Clipboard sync was switched on while a link that did not declare it is up: the computer must hear it. */
    fun clipboardNeedsReconnect(declaredEnabled: Boolean, nowEnabled: Boolean, linkRunning: Boolean) =
        linkRunning && nowEnabled && !declaredEnabled
}
