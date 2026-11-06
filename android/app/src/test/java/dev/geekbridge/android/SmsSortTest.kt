// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Test

class SmsSortTest {
    @Test
    fun `both columns sort newest first, the direction is not left to the last column only`() {
        assertEquals("date DESC, _id DESC", newestFirstSql("date", "_id"))
    }
}
