package dev.geekbridge.android

/** Size limits shared by the UI and the JNI request builders. Mirrors bridge-core defaults. */
object Limits {
    const val SMS_MAX_TEXT_CHARS = 2000
    const val SMS_MAX_ADDRESS_CHARS = 64
    const val CLIPBOARD_MAX_BYTES = 64 * 1024
    // Matches bridge-android MAX_REQUEST_BYTES (6 x default clipboard limit + 1 KiB).
    const val MAX_REQUEST_CHARS = 6 * CLIPBOARD_MAX_BYTES + 1024
    const val MAX_RESPONSE_CHARS = 64 * 1024
    // transport_status stays well under 100 KiB at the documented limits (docs/android-transport.md).
    const val MAX_TRANSPORT_RESPONSE_CHARS = 160 * 1024
    // transport_clipboard_wait can carry several 64 KiB text/HTML items.
    const val MAX_CLIPBOARD_RESPONSE_CHARS = 1024 * 1024
    const val MAX_ERROR_MESSAGE_CHARS = 200
    const val PREVIEW_FIELD_CHARS = 80
}

object Bounds {
    /** UTF-8 length without allocating the encoded array. Lone surrogates count as 3 bytes (conservative). */
    fun utf8Size(s: String): Int {
        var n = 0
        var i = 0
        while (i < s.length) {
            val c = s[i]
            when {
                c.code < 0x80 -> n += 1
                c.code < 0x800 -> n += 2
                Character.isHighSurrogate(c) && i + 1 < s.length && Character.isLowSurrogate(s[i + 1]) -> {
                    n += 4
                    i++
                }
                else -> n += 3
            }
            i++
        }
        return n
    }

    /**
     * Single-line, bounded text for display: control characters become spaces, runs of whitespace
     * collapse, and the result is cut to [max] chars (with an ellipsis) without splitting a
     * surrogate pair.
     */
    fun clip(raw: String?, max: Int): String {
        if (raw == null || max <= 0) return ""
        // Look at no more than needed so a huge provider field cannot cost memory/time.
        val head = if (raw.length > max * 4 + 16) raw.substring(0, max * 4 + 16) else raw
        val sb = StringBuilder(minOf(head.length, max + 1))
        var pendingSpace = false
        for (ch in head) {
            if (Character.isISOControl(ch) || Character.isWhitespace(ch)) {
                pendingSpace = sb.isNotEmpty()
            } else {
                if (pendingSpace) sb.append(' ')
                pendingSpace = false
                sb.append(ch)
            }
        }
        if (sb.length <= max && head.length == raw.length) return sb.toString()
        var end = (max - 1).coerceAtLeast(0).coerceAtMost(sb.length)
        if (end > 0 && Character.isHighSurrogate(sb[end - 1])) end--
        return sb.substring(0, end) + "…"
    }
}

/** Where a runtime permission stands, as far as this process knows. */
enum class PermissionUi { NotGranted, Granted, Denied, Blocked }

object PermissionLogic {
    /**
     * Interprets a permission result. When the system refuses and will not show its dialog again
     * ([shouldShowRationale] false) the only way forward is the app-settings screen.
     */
    fun afterRequest(granted: Boolean, shouldShowRationale: Boolean): PermissionUi = when {
        granted -> PermissionUi.Granted
        shouldShowRationale -> PermissionUi.Denied
        else -> PermissionUi.Blocked
    }

    fun current(granted: Boolean, previous: PermissionUi): PermissionUi = when {
        granted -> PermissionUi.Granted
        previous == PermissionUi.Granted -> PermissionUi.NotGranted // revoked while we were away
        else -> previous
    }
}
