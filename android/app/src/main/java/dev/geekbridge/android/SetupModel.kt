package dev.geekbridge.android

import android.os.Build

enum class StepId { Pair, Notifications, Contacts, Calendar, PhoneState, AnswerCalls, CallerNumber, Sms, Battery }

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
    fun steps(i: SetupInputs): List<StepInfo> {
        val out = ArrayList<StepInfo>()
        out += StepInfo(
            StepId.Pair,
            if (i.pairedCount > 0) StepStatus.Done else StepStatus.Todo,
            required = true,
            detail = if (i.pairedCount > 0) "${i.pairedCount} paired" else "No computer paired yet",
        )
        out += if (i.sdk >= 33) {
            permissionStep(StepId.Notifications, i.notifications, required = true)
        } else {
            StepInfo(StepId.Notifications, StepStatus.Done, true, "Not needed on this Android version")
        }
        out += permissionStep(StepId.Contacts, i.contacts, required = false)
        out += permissionStep(StepId.Calendar, i.calendar, required = false)
        out += permissionStep(StepId.PhoneState, i.phoneState, required = false)
        out += permissionStep(StepId.AnswerCalls, i.answerCalls, required = false)
        out += permissionStep(StepId.CallerNumber, i.callLog, required = false)
        out += StepInfo(
            StepId.Sms, StepStatus.Info, required = false,
            detail = "Optional · contacts, calendar and texts for your computer, off until you switch them on",
        )
        out += batteryStep(i)
        return out
    }

    private fun permissionStep(id: StepId, p: PermissionStatus, required: Boolean): StepInfo = when (p) {
        PermissionStatus.Granted -> StepInfo(id, StepStatus.Done, required, "Allowed")
        PermissionStatus.NotAsked -> StepInfo(id, StepStatus.Todo, required, "Not allowed yet")
        PermissionStatus.Denied -> StepInfo(id, StepStatus.Denied, required, "Denied · you can allow it again")
        PermissionStatus.Blocked -> StepInfo(id, StepStatus.Blocked, required, "Denied · allow it in app settings")
    }

    private fun batteryStep(i: SetupInputs): StepInfo = when (i.battery) {
        BatteryState.BackgroundRestricted ->
            StepInfo(StepId.Battery, StepStatus.Blocked, true, "Background use is restricted · change it in app settings")
        BatteryState.Optimised ->
            StepInfo(StepId.Battery, StepStatus.Todo, true, "Android may stop the connection to save battery")
        BatteryState.Unrestricted -> if (i.oem.hasExtraSwitch && i.oem != Oem.Other && !i.oemAcknowledged) {
            // The system exemption is verified; the vendor's extra switch cannot be read, so it stays a to-do.
            StepInfo(StepId.Battery, StepStatus.Todo, true, "Unrestricted · ${i.oem.label} has an extra autostart switch")
        } else {
            StepInfo(StepId.Battery, StepStatus.Done, true, "Unrestricted")
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
    fun warnings(i: SetupInputs, setupFinished: Boolean): List<Warning> {
        val inUse = i.pairedCount > 0 || setupFinished
        val out = ArrayList<Warning>()
        for (s in steps(i)) {
            if (s.status == StepStatus.Done || s.status == StepStatus.Info) continue
            val revoked = s.id in i.wasDone
            when {
                s.id == StepId.Battery && inUse -> out += Warning(
                    s.id,
                    if (revoked) "Battery restriction was turned back on. GeekBridge may disconnect in the background."
                    else "Battery optimisation is on. GeekBridge may disconnect in the background.",
                )
                s.id == StepId.Notifications && inUse && s.required -> out += Warning(
                    s.id, "Notifications are off. The background connection notice and alerts will not show.",
                )
                revoked && s.id != StepId.Pair -> out += Warning(s.id, "${title(s.id)} access was turned off. Related features are paused.")
            }
        }
        // Most impactful first: the battery exemption decides whether the connection survives at all.
        return out.sortedBy { if (it.step == StepId.Battery) 0 else 1 }
    }

    fun title(id: StepId): String = when (id) {
        StepId.Pair -> "Pair with your computer"
        StepId.Notifications -> "Notifications"
        StepId.Contacts -> "Contacts"
        StepId.Calendar -> "Calendar"
        StepId.PhoneState -> "Phone state"
        StepId.AnswerCalls -> "Answer calls"
        StepId.CallerNumber -> "Caller number"
        StepId.Sms -> "Share with computer"
        StepId.Battery -> "Battery"
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
