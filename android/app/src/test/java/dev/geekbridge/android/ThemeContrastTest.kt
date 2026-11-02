// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import kotlin.math.pow
import org.junit.Assert.assertTrue
import org.junit.Test

/** WCAG 2.x contrast of the theme's text colours against the surfaces they are drawn on, both themes. */
class ThemeContrastTest {
    private fun load(path: String): Map<String, Int> {
        val f = listOf(File("src/main/res/$path"), File("app/src/main/res/$path")).first { it.exists() }
        return Regex("""<color name="(\w+)">#([0-9A-Fa-f]{6})</color>""").findAll(f.readText())
            .associate { it.groupValues[1] to it.groupValues[2].toInt(16) }
    }

    private fun lum(rgb: Int): Double {
        fun ch(v: Int): Double { val c = v / 255.0; return if (c <= 0.03928) c / 12.92 else ((c + 0.055) / 1.055).pow(2.4) }
        return 0.2126 * ch(rgb shr 16 and 255) + 0.7152 * ch(rgb shr 8 and 255) + 0.0722 * ch(rgb and 255)
    }

    private fun ratio(a: Int, b: Int): Double {
        val (hi, lo) = lum(a).let { x -> lum(b).let { y -> maxOf(x, y) to minOf(x, y) } }
        return (hi + 0.05) / (lo + 0.05)
    }

    private val textOnSurface = listOf("gb_text", "gb_muted", "gb_warn", "gb_bad", "gb_ok")
    private val surfaces = listOf("gb_surface", "gb_card", "gb_stripe", "gb_fill")

    private fun check(theme: String, colors: Map<String, Int>) {
        val failures = mutableListOf<String>()
        fun need(fg: String, bg: String, min: Double) {
            val r = ratio(colors.getValue(fg), colors.getValue(bg))
            if (r < min) failures += "$theme $fg on $bg = %.2f < $min".format(r)
        }
        for (t in textOnSurface) for (s in surfaces) need(t, s, 4.5)
        need("gb_on_accent", "gb_accent", 4.5)
        need("gb_on_hero", "gb_hero", 4.5)
        need("gb_hero_muted", "gb_hero", 4.5)
        // Non-text: the separators are decorative, but the accent dot and "off" dots must stay visible.
        need("gb_red", "gb_card", 3.0)
        assertTrue(failures.joinToString("\n"), failures.isEmpty())
    }

    @Test fun lightTheme() = check("light", load("values/colors.xml"))

    @Test fun darkTheme() = check("dark", load("values/colors.xml") + load("values-night/colors.xml"))
}
