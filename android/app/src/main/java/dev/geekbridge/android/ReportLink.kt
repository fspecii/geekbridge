// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/**
 * "Report a problem": turns the already-redacted diagnostics report into a prefilled GitHub issue-form link.
 * Pure text work, no network: the browser opens the link. Twin of `crates/bridge-core/src/report.rs`; every name
 * that must match `.github/ISSUE_TEMPLATE/bug_report.yml` is in [Names], and ReportLinkTest fails if the
 * template stops declaring them.
 */
object ReportLink {
    object Names {
        const val TEMPLATE_FILE = "bug_report.yml"
        const val PARAM_TEMPLATE = "template"
        const val PARAM_TITLE = "title"
        const val FIELD_VERSION = "version"
        const val FIELD_DESKTOP = "desktop"
        const val FIELD_PHONE = "phone"
        const val FIELD_LOGS = "logs"
        val PREFILLED = listOf(FIELD_VERSION, FIELD_DESKTOP, FIELD_PHONE, FIELD_LOGS)
    }

    const val MAX_URL_BYTES = 6000
    const val DEFAULT_TITLE = "[Bug]"

    /** What the user is shown and what the link carries: exactly these fields. */
    data class Link(val url: String, val title: String, val fields: List<Pair<String, String>>, val truncated: Boolean) {
        fun preview(): String = buildString {
            append("Title: ").append(title).append('\n')
            for ((id, text) in fields) append("\n[").append(id).append("]\n").append(text).append('\n')
        }
    }

    /** `owner/name` or the built-in default; same rule as the updater. */
    fun repoOrDefault(repo: String?, default: String): String = repo?.trim()?.takeIf { UpdateModel.validRepo(it) } ?: default

    fun build(repo: String, title: String, shortFields: List<Pair<String, String>>, logs: String): Link {
        val cleanTitle = redact(title).take(120)
        val fields = shortFields
            .filter { (id, text) -> id in Names.PREFILLED && text.isNotBlank() }
            .map { (id, text) -> id to redact(text).take(200) }
        val lines = redact(logs).lines().map { it.trimEnd() }.filter { it.isNotEmpty() }
        var head = (lines.size + 1) / 2
        var tail = lines.size / 2
        var truncated = false
        while (true) {
            val text = buildString {
                append(lines.subList(0, head).joinToString("\n"))
                if (truncated) {
                    append("\n[... ${lines.size - head - tail} lines left out to fit the link ...]")
                }
                if (tail > 0) {
                    append('\n')
                    append(lines.subList(lines.size - tail, lines.size).joinToString("\n"))
                }
            }
            val all = if (lines.isEmpty()) fields else fields + (Names.FIELD_LOGS to text)
            val url = urlFor(repo, cleanTitle, all)
            if (url.length <= MAX_URL_BYTES || (head == 0 && tail == 0)) return Link(url, cleanTitle, all, truncated)
            truncated = true
            if (head >= tail && head > 0) head-- else tail = maxOf(0, tail - 1)
        }
    }

    private fun urlFor(repo: String, title: String, fields: List<Pair<String, String>>): String =
        buildString {
            append("https://github.com/").append(repo).append("/issues/new?")
            append(Names.PARAM_TEMPLATE).append('=').append(encode(Names.TEMPLATE_FILE))
            append('&').append(Names.PARAM_TITLE).append('=').append(encode(title))
            for ((id, text) in fields) append('&').append(id).append('=').append(encode(text))
        }

    fun encode(text: String): String = buildString {
        for (b in text.toByteArray(Charsets.UTF_8)) {
            val c = b.toInt() and 0xFF
            if (c in 'a'.code..'z'.code || c in 'A'.code..'Z'.code || c in '0'.code..'9'.code || c == '-'.code ||
                c == '_'.code || c == '.'.code || c == '~'.code
            ) {
                append(c.toChar())
            } else {
                append('%').append("%02X".format(c))
            }
        }
    }

    fun decode(text: String): String = java.net.URLDecoder.decode(text.replace("+", "%2B"), "UTF-8")

    /** Second line of defence: e-mail, paths, IP/MAC addresses, UUIDs, long hex ids and phone numbers. */
    fun redact(text: String): String = text.lines().joinToString("\n") { line ->
        var inPhone = false
        var prev = ""
        line.split(Regex("\\s+")).filter { it.isNotEmpty() }.joinToString(" ") { word ->
            val digitsOnly = word.all { it.isDigit() || it in "-()" }
            val startsNumber = word.length > 1 && word[0] == '+' && word[1].isDigit()
            val out = if (startsNumber || (inPhone && digitsOnly)) {
                inPhone = true
                "<redacted>"
            } else {
                inPhone = false
                redactWord(word, prev)
            }
            prev = word
            out
        }
    }

    private fun redactWord(word: String, prev: String): String {
        val core = word.trim { !(it.isLetterOrDigit() && it.code < 128 || it in "/\\~@:.+_-") }.trimEnd('.', ':')
        return if (core.isNotEmpty() && sensitive(core, prev)) word.replace(core, "<redacted>") else word
    }

    private fun isHex(c: Char) = c in '0'..'9' || c in 'a'..'f' || c in 'A'..'F'

    private fun sensitive(core: String, prev: String): Boolean {
        val hex = core.count(::isHex)
        val digits = core.count { it in '0'..'9' }
        val at = core.indexOf('@')
        val email = at > 0 && at < core.length - 1
        val unixPath = (core.startsWith("/") && core.indexOf('/', 1) > 0) || core.startsWith("~/")
        val winPath = core.contains('\\') || (core.length > 2 && core[1] == ':' && core[2] in "/\\")
        val fileUrl = core.startsWith("file:")
        val host = core.substringBefore(':')
        val hostParts = host.split('.')
        val v4 = hostParts.size == 4 && hostParts.all { p -> p.isNotEmpty() && p.length <= 3 && p.all { it in '0'..'9' } }
        val v6OrMac = core.count { it == ':' } >= 2 && hex >= 4 && !core.contains("//")
        val uuid = core.split('-').map { it.length } == listOf(8, 4, 4, 4, 12) && hex == 32
        val longHex = core.length >= 16 && hex * 10 >= core.length * 9
        val timestamp = prev.endsWith("unix_ms")
        val phone = (core.startsWith("+") && digits >= 6) || (digits >= 7 && !timestamp)
        return email || unixPath || winPath || fileUrl || v4 || v6OrMac || uuid || longHex || phone
    }
}
