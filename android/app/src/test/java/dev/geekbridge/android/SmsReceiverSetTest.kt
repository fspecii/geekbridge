// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class SmsReceiverSetTest {
    @Test fun aThrowingSendReleasesBothReceiversAtOnce() {
        val released = ArrayList<String>()
        val set = SmsReceiverSet<String> { released += it }
        try {
            set.guarded {
                set.add("sent")
                set.add("delivered")
                throw SecurityException("SEND_SMS revoked")
            }
            throw AssertionError("must rethrow")
        } catch (e: SecurityException) {
            assertEquals("SEND_SMS revoked", e.message)
        }
        assertEquals(listOf("sent", "delivered"), released)
    }

    @Test fun aSendThatStartsKeepsThemForTheReports() {
        val released = ArrayList<String>()
        val set = SmsReceiverSet<String> { released += it }
        assertEquals(7, set.guarded { set.add("sent"); set.add("delivered"); 7 })
        assertTrue(released.isEmpty())
    }

    @Test fun releaseSurvivesAnAlreadyUnregisteredReceiver() {
        val released = ArrayList<String>()
        val set = SmsReceiverSet<String> { if (it == "sent") throw IllegalArgumentException("gone") else released += it }
        set.add("sent"); set.add("delivered")
        set.releaseAll()
        assertEquals(listOf("delivered"), released)
    }
}
