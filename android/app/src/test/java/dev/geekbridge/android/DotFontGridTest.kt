// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class DotFontGridTest {
    private val samples = listOf("", " ", "A", "geekbridge", "ONLINE", "Waiting...", "12:45 PM", "100%", "a-b/c (d)", "é?#>_", "x y  z")

    @Test fun gridMatchesIsLitForEveryDot() {
        for (s in samples) {
            val grid = DotFont.litGrid(s)
            assertEquals(DotFont.ROWS, grid.size)
            val cols = DotFont.columns(s)
            for (row in 0 until DotFont.ROWS) {
                assertEquals("width of '$s'", cols, grid[row].size)
                for (col in 0 until cols) assertEquals("'$s' ($col,$row)", DotFont.isLit(s, col, row), grid[row][col])
            }
        }
    }

    @Test fun gapColumnsBetweenGlyphsAreDark() {
        val grid = DotFont.litGrid("II")
        val gap = DotFont.glyph('I')[0].length
        assertTrue((0 until DotFont.ROWS).none { grid[it][gap] })
    }

    /** The reason the grid exists: building it once is far cheaper than asking [DotFont.isLit] for every dot of every frame. */
    @Test fun gridBuildIsCheaperThanPerDotScan() {
        val s = "WAITING FOR YOUR COMPUTER"
        val cols = DotFont.columns(s)
        fun perDot(): Int { var n = 0; for (r in 0 until DotFont.ROWS) for (c in 0 until cols) if (DotFont.isLit(s, c, r)) n++; return n }
        fun grid(): Int { val g = DotFont.litGrid(s); var n = 0; for (r in g) for (b in r) if (b) n++; return n }
        assertEquals(perDot(), grid())
        repeat(2000) { perDot(); grid() } // warm up
        val t0 = System.nanoTime(); repeat(5000) { perDot() }; val scan = System.nanoTime() - t0
        val g = DotFont.litGrid(s)
        val t1 = System.nanoTime(); repeat(5000) { var n = 0; for (r in g) for (b in r) if (b) n++ }; val read = System.nanoTime() - t1
        println("dot-grid per frame: per-dot scan $scan ns, grid read $read ns for 5000 frames")
        assertTrue("per-frame grid read ($read ns) should beat the per-dot scan ($scan ns)", read < scan)
    }
}
