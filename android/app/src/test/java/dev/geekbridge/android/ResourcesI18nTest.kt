// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * Keeps the string resources translatable: every format argument is numbered, plurals can be completed, nothing
 * hard-coded in a notification or dialog, and any locale (including the test-only fixture in
 * src/test/i18n-fixtures, the worked example of docs/translating.md) keeps the same keys and placeholders as English.
 */
class ResourcesI18nTest {
    private data class Res(val strings: Map<String, String>, val plurals: Map<String, Map<String, String>>)

    private fun dir(rel: String): File = File("src/$rel").takeIf { it.isDirectory } ?: File("app/src/$rel")

    private fun load(values: File): Res {
        val strings = LinkedHashMap<String, String>()
        val plurals = LinkedHashMap<String, Map<String, String>>()
        val b = DocumentBuilderFactory.newInstance().newDocumentBuilder()
        for (f in values.listFiles { x -> x.name.endsWith(".xml") }!!.sortedBy { it.name }) {
            val kids = b.parse(f).documentElement.childNodes
            for (i in 0 until kids.length) {
                val e = kids.item(i) as? Element ?: continue
                if (e.getAttribute("translatable") == "false") continue
                when (e.tagName) {
                    "string" -> strings[e.getAttribute("name")] = e.textContent
                    "plurals" -> {
                        val items = LinkedHashMap<String, String>()
                        val li = e.getElementsByTagName("item")
                        for (j in 0 until li.length) (li.item(j) as Element).let { items[it.getAttribute("quantity")] = it.textContent }
                        plurals[e.getAttribute("name")] = items
                    }
                }
            }
        }
        return Res(strings, plurals)
    }

    private val english by lazy { load(dir("main/res/values")) }

    /** Every res/values-<language> folder that ships, plus the test-only fixtures. Qualifiers like -night, -v31 are not languages. */
    private fun locales(): List<File> {
        val lang = Regex("values-(b\\+.+|[a-z]{2,3}(-r[A-Z]{2})?)")
        val shipped = dir("main/res").listFiles { f -> f.isDirectory && lang.matches(f.name) }.orEmpty().toList()
        val fixtures = dir("test/i18n-fixtures").listFiles { f -> f.isDirectory }.orEmpty().toList()
        return shipped + fixtures
    }

    /** `%1$s`, `%2$d`, `%%`; returns the numbered specs, or null for an unnumbered one (`%s`, `%d`). */
    private val spec = Regex("%(?:(\\d+)\\$)?[-#+ 0,(]*\\d*(?:\\.\\d+)?[a-zA-Z%]")

    private fun specs(s: String): List<String> =
        spec.findAll(s).map { it.value }.filter { it != "%%" && it != "%n" }.toList()

    private fun placeholders(s: String): List<String> = specs(s).sorted()

    @Test fun formatArgumentsAreNumberedWhenThereIsMoreThanOne() {
        val bad = ArrayList<String>()
        fun check(where: String, s: String) {
            val all = specs(s)
            if (all.size >= 2 && all.any { !it.matches(Regex("%\\d+\\$.*")) }) bad += "$where: $s"
            if (all.any { it.matches(Regex("%\\d+\\$.*")) } && all.any { !it.matches(Regex("%\\d+\\$.*")) }) bad += "$where (mixed): $s"
        }
        english.strings.forEach { (k, v) -> check(k, v) }
        english.plurals.forEach { (k, items) -> items.forEach { (q, v) -> check("$k.$q", v) } }
        assertTrue("Positional placeholders (%1\$s) are required for two or more arguments:\n" + bad.joinToString("\n"), bad.isEmpty())
    }

    @Test fun pluralsHaveOtherAndNoUnknownQuantity() {
        val known = setOf("zero", "one", "two", "few", "many", "other")
        for ((name, items) in english.plurals) {
            assertTrue("$name needs quantity=other", "other" in items)
            assertTrue("$name has an unknown quantity", known.containsAll(items.keys))
        }
    }

    @Test fun apostrophesAndAmpersandsAreEscapedForAapt() {
        // An unescaped ' inside an unquoted resource string is a build error or silently eaten text.
        val bad = english.strings.filter { (_, v) -> Regex("(?<!\\\\)'").containsMatchIn(v) && !(v.startsWith("\"") && v.endsWith("\"")) }
        assertTrue("Unescaped apostrophes: ${bad.keys}", bad.isEmpty())
    }

    @Test fun everyLocaleKeepsEnglishKeysAndPlaceholders() {
        for (loc in locales()) {
            val tr = load(loc)
            for ((k, v) in tr.strings) {
                val en = english.strings[k] ?: error("${loc.name}: $k is not an English key (ExtraTranslation)")
                assertEquals("${loc.name}: placeholders of $k", placeholders(en), placeholders(v))
            }
            for ((k, items) in tr.plurals) {
                val en = english.plurals[k] ?: error("${loc.name}: plurals $k is not an English key")
                assertTrue("${loc.name}: plurals $k needs quantity=other", "other" in items)
                val want = placeholders(en.getValue("other"))
                for ((q, v) in items) {
                    // zero/one/two/few/many may drop the count ("one app"), so only the arguments that are not the count must match.
                    val got = placeholders(v)
                    assertTrue("${loc.name}: $k.$q uses a placeholder English does not: $got vs $want", want.containsAll(got))
                }
            }
        }
    }

    @Test fun theFixtureIsPickedUpAndStillReordersArguments() {
        val eo = locales().firstOrNull { it.name == "values-eo" }
        assertTrue("test-only fixture values-eo is missing", eo != null)
        val tr = load(eo!!)
        assertEquals("Komputilo %2\$s petis vokon al %1\$s", tr.strings["call_dial_notice"])
        assertFalse("the fixture must never be a shipped resource folder", dir("main/res").resolve("values-eo").exists())
    }

    @Test fun userFacingTextIsNeverBuiltFromLiteralsInKotlin() {
        // Lint's HardcodedText only reads XML layouts and SetTextI18n only TextView.setText; notifications, toasts and
        // dialogs built in code would slip past both.
        val forbidden = listOf(
            Regex("""setContent(Title|Text)\(\s*"[^"]*[A-Za-z]{3}"""),
            Regex("""setTicker\(\s*"[^"]*[A-Za-z]{3}"""),
            Regex("""Toast\.makeText\([^,]+,\s*"[^"]*[A-Za-z]{3}"""),
            Regex("""\.set(Title|Message|PositiveButton|NegativeButton|NeutralButton)\(\s*"[^"]*[A-Za-z]{3}"""),
            Regex("""contentDescription\s*=\s*"[^"]*[A-Za-z]{3}"""),
            Regex("""setContentDescription\(\s*"[^"]*[A-Za-z]{3}"""),
            Regex("""setHint\(\s*"[^"]*[A-Za-z]{3}"""),
            Regex("""NotificationChannel\([^,]+,\s*"[^"]*[A-Za-z]{3}"""),
        )
        val bad = ArrayList<String>()
        for (f in dir("main/java").walkTopDown().filter { it.extension == "kt" }) {
            f.readLines().forEachIndexed { n, line ->
                if (line.trimStart().startsWith("//") || line.trimStart().startsWith("*")) return@forEachIndexed
                if (forbidden.any { it.containsMatchIn(line) }) bad += "${f.name}:${n + 1}: ${line.trim()}"
            }
        }
        assertTrue("Hard-coded user-facing text, move it to res/values:\n" + bad.joinToString("\n"), bad.isEmpty())
    }

    @Test fun bidiIsolationWrapsNamesAndNumbers() {
        assertEquals("⁨مكتب⁩", Bidi.isolate("مكتب"))
        assertEquals("⁦+15550100⁩", Bidi.ltr("+15550100"))
        val s = CallRules.dialNotice("+15550100", "مكتب", TestStrings)
        // The number and the Arabic name each sit between their own isolate and pop, so neither can reorder the other.
        assertTrue(s.contains("⁦+15550100⁩"))
        assertTrue(s.endsWith("⁨مكتب⁩"))
    }

    @Test fun dialNoticeNeverLetsAComputerNameInjectDirectionControls() {
        val s = CallRules.dialNotice("5550100", "x‮evil⁩", TestStrings)
        assertFalse(s.contains('‮'))
        assertEquals(2, s.count { it == '⁩' })
    }
}
