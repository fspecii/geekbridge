// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The SIM chooser needs READ_PHONE_STATE, which the Messages step now offers itself (only on a phone with two SIM slots). */
class SmsSimChoiceTest {
    private val G = PermissionStatus.Granted
    private fun st(on: Boolean = true, sms: Boolean = true, slots: Int = 2, phone: PermissionStatus = PermissionStatus.NotAsked) =
        SmsSimChoice.state(on, sms, slots, phone)

    @Test fun offeredOnlyOnADualSimPhoneWithMessagesWorking() {
        assertEquals(SimChoiceState.Ask, st())
        assertEquals(SimChoiceState.NotNeeded, st(slots = 1))
        assertEquals(SimChoiceState.NotNeeded, st(slots = 0))
        assertEquals(SimChoiceState.NotNeeded, st(on = false))
        assertEquals(SimChoiceState.NotNeeded, st(sms = false))
    }

    @Test fun grantedMeansNothingMoreToDo() {
        assertEquals(SimChoiceState.Granted, st(phone = G))
        assertEquals(SimChoiceState.NotNeeded, st(phone = G, slots = 1))
    }

    @Test fun denialAsksAgainAndAPermanentDenialPointsToSettingsWithTheFallbackSaid() {
        assertEquals(SimChoiceState.Ask, st(phone = PermissionStatus.Denied))
        assertEquals(SimChoiceState.Blocked, st(phone = PermissionStatus.Blocked))
    }

    @Test fun wordsSayWhatHappensWithoutIt() {
        val ask = SmsSimChoice.message(SimChoiceState.Ask, TestStrings)
        val blocked = SmsSimChoice.message(SimChoiceState.Blocked, TestStrings)
        assertTrue(ask, ask.contains("default SIM"))
        assertTrue(blocked, blocked.contains("default SIM") && blocked.contains("Permissions"))
        assertEquals("", SmsSimChoice.message(SimChoiceState.NotNeeded, TestStrings))
        assertEquals("", SmsSimChoice.message(SimChoiceState.Granted, TestStrings))
        assertFalse(ask.contains("READ_PHONE_STATE"))
    }
}
