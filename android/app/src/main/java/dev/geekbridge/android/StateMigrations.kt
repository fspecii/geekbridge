// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Context

/** One preferences file, as much of it as the migrations and backed blobs need. Tests fake it with a map. */
interface KeyValues {
    fun getString(key: String): String?
    fun putString(key: String, value: String?)
    fun getInt(key: String, default: Int): Int
    fun putInt(key: String, value: Int)
}

/**
 * The one place that versions the app's persisted state (docs/state-formats.md). The version lives in the
 * `gb_state` preferences file under `schema_version`; a file without it is version 1 (every format shipped so
 * far). A build runs [run] first thing at process start: it applies the ordered [Step]s from the stored version
 * up to [CURRENT], saving the version after each step, so an interrupted run resumes and a finished one is a no-op.
 *
 * A stored version NEWER than this build understands (a downgrade) is never touched: no step runs, the version is
 * not rewritten, [readOnly] turns on so blob writers keep their hands off, and [diagnostics] says why. The data
 * stays as the newer build left it. Additive changes (new keys) do not need a step: unknown keys are ignored.
 */
object StateMigrations {
    const val CURRENT = 1
    const val FILE = "gb_state"
    const val KEY = "schema_version"

    /** Upgrades stored state from [from] to [from]+1. Must be idempotent (it can run twice after a crash). */
    class Step(val from: Int, val name: String, val run: (open: (String) -> KeyValues) -> Unit)

    /** Steps from version 1 to [CURRENT]. Empty: version 1 is the first versioned format. */
    val STEPS: List<Step> = emptyList()

    data class Outcome(val from: Int, val to: Int, val applied: List<String>, val readOnly: Boolean, val message: String?)

    @Volatile var last: Outcome? = null
        private set

    /** True after a downgrade: state written by a newer build is present and must not be overwritten. */
    val readOnly: Boolean get() = last?.readOnly == true

    /** A support-report line, or null when everything is normal. */
    fun diagnostics(): String? = last?.message

    fun run(ctx: Context): Outcome = run({ name -> prefs(ctx.applicationContext, name) })

    fun run(
        open: (String) -> KeyValues,
        steps: List<Step> = STEPS,
        current: Int = CURRENT,
    ): Outcome {
        val meta = open(FILE)
        val stored = meta.getInt(KEY, 1).coerceAtLeast(1)
        if (stored > current) {
            return Outcome(stored, stored, emptyList(), true, "saved data is from a newer app version ($stored, this build reads $current): kept as is, not modified").also { last = it }
        }
        val applied = ArrayList<String>()
        var v = stored
        try {
            while (v < current) {
                val step = steps.firstOrNull { it.from == v } ?: error("no migration from $v")
                step.run(open)
                v++
                meta.putInt(KEY, v)
                applied += step.name
            }
        } catch (e: Exception) {
            return Outcome(stored, v, applied, true, "saved data could not be upgraded past version $v (${e.message}): kept as is").also { last = it }
        }
        if (meta.getInt(KEY, 0) != current) meta.putInt(KEY, current)
        return Outcome(stored, current, applied, false, null).also { last = it }
    }

    private fun prefs(ctx: Context, name: String): KeyValues = kv(ctx.getSharedPreferences(name, Context.MODE_PRIVATE))

    fun kv(p: android.content.SharedPreferences): KeyValues {
        return object : KeyValues {
            override fun getString(key: String) = p.getString(key, null)
            override fun putString(key: String, value: String?) { p.edit().apply { if (value == null) remove(key) else putString(key, value) }.apply() }
            override fun getInt(key: String, default: Int) = p.getInt(key, default)
            override fun putInt(key: String, value: Int) { p.edit().putInt(key, value).commit() }
        }
    }
}

/**
 * A JSON blob in preferences with one generation of backup: the last readable value is copied to `<key>.bak`
 * before it is replaced, and a value that [valid] rejects (truncated, garbled, empty) falls back to that backup,
 * else to "". Never throws. [note] receives a diagnostics line when the fallback was used. Writes are skipped
 * while [StateMigrations.readOnly] so a downgraded build cannot clobber newer data.
 */
class BackedBlob(
    private val kv: KeyValues,
    private val key: String,
    private val valid: (String) -> Boolean,
    private val readOnly: () -> Boolean = { StateMigrations.readOnly },
    private val note: (String) -> Unit = { lastNote = it },
) {
    fun read(): String {
        val main = kv.getString(key)
        if (main != null && valid(main)) return main
        if (main == null) return "" // missing is normal; present-but-damaged (even empty) falls back below
        val bak = kv.getString("$key.bak")
        if (bak != null && valid(bak)) {
            note("$key was unreadable; restored the previous copy")
            return bak
        }
        note("$key was unreadable and had no backup; started empty")
        return ""
    }

    fun write(text: String) {
        if (readOnly()) return
        val main = kv.getString(key)
        if (main != null && main.isNotEmpty() && valid(main)) kv.putString("$key.bak", main)
        kv.putString(key, text)
    }

    companion object {
        @Volatile var lastNote: String? = null
        fun jsonObject(s: String) = try { org.json.JSONObject(s); true } catch (_: Exception) { false }
        fun jsonArray(s: String) = try { org.json.JSONArray(s.ifBlank { "[]" }); true } catch (_: Exception) { false }
    }
}
