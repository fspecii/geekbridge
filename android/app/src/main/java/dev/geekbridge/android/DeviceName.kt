package dev.geekbridge.android

import android.content.Context
import android.os.Build

/**
 * The name this phone announces and a computer shows for it. The user can change it (Settings: "This
 * phone's name"); until then it is the phone model. The rules mirror bridge-kde's `validate_device_name`
 * so the native side never refuses a name this screen accepted.
 */
object DeviceName {
    const val MAX_CHARS = 32
    private const val PREFS = "geekbridge_device_name"
    private const val KEY = "name"

    /** Characters peers strip from names; removed here so what is saved is what the computer shows. */
    private val STRIPPED = "\"',;:.!?()[]<>".toSet()

    sealed class Check {
        data class Ok(val name: String) : Check()
        data class Bad(val reason: String) : Check()
    }

    fun check(raw: String): Check {
        if (raw.any { Character.isISOControl(it) }) return Check.Bad("Use letters, numbers and spaces only.")
        if (raw.trim().codePointCount(0, raw.trim().length) > MAX_CHARS) return Check.Bad("Use at most $MAX_CHARS characters.")
        val cleaned = raw.filter { it !in STRIPPED && !Bounds.isInvisibleFormat(it) }.trim()
        if (cleaned.isEmpty()) return Check.Bad("Enter a name.")
        return Check.Ok(cleaned)
    }

    fun default(): String = check(Build.MODEL ?: "").let { if (it is Check.Ok) it.name else "Android" }

    fun current(ctx: Context): String {
        val saved = ctx.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(KEY, null)
        return saved?.let { (check(it) as? Check.Ok)?.name } ?: default()
    }

    /** Saves a validated name; returns what was stored, or null when [raw] is not acceptable. */
    fun save(ctx: Context, raw: String): String? {
        val ok = check(raw) as? Check.Ok ?: return null
        val stored = if (ok.name == default()) null else ok.name
        ctx.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().apply {
            if (stored == null) remove(KEY) else putString(KEY, stored)
        }.apply()
        return ok.name
    }
}
