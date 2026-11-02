// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DiagnosticsReportTest {
    @Test fun scrubHidesAddressesKeysAndNumbers() {
        val s = DiagnosticsReport.scrub("lost 192.168.1.20:1716 call +491701234567 fp AABBCCDDEEFF00112233445566778899")
        assertFalse(s, s.contains("192.168"))
        assertFalse(s, s.contains("1701234567"))
        assertFalse(s, s.contains("AABBCC"))
        assertTrue(s.contains("lost"))
    }

    @Test fun scrubKeepsPlainText() = assertEquals("link stopped", DiagnosticsReport.scrub("link stopped"))

    @Test fun scrubIsBounded() = assertTrue(DiagnosticsReport.scrub("word ".repeat(500)).length <= 160)
}
