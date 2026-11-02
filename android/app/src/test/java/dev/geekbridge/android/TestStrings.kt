// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import java.util.Locale
import javax.xml.parsers.DocumentBuilderFactory
import org.w3c.dom.Element

/**
 * A [Strings] for JVM tests: resolves resource ids through R.string / R.plurals reflection and reads the English
 * the res/values XML files, so model tests assert the real shipped English text with no Robolectric.
 */
object TestStrings : Strings {
    private val strings = HashMap<String, String>()
    private val plurals = HashMap<String, Map<String, String>>()
    private val stringNames: Map<Int, String> by lazy { names("dev.geekbridge.android.R\$string") }
    private val pluralNames: Map<Int, String> by lazy { names("dev.geekbridge.android.R\$plurals") }

    init {
        val dir = File("src/main/res/values").takeIf { it.isDirectory } ?: File("app/src/main/res/values")
        val builder = DocumentBuilderFactory.newInstance().newDocumentBuilder()
        for (f in dir.listFiles { x -> x.name.endsWith(".xml") }!!.sortedBy { it.name }) {
            val root = builder.parse(f).documentElement
            val kids = root.childNodes
            for (i in 0 until kids.length) {
                val e = kids.item(i) as? Element ?: continue
                if (e.getAttribute("translatable") == "false" && e.tagName != "string") continue
                when (e.tagName) {
                    "string" -> strings[e.getAttribute("name")] = unescape(e.textContent)
                    "plurals" -> {
                        val items = HashMap<String, String>()
                        val li = e.getElementsByTagName("item")
                        for (j in 0 until li.length) {
                            val it = li.item(j) as Element
                            items[it.getAttribute("quantity")] = unescape(it.textContent)
                        }
                        plurals[e.getAttribute("name")] = items
                    }
                }
            }
        }
    }

    private fun names(cls: String): Map<Int, String> =
        Class.forName(cls).fields.associate { (it.get(null) as Int) to it.name }

    /** Android resource escaping: backslash sequences, and a quoted run keeps its spaces. */
    private fun unescape(raw: String): String {
        var s = raw.trim()
        if (s.length >= 2 && s.startsWith("\"") && s.endsWith("\"")) s = s.substring(1, s.length - 1)
        val out = StringBuilder()
        var i = 0
        while (i < s.length) {
            val c = s[i]
            if (c == '\\' && i + 1 < s.length) {
                when (val n = s[i + 1]) {
                    'n' -> out.append('\n'); 't' -> out.append('\t')
                    'u' -> { out.append(s.substring(i + 2, i + 6).toInt(16).toChar()); i += 4 }
                    else -> out.append(n)
                }
                i += 2
            } else { out.append(c); i++ }
        }
        return out.toString()
    }

    override fun get(id: Int, vararg args: Any): String {
        val name = stringNames[id] ?: error("no R.string for id $id")
        val fmt = strings[name] ?: error("no English string $name")
        return if (args.isEmpty()) fmt else String.format(Locale.ENGLISH, fmt, *args)
    }

    override fun plural(id: Int, quantity: Int, vararg args: Any): String {
        val name = pluralNames[id] ?: error("no R.plurals for id $id")
        val items = plurals[name] ?: error("no English plurals $name")
        val fmt = (if (quantity == 1) items["one"] else null) ?: items["other"] ?: error("plurals $name has no other")
        return String.format(Locale.ENGLISH, fmt, *args)
    }
}
