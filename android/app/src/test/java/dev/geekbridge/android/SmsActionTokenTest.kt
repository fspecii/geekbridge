// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class SmsActionTokenTest {
    @Test fun tokensAreLongAndDoNotRepeat() {
        val seen = HashSet<String>()
        repeat(500) {
            val t = smsActionToken()
            assertEquals(24, t.length)
            assertTrue(t.all { it in "0123456789abcdef" })
            assertTrue("repeated $t", seen.add(t))
        }
    }
}
