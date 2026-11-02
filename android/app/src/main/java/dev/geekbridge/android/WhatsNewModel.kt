// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Context
import org.json.JSONObject

/**
 * The few lines shown once after an update (the AFTER view; the updater's release notes are the BEFORE view).
 * [highlights] is plain text only, at most [WhatsNewModel.MAX_HIGHLIGHTS] lines.
 */
data class WhatsNew(val version: String, val highlights: List<String>)

/** What to do at startup, given the stored version code and the running one. */
enum class WhatsNewDecision {
    /** Newer than last time: show the card; the version is remembered when it is dismissed. */
    Show,

    /** Nothing to show; remember the running version now (fresh install, downgrade, damaged state). */
    Record,

    /** Same version: nothing to do. */
    Same,
}

/**
 * Pure logic of the "What's new" card. The text comes from the bundled asset `whats_new.json`, generated from
 * CHANGELOG.md by `scripts/gen-whats-new.sh` and checked for drift by WhatsNewModelTest. It is re-sanitised here
 * because it is cheap and the card must never show anything but plain text.
 */
object WhatsNewModel {
    const val ASSET = "whats_new.json"
    const val MAX_HIGHLIGHTS = 6
    const val MAX_LINE_CHARS = 140
    private const val MAX_ASSET_BYTES = 8 * 1024

    /**
     * [json] is the asset. A missing, oversized, malformed or stale asset (its version is not [versionName]) gives the
     * single "See the release notes" fallback line, so the card is never wrong about which release it describes.
     */
    fun parse(s: Strings, json: String?, versionName: String): WhatsNew {
        val lines = try {
            if (json == null || json.length > MAX_ASSET_BYTES) throw IllegalArgumentException("size")
            val o = JSONObject(json)
            require(o.optString("version") == versionName) { "stale" }
            val arr = o.getJSONArray("highlights")
            (0 until minOf(arr.length(), 64)).mapNotNull { arr.optString(it, "").takeIf { s -> s.isNotEmpty() } }
                .map(::sanitise).filter { it.isNotEmpty() }.take(MAX_HIGHLIGHTS)
        } catch (_: Exception) {
            emptyList()
        }
        return WhatsNew(versionName, lines.ifEmpty { listOf(s.get(R.string.whatsnew_fallback)) })
    }

    /** One line of plain text: markdown punctuation, markup and control / bidi characters removed, at most [MAX_LINE_CHARS]. */
    fun sanitise(raw: String): String {
        val src = raw.take(4096)
        val sb = StringBuilder(src.length)
        var inTag = false
        var i = 0
        val noLinks = stripLinks(src)
        while (i < noLinks.length) {
            val c = noLinks[i++]
            when {
                c == '<' -> inTag = true
                c == '>' && inTag -> inTag = false
                inTag -> {}
                c in "`*_~#>|\\[]" -> {}
                c.code in 0x200b..0x200f || c.code in 0x202a..0x202e || c.code in 0x2060..0x2064 || c.code in 0x2066..0x2069 || c.code == 0xfeff -> {}
                Character.isISOControl(c) || c.code == 0x2028 || c.code == 0x2029 -> sb.append(' ')
                else -> sb.append(c)
            }
        }
        val collapsed = sb.toString().trim().split(Regex("\\s+")).filter { it.isNotEmpty() }.joinToString(" ")
        if (collapsed.length <= MAX_LINE_CHARS) return collapsed
        var cut = collapsed.take(MAX_LINE_CHARS - 1)
        val space = cut.lastIndexOf(' ')
        if (space > 0) cut = cut.substring(0, space)
        return cut + "\u2026"
    }

    /** `[text](url)` and `![alt](url)` become `text`; a bracket with no following `(...)` stays (and is dropped later). */
    private fun stripLinks(s: String): String {
        val out = StringBuilder(s.length)
        var pos = 0
        while (true) {
            val open = s.indexOf('[', pos)
            if (open < 0) break
            val close = s.indexOf(']', open + 1)
            val ok = close > 0 && close + 1 < s.length && s[close + 1] == '(' && s.indexOf(')', close + 2) > 0
            if (!ok) {
                out.append(s, pos, open + 1)
                pos = open + 1
                continue
            }
            out.append(s, pos, open)
            if (out.endsWith("!")) out.setLength(out.length - 1)
            out.append(s, open + 1, close)
            pos = s.indexOf(')', close + 2) + 1
        }
        out.append(s, pos, s.length)
        return out.toString()
    }

    /**
     * [lastSeen]: the stored version code, null when none was stored. [damaged]: state exists but cannot be read.
     * [hadPriorUse]: the app was used before this value existed (setup was finished), so a missing value means
     * "updated from a build that did not record it", not a fresh install.
     */
    fun decide(lastSeen: Int?, current: Int, hadPriorUse: Boolean, damaged: Boolean = false): WhatsNewDecision = when {
        damaged || current <= 0 -> WhatsNewDecision.Record
        lastSeen == null -> if (hadPriorUse) WhatsNewDecision.Show else WhatsNewDecision.Record
        lastSeen < current -> if (lastSeen > 0) WhatsNewDecision.Show else WhatsNewDecision.Record
        lastSeen == current -> WhatsNewDecision.Same
        else -> WhatsNewDecision.Record // downgrade: remember the lower version, so the next upgrade shows the card again
    }

    /** The release page: `https://github.com/<repo>/releases/tag/v<version>`; null for a repo or version that is not well formed. */
    fun releaseUrl(repo: String, versionName: String): String? {
        if (!UpdateModel.validRepo(repo)) return null
        val v = AppVersion.parse(versionName) ?: return null
        return "https://github.com/$repo/releases/tag/v$v"
    }
}

/**
 * The stored "last seen" version code, in its own preferences file (`geekbridge_whatsnew`) with its own
 * `state_version` (docs/state-formats.md). Only gains keys additively; a file written by a NEWER state version is
 * read for nothing and never rewritten.
 */
class WhatsNewStore(private val kv: KeyValues) {
    /** Result of [read]: the stored code (null when none) and whether the file could not be understood. */
    data class Stored(val lastSeen: Int?, val damaged: Boolean, val readOnly: Boolean)

    fun read(): Stored = try {
        val version = kv.getInt(KEY_STATE, 0)
        when {
            version > STATE_VERSION -> Stored(null, damaged = true, readOnly = true)
            kv.getInt(KEY_LAST, MISSING) == MISSING -> Stored(null, damaged = false, readOnly = false)
            else -> kv.getInt(KEY_LAST, MISSING).let { if (it <= 0) Stored(null, true, false) else Stored(it, false, false) }
        }
    } catch (_: Exception) {
        Stored(null, damaged = true, readOnly = false) // a wrongly typed value: ClassCastException
    }

    fun write(versionCode: Int) {
        if (kv.getIntSafe(KEY_STATE) > STATE_VERSION) return
        kv.putInt(KEY_STATE, STATE_VERSION)
        kv.putInt(KEY_LAST, versionCode)
    }

    private fun KeyValues.getIntSafe(key: String) = try { getInt(key, 0) } catch (_: Exception) { 0 }

    companion object {
        const val FILE = "geekbridge_whatsnew"
        const val KEY_STATE = "state_version"
        const val KEY_LAST = "last_seen_code"
        const val STATE_VERSION = 1
        private const val MISSING = Int.MIN_VALUE
    }
}

/**
 * Process-wide holder. [init] decides once per process, at process start (before any screen), so a fresh install
 * (setup not finished yet) is recorded silently and finishing the setup wizard later does not turn it into an "update".
 * The card stays pending until it is dismissed; a process that dies first just decides the same way next time.
 */
object WhatsNewRuntime {
    @Volatile private var pending: WhatsNew? = null
    private var store: WhatsNewStore? = null

    fun pending(): WhatsNew? = pending

    @Synchronized
    fun init(ctx: Context) {
        if (store != null) return
        val app = ctx.applicationContext
        val s = WhatsNewStore(StateMigrations.kv(app.getSharedPreferences(WhatsNewStore.FILE, Context.MODE_PRIVATE))).also { store = it }
        val stored = s.read()
        // "Used before": setup was finished (same preferences file as AppState.setupFinished).
        val used = app.getSharedPreferences("geekbridge_state", Context.MODE_PRIVATE).getBoolean("setup_finished", false)
        when (WhatsNewModel.decide(stored.lastSeen, BuildConfig.VERSION_CODE, used, stored.damaged)) {
            WhatsNewDecision.Show -> pending = WhatsNewModel.parse(app.strings, readAsset(app), BuildConfig.VERSION_NAME)
            WhatsNewDecision.Record -> s.write(BuildConfig.VERSION_CODE)
            WhatsNewDecision.Same -> {}
        }
    }

    @Synchronized
    fun dismiss() {
        pending = null
        store?.write(BuildConfig.VERSION_CODE)
    }

    private fun readAsset(ctx: Context): String? = try {
        ctx.assets.open(WhatsNewModel.ASSET).use { it.readBytes().toString(Charsets.UTF_8) }
    } catch (_: Exception) {
        null
    }
}
