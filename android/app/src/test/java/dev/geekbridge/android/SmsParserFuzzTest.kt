// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Random

/**
 * Seeded, deterministic abuse of the parsers that read what a paired computer (or a damaged link) hands the phone:
 * the send request and the relayed event list. Neither may throw, and what they accept must stay inside the limits
 * the rest of the phone relies on.
 */
class SmsParserFuzzTest {
    private val letters = "abcXYZ0123456789+-_ ().\u0000\u0007\n\t‮😀é"

    private fun junkString(r: Random): String {
        val n = when (r.nextInt(6)) { 0 -> 0; 1 -> 1; 2 -> 8; 3 -> 70; 4 -> 300; else -> 25_000 }
        return buildString { repeat(n) { append(letters[r.nextInt(letters.length)]) } }
    }

    private fun junkValue(r: Random, depth: Int = 0): Any? = when (r.nextInt(if (depth > 2) 7 else 9)) {
        0 -> null
        1 -> JSONObject.NULL
        2 -> r.nextInt()
        3 -> r.nextLong()
        4 -> r.nextDouble() * 1e30
        5 -> r.nextBoolean()
        6 -> junkString(r)
        7 -> JSONArray().also { a -> repeat(r.nextInt(12)) { a.put(junkValue(r, depth + 1)) } }
        else -> JSONObject().also { o -> repeat(r.nextInt(5)) { o.put(junkString(r).take(12), junkValue(r, depth + 1)) } }
    }

    private fun validAddress(r: Random) = "+49151" + (1000000 + r.nextInt(8999999))

    private fun body(r: Random): JSONObject {
        val o = JSONObject()
        if (r.nextInt(4) > 0) o.put("requestId", if (r.nextBoolean()) "req-${r.nextInt(100)}" else junkValue(r))
        if (r.nextInt(4) > 0) o.put("messageBody", if (r.nextInt(3) > 0) junkString(r) else junkValue(r))
        when (r.nextInt(4)) {
            0 -> o.put("phoneNumber", junkValue(r))
            1 -> o.put("addresses", junkValue(r))
            2 -> o.put("addresses", JSONArray().put(JSONObject().put("address", if (r.nextBoolean()) validAddress(r) else junkString(r))))
            else -> o.put("addresses", JSONArray().also { a -> repeat(r.nextInt(12)) { a.put(JSONObject().put("address", validAddress(r))) } })
        }
        if (r.nextInt(3) == 0) o.put("subID", junkValue(r))
        if (r.nextInt(5) == 0) o.put("attachments", junkValue(r))
        repeat(r.nextInt(3)) { o.put(junkString(r).take(10), junkValue(r)) }
        return o
    }

    @Test fun theSendRequestParserNeverThrowsAndAcceptsOnlyWithinLimits() {
        val r = Random(20261103)
        var ok = 0
        repeat(6000) {
            val p = PhoneDataProtocol.parseSmsRequest(body(r))
            when (p) {
                is PhoneDataProtocol.SmsParse.Ok -> {
                    ok++
                    assertTrue(p.address.isNotEmpty() && p.address.length <= Limits.SMS_MAX_ADDRESS_CHARS)
                    assertFalse(p.address.any { Character.isISOControl(it) })
                    assertTrue(p.text.isNotBlank() && p.text.codePointCount(0, p.text.length) <= Limits.SMS_MAX_TEXT_CHARS)
                    assertFalse(p.text.any { Character.isISOControl(it) && it != '\n' && it != '\r' && it != '\t' })
                    assertTrue(p.subId == null || p.subId > 0)
                    assertTrue(p.requestId == null || PhoneDataProtocol.validRequestId(p.requestId))
                }
                is PhoneDataProtocol.SmsParse.Bad -> {
                    assertTrue(p.reason, p.reason in setOf("invalid", "group_unsupported", "mms_unsupported"))
                    assertTrue(p.requestId == null || PhoneDataProtocol.validRequestId(p.requestId))
                }
            }
        }
        assertTrue("the generator must also reach the accepting branch ($ok)", ok > 50)
    }

    @Test fun textLengthIsCountedInCharactersNotUtf16Units() {
        // 2000 emoji are 4000 UTF-16 units but 2000 characters: at the limit they are fine, one more is not.
        val atLimit = "😀".repeat(Limits.SMS_MAX_TEXT_CHARS)
        fun parse(t: String) = PhoneDataProtocol.parseSmsRequest(
            JSONObject().put("messageBody", t).put("addresses", JSONArray().put(JSONObject().put("address", "+4915112345678"))),
        )
        assertTrue(parse(atLimit) is PhoneDataProtocol.SmsParse.Ok)
        assertTrue(parse(atLimit + "a") is PhoneDataProtocol.SmsParse.Bad)
    }

    @Test fun theEventListParserNeverThrowsAndStaysBounded() {
        val r = Random(7)
        repeat(3000) {
            val events = JSONArray()
            repeat(r.nextInt(90)) {
                val e = JSONObject()
                if (r.nextInt(5) > 0) e.put("seq", if (r.nextInt(8) == 0) junkValue(r) else r.nextInt(1000).toLong())
                if (r.nextInt(5) > 0) e.put("kind", if (r.nextInt(3) == 0) junkValue(r) else "kdeconnect.sms.request")
                if (r.nextInt(3) > 0) e.put("body", junkValue(r))
                if (r.nextInt(3) > 0) e.put("device", junkValue(r))
                if (r.nextInt(3) > 0) e.put("computer", junkValue(r))
                events.put(if (r.nextInt(12) == 0) junkValue(r) else e)
            }
            val result = JSONObject().put("events", events).put("seq", junkValue(r)).put("stopped", junkValue(r))
            val w = PhoneDataProtocol.parseWait(if (r.nextInt(20) == 0) junkValue(r) else result) ?: return@repeat
            assertTrue(w.events.size <= 64)
            for (e in w.events) {
                assertTrue(e.device.length <= 64 && e.device.all { it.isLetterOrDigit() || it == '-' || it == '_' })
                assertTrue(e.computer.length <= 40 && e.computer.none { Character.isISOControl(it) })
                assertTrue(PhoneDataProtocol.validRequestId("x") && e.kind.length <= 64)
            }
        }
    }

    @Test fun outgoingMessagesAreAlwaysPlainJsonWithinTheLimits() {
        val r = Random(99)
        repeat(1500) {
            val row = SmsRow(
                id = r.nextLong(), threadId = r.nextLong(), address = junkString(r), body = junkString(r), date = r.nextLong(),
                type = r.nextInt(8), read = r.nextBoolean(), subId = r.nextInt(), mms = r.nextInt(4) == 0,
            )
            val j = PhoneDataProtocol.smsMessage(row)
            val addr = j.getJSONArray("addresses").getJSONObject(0).getString("address")
            assertTrue(addr.length <= 64 && addr.none { Character.isISOControl(it) })
            assertTrue(j.getString("body").length <= PhoneDataLimits.MAX_SMS_BODY_CHARS)
            assertEquals(row.mms, j.has("attachments"))
            if (row.mms) assertEquals("", j.getString("body"))
            JSONObject(j.toString()) // serialises and parses back
        }
    }

    @Test fun simAndUnreadListsAreCapped() {
        val sims = (1..40).map { SimInfo(it, it, "S".repeat(100) + "\u0007") }
        val a = PhoneDataProtocol.simsJson(sims)
        assertTrue(a.length() <= PhoneDataLimits.MAX_SIMS)
        for (i in 0 until a.length()) assertTrue(a.getJSONObject(i).getString("label").let { it.length <= 24 && it.none(Character::isISOControl) })
        assertTrue(PhoneDataProtocol.simsJson(listOf(SimInfo(Int.MAX_VALUE, 1, "x"), SimInfo(0, 1, "y"), SimInfo(-3, 1, "z"))).length() == 0)
        val u = PhoneDataProtocol.unreadJson((1L..1000L).associateWith { 50_000 } + (-5L to 3))
        assertTrue(u.length() <= 300 && !u.has("-5"))
        for (k in u.keys()) assertTrue(u.getInt(k) <= 10_000)
    }
}
