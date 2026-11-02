// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.animation.ValueAnimator
import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.os.Build
import android.view.View
import android.view.accessibility.AccessibilityNodeInfo
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/**
 * Dot-matrix text: any string in the [DotFont], drawn from code (one drawPoints call per colour; no font
 * file, no continuous animation). It names itself to TalkBack with the real text, wraps on word breaks and
 * shrinks its dot pitch to fit narrow widths and large font scales, so it is safe for headings, numerals and
 * status words. [reveal] plays a one-shot left-to-right wipe that is skipped when the user removed animations.
 */
@SuppressLint("ViewConstructor")
class DotText(
    context: Context,
    text: CharSequence = "",
    /** Largest dot pitch in dp (the type size); shrinks to fit. */
    private val maxPitchDp: Float = Tok.DOT_DISPLAY,
    private val minPitchDp: Float = 2.4f,
    private val maxLines: Int = 2,
    /** Draw the faint unlit dots behind the text (the "display" look). */
    private val grid: Boolean = false,
    /** A single red dot after the last character. */
    private val accentDot: Boolean = false,
    /** Decorative only: hidden from screen readers (the surrounding label already says it). */
    private val decorative: Boolean = false,
) : View(context) {
    private val dens = resources.displayMetrics.density
    private val on = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_text); strokeCap = Paint.Cap.ROUND }
    private val off = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_dot_off); strokeCap = Paint.Cap.ROUND }
    private val red = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_red); strokeCap = Paint.Cap.ROUND }

    private var lines: List<String> = emptyList()
    private var pitch = 0f
    private var textCols = 0
    private var revealCols = Int.MAX_VALUE
    private var anim: ValueAnimator? = null
    private var litPts = FloatArray(0)
    private var litCols = IntArray(0)
    private var scratch = FloatArray(0)
    private var offPts = FloatArray(0)
    private var redPts = FloatArray(0)
    private var builtFor = ""

    var text: CharSequence = text
        set(v) {
            if (field.toString() == v.toString()) return
            field = v
            if (!decorative) contentDescription = A11yLabels.spoken(v)
            requestLayout()
            invalidate()
        }

    init {
        if (decorative) importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO else contentDescription = A11yLabels.spoken(text)
        if (Build.VERSION.SDK_INT >= 28 && maxPitchDp >= Tok.DOT_NUMERAL) isAccessibilityHeading = true
    }

    fun setInk(color: Int): DotText = apply { on.color = color; invalidate() }
    fun setDim(color: Int): DotText = apply { off.color = color; invalidate() }

    /** Show [s]; with [animate] the new text wipes in once (no-op when animations are removed). */
    fun reveal(s: CharSequence) {
        val changed = text.toString() != s.toString()
        text = s
        if (!changed || !Motion.enabled(context)) return
        anim?.cancel()
        val cols = DotFont.columns(s.toString())
        anim = ValueAnimator.ofInt(0, cols + 1).apply {
            duration = Motion.duration(context, 240)
            addUpdateListener { revealCols = it.animatedValue as Int; invalidate() }
            addListener(object : android.animation.AnimatorListenerAdapter() {
                override fun onAnimationEnd(a: android.animation.Animator) { revealCols = Int.MAX_VALUE; invalidate() }
            })
            start()
        }
    }

    override fun onDetachedFromWindow() {
        anim?.cancel()
        revealCols = Int.MAX_VALUE
        super.onDetachedFromWindow()
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val mode = MeasureSpec.getMode(widthMeasureSpec)
        val avail = if (mode == MeasureSpec.UNSPECIFIED) Int.MAX_VALUE / 4 else MeasureSpec.getSize(widthMeasureSpec)
        val scale = min(resources.configuration.fontScale, 1.4f).coerceAtLeast(1f)
        var p = maxPitchDp * dens * scale
        val minP = minPitchDp * dens
        val s = text.toString().uppercase(java.util.Locale.ROOT)
        var wrapped: List<String>
        while (true) {
            val cols = max(1, floor(avail / p).toInt() - if (accentDot) 2 else 0)
            wrapped = wrap(s, cols)
            val longest = s.split(' ').maxOf { DotFont.columns(it) }
            // Prefer a smaller dot pitch over breaking a word mid-way.
            if ((wrapped.size <= maxLines && longest <= cols) || p <= minP) break
            p = max(minP, p * 0.93f)
        }
        lines = wrapped
        pitch = p
        textCols = lines.maxOfOrNull { DotFont.columns(it) } ?: 0
        val contentCols = textCols + if (accentDot) 2 else 0
        val w = if (mode == MeasureSpec.EXACTLY) avail else min(avail, ceilPx(contentCols * p))
        val rows = lines.size * DotFont.ROWS + max(0, lines.size - 1) * 2
        val h = ceilPx(max(1, rows) * p)
        setMeasuredDimension(w, h)
        builtFor = ""
    }

    private fun ceilPx(f: Float) = (f + 0.999f).toInt()

    private fun wrap(s: String, cols: Int): List<String> {
        if (s.isEmpty()) return listOf("")
        val out = ArrayList<String>()
        var cur = StringBuilder()
        fun flush() { if (cur.isNotEmpty()) { out.add(cur.toString().trimEnd()); cur = StringBuilder() } }
        for (word in s.split(' ')) {
            var w = word
            while (DotFont.columns(w) > cols && w.length > 1) { // hard-break a word that can never fit
                var n = w.length - 1
                while (n > 1 && DotFont.columns(w.substring(0, n)) > cols) n--
                flush(); out.add(w.substring(0, n)); w = w.substring(n)
            }
            val candidate = if (cur.isEmpty()) w else "$cur $w"
            if (DotFont.columns(candidate) <= cols) { cur = StringBuilder(candidate) } else { flush(); cur = StringBuilder(w) }
        }
        flush()
        return out
    }

    private fun build() {
        val key = "${lines.joinToString("|")}@$pitch@$width@$layoutDirection"
        if (key == builtFor) return
        builtFor = key
        val r = pitch * 0.34f
        on.strokeWidth = 2 * r
        off.strokeWidth = 2 * pitch * 0.15f
        red.strokeWidth = 2 * r * 1.15f
        val rtl = layoutDirection == LAYOUT_DIRECTION_RTL
        val contentW = (textCols + if (accentDot) 2 else 0) * pitch
        val x0 = if (rtl) max(0f, width - contentW) else 0f
        val lit = ArrayList<Float>()
        val cols = ArrayList<Int>()
        val dim = ArrayList<Float>()
        val gridCols = if (grid) max(textCols, (width / pitch).toInt()) else 0
        for ((li, line) in lines.withIndex()) {
            val top = li * (DotFont.ROWS + 2) * pitch
            for (row in 0 until DotFont.ROWS) {
                val cy = top + row * pitch + pitch / 2
                for (col in 0 until max(DotFont.columns(line), gridCols)) {
                    val cx = x0 + col * pitch + pitch / 2
                    if (col < DotFont.columns(line) && DotFont.isLit(line, col, row)) {
                        lit.add(cx); lit.add(cy); cols.add(col)
                    } else if (grid) { dim.add(cx); dim.add(cy) }
                }
            }
        }
        litPts = lit.toFloatArray()
        litCols = cols.toIntArray()
        scratch = FloatArray(litPts.size)
        offPts = dim.toFloatArray()
        redPts = if (accentDot && lines.isNotEmpty()) {
            val last = lines.size - 1
            floatArrayOf(x0 + (DotFont.columns(lines[last]) + 1) * pitch + pitch / 2, last * (DotFont.ROWS + 2) * pitch + (DotFont.ROWS - 1) * pitch + pitch / 2)
        } else FloatArray(0)
    }

    override fun onDraw(canvas: Canvas) {
        build()
        if (offPts.isNotEmpty()) canvas.drawPoints(offPts, off)
        if (revealCols == Int.MAX_VALUE) {
            canvas.drawPoints(litPts, on)
            if (redPts.isNotEmpty()) canvas.drawPoints(redPts, red)
        } else {
            var j = 0
            for (i in litCols.indices) if (litCols[i] < revealCols) { scratch[j++] = litPts[2 * i]; scratch[j++] = litPts[2 * i + 1] }
            canvas.drawPoints(scratch, 0, j, on)
        }
    }
}

/**
 * Dotted progress / status bar: a row of dots, the first [lit] of them filled. The last lit dot can be the red
 * "head" so a live transfer or link reads at a glance. Static between updates (no animation); TalkBack gets the
 * percentage as a range value plus [label].
 */
@SuppressLint("ViewConstructor")
class DotProgress(context: Context, private val pitchDp: Float = 9f) : View(context) {
    private val dens = resources.displayMetrics.density
    private val on = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_text); strokeCap = Paint.Cap.ROUND }
    private val off = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_dot_off); strokeCap = Paint.Cap.ROUND }
    private val red = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_red); strokeCap = Paint.Cap.ROUND }
    private var fraction = 0f
    private var head = true
    private var label = ""

    init {
        accessibilityDelegate = object : AccessibilityDelegate() {
            override fun onInitializeAccessibilityNodeInfo(host: View, info: AccessibilityNodeInfo) {
                super.onInitializeAccessibilityNodeInfo(host, info)
                info.className = "android.widget.ProgressBar"
                info.rangeInfo = AccessibilityNodeInfo.RangeInfo.obtain(AccessibilityNodeInfo.RangeInfo.RANGE_TYPE_PERCENT, 0f, 100f, fraction * 100f)
            }
        }
    }

    /** [dark]/[dim] recolour the dots for use on the inverted hero tile. */
    fun colors(onColor: Int, offColor: Int): DotProgress = apply { on.color = onColor; off.color = offColor; invalidate() }

    fun set(fraction: Float, label: String, redHead: Boolean = true) {
        val f = fraction.coerceIn(0f, 1f)
        if (f == this.fraction && label == this.label && redHead == head) return
        this.fraction = f
        this.label = label
        head = redHead
        contentDescription = label
        invalidate()
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        setMeasuredDimension(MeasureSpec.getSize(widthMeasureSpec), (pitchDp * dens + 0.5f).toInt())
    }

    override fun onDraw(canvas: Canvas) {
        val pitch = pitchDp * dens
        val n = max(1, floor(width / pitch).toInt())
        val used = n * pitch
        val x0 = if (layoutDirection == LAYOUT_DIRECTION_RTL) width - used else 0f
        val lit = (fraction * n).roundToInt().coerceIn(0, n)
        val cy = height / 2f
        on.strokeWidth = pitch * 0.62f
        off.strokeWidth = pitch * 0.3f
        red.strokeWidth = pitch * 0.74f
        val litPts = ArrayList<Float>()
        val offPts = ArrayList<Float>()
        for (i in 0 until n) {
            val col = if (layoutDirection == LAYOUT_DIRECTION_RTL) n - 1 - i else i
            val cx = x0 + i * pitch + pitch / 2
            if (col < lit) { litPts.add(cx); litPts.add(cy) } else { offPts.add(cx); offPts.add(cy) }
        }
        if (head && lit in 1 until n && litPts.size >= 2) {
            val hx = litPts[litPts.size - 2]; val hy = litPts[litPts.size - 1]
            litPts.removeAt(litPts.size - 1); litPts.removeAt(litPts.size - 1)
            canvas.drawPoint(hx, hy, red)
        }
        if (offPts.isNotEmpty()) canvas.drawPoints(offPts.toFloatArray(), off)
        if (litPts.isNotEmpty()) canvas.drawPoints(litPts.toFloatArray(), on)
    }
}
