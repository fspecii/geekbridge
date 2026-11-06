// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

class GrantCountsTest {
    @Test
    fun theDebugLineCountsComputersPerFeatureAndNeverNamesThem() {
        val a = "a".repeat(32)
        val b = "b".repeat(32)
        val g = ComputerGrants().set(a, GrantFeature.Calls, true).set(b, GrantFeature.Calls, true).set(a, GrantFeature.Sms, true)
        val line = GrantText.counts(g)
        assertEquals(true, line.startsWith("calls=2 contacts=0 calendar=0 calendar_edit=0 sms=1 "))
        assertFalse(line.contains(a))
        assertEquals("calls=0", GrantText.counts(ComputerGrants()).substringBefore(' '))
    }
}
