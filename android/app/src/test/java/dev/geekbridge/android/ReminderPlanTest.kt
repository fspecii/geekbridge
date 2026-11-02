// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ReminderPlanTest {
    private fun r(id: Long, min: Int, alert: Boolean = true) = ExistingReminder(id, min, alert)

    @Test fun anEventWithoutReminderGetsOneInserted() {
        assertEquals(ReminderPlan(insert = 10), ReminderPlan.of(emptyList(), 10))
        assertEquals(ReminderPlan(), ReminderPlan.of(emptyList(), null))
    }

    @Test fun theFirstAlertIsChangedInPlaceNeverDeletedFirst() {
        val plan = ReminderPlan.of(listOf(r(7, 5), r(8, 30)), 10)
        assertEquals(ReminderPlan(updateId = 7, updateMinutes = 10), plan)
        assertNull(plan.deleteId)
        assertNull(plan.insert)
    }

    @Test fun unchangedMinutesTouchNothing() {
        assertEquals(ReminderPlan(), ReminderPlan.of(listOf(r(7, 10), r(8, 30)), 10))
    }

    @Test fun removingTheReminderRemovesOnlyTheFirstAlertAndKeepsTheRest() {
        // Six reminders on the event, five are shown: the editor owns the first one only.
        val many = (1L..6L).map { r(it, it.toInt() * 5) }
        assertEquals(ReminderPlan(deleteId = 1), ReminderPlan.of(many, null))
    }

    @Test fun emailRemindersAreNeverTouchedOrCountedAsTheEditedOne() {
        val plan = ReminderPlan.of(listOf(r(3, 60, alert = false)), 10)
        assertEquals(ReminderPlan(insert = 10), plan)
        assertEquals(ReminderPlan(), ReminderPlan.of(listOf(r(3, 60, alert = false)), null))
    }

    @Test fun theAlarmFlagStaysSetWhileAnyReminderRemains() {
        assertEquals(true, ReminderPlan.of(listOf(r(1, 5), r(2, 9)), null).hasAlarmAfter(listOf(r(1, 5), r(2, 9)), null))
        assertEquals(false, ReminderPlan.of(listOf(r(1, 5)), null).hasAlarmAfter(listOf(r(1, 5)), null))
        assertEquals(true, ReminderPlan.of(emptyList(), 10).hasAlarmAfter(emptyList(), 10))
    }
}
