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

/** Reads the real phone state for [SmsReadinessLogic]. */
object SmsReadinessAndroid {
    fun status(a: Activity, p: SmsPerm): PermissionStatus = PermissionStatusLogic.compute(
        granted = a.checkSelfPermission(p.permission) == PackageManager.PERMISSION_GRANTED,
        askedBefore = AppState.wasAsked(p.permission),
        shouldShowRationale = a.shouldShowRequestPermissionRationale(p.permission),
    )

    /** Android 13+ treats SMS as hard-restricted for apps not installed by a store until "Allow restricted settings". */
    fun restrictedCapable(a: Activity): Boolean {
        if (Build.VERSION.SDK_INT < 33) return false
        return try {
            (a.packageManager.getPermissionInfo(Manifest.permission.READ_SMS, 0).flags and PermissionInfo.FLAG_HARD_RESTRICTED) != 0
        } catch (_: PackageManager.NameNotFoundException) {
            false
        }
    }

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
}
