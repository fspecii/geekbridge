package dev.geekbridge.android

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.view.View

/**
 * A 5x7 dot-matrix wordmark on a faint dot grid with a trailing red dot (Nothing-OS-style).
 * Purely decorative typography: it draws only [word] (letters in [GLYPHS]; others render blank), does not
 * animate and is hidden from accessibility services (the section caption names the section).
 */
@SuppressLint("ViewConstructor") // only ever created programmatically with a word
class DotMatrixView(context: Context, private val word: String) : View(context) {
    private val on = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_text) }
    private val off = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_line) }
    private val red = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_red) }

    private val wordCols = word.length * (GLYPH_COLS + 1) - 1
    private var pitch = 0f

    init {
        // The section caption above already names the section; keep screen readers from repeating it.
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val w = MeasureSpec.getSize(widthMeasureSpec)
        val dens = resources.displayMetrics.density
        // Leave room for the word plus a gap and the red dot; shrink the pitch on narrow widths.
        pitch = minOf(MAX_PITCH_DP * dens, w.toFloat() / (wordCols + 3))
        setMeasuredDimension(w, (GLYPH_ROWS * pitch + 0.5f).toInt())
    }

    override fun onDraw(canvas: Canvas) {
        val cols = (width / pitch).toInt()
        val r = pitch * 0.28f
        for (row in 0 until GLYPH_ROWS) {
            for (col in 0 until cols) {
                val cx = col * pitch + pitch / 2
                val cy = row * pitch + pitch / 2
                val lit = col < wordCols && isLit(col, row)
                canvas.drawCircle(cx, cy, if (lit) r * 1.25f else r * 0.6f, if (lit) on else off)
            }
        }
        // The accent dot sits on the baseline row, one gap after the word.
        val cx = (wordCols + 1) * pitch + pitch / 2
        if (cx < width) canvas.drawCircle(cx, (GLYPH_ROWS - 1) * pitch + pitch / 2, r * 1.5f, red)
    }

    private fun isLit(col: Int, row: Int): Boolean {
        val cell = GLYPH_COLS + 1
        val idx = col / cell
        val gc = col % cell
        if (gc >= GLYPH_COLS) return false
        val g = GLYPHS[word[idx].uppercaseChar()] ?: return false
        return g[row][gc] == '1'
    }

    private companion object {
        const val GLYPH_COLS = 5
        const val GLYPH_ROWS = 7
        const val MAX_PITCH_DP = 6.5f
        val GLYPHS: Map<Char, Array<String>> = mapOf(
            'M' to arrayOf("10001", "11011", "10101", "10101", "10001", "10001", "10001"),
            'E' to arrayOf("11111", "10000", "10000", "11110", "10000", "10000", "11111"),
            'S' to arrayOf("01111", "10000", "10000", "01110", "00001", "00001", "11110"),
            'A' to arrayOf("01110", "10001", "10001", "11111", "10001", "10001", "10001"),
            'G' to arrayOf("01110", "10001", "10000", "10111", "10001", "10001", "01110"),
        )
    }
}
