// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.view.View

/**
 * A 5x7 dot-matrix wordmark on a faint dot grid with a trailing red dot (see [DotFont], shared with
 * [DotText]). Purely decorative typography: it does not animate and is hidden from accessibility services
 * (the section caption names the section).
 */
@SuppressLint("ViewConstructor") // only ever created programmatically with a word
class DotMatrixView(context: Context, private val word: String) : View(context) {
    private val on = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_text) }
    private val off = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_dot_off) }
    private val red = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_red) }

    private val grid = DotFont.litGrid(word.uppercase(java.util.Locale.ROOT))
    private val wordCols = grid[0].size
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
        setMeasuredDimension(w, (DotFont.ROWS * pitch + 0.5f).toInt())
    }

    override fun onDraw(canvas: Canvas) {
        val cols = (width / pitch).toInt()
        val r = pitch * 0.28f
        for (row in 0 until DotFont.ROWS) {
            for (col in 0 until cols) {
                val cx = col * pitch + pitch / 2
                val cy = row * pitch + pitch / 2
                val lit = col < wordCols && grid[row][col]
                canvas.drawCircle(cx, cy, if (lit) r * 1.25f else r * 0.6f, if (lit) on else off)
            }
        }
        // The accent dot sits on the baseline row, one gap after the word.
        val cx = (wordCols + 1) * pitch + pitch / 2
        if (cx < width) canvas.drawCircle(cx, (DotFont.ROWS - 1) * pitch + pitch / 2, r * 1.5f, red)
    }

    private companion object {
        const val MAX_PITCH_DP = 6.5f
    }
}
