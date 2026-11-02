// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ReportLinkTest {
    /** Gradle runs unit tests in android/app; the template lives at the repository root. */
    private val template: String = generateSequence(File("").absoluteFile) { it.parentFile }
        .map { File(it, ".github/ISSUE_TEMPLATE/bug_report.yml") }
        .first { it.isFile }.readText()

    private val ids = template.lines().map { it.trim() }.filter { it.startsWith("id:") }.map { it.removePrefix("id:").trim() }

    @Test fun prefilledFieldsExistInIssueTemplate() {
        for (f in ReportLink.Names.PREFILLED) assertTrue("bug_report.yml has no field id `$f`: $ids", f in ids)
    }

    @Test fun templateFileNameMatches() = assertEquals("bug_report.yml", ReportLink.Names.TEMPLATE_FILE)

    @Test fun urlShapeAndRoundTrip() {
        val l = ReportLink.build("fspecii/geekbridge", "[Bug]", listOf("version" to "phone 0.1.0"), "line one\nline two")
        assertTrue(l.url, l.url.startsWith("https://github.com/fspecii/geekbridge/issues/new?template=bug_report.yml&title=%5BBug%5D&version=phone%200.1.0"))
        assertEquals("line one\nline two", ReportLink.decode(l.url.substringAfter("&logs=")))
        assertFalse(l.truncated)
    }

    @Test fun unknownFieldsAreNeverSent() {
        val l = ReportLink.build("a/b", "t", listOf("secret" to "x", "phone" to "Pixel"), "")
        assertFalse(l.url.contains("secret"))
        assertEquals(1, l.fields.size)
    }

    @Test fun longReportIsCappedAndKeepsBothEnds() {
        val lines = listOf("== Versions ==") + (0 until 400).map { "filler line $it with some words" } + "last issue: timed out)"
        val l = ReportLink.build("a/b", "t", emptyList(), lines.joinToString("\n"))
        assertTrue("${l.url.length}", l.url.length <= ReportLink.MAX_URL_BYTES)
        assertTrue(l.truncated)
        val logs = l.fields.single().second
        assertTrue(logs.startsWith("== Versions =="))
        assertTrue(logs.endsWith("timed out)"))
    }

    @Test fun repoOverrideIsValidated() {
        assertEquals("me/fork", ReportLink.repoOrDefault(" me/fork ", "d/d"))
        assertEquals("d/d", ReportLink.repoOrDefault("a/b/c", "d/d"))
        assertEquals("d/d", ReportLink.repoOrDefault("evil.com/../x", "d/d"))
        assertEquals("d/d", ReportLink.repoOrDefault(null, "d/d"))
    }

    @Test fun seededPersonalDataNeverReachesTheLink() {
        val report = """
            GeekBridge Android support report
            app: 0.1.0 (code 1)
            phone: Google Pixel 8, Android 15 (API 35)
            last issue: refused by 192.168.1.77:1716 for +49 170 1234567 alice@example.com /home/alice/Documents/secret.txt
            saved data: C:\Users\Alice\AppData\x device 0123456789abcdef id 3f2a9c1e-1111-2222-3333-444455556666
            mac aa:bb:cc:dd:ee:ff v6 fe80::1234:abcd call 01234567890
        """.trimIndent()
        val l = ReportLink.build("fspecii/geekbridge", "[Bug]", listOf("phone" to "Google Pixel 8, Android 15"), report)
        val sent = l.url + "\n" + ReportLink.decode(l.url)
        for (secret in listOf(
            "Alice", "192.168", "1234567", "170", "example.com", "/home", "Documents", "secret.txt", "Users",
            "0123456789abcdef", "3f2a9c1e", "aa:bb", "fe80", "01234567890",
        )) assertFalse("`$secret` leaked: $sent", sent.contains(secret))
        assertTrue(sent.contains("Pixel 8"))
    }

    @Test fun redactKeepsReportVocabulary() {
        val line = "app: 0.1.0 (code 1) phone: Google Pixel 8, Android 15 (API 35) battery: OK"
        assertEquals(line, ReportLink.redact(line))
    }
}
