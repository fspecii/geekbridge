package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneDataWaitTest {
    private fun wait(count: Int, seq: Long): JSONObject {
        val events = JSONArray()
        for (i in 1..count) {
            events.put(JSONObject().put("seq", i.toLong()).put("kind", "contacts.req.uids").put("body", JSONObject()))
        }
        return JSONObject().put("events", events).put("seq", seq)
    }

    @Test fun moreThanSixtyFourEventsAreNotAnOddResultThatEndsTheWaiter() {
        val w = PhoneDataProtocol.parseWait(wait(100, 100))
        assertNotNull(w)
        assertEquals(64, w!!.events.size)
        assertTrue(w.truncated)
        // The rest is fetched on the next wait: `since` stops at what was handed on.
        assertEquals(64L, PhoneDataProtocol.nextSince(0, w))
    }

    @Test fun anOrdinaryResultAdvancesToTheReportedSequence() {
        val w = PhoneDataProtocol.parseWait(wait(3, 9))!!
        assertFalse(w.truncated)
        assertEquals(9L, PhoneDataProtocol.nextSince(2, w))
        assertEquals(9L, PhoneDataProtocol.nextSince(12, w).coerceAtMost(9))
    }
}
