// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SetupModelTest {
    private fun inputs(
        sdk: Int = 36,
        paired: Int = 0,
        notifications: PermissionStatus = PermissionStatus.NotAsked,
        contacts: PermissionStatus = PermissionStatus.NotAsked,
        calendar: PermissionStatus = PermissionStatus.NotAsked,
        phone: PermissionStatus = PermissionStatus.NotAsked,
        battery: BatteryState = BatteryState.Optimised,
        oemAck: Boolean = false,
        oem: Oem = Oem.Other,
        wasDone: Set<StepId> = emptySet(),
        answerCalls: PermissionStatus = PermissionStatus.NotAsked,
        callLog: PermissionStatus = PermissionStatus.NotAsked,
    ) = SetupInputs(sdk, paired, notifications, contacts, calendar, phone, battery, oemAck, oem, wasDone, answerCalls, callLog)

    private fun status(i: SetupInputs, id: StepId) = SetupModel.steps(TestStrings, i).first { it.id == id }.status

    @Test fun permissionStatusFromAndroidSignals() {
        assertEquals(PermissionStatus.Granted, PermissionStatusLogic.compute(true, true, false))
        assertEquals(PermissionStatus.NotAsked, PermissionStatusLogic.compute(false, false, false))
        assertEquals(PermissionStatus.Denied, PermissionStatusLogic.compute(false, true, true))
        assertEquals(PermissionStatus.Blocked, PermissionStatusLogic.compute(false, true, false))
    }

    @Test fun stepOrderIsStable() {
        assertEquals(
            listOf(StepId.Pair, StepId.Notifications, StepId.Battery, StepId.Contacts, StepId.Calendar, StepId.PhoneState, StepId.AnswerCalls, StepId.CallerNumber, StepId.Sms),
            SetupModel.steps(TestStrings, inputs()).map { it.id },
        )
    }

    @Test fun pairingFollowsPairedCount() {
        assertEquals(StepStatus.Todo, status(inputs(paired = 0), StepId.Pair))
        assertEquals(StepStatus.Done, status(inputs(paired = 2), StepId.Pair))
    }

    @Test fun notificationsNotNeededBeforeAndroid13() {
        assertEquals(StepStatus.Done, status(inputs(sdk = 31), StepId.Notifications))
        assertEquals(StepStatus.Todo, status(inputs(sdk = 33), StepId.Notifications))
        assertEquals(StepStatus.Blocked, status(inputs(sdk = 36, notifications = PermissionStatus.Blocked), StepId.Notifications))
    }

    @Test fun smsIsInformationalNeverDone() {
        val s = SetupModel.steps(TestStrings, inputs()).first { it.id == StepId.Sms }
        assertEquals(StepStatus.Info, s.status)
        assertTrue(!s.required)
    }

    @Test fun batteryStates() {
        assertEquals(StepStatus.Todo, status(inputs(battery = BatteryState.Optimised), StepId.Battery))
        assertEquals(StepStatus.Blocked, status(inputs(battery = BatteryState.BackgroundRestricted), StepId.Battery))
        assertEquals(StepStatus.Done, status(inputs(battery = BatteryState.Unrestricted), StepId.Battery))
    }

    @Test fun oemSwitchKeepsBatteryOpenUntilAcknowledged() {
        val i = inputs(battery = BatteryState.Unrestricted, oem = Oem.Xiaomi)
        assertEquals(StepStatus.Todo, status(i, StepId.Battery))
        assertEquals(StepStatus.Done, status(i.copy(oemAcknowledged = true), StepId.Battery))
        // The acknowledgement never hides a real system restriction.
        assertEquals(StepStatus.Todo, status(i.copy(battery = BatteryState.Optimised, oemAcknowledged = true), StepId.Battery))
    }

    @Test fun firstOpenSkipsDoneAndInfo() {
        val all = SetupModel.steps(TestStrings, inputs(paired = 1, notifications = PermissionStatus.Granted))
        assertEquals(StepId.Battery, SetupModel.firstOpen(all)?.id) // required before optional: Skip cannot walk past it unasked
        val batteryDone = SetupModel.steps(TestStrings, inputs(paired = 1, notifications = PermissionStatus.Granted, battery = BatteryState.Unrestricted))
        assertEquals(StepId.Contacts, SetupModel.firstOpen(batteryDone)?.id)
        val done = SetupModel.steps(TestStrings, 
            inputs(
                paired = 1, notifications = PermissionStatus.Granted, contacts = PermissionStatus.Granted,
                calendar = PermissionStatus.Granted, phone = PermissionStatus.Granted, battery = BatteryState.Unrestricted,
                answerCalls = PermissionStatus.Granted, callLog = PermissionStatus.Granted,
            ),
        )
        assertNull(SetupModel.firstOpen(done))
    }

    @Test fun batteryWarningOnlyWhenAppIsInUse() {
        assertTrue(SetupModel.warnings(TestStrings, inputs(), setupFinished = false).none { it.step == StepId.Battery })
        assertTrue(SetupModel.warnings(TestStrings, inputs(paired = 1), setupFinished = false).any { it.step == StepId.Battery })
        assertTrue(SetupModel.warnings(TestStrings, inputs(), setupFinished = true).any { it.step == StepId.Battery })
        assertTrue(SetupModel.warnings(TestStrings, inputs(paired = 1, battery = BatteryState.Unrestricted, notifications = PermissionStatus.Granted), true).isEmpty())
    }

    @Test fun revokedBatteryExemptionIsCalledOut() {
        val w = SetupModel.warnings(TestStrings, inputs(paired = 1, wasDone = setOf(StepId.Battery)), setupFinished = true)
            .first { it.step == StepId.Battery }
        assertTrue(w.text.contains("turned back on"))
    }

    @Test fun optionalPermissionWarnsOnlyWhenRevoked() {
        val base = inputs(paired = 1, battery = BatteryState.Unrestricted, notifications = PermissionStatus.Granted)
        assertTrue(SetupModel.warnings(TestStrings, base, true).isEmpty()) // never granted: not a warning
        val revoked = base.copy(contacts = PermissionStatus.Denied, wasDone = setOf(StepId.Contacts))
        assertEquals(listOf(StepId.Contacts), SetupModel.warnings(TestStrings, revoked, true).map { it.step })
    }

    @Test fun deniedNotificationsWarnWhenInUse() {
        val i = inputs(paired = 1, battery = BatteryState.Unrestricted, notifications = PermissionStatus.Denied)
        assertEquals(listOf(StepId.Notifications), SetupModel.warnings(TestStrings, i, true).map { it.step })
    }

    @Test fun backFromAPageOpenedElsewhereReturnsThereNotThroughEveryStep() {
        // A Home tile opened step 9 (index 8): one Back leaves the wizard.
        assertEquals(SetupModel.BACK_FINISH, SetupModel.back(page = 8, anchor = 8, anchorTarget = SetupModel.BACK_FINISH))
        // A row of the overview (index 9 is the overview itself): Back returns to it.
        assertEquals(9, SetupModel.back(page = 3, anchor = 3, anchorTarget = 9))
        // Walking on from the anchored page and back still goes one step at a time.
        assertEquals(7, SetupModel.back(page = 8, anchor = 3, anchorTarget = 9))
        // No anchor: previous page, and the first page leaves.
        assertEquals(1, SetupModel.back(page = 2, anchor = -1, anchorTarget = SetupModel.BACK_FINISH))
        assertEquals(SetupModel.BACK_FINISH, SetupModel.back(page = 0, anchor = -1, anchorTarget = SetupModel.BACK_FINISH))
    }
}
