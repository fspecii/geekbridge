package dev.geekbridge.android

import android.content.Context
import android.provider.Settings

/**
 * Design tokens for the dot-matrix visual language (monochrome ink + one red accent). Colours live in
 * res/values(-night)/colors.xml so light and dark follow the system; shapes, type and spacing live here so
 * every screen draws from one place. Values are dp / sp unless noted.
 */
object Tok {
    // Shapes: generous radii; buttons, chips and switches are pills.
    const val R_CARD = 28
    const val R_TILE = 30
    const val R_INPUT = 18
    const val R_ROW = 16
    const val R_PILL = 100

    // Spacing scale.
    const val S1 = 4
    const val S2 = 8
    const val S3 = 12
    const val S4 = 16
    const val S5 = 24
    const val S6 = 32

    // Type scale (sp). Sans for reading text, dot-matrix (DotText) for titles, numerals and status.
    const val T_CAPTION = 12f
    const val T_SMALL = 13f
    const val T_BODY = 15f
    const val T_LEAD = 16f

    /** Touch targets are never smaller than this. */
    const val TARGET = 48

    /** Dot pitch (dp) of the dot-matrix type: large display, medium numerals, small labels. */
    const val DOT_DISPLAY = 6.5f
    const val DOT_NUMERAL = 5f
    const val DOT_LABEL = 2.6f
}

/** Motion is subtle, one-shot and off when the user removed animations (system "remove animations"). */
object Motion {
    fun enabled(ctx: Context): Boolean =
        Settings.Global.getFloat(ctx.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) > 0f

    /** Duration scaled by the system setting; 0 means "do not animate, jump to the end state". */
    fun duration(ctx: Context, ms: Long): Long = if (enabled(ctx)) ms else 0L
}
