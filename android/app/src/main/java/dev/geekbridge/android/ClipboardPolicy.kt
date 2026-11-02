// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/*
 * Clipboard rules that do not depend on Android types, so they run in JVM tests.
 *
 * The phone never pushes its clipboard on its own. The computer asks ("pull"); the phone answers only
 * with an item that was copied within a freshness window, while it is unlocked, and that is not marked
 * sensitive. Android 10+ also stops apps from reading the clipboard in the background, so "the item" is
 * whatever this app was last allowed to look at (see [PullGate.CACHE_TRUST_MS]) or what a tap shows it now.
 */

object ClipboardLimits {
    const val DEFAULT_WINDOW_MS = 30_000L
    const val MIN_WINDOW_MS = 5_000L
    const val MAX_WINDOW_MS = 10 * 60_000L
    const val MAX_TEXT_BYTES = 64 * 1024
    const val DEFAULT_MAX_IMAGE_BYTES = 8L * 1024 * 1024
    const val MIN_IMAGE_BYTES = 256L * 1024
    const val HARD_MAX_IMAGE_BYTES = 32L * 1024 * 1024

    fun clampWindow(ms: Long): Long = ms.coerceIn(MIN_WINDOW_MS, MAX_WINDOW_MS)
    fun clampImageBytes(n: Long): Long = n.coerceIn(MIN_IMAGE_BYTES, HARD_MAX_IMAGE_BYTES)

    /** Offered in the settings UI. */
    val WINDOW_CHOICES_MS = listOf(10_000L, 30_000L, 60_000L, 120_000L, 300_000L)
}

enum class ClipKind(val wire: String) {
    Text("text"), Url("url"), Html("html"), Image("image");

    companion object {
        fun parse(s: String?): ClipKind? = entries.firstOrNull { it.wire == s }
    }
}

/**
 * What the clipboard held when the app was allowed to look. Holds user text in memory only; it is
 * dropped after [PullGate.CACHE_TRUST_MS], when used, and when the link stops.
 */
class ClipSnapshot(
    val kind: ClipKind,
    val text: String?,
    val html: String?,
    /** Opaque content URI of a PNG/JPEG, resolved by the Android layer. */
    val imageUri: String?,
    val imageMime: String?,
    /** When the item was copied, if Android says (ClipDescription.getTimestamp); else the capture time. */
    val copiedAtMs: Long,
    val capturedAtMs: Long,
    val sensitive: Boolean,
) {
    override fun toString() = "ClipSnapshot($kind, ${text?.length ?: 0} chars)" // never the content
}

object ClipClassifier {
    private val imageTypes = setOf("image/png", "image/jpeg")
    private val urlPattern = Regex("https?://\\S{1,4090}")

    fun isUrl(s: String): Boolean = s.length in 9..4096 && urlPattern.matches(s.trim())

    fun imageMimeOf(mime: String?): String? = mime?.lowercase()?.takeIf { it in imageTypes }

    /** Text with HTML wins over plain; a single URL is a URL; an image URI wins over everything. */
    fun classify(text: String?, html: String?, imageMime: String?): ClipKind? = when {
        imageMimeOf(imageMime) != null -> ClipKind.Image
        !text.isNullOrEmpty() && !html.isNullOrEmpty() -> ClipKind.Html
        !text.isNullOrEmpty() -> if (isUrl(text)) ClipKind.Url else ClipKind.Text
        else -> null
    }
}

sealed interface Verdict {
    data class Send(val snapshot: ClipSnapshot) : Verdict

    /** [reason] is one of the wire reasons: locked, expired, empty, needs_gesture, disabled, too_large, sensitive. */
    data class Refuse(val reason: String) : Verdict
}

object PullGate {
    /**
     * A snapshot taken earlier is trusted without a tap for this long only: Android hides clipboard
     * changes from a backgrounded app, so something newer may exist. Longer than this we ask the user.
     */
    const val CACHE_TRUST_MS = 10_000L

    /** The freshness limit that applies: the user's setting, narrowed by what the computer asked for. */
    fun effectiveWindowMs(settingMs: Long, requestedMs: Long?): Long {
        val s = ClipboardLimits.clampWindow(settingMs)
        return if (requestedMs == null) s else minOf(s, ClipboardLimits.clampWindow(requestedMs))
    }

    /**
     * Decide an answer to a pull. [snapshot] is what the app has; [fresh] means it was read just now, in a
     * focused window, because the user tapped. Locked is checked before anything else and the snapshot is
     * never looked at in that case.
     */
    fun decide(
        enabled: Boolean,
        locked: Boolean,
        snapshot: ClipSnapshot?,
        fresh: Boolean,
        nowMs: Long,
        settingMs: Long,
        requestedMs: Long?,
    ): Verdict {
        if (!enabled) return Verdict.Refuse("disabled")
        if (locked) return Verdict.Refuse("locked")
        if (snapshot == null) return Verdict.Refuse(if (fresh) "empty" else "needs_gesture")
        if (snapshot.sensitive) return Verdict.Refuse("sensitive")
        if (!fits(snapshot)) return Verdict.Refuse("too_large")
        if (!fresh && nowMs - snapshot.capturedAtMs > CACHE_TRUST_MS) return Verdict.Refuse("needs_gesture")
        val age = (nowMs - snapshot.copiedAtMs).coerceAtLeast(0)
        if (age > effectiveWindowMs(settingMs, requestedMs)) {
            return Verdict.Refuse(if (fresh) "expired" else "needs_gesture")
        }
        return Verdict.Send(snapshot)
    }

    /** An explicit "send this" by the user: no age limit (they chose it), but never locked or sensitive. */
    fun decideExplicit(enabled: Boolean, locked: Boolean, snapshot: ClipSnapshot?): Verdict = when {
        !enabled -> Verdict.Refuse("disabled")
        locked -> Verdict.Refuse("locked")
        snapshot == null -> Verdict.Refuse("empty")
        snapshot.sensitive -> Verdict.Refuse("sensitive")
        !fits(snapshot) -> Verdict.Refuse("too_large")
        else -> Verdict.Send(snapshot)
    }

    private fun fits(s: ClipSnapshot): Boolean {
        if (s.kind == ClipKind.Image) return true // size is checked against the real stream
        val text = s.text ?: return false
        if (Bounds.utf8Size(text) > ClipboardLimits.MAX_TEXT_BYTES) return false
        val html = s.html
        return html == null || Bounds.utf8Size(html) <= ClipboardLimits.MAX_TEXT_BYTES
    }
}

/** What the user can see and change. Persisted by the Android layer. */
data class ClipSettings(
    val enabled: Boolean = false,
    val windowMs: Long = ClipboardLimits.DEFAULT_WINDOW_MS,
    val maxImageBytes: Long = ClipboardLimits.DEFAULT_MAX_IMAGE_BYTES,
) {
    fun normalized() = copy(
        windowMs = ClipboardLimits.clampWindow(windowMs),
        maxImageBytes = ClipboardLimits.clampImageBytes(maxImageBytes),
    )
}

/** Plain-language lines for the settings UI; what Android allows is stated, not hidden. */
object ClipboardText {
    const val HONEST_NOTE =
        "Android doesn't let apps read the clipboard in the background. GeekBridge looks at it only when you open " +
            "the app, tap the Clipboard tile or notification, or use \"Send to computer\" in the text menu. " +
            "Text from your computer is placed on this phone automatically."

    fun windowLabel(ms: Long): String {
        val s = ClipboardLimits.clampWindow(ms) / 1000
        return if (s >= 60 && s % 60 == 0L) "${s / 60} min" else "$s s"
    }

    fun summary(s: ClipSettings): String = if (!s.enabled) {
        "Off. Nothing is shared with your computer."
    } else {
        "On. Your computer can ask for something you copied in the last ${windowLabel(s.windowMs)}, " +
            "and only while this phone is unlocked."
    }

    fun refusal(reason: String): String = when (reason) {
        "locked" -> "Unlock your phone first."
        "expired" -> "That was copied too long ago. Copy it again."
        "empty" -> "The clipboard is empty."
        "disabled" -> "Clipboard sync is off."
        "too_large" -> "That item is too large to send."
        "sensitive" -> "That item is marked sensitive, so it isn't sent."
        "not_connected" -> "Not connected to a computer."
        else -> "Couldn't send the clipboard."
    }
}
