// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceNameTest {
    private fun ok(raw: String) = (DeviceName.check(raw) as DeviceName.Check.Ok).name

    @Test fun `a good name is trimmed and the characters peers strip are removed`() {
        assertEquals("Ana Pixel", ok("  Ana Pixel "))
        assertEquals("Anas Pixel", ok("Ana's Pixel"))
    }

    @Test fun `control characters, empty and overlong names are refused`() {
        for (bad in listOf("", "   ", "...", "a\u001bb", "tab\there", "x\ny", "n".repeat(DeviceName.MAX_CHARS + 1))) {
            assertTrue("$bad", DeviceName.check(bad) is DeviceName.Check.Bad)
        }
        assertEquals("n".repeat(DeviceName.MAX_CHARS), ok("n".repeat(DeviceName.MAX_CHARS)))
    }

    @Test fun `bidirectional and zero width characters never reach a saved name`() {
        assertEquals("abc", ok("a‮b​c"))
    }

    @Test fun `names shown from the computer are single line, bounded and cannot reorder the line`() {
        assertEquals("evil exe", Bounds.name("evil‮ exe", 48))
        assertEquals("a b", Bounds.name("a\n\u0007b", 48))
        assertTrue(Bounds.name("x".repeat(500), 48).length <= 48)
        assertFalse(Bounds.name("‏⁦hi⁩", 48).any { Bounds.isInvisibleFormat(it) })
    }

    @Test fun `the rename request names the action and the device_name field`() {
        val o = JSONObject(TransportProtocol.setNameRequest("Pixel"))
        assertEquals("transport_set_name", o.getString("action"))
        assertEquals("Pixel", o.getString("device_name"))
    }
}
