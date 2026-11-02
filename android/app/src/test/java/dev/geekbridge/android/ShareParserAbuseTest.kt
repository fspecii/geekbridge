// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Locale

/**
 * Untrusted input to the share feature: the transfer list from the native core, and file names chosen by the other
 * device. A name must never leave the downloads folder or be unreadable; a transfer entry that is not exactly
 * understood is dropped, never guessed at.
 */
class ShareParserAbuseTest {
    private fun entry(id: Any? = 1, conn: Any? = 2, device: Any? = "dev_1", dir: Any? = "incoming", kind: Any? = "file", state: Any? = "active") =
        JSONObject().put("transfer_id", id).put("connection_id", conn).put("device_id", device).put("direction", dir)
            .put("kind", kind).put("state", state).put("name", "a.txt").put("size", 100).put("bytes", 10)

    private fun list(vararg e: Any?) = JSONArray().also { a -> e.forEach { a.put(it) } }

    @Test fun `transfer entries that are not exactly understood are dropped one by one`() {
        val parsed = ShareProtocol.parseTransfers(
            list(
                entry(),
                entry(id = -1),
                entry(id = "1"),
                entry(conn = -5),
                entry(conn = null),
                entry(device = "has space"),
                entry(device = 7),
                entry(dir = "sideways"),
                entry(dir = null),
                entry(kind = "folder"),
                entry(state = "paused"),
                entry(state = "waiting").put("size", -1),
                "not an object",
                JSONObject.NULL,
            ),
        )
        assertEquals(listOf(1L, 1L), parsed.map { it.id })
        assertNull("a negative size is unknown, not a size", parsed[1].size)
        assertEquals(TransferState.Waiting, parsed[1].state)
        assertTrue(ShareProtocol.parseTransfers(null).isEmpty())
    }

    @Test fun `every state, kind and direction the core can send is understood`() {
        val states = listOf("offered" to TransferState.Offered, "waiting" to TransferState.Waiting, "active" to TransferState.Active, "done" to TransferState.Done, "failed" to TransferState.Failed, "cancelled" to TransferState.Cancelled, "declined" to TransferState.Declined)
        for ((s, want) in states) assertEquals(want, ShareProtocol.parseTransfers(list(entry(state = s))).single().state)
        assertEquals(TransferKind.Url, ShareProtocol.parseTransfers(list(entry(kind = "url"))).single().kind)
        assertEquals(TransferKind.Text, ShareProtocol.parseTransfers(list(entry(kind = "text"))).single().kind)
        assertEquals(TransferDirection.Outgoing, ShareProtocol.parseTransfers(list(entry(dir = "outgoing"))).single().direction)
    }

    @Test fun `the list, text, name and code are bounded and text kinds carry no file name`() {
        val many = JSONArray()
        for (i in 0 until 100) many.put(entry(id = i))
        assertEquals(ShareLimits.MAX_TRANSFERS, ShareProtocol.parseTransfers(many).size)

        val text = entry(kind = "text").put("text", "x".repeat(ShareLimits.MAX_TEXT_CHARS_SHOWN * 2)).put("name", "ignored.txt")
        val t = ShareProtocol.parseTransfers(list(text)).single()
        assertEquals(ShareLimits.MAX_TEXT_CHARS_SHOWN, t.text!!.length)
        assertEquals("", t.name)

        val file = entry().put("name", "n".repeat(5000)).put("text", "must not show for files").put("code", "Not A Code!")
        val f = ShareProtocol.parseTransfers(list(file)).single()
        assertTrue(f.name.length <= ShareLimits.MAX_NAME_CHARS)
        assertNull(f.text)
        assertNull("a code that is not a lowercase token is dropped", f.code)
        assertEquals("pin_mismatch", ShareProtocol.parseTransfers(list(entry(state = "failed").put("code", "pin_mismatch"))).single().code)
    }

    @Test fun `received file names cannot leave the folder or hide`() {
        val cases = mapOf(
            "../../etc/passwd" to "passwd",
            "..\\..\\Windows\\system32\\evil.dll" to "evil.dll",
            "/absolute/path.txt" to "path.txt",
            "C:\\dir\\name.txt" to "name.txt",
            ".hidden" to "hidden",
            "...dots" to "dots",
            "trailing. . " to "trailing",
            "a<b>c:d\"e|f?g*h.txt" to "a_b_c_d_e_f_g_h.txt",
            "tab\tnew\nline\u0000nul" to "tab_new_line_nul",
            "" to "file",
            "   " to "file",
            ".." to "file",
            "../" to "file",
            "dir/" to "file",
        )
        for ((raw, want) in cases) assertEquals("sanitize(${raw.replace("\u0000", "\\0")})", want, NameRules.sanitize(raw))
        assertEquals("file", NameRules.sanitize(null))
    }

    @Test fun `long names are shortened to the byte limit, keep their extension and never split a character`() {
        val long = NameRules.sanitize("a".repeat(400) + ".jpeg")
        assertTrue(Bounds.utf8Size(long) <= 120)
        assertTrue(long.endsWith(".jpeg"))
        val emoji = "\uD83D\uDE00" // a surrogate pair: 4 bytes in UTF-8
        val wide = NameRules.sanitize(emoji.repeat(100) + ".png")
        assertTrue(Bounds.utf8Size(wide) <= 120)
        assertTrue(wide.endsWith(".png"))
        assertFalse("no lone surrogate may remain", wide.any { Character.isSurrogate(it) && !wide.contains(emoji) })
        var i = 0
        while (i < wide.length) {
            if (Character.isHighSurrogate(wide[i])) { assertTrue(Character.isLowSurrogate(wide[i + 1])); i += 2 } else { assertFalse(Character.isLowSurrogate(wide[i])); i++ }
        }
        // An absurd "extension" is not preserved at the cost of the whole name.
        val noExt = NameRules.sanitize("b".repeat(300) + "." + "x".repeat(100))
        assertTrue(Bounds.utf8Size(noExt) <= 120)
        assertTrue(noExt.isNotEmpty())
    }

    @Test fun `unique names count up and give up gracefully`() {
        assertEquals("a.txt", NameRules.unique("a.txt") { false })
        assertEquals("a (1).txt", NameRules.unique("a.txt") { it == "a.txt" })
        assertEquals("a (2).txt", NameRules.unique("a.txt") { it == "a.txt" || it == "a (1).txt" })
        assertEquals("noext (1)", NameRules.unique("noext") { it == "noext" })
        assertEquals("a.tar (1).gz", NameRules.unique("a.tar.gz") { it == "a.tar.gz" })
        val exhausted = NameRules.unique("a.txt") { true }
        assertTrue("falls back to a time-based suffix instead of looping or overwriting: $exhausted", exhausted.startsWith("a (") && exhausted.endsWith(").txt"))
        assertNotEquals("a (999).txt", exhausted)
    }

    private val strings = object : Strings {
        override fun get(id: Int, vararg args: Any) = "$id|" + args.joinToString(",")
        override fun plural(id: Int, quantity: Int, vararg args: Any) = "$id#$quantity"
    }

    @Test fun `size text switches unit at the binary boundaries and formats by locale`() {
        fun b(n: Long, l: Locale = Locale.US) = ProgressText.bytes(n, strings, l)
        assertEquals("${R.string.share_size_b}|0", b(0))
        assertEquals("${R.string.share_size_b}|1,023", b(1023))
        assertEquals("${R.string.share_size_kib}|1.0", b(1024))
        assertEquals("${R.string.share_size_mib}|1.0", b(1024L * 1024))
        assertEquals("${R.string.share_size_gib}|1.00", b(1024L * 1024 * 1024))
        assertEquals("${R.string.share_size_kib}|1,5", b(1536, Locale.GERMANY))
    }

    @Test fun `progress percentage is clamped and unknown totals have none`() {
        fun t(size: Long?, bytes: Long) = ShareProtocol.parseTransfers(list(entry().put("size", size ?: JSONObject.NULL).put("bytes", bytes))).single()
        assertEquals(50, ProgressText.percent(t(200, 100)))
        assertEquals(100, ProgressText.percent(t(200, 9999)))
        assertNull(ProgressText.percent(t(0, 0)))
        assertNull(ProgressText.percent(t(null, 5)))
    }

    @Test fun `every transfer state reads as a line and every share error and failure has wording`() {
        for (s in TransferState.entries) {
            val tr = ShareProtocol.parseTransfers(list(entry(state = s.name.lowercase()))).single()
            assertTrue(ProgressText.line(tr, strings).startsWith("${R.string.share_line}|"))
        }
        val failed = ShareProtocol.parseTransfers(list(entry(state = "failed").put("code", "timeout"))).single()
        assertTrue(ProgressText.line(failed, strings).contains("${R.string.share_state_failed_code}"))
        val noName = ShareProtocol.parseTransfers(list(entry().put("name", ""))).single()
        assertTrue(ProgressText.line(noName, strings).contains("${R.string.share_what_file}"))
        val link = ShareProtocol.parseTransfers(list(entry(kind = "url"))).single()
        assertTrue(ProgressText.line(link, strings).contains("${R.string.share_what_link}"))

        val generic = ShareText.error("brand_new", strings).substringBefore('|')
        for (code in listOf("not_running", "unknown_connection", "not_trusted", "unknown_transfer", "wrong_state", "invalid_fd", "too_large", "empty", "busy", "invalid_field")) {
            assertNotEquals("error $code", generic, ShareText.error(code, strings).substringBefore('|'))
        }
        val failure = ShareText.failure("brand_new", strings)
        for (code in listOf("cancelled", "timeout", "too_large", "pin_mismatch", "tls_failed", "truncated", "expired", null)) {
            assertNotEquals("failure $code", failure.substringBefore('|'), ShareText.failure(code, strings).substringBefore('|'))
        }
    }
}
