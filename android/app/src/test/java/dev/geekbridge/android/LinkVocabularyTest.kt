// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import org.junit.Assert.assertTrue
import org.junit.Test

/** The phone-to-computer relationship is "link" everywhere a user reads it. "Pair" is kept for Bluetooth only. */
class LinkVocabularyTest {
    private val dir = File("src/main/res/values").takeIf { it.isDirectory } ?: File("app/src/main/res/values")

    // Developer card strings (Diagnostics, debug builds) and the Bluetooth call-audio helper are allowed to say pair.
    private fun allowed(name: String, value: String) =
        name.startsWith("dev_") || name.startsWith("link_") ||
            "Bluetooth" in value || "bluetooth" in value || "hands-free" in value || name == "qr_link_hint" || name == "setup_ca_pair" ||
            name.startsWith("upd_channel")

    @Test fun userFacingTextSaysLinkNotPair() {
        val bad = ArrayList<String>()
        val re = Regex("""<string name="([^"]+)"[^>]*>(.*?)</string>""")
        for (f in dir.listFiles { x -> x.name.endsWith(".xml") }!!.sortedBy { it.name }) {
            for (line in f.readLines()) {
                val m = re.find(line) ?: continue
                val (name, value) = m.destructured
                if (Regex("(?i)pair").containsMatchIn(value) && !allowed(name, value)) bad += "${f.name}: $name: $value"
            }
        }
        assertTrue("these say pair:\n" + bad.joinToString("\n"), bad.isEmpty())
    }

    /** First-run copy does not send a newcomer to another app: no "KDE Connect" in the setup or link-a-computer strings. */
    @Test fun firstRunCopyDoesNotMentionKdeConnect() {
        val re = Regex("""<string name="((?:setup_|pair_|qr_)[^"]*)"[^>]*>(.*?)</string>""")
        val bad = ArrayList<String>()
        for (f in dir.listFiles { x -> x.name.endsWith(".xml") }!!) {
            for (line in f.readLines()) {
                val m = re.find(line) ?: continue
                if ("KDE Connect" in m.groupValues[2]) bad += "${f.name}: ${m.groupValues[1]}"
            }
        }
        assertTrue("first-run copy mentions KDE Connect:\n" + bad.joinToString("\n"), bad.isEmpty())
    }
}
