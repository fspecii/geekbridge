// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import java.util.Locale

/**
 * Dot-matrix text for RemoteViews, which cannot hold a custom View: the word is drawn once into a small
 * ALPHA_8 mask (about 24 KB for a long word). The ImageView tints it with a colour resource, so light and dark
 * follow the system without a second bitmap, and lit dots are opaque while unlit ones stay faint.
 */
object WidgetDots {
    private const val PITCH = 8
    private const val MAX_CHARS = 16

    fun mask(text: String): Bitmap {
        val t = text.uppercase(Locale.ROOT).take(MAX_CHARS)
        val cols = DotFont.columns(t).coerceAtLeast(1)
        val bmp = Bitmap.createBitmap(cols * PITCH, DotFont.ROWS * PITCH, Bitmap.Config.ALPHA_8)
        val canvas = Canvas(bmp)
        val paint = Paint(Paint.ANTI_ALIAS_FLAG)
        for (row in 0 until DotFont.ROWS) {
            for (col in 0 until cols) {
                val lit = DotFont.isLit(t, col, row)
                paint.alpha = if (lit) 255 else 48
                canvas.drawCircle(col * PITCH + PITCH / 2f, row * PITCH + PITCH / 2f, PITCH * if (lit) 0.38f else 0.16f, paint)
            }
        }
        return bmp
    }
}
