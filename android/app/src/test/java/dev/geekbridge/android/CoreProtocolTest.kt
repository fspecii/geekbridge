// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CoreProtocolTest {
    @Test
    fun versionRequestShape() {
        val o = JSONObject(CoreProtocol.versionRequest())
        assertEquals(1, o.getInt("version"))
        assertEquals("version", o.getString("action"))
        assertEquals(2, o.length())
    }

    @Test
    fun smsRequestShapeWithAndWithoutSubscription() {
        val withSub = JSONObject(CoreProtocol.validateSmsRequest("+15550100", "hi \"there\"\n", 3))
        assertEquals("validate_sms", withSub.getString("action"))
        assertEquals("+15550100", withSub.getString("recipient"))
        assertEquals("hi \"there\"\n", withSub.getString("text"))
        assertEquals(3, withSub.getInt("subscription_id"))

        val without = JSONObject(CoreProtocol.validateSmsRequest("1", "x", null))
        assertFalse(without.has("subscription_id"))
        assertEquals(5, JSONObject(CoreProtocol.validateSmsRequest("1", "x", 0)).length())
    }

    @Test
    fun clipboardRequestShape() {
        val o = JSONObject(CoreProtocol.validateClipboardRequest("a\u0000b"))
        assertEquals("validate_clipboard", o.getString("action"))
        assertEquals("a\u0000b", o.getString("text")) // NUL is forwarded; the core decides
        assertEquals(3, o.length())
    }

    @Test
    fun okResponse() {
        val r = CoreProtocol.parseResponse("""{"version":1,"ok":true,"result":{"core_version":"0.1.0"}}""")
        r as CoreOutcome.Ok
        assertEquals("0.1.0", CoreProtocol.coreVersionOf(r.result))
    }

    @Test
    fun okWithNullResultIsOk() {
        val r = CoreProtocol.parseResponse("""{"version":1,"ok":true,"result":null}""")
        assertEquals(CoreOutcome.Ok(null), r)
    }

    @Test
    fun errorResponse() {
        val r = CoreProtocol.parseResponse(
            """{"version":1,"ok":false,"error":{"code":"invalid_input","message":"text\nis empty"}}""",
        )
        assertEquals(CoreOutcome.Rejected("invalid_input", "text is empty"), r)
    }

    @Test
    fun errorMessageIsBounded() {
        val long = "x".repeat(5000)
        val r = CoreProtocol.parseResponse("""{"version":1,"ok":false,"error":{"code":"e","message":"$long"}}""")
        r as CoreOutcome.Rejected
        assertTrue(r.message.length <= Limits.MAX_ERROR_MESSAGE_CHARS)
    }

    @Test
    fun malformedResponsesAreRejected() {
        val bad = listOf(
            null,
            "",
            "not json",
            "[]",
            """{"ok":true,"result":1}""",
            """{"version":2,"ok":true}""",
            """{"version":"1","ok":true}""",
            """{"version":1}""",
            """{"version":1,"ok":"true"}""",
            """{"version":1,"ok":false}""",
            """{"version":1,"ok":false,"error":"boom"}""",
            """{"version":1,"ok":false,"error":{"message":"no code"}}""",
            """{"version":1,"ok":false,"error":{"code":"has space"}}""",
            """{"version":1,"ok":false,"error":{"code":"${"c".repeat(49)}"}}""",
        )
        for (raw in bad) {
            assertTrue("should be malformed: $raw", CoreProtocol.parseResponse(raw) is CoreOutcome.Malformed)
        }
    }

    @Test
    fun oversizedResponseIsRejectedBeforeParsing() {
        val raw = """{"version":1,"ok":true,"result":"${"a".repeat(Limits.MAX_RESPONSE_CHARS)}"}"""
        assertTrue(CoreProtocol.parseResponse(raw) is CoreOutcome.Malformed)
    }

    @Test
    fun versionLabelNeedsRecognisedStringField() {
        assertNull(CoreProtocol.coreVersionOf(null))
        assertNull(CoreProtocol.coreVersionOf("0.1.0"))
        assertNull(CoreProtocol.coreVersionOf(JSONObject("""{"schema":1}""")))
        assertEquals("1.2.3", CoreProtocol.coreVersionOf(JSONObject("""{"version":"1.2.3"}""")))
    }

    // ---- CoreClient

    private fun client(available: Boolean = true, transport: (String) -> String) =
        CoreClient({ available }, { if (available) null else "library could not be loaded" }, transport)

    @Test
    fun unavailableCoreNeverCallsTransport() {
        var called = false
        val c = client(available = false) { called = true; "" }
        val o = c.version()
        assertEquals(CoreOutcome.Unavailable("library could not be loaded"), o)
        assertFalse(called)
        assertTrue(c.validateClipboard("x") is CoreOutcome.Unavailable)
        assertTrue(c.validateSms("1", "x", null) is CoreOutcome.Unavailable)
    }

    @Test
    fun missingSymbolBecomesUnavailableWithoutMessage() {
        val c = client { throw UnsatisfiedLinkError("secret input text") }
        val o = c.validateClipboard("secret input text") as CoreOutcome.Unavailable
        assertFalse(o.reason.contains("secret"))
        assertTrue(o.reason.contains("UnsatisfiedLinkError"))
    }

    @Test
    fun runtimeFailureBecomesUnavailable() {
        val c = client { throw IllegalStateException("boom") }
        assertTrue(c.version() is CoreOutcome.Unavailable)
    }

    @Test
    fun clipboardOverLimitIsRejectedLocallyWithoutCallingCore() {
        var called = false
        val c = client { called = true; "" }
        val o = c.validateClipboard("a".repeat(Limits.CLIPBOARD_MAX_BYTES + 1))
        assertEquals("input_too_large", (o as CoreOutcome.Rejected).code)
        assertTrue(o.local)
        assertFalse(called)
    }

    @Test
    fun clipboardAtLimitReachesCore() {
        var seen: String? = null
        val c = client { seen = it; """{"version":1,"ok":true,"result":{}}""" }
        assertTrue(c.validateClipboard("a".repeat(Limits.CLIPBOARD_MAX_BYTES)) is CoreOutcome.Ok)
        assertEquals("validate_clipboard", JSONObject(seen!!).getString("action"))
    }

    @Test
    fun multibyteTextIsMeasuredInBytes() {
        // 3 bytes each: just over the limit in bytes though under it in chars.
        val text = "€".repeat(Limits.CLIPBOARD_MAX_BYTES / 3 + 1)
        assertTrue(text.length < Limits.CLIPBOARD_MAX_BYTES)
        val c = client { error("must not be called") }
        assertTrue((c.validateClipboard(text) as CoreOutcome.Rejected).local)
    }

    @Test
    fun smsGrosslyOversizedInputNeverReachesCore() {
        val c = client { error("must not be called") }
        val o = c.validateSms("1", "a".repeat(Limits.SMS_MAX_TEXT_CHARS * 4 + 1), null)
        assertTrue((o as CoreOutcome.Rejected).local)
    }

    @Test
    fun smsNearLimitIsForwardedSoCoreOwnsTheRule() {
        var seen: String? = null
        val c = client { seen = it; """{"version":1,"ok":false,"error":{"code":"text_too_long","message":"too long"}}""" }
        val o = c.validateSms("1", "a".repeat(Limits.SMS_MAX_TEXT_CHARS + 1), null)
        assertEquals("text_too_long", (o as CoreOutcome.Rejected).code)
        assertFalse(o.local)
        assertTrue(seen != null)
    }
}
