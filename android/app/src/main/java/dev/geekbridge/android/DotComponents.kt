// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Dot-matrix building blocks shared by every screen (built on [UiKit] and the [Tok] tokens): display title,
 * status hero tile, widget-style tiles, the setup stepper. Each one is static between state changes.
 */

/** Screen title in display dot-matrix with the red accent dot; [trailing] sits at the end (e.g. a settings button). */
fun UiKit.dotHeader(title: String, trailing: View? = null, pitchDp: Float = Tok.DOT_DISPLAY): LinearLayout {
    val row = LinearLayout(context).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
    row.addView(
        DotText(context, title, maxPitchDp = pitchDp, maxLines = 2, accentDot = true),
        LinearLayout.LayoutParams(0, Ui.WRAP, 1f).apply { topMargin = dp(8); bottomMargin = dp(8) },
    )
    trailing?.let { row.addView(it) }
    return row
}

/** Small dot-matrix label (status words, counters). */
fun UiKit.dotLabel(text: String, ink: Int = color(R.color.gb_text), pitchDp: Float = Tok.DOT_LABEL * 1.35f, decorative: Boolean = false): DotText =
    DotText(context, text, maxPitchDp = pitchDp, minPitchDp = 1.6f, maxLines = 1, decorative = decorative).setInk(ink)

/** A round icon button with a 48dp target and a spoken [description]. */
fun UiKit.iconButton(res: Int, description: String, onClick: () -> Unit): View =
    icon(res, 24, color(R.color.gb_muted)).apply {
        contentDescription = description
        isClickable = true
        isFocusable = true
        asButton(this)
        setPadding(dp(12), dp(12), dp(12), dp(12))
        background = android.graphics.drawable.RippleDrawable(
            android.content.res.ColorStateList.valueOf(withAlpha(color(R.color.gb_muted), 0x33)),
            null,
            shape(android.graphics.Color.WHITE, Tok.R_PILL),
        )
        setOnClickListener { onClick() }
        layoutParams = LinearLayout.LayoutParams(dp(Tok.TARGET), dp(Tok.TARGET))
    }

/** What a [heroTile] shows. [level] is 0..1 of the link dots lit; [transfer] is an optional live transfer. */
data class HeroView(
    val word: String,
    val level: Float,
    val title: String,
    val subtitle: String,
    val live: Boolean,
    val transferLabel: String? = null,
    val transferFraction: Float? = null,
)

/**
 * The big dotted status tile: the link state as a dot-matrix word, a dotted link strip, the sentence for it and,
 * when something is moving, a dotted transfer progress bar. Inverted (dark) in light mode so the dots read as lit.
 */
fun UiKit.heroTile(parent: LinearLayout, h: HeroView): LinearLayout {
    val ink = color(R.color.gb_on_hero)
    val dim = color(R.color.gb_hero_dim)
    val muted = color(R.color.gb_hero_muted)
    val tile = LinearLayout(context).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(dp(22), dp(20), dp(22), dp(22))
        background = shape(color(R.color.gb_hero), Tok.R_TILE + 4)
    }
    tile.addView(dotLabel(context.getString(R.string.hero_caption), muted, decorative = true).apply { })
    // One TalkBack stop for the whole status ("Link: Online. Connected to X. ..."): the dots, title and subtitle
    // are hidden individually so the state is not read three times, and only the tile text is a live region.
    val word = DotText(context, h.word, maxPitchDp = 7f, maxLines = 1, grid = true, accentDot = h.live, decorative = true)
        .setInk(ink).setDim(dim)
    tile.addView(word, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = dp(14) })
    val strip = DotProgress(context, 10f).apply {
        colors(ink, dim)
        set(h.level, "", redHead = h.live)
        importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
    }
    tile.addView(strip, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = dp(16) })
    tile.addView(TextView(context).apply {
        text = h.title
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 18f)
        typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
        setTextColor(ink)
        importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
    }, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = dp(18) })
    tile.addView(TextView(context).apply {
        text = h.subtitle
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
        setTextColor(muted)
        setLineSpacing(0f, 1.1f)
        importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
    }, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = dp(4) })
    if (h.transferLabel != null && h.transferFraction != null) {
        tile.addView(TextView(context).apply {
            text = h.transferLabel
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setTextColor(ink)
        }, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = dp(18) })
        tile.addView(
            DotProgress(context, 8f).apply { colors(ink, dim); set(h.transferFraction, h.transferLabel) },
            LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = dp(8) },
        )
    }
    tile.contentDescription = A11yLabels.hero(context.getString(R.string.hero_cd_caption), h.word, h.title, h.subtitle)
    tile.isFocusable = true
    if (android.os.Build.VERSION.SDK_INT >= 28) tile.isScreenReaderFocusable = true
    parent.addView(tile, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
    return tile
}

/**
 * Widget-style square tile for a phone feature: ink icon disc, a dot-matrix status word and the feature name.
 * One focus stop that reads "name, status". Add two per row for the 2-column grid.
 */
fun UiKit.widgetTile(row: LinearLayout, icon: Int, title: String, word: String, active: Boolean, onClick: () -> Unit): View {
    val box = LinearLayout(context).apply {
        orientation = LinearLayout.VERTICAL
        minimumHeight = dp(132)
        setPadding(dp(18), dp(18), dp(18), dp(16))
        background = android.graphics.drawable.RippleDrawable(
            android.content.res.ColorStateList.valueOf(withAlpha(color(R.color.gb_muted), 0x33)),
            shape(color(R.color.gb_panel), Tok.R_TILE), null,
        )
        isClickable = true
        isFocusable = true
        contentDescription = A11yLabels.tile(title, word, !active, context.getString(R.string.home_cd_needs_attention))
        setOnClickListener { onClick() }
        asButton(this)
    }
    val top = LinearLayout(context).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
    top.addView(badge(icon, if (active) null else Tone.Muted, 44, solid = active))
    top.addView(View(context), LinearLayout.LayoutParams(0, 1, 1f))
    // Red dot = needs attention; keeps state off colour alone because the word beside it says the same.
    if (!active) top.addView(DotText(context, ".", maxPitchDp = 6f, maxLines = 1, decorative = true).setInk(color(R.color.gb_red)))
    box.addView(top, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
    box.addView(View(context), LinearLayout.LayoutParams(1, 0, 1f))
    box.addView(
        dotLabel(word, if (active) color(R.color.gb_text) else color(R.color.gb_muted), decorative = true),
        LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = dp(14) },
    )
    box.addView(text(title, 15f, bold = true).apply { setPadding(0, dp(8), 0, 0) })
    row.addView(box, LinearLayout.LayoutParams(0, Ui.WRAP, 1f).apply { if (row.childCount > 0) marginStart = dp(12) })
    return box
}

/** One setup step as seen by [DotStepper]. */
enum class DotStep { Done, Todo, Attention }

/**
 * Setup stepper: one dot per step. Done = filled ink, to do = small grey, needs attention = hollow ring, and the
 * current step is the big red dot. TalkBack hears "Step N of M" (the wizard also sets the pane title).
 */
class DotStepper(context: Context) : View(context) {
    private val dens = resources.displayMetrics.density
    private val on = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_text) }
    private val dim = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_dot_off) }
    private val red = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_red) }
    private val ring = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = context.getColor(R.color.gb_red); style = Paint.Style.STROKE; strokeWidth = 1.6f * dens }
    private var steps: List<DotStep> = emptyList()
    private var current = 0

    fun set(steps: List<DotStep>, current: Int, description: String) {
        this.steps = steps
        this.current = current
        contentDescription = description
        invalidate()
    }

    override fun onMeasure(w: Int, h: Int) = setMeasuredDimension(MeasureSpec.getSize(w), (22 * dens).toInt())

    override fun onDraw(canvas: Canvas) {
        val n = steps.size
        if (n == 0) return
        val rtl = layoutDirection == LAYOUT_DIRECTION_RTL
        val cy = height / 2f
        for (i in 0 until n) {
            val slot = if (rtl) n - 1 - i else i
            val cx = (slot + 0.5f) * width / n
            when {
                i == current -> canvas.drawCircle(cx, cy, 6.5f * dens, red)
                steps[i] == DotStep.Done -> canvas.drawCircle(cx, cy, 4f * dens, on)
                steps[i] == DotStep.Attention -> canvas.drawCircle(cx, cy, 3.8f * dens, ring)
                else -> canvas.drawCircle(cx, cy, 2.6f * dens, dim)
            }
        }
    }
}
