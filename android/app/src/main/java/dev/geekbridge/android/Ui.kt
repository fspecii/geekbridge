package dev.geekbridge.android

import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Typeface
import android.graphics.drawable.Drawable
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.RippleDrawable
import android.graphics.drawable.StateListDrawable
import android.os.Build
import android.text.InputFilter
import android.text.InputType
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView

enum class Tone { Normal, Ok, Warn, Bad, Muted }

/** Button weight: Primary is the one filled action in a group, Quiet is outlined. */
enum class Emphasis { Primary, Secondary, Quiet }

/**
 * Programmatic view helpers (platform widgets only; no AndroidX/Compose).
 *
 * Visual language: macOS-style grouped sections (small caption label above a hairline-bordered rounded
 * group), monochrome type and fills, and one red accent used for dots and the Messages section. Colour is
 * never the only carrier of state: every status also has words.
 */
class Ui(private val ctx: Context) {
    fun dp(v: Int): Int = (v * ctx.resources.displayMetrics.density + 0.5f).toInt()

    private fun color(id: Int) = ctx.getColor(id)

    fun toneColor(t: Tone): Int = when (t) {
        Tone.Normal -> color(R.color.gb_text)
        Tone.Ok -> color(R.color.gb_ok)
        Tone.Warn -> color(R.color.gb_warn)
        Tone.Bad -> color(R.color.gb_bad)
        Tone.Muted -> color(R.color.gb_muted)
    }

    private fun rounded(fill: Int, radiusDp: Int, strokeColor: Int? = null): GradientDrawable =
        GradientDrawable().apply {
            setColor(fill)
            cornerRadius = dp(radiusDp).toFloat()
            if (strokeColor != null) setStroke(maxOf(1, dp(1) / 2), strokeColor)
        }

    private fun dot(colorInt: Int, sizeDp: Int = 8): Drawable =
        GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            setColor(colorInt)
            setSize(dp(sizeDp), dp(sizeDp))
        }

    /**
     * A grouped section: a small caption (with a dot) above a rounded, hairline-bordered card.
     * [accent] makes the dot red; the card content goes into the returned layout.
     */
    fun card(parent: LinearLayout, title: String, accent: Boolean = false): LinearLayout {
        val label = TextView(ctx).apply {
            text = title.uppercase(java.util.Locale.getDefault())
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            letterSpacing = 0.08f
            setTextColor(color(R.color.gb_muted))
            compoundDrawablePadding = dp(8)
            setCompoundDrawablesRelativeWithIntrinsicBounds(
                dot(if (accent) color(R.color.gb_red) else color(R.color.gb_line), 8), null, null, null,
            )
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(6), 0, 0, 0)
            if (Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
        }
        parent.addView(label, LinearLayout.LayoutParams(MATCH, WRAP).apply { topMargin = dp(24); bottomMargin = dp(8) })
        val card = LinearLayout(ctx).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(14), dp(16), dp(16))
            background = rounded(color(R.color.gb_card), 14, color(R.color.gb_line))
        }
        parent.addView(card, LinearLayout.LayoutParams(MATCH, WRAP))
        return card
    }

    fun text(s: String, tone: Tone = Tone.Normal, sizeSp: Float = 14f, bold: Boolean = false): TextView =
        TextView(ctx).apply {
            text = s
            setTextSize(TypedValue.COMPLEX_UNIT_SP, sizeSp)
            if (bold) typeface = Typeface.create("sans-serif-medium", Typeface.BOLD)
            setTextColor(toneColor(tone))
            setLineSpacing(0f, 1.1f)
            setPadding(0, dp(6), 0, 0)
        }

    /** Large screen title. */
    fun title(s: String): TextView = text(s, sizeSp = 32f, bold = true).apply {
        typeface = Typeface.create("sans-serif", Typeface.BOLD)
        letterSpacing = -0.01f
        if (Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
    }

    fun add(card: LinearLayout, v: View): View {
        card.addView(v, LinearLayout.LayoutParams(MATCH, WRAP))
        return v
    }

    fun body(card: LinearLayout, s: String, tone: Tone = Tone.Normal): TextView =
        text(s, tone).also { add(card, it) }

    fun setTone(v: TextView, s: String, tone: Tone) {
        v.text = s
        v.setTextColor(toneColor(tone))
    }

    /**
     * Status line with a dot tinted by [tone] pinned beside the first line; the words always carry the
     * same meaning. Update it with [setStatus].
     */
    fun statusLine(card: LinearLayout, s: String, tone: Tone = Tone.Muted): TextView {
        val row = LinearLayout(ctx).apply { orientation = LinearLayout.HORIZONTAL }
        val dotView = View(ctx).apply { importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO }
        val v = text(s, tone)
        v.tag = dotView
        row.addView(dotView, LinearLayout.LayoutParams(dp(8), dp(8)).apply { topMargin = dp(15); marginEnd = dp(10) })
        row.addView(v, LinearLayout.LayoutParams(0, WRAP, 1f))
        card.addView(row, LinearLayout.LayoutParams(MATCH, WRAP))
        setStatus(v, s, tone)
        return v
    }

    fun setStatus(v: TextView, s: String, tone: Tone) {
        setTone(v, s, tone)
        (v.tag as? View)?.background = dot(toneColor(tone), 8)
    }

    /** iTunes-style striped list row: [label] left, [value] right; even rows get the stripe fill. */
    fun row(card: LinearLayout, index: Int, label: String, value: String, valueTone: Tone = Tone.Muted): TextView {
        val row = LinearLayout(ctx).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            minimumHeight = dp(40)
            setPadding(dp(10), dp(6), dp(10), dp(6))
            if (index % 2 == 0) background = rounded(color(R.color.gb_stripe), 8)
        }
        val l = TextView(ctx).apply {
            text = label
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            setTextColor(color(R.color.gb_text))
        }
        val v = TextView(ctx).apply {
            text = value
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            setTextColor(toneColor(valueTone))
            gravity = Gravity.END
        }
        row.addView(l, LinearLayout.LayoutParams(0, WRAP, 1f))
        row.addView(v, LinearLayout.LayoutParams(0, WRAP, 1.2f).apply { marginStart = dp(12) })
        card.addView(row, LinearLayout.LayoutParams(MATCH, WRAP).apply { topMargin = if (index == 0) dp(8) else 0 })
        return v
    }

    /** Striped, text-only list row for previews (contacts, events). */
    fun listRow(parent: LinearLayout, index: Int, s: String, tone: Tone = Tone.Normal) {
        val v = TextView(ctx).apply {
            text = s
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            setTextColor(toneColor(tone))
            setPadding(dp(10), dp(8), dp(10), dp(8))
            if (index % 2 == 0) background = rounded(color(R.color.gb_stripe), 8)
        }
        parent.addView(v, LinearLayout.LayoutParams(MATCH, WRAP))
    }

    /** Monospace block for identifiers that are compared by eye (ids, fingerprints, pairing codes). */
    fun mono(s: String, sizeSp: Float = 12f, bold: Boolean = false, tone: Tone = Tone.Normal): TextView =
        text(s, tone, sizeSp).apply {
            typeface = Typeface.create(Typeface.MONOSPACE, if (bold) Typeface.BOLD else Typeface.NORMAL)
            setTextIsSelectable(false)
        }

    fun check(card: LinearLayout, label: String, checked: Boolean = false): CheckBox =
        CheckBox(ctx).apply {
            text = label
            isChecked = checked
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            setTextColor(color(R.color.gb_text))
            buttonTintList = ColorStateList.valueOf(color(R.color.gb_accent))
            minHeight = dp(48)
            minimumHeight = dp(48)
            isSaveEnabled = false
        }.also { card.addView(it, LinearLayout.LayoutParams(MATCH, WRAP).apply { topMargin = dp(4) }) }

    fun divider(card: LinearLayout) {
        val v = View(ctx).apply { setBackgroundColor(color(R.color.gb_line)) }
        card.addView(v, LinearLayout.LayoutParams(MATCH, maxOf(1, dp(1) / 2)).apply { topMargin = dp(12) })
    }

    private fun styledButton(
        label: String,
        enabled: Boolean,
        emphasis: Emphasis,
        accent: Int?,
        onClick: () -> Unit,
    ): Button = Button(ctx).apply {
        text = label
        setAllCaps(false)
        isEnabled = enabled
        isSingleLine = false
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
        typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
        minHeight = dp(48)
        minimumHeight = dp(48)
        minWidth = 0
        minimumWidth = 0
        stateListAnimator = null
        elevation = 0f
        setPadding(dp(14), dp(8), dp(14), dp(8))

        val fillColor = accent ?: color(R.color.gb_accent)
        val (fill, stroke, textColor) = when (emphasis) {
            Emphasis.Primary -> Triple(fillColor, null, color(R.color.gb_on_accent))
            Emphasis.Secondary -> Triple(color(R.color.gb_fill), null, color(R.color.gb_text))
            Emphasis.Quiet -> Triple(color(R.color.gb_card), color(R.color.gb_line), color(R.color.gb_text))
        }
        val disabledFill = if (emphasis == Emphasis.Quiet) color(R.color.gb_card) else color(R.color.gb_fill)
        val normal = rounded(fill, 10, stroke)
        val off = rounded(disabledFill, 10, stroke ?: color(R.color.gb_line))
        val states = StateListDrawable().apply {
            addState(intArrayOf(-android.R.attr.state_enabled), off)
            addState(intArrayOf(), normal)
        }
        val ripple = ColorStateList.valueOf((color(R.color.gb_muted) and 0x00FFFFFF) or 0x33000000)
        background = RippleDrawable(ripple, states, null)
        setTextColor(
            ColorStateList(
                arrayOf(intArrayOf(-android.R.attr.state_enabled), intArrayOf()),
                intArrayOf(color(R.color.gb_muted), textColor),
            ),
        )
        setOnClickListener { onClick() }
    }

    fun button(
        card: LinearLayout,
        label: String,
        enabled: Boolean = true,
        emphasis: Emphasis = Emphasis.Secondary,
        accent: Int? = null,
        onClick: () -> Unit,
    ): Button = styledButton(label, enabled, emphasis, accent, onClick).also {
        card.addView(it, LinearLayout.LayoutParams(MATCH, WRAP).apply { topMargin = dp(10) })
    }

    /** Equal-width buttons in one row. */
    fun buttonRow(card: LinearLayout, vararg specs: Pair<String, () -> Unit>): List<Button> {
        val row = LinearLayout(ctx).apply { orientation = LinearLayout.HORIZONTAL }
        val buttons = specs.mapIndexed { i, (label, action) ->
            styledButton(label, true, Emphasis.Secondary, null, action).also {
                row.addView(it, LinearLayout.LayoutParams(0, WRAP, 1f).apply { if (i > 0) marginStart = dp(8) })
            }
        }
        card.addView(row, LinearLayout.LayoutParams(MATCH, WRAP).apply { topMargin = dp(10) })
        return buttons
    }

    fun input(
        card: LinearLayout,
        hint: String,
        inputType: Int,
        maxChars: Int,
        lines: Int = 1,
    ): EditText = EditText(ctx).apply {
        this.hint = hint
        this.inputType = inputType or if (lines > 1) InputType.TYPE_TEXT_FLAG_MULTI_LINE else 0
        if (lines > 1) {
            minLines = lines
            gravity = Gravity.TOP or Gravity.START
        }
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
        setTextColor(color(R.color.gb_text))
        setHintTextColor(color(R.color.gb_muted))
        minHeight = dp(48)
        setPadding(dp(12), dp(10), dp(12), dp(10))
        background = rounded(color(R.color.gb_surface), 10, color(R.color.gb_line))
        filters = arrayOf(InputFilter.LengthFilter(maxChars))
        // Private text must not be written into saved instance state.
        isSaveEnabled = false
        importantForAutofill = View.IMPORTANT_FOR_AUTOFILL_NO
    }.also { card.addView(it, LinearLayout.LayoutParams(MATCH, WRAP).apply { topMargin = dp(10) }) }

    /** Decorative-but-labelled dot-matrix banner (see [DotMatrixView]). */
    fun dotBanner(card: LinearLayout, word: String): DotMatrixView =
        DotMatrixView(ctx, word).also {
            card.addView(it, LinearLayout.LayoutParams(MATCH, WRAP).apply { bottomMargin = dp(4) })
        }

    companion object {
        const val MATCH = ViewGroup.LayoutParams.MATCH_PARENT
        const val WRAP = ViewGroup.LayoutParams.WRAP_CONTENT
    }
}
