// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.os.Build

enum class StepId { Pair, Notifications, Battery, Contacts, Calendar, PhoneState, AnswerCalls, CallerNumber, Sms }

/** What the user (and Android) have done about one runtime permission. */
enum class PermissionStatus { Granted, NotAsked, Denied, Blocked }

object PermissionStatusLogic {
    /**
     * [askedBefore] is persisted (a denial must survive process death), because Android only says "do not ask
     * again" indirectly: not granted + asked before + no rationale = the dialog will no longer appear.
     */
    fun compute(granted: Boolean, askedBefore: Boolean, shouldShowRationale: Boolean): PermissionStatus = when {
        granted -> PermissionStatus.Granted
        !askedBefore -> PermissionStatus.NotAsked
        shouldShowRationale -> PermissionStatus.Denied
        else -> PermissionStatus.Blocked
    }
}

enum class StepStatus {
    /** Finished and verified on this phone. */
    Done,

    /** Not done yet; the step button will do it. */
    Todo,

    /** Asked and denied; can be asked again. */
    Denied,

    /** Asked and denied for good; only app settings can change it. */
    Blocked,

    /** Nothing to do here (informational), e.g. SMS access that Android restricts. */
    Info,
}

data class StepInfo(val id: StepId, val status: StepStatus, val required: Boolean, val detail: String)

/** Everything the setup list is computed from, so the model is testable without a device. */
data class SetupInputs(
    val sdk: Int,
    val pairedCount: Int,
    val notifications: PermissionStatus,
    val contacts: PermissionStatus,
    val calendar: PermissionStatus,
    val phoneState: PermissionStatus,
    val battery: BatteryState,
    val oemAcknowledged: Boolean,
    val oem: Oem,
    /** Steps that were Done at least once before (so a later regression is a revocation, not a first run). */
    val wasDone: Set<StepId> = emptySet(),
    /** ANSWER_PHONE_CALLS: lets a paired computer answer, decline or end calls (API 26+). */
    val answerCalls: PermissionStatus = PermissionStatus.NotAsked,
    /** READ_CALL_LOG: the only way Android hands this app the caller's number. */
    val callLog: PermissionStatus = PermissionStatus.NotAsked,
)

object SetupModel {
    fun steps(s: Strings, i: SetupInputs): List<StepInfo> {
        val out = ArrayList<StepInfo>()
        out += StepInfo(
            StepId.Pair,
            if (i.pairedCount > 0) StepStatus.Done else StepStatus.Todo,
            required = true,
            detail = if (i.pairedCount > 0) s.plural(R.plurals.setup_paired_count, i.pairedCount, i.pairedCount) else s.get(R.string.setup_detail_none_paired),
        )
        out += if (i.sdk >= 33) {
            permissionStep(s, StepId.Notifications, i.notifications, required = true)
        } else {
            StepInfo(StepId.Notifications, StepStatus.Done, true, s.get(R.string.setup_detail_notif_na))
        }
        // The required battery step comes before the optional ones: someone who taps "Skip" through six optional pages
        // must not reach the end without having been asked the one thing that keeps the connection alive.
        out += batteryStep(s, i)
        out += permissionStep(s, StepId.Contacts, i.contacts, required = false)
        out += permissionStep(s, StepId.Calendar, i.calendar, required = false)
        out += permissionStep(s, StepId.PhoneState, i.phoneState, required = false)
        out += permissionStep(s, StepId.AnswerCalls, i.answerCalls, required = false)
        out += permissionStep(s, StepId.CallerNumber, i.callLog, required = false)
        out += StepInfo(
            StepId.Sms, StepStatus.Info, required = false,
            detail = s.get(R.string.setup_detail_sms),
        )
        return out
    }

    private fun permissionStep(s: Strings, id: StepId, p: PermissionStatus, required: Boolean): StepInfo = when (p) {
        PermissionStatus.Granted -> StepInfo(id, StepStatus.Done, required, s.get(R.string.setup_detail_allowed))
        PermissionStatus.NotAsked -> StepInfo(id, StepStatus.Todo, required, s.get(R.string.setup_detail_not_allowed))
        PermissionStatus.Denied -> StepInfo(id, StepStatus.Denied, required, s.get(R.string.setup_detail_denied))
        PermissionStatus.Blocked -> StepInfo(id, StepStatus.Blocked, required, s.get(R.string.setup_detail_blocked))
    }

    private fun batteryStep(s: Strings, i: SetupInputs): StepInfo = when (i.battery) {
        BatteryState.BackgroundRestricted ->
            StepInfo(StepId.Battery, StepStatus.Blocked, true, s.get(R.string.setup_detail_bg_restricted))
        BatteryState.Optimised ->
            StepInfo(StepId.Battery, StepStatus.Todo, true, s.get(R.string.setup_detail_battery_todo))
        BatteryState.Unrestricted -> if (i.oem.hasExtraSwitch && i.oem != Oem.Other && !i.oemAcknowledged) {
            // The system exemption is verified; the vendor's extra switch cannot be read, so it stays a to-do.
            StepInfo(StepId.Battery, StepStatus.Todo, true, s.get(R.string.setup_detail_extra_switch, i.oem.label))
        } else {
            StepInfo(StepId.Battery, StepStatus.Done, true, s.get(R.string.setup_detail_unrestricted))
        }
    }

    /** First step that still needs attention, in wizard order; null when everything is done. */
    fun firstOpen(steps: List<StepInfo>): StepInfo? =
        steps.firstOrNull { it.status != StepStatus.Done && it.status != StepStatus.Info }

    data class Warning(val step: StepId, val text: String)

    /**
     * Persistent warnings for the home screen. Required steps warn whenever the app is in use (a computer is
     * paired or setup was finished); optional steps only warn when a permission that worked was later revoked.
     */
    fun warnings(s: Strings, i: SetupInputs, setupFinished: Boolean): List<Warning> {
        val inUse = i.pairedCount > 0 || setupFinished
        val out = ArrayList<Warning>()
        for (st in steps(s, i)) {
            if (st.status == StepStatus.Done || st.status == StepStatus.Info) continue
            val revoked = st.id in i.wasDone
            when {
                st.id == StepId.Battery && inUse -> out += Warning(
                    st.id,
                    if (revoked) s.get(R.string.setup_warn_battery_revoked)
                    else s.get(R.string.setup_warn_battery),
                )
                st.id == StepId.Notifications && inUse && st.required -> out += Warning(
                    st.id, s.get(R.string.setup_warn_notifications),
                )
                revoked && st.id != StepId.Pair -> out += Warning(st.id, s.get(R.string.setup_warn_revoked, title(s, st.id)))
            }
        }
        // Most impactful first: the battery exemption decides whether the connection survives at all.
        return out.sortedBy { if (it.step == StepId.Battery) 0 else 1 }
    }

    fun title(s: Strings, id: StepId): String = when (id) {
        StepId.Pair -> s.get(R.string.setup_title_pair)
        StepId.Notifications -> s.get(R.string.setup_title_notifications)
        StepId.Contacts -> s.get(R.string.setup_title_contacts)
        StepId.Calendar -> s.get(R.string.setup_title_calendar)
        StepId.PhoneState -> s.get(R.string.setup_title_phonestate)
        StepId.AnswerCalls -> s.get(R.string.setup_title_answercalls)
        StepId.CallerNumber -> s.get(R.string.setup_title_callernumber)
        StepId.Sms -> s.get(R.string.setup_title_sms)
        StepId.Battery -> s.get(R.string.setup_title_battery)
    }

    fun permissionFor(id: StepId): String? = when (id) {
        StepId.Notifications -> if (Build.VERSION.SDK_INT >= 33) "android.permission.POST_NOTIFICATIONS" else null
        StepId.Contacts -> "android.permission.READ_CONTACTS"
        StepId.Calendar -> "android.permission.READ_CALENDAR"
        StepId.PhoneState -> "android.permission.READ_PHONE_STATE"
        StepId.AnswerCalls -> "android.permission.ANSWER_PHONE_CALLS"
        StepId.CallerNumber -> "android.permission.READ_CALL_LOG"
        else -> null
    }
}
