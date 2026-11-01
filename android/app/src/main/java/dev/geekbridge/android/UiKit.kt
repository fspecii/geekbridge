package dev.geekbridge.android

import android.app.Activity
import android.content.Context
import android.content.res.Configuration
import android.graphics.Color
import android.content.res.ColorStateList
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.RippleDrawable
import android.graphics.drawable.StateListDrawable
import android.os.Build
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.WindowInsets
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.Button
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView

enum class BtnStyle { Primary, Tonal, Plain }

/**
 * Building blocks for the modern phone look (cards, chips, tiles, pill buttons). Reuses the palette in
 * res/values(-night) so dark mode follows the system. Platform widgets only.
 */
class UiKit(private val ctx: Context, val ui: Ui) {
    val context: Context get() = ctx
    fun dp(v: Int) = ui.dp(v)
    fun color(id: Int) = ctx.getColor(id)
    val blue get() = color(R.color.gb_blue)

    fun withAlpha(c: Int, alpha: Int) = (c and 0x00FFFFFF) or (alpha shl 24)

    fun shape(fill: Int, radiusDp: Int, stroke: Int? = null) = GradientDrawable().apply {
        setColor(fill)
        cornerRadius = dp(radiusDp).toFloat()
        if (stroke != null) setStroke(maxOf(1, dp(1) / 2), stroke)
    }

    private fun ripple(content: android.graphics.drawable.Drawable) =
        RippleDrawable(ColorStateList.valueOf(withAlpha(color(R.color.gb_muted), 0x33)), content, null)

    fun label(parent: LinearLayout, text: String) {
        val v = TextView(ctx).apply {
            this.text = text.uppercase()
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            letterSpacing = 0.08f
            setTextColor(color(R.color.gb_muted))
            setPaddingRelative(dp(8), 0, 0, 0)
            if (Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
        }
        parent.addView(v, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = dp(24); bottomMargin = dp(8) })
    }

    /** Rounded panel, no heavy border: separation comes from the surface contrast (hairline in dark mode). */
    fun panel(parent: LinearLayout, pad: Int = 16, fill: Int = color(R.color.gb_panel), stroke: Int? = null): LinearLayout =
        LinearLayout(ctx).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(pad), dp(pad), dp(pad), dp(pad))
            background = shape(fill, Tok.R_CARD, stroke)
        }.also { parent.addView(it, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP)) }

    fun text(s: String, sp: Float = 15f, tone: Tone = Tone.Normal, bold: Boolean = false): TextView =
        TextView(ctx).apply {
            text = s
            setTextSize(TypedValue.COMPLEX_UNIT_SP, sp)
            setTextColor(ui.toneColor(tone))
            if (bold) typeface = Typeface.create("sans-serif-medium", Typeface.BOLD)
            setLineSpacing(0f, 1.12f)
        }

    fun heading(s: String, sp: Float = 28f): TextView = text(s, sp, bold = true).apply {
        typeface = Typeface.create("sans-serif", Typeface.BOLD)
        letterSpacing = -0.01f
        if (Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
    }

    fun icon(res: Int, sizeDp: Int = 22, tint: Int = color(R.color.gb_text)): ImageView = ImageView(ctx).apply {
        setImageResource(res)
        imageTintList = ColorStateList.valueOf(tint)
        importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        layoutParams = LinearLayout.LayoutParams(dp(sizeDp), dp(sizeDp))
    }

    /** Round tinted badge holding an icon. */
    fun badge(res: Int, tone: Tone? = null, sizeDp: Int = 44, solid: Boolean = false): FrameLayout {
        val tint = if (tone == null) blue else ui.toneColor(tone)
        return FrameLayout(ctx).apply {
            // Solid: ink disc with an inverse glyph (widget-style); otherwise a quiet grey disc.
            background = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(if (solid) tint else withAlpha(tint, 0x1A)) }
            val glyph = if (solid) color(R.color.gb_on_accent) else tint
            addView(icon(res, sizeDp / 2, glyph), FrameLayout.LayoutParams(dp(sizeDp / 2), dp(sizeDp / 2), Gravity.CENTER))
            layoutParams = LinearLayout.LayoutParams(dp(sizeDp), dp(sizeDp))
        }
    }

    /** Small status pill. The words carry the state; colour only reinforces it. */
    fun chip(text: String, tone: Tone): TextView = TextView(ctx).apply {
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
        typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
        setPadding(dp(12), dp(5), dp(12), dp(5))
        compoundDrawablePadding = dp(6)
        gravity = Gravity.CENTER_VERTICAL
        setChip(this, text, tone)
    }

    /** Pill with a leading dot: filled for done/attention states, hollow for neutral ones (not colour alone). */
    fun setChip(v: TextView, text: String, tone: Tone) {
        v.text = text
        val c = ui.toneColor(tone)
        v.setTextColor(c)
        v.background = shape(withAlpha(c, 0x1A), Tok.R_PILL)
        val hollow = tone == Tone.Muted || tone == Tone.Normal
        val dot = GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            setSize(dp(7), dp(7))
            if (hollow) setStroke(maxOf(1, dp(1)), c) else setColor(c)
        }
        v.setCompoundDrawablesRelativeWithIntrinsicBounds(dot, null, null, null)
    }

    fun button(parent: LinearLayout, label: String, style: BtnStyle = BtnStyle.Tonal, enabled: Boolean = true, onClick: () -> Unit): TextView {
        val v = pill(label, style, enabled, onClick)
        parent.addView(v, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = dp(10) })
        return v
    }

    fun pill(label: String, style: BtnStyle, enabled: Boolean = true, onClick: () -> Unit): TextView = TextView(ctx).apply {
        text = label
        gravity = Gravity.CENTER
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
        typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
        minHeight = dp(48)
        setPadding(dp(20), dp(8), dp(20), dp(8))
        isClickable = true
        isFocusable = true
        applyStyle(this, style, enabled)
        setOnClickListener { if (isEnabled) onClick() }
        asButton(this)
    }

    /** Announce a clickable custom view as a button ("Button" in TalkBack) instead of plain text. */
    fun asButton(v: View): View = v.apply {
        accessibilityDelegate = object : View.AccessibilityDelegate() {
            override fun onInitializeAccessibilityNodeInfo(host: View, info: AccessibilityNodeInfo) {
                super.onInitializeAccessibilityNodeInfo(host, info)
                info.className = Button::class.java.name
            }
        }
    }

    /**
     * Make the whole [row] the switch: one big target, one TalkBack stop that reads title, summary, "Switch" and
     * its state. The inner [Switch] only draws.
     */
    private fun asSwitchRow(row: LinearLayout, sw: Switch) {
        sw.isClickable = false
        sw.isFocusable = false
        sw.importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        row.background = ripple(shape(0, 12))
        row.isClickable = true
        row.isFocusable = true
        row.setOnClickListener {
            sw.toggle()
            row.sendAccessibilityEvent(AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED)
        }
        row.accessibilityDelegate = object : View.AccessibilityDelegate() {
            override fun onInitializeAccessibilityNodeInfo(host: View, info: AccessibilityNodeInfo) {
                super.onInitializeAccessibilityNodeInfo(host, info)
                info.className = Switch::class.java.name
                info.isCheckable = true
                info.isChecked = sw.isChecked
            }
        }
    }

    fun applyStyle(v: TextView, style: BtnStyle, enabled: Boolean) {
        v.isEnabled = enabled
        val (fill, text) = when {
            !enabled -> color(R.color.gb_fill) to color(R.color.gb_muted)
            style == BtnStyle.Primary -> blue to color(R.color.gb_on_blue)
            style == BtnStyle.Tonal -> withAlpha(blue, 0x24) to blue
            else -> 0 to blue
        }
        v.setTextColor(text)
        v.background = ripple(shape(fill, Tok.R_PILL))
    }

    /** One tappable row inside a panel: badge, title + subtitle, trailing view. */
    fun row(parent: LinearLayout, badgeRes: Int, title: String, subtitle: String?, trailing: View?, tone: Tone? = null, onClick: (() -> Unit)? = null): LinearLayout {
        val sub = subtitle?.let { text(it, 13f, Tone.Muted) }
        return rowWith(parent, badgeRes, title, sub, trailing, tone, onClick)
    }

    fun rowWith(parent: LinearLayout, badgeRes: Int, title: String, sub: TextView?, trailing: View?, tone: Tone? = null, onClick: (() -> Unit)? = null): LinearLayout {
        val row = LinearLayout(ctx).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            minimumHeight = dp(56)
            setPadding(dp(4), dp(8), dp(4), dp(8))
            if (onClick != null) {
                background = ripple(shape(0, 12))
                isClickable = true
                isFocusable = true
                setOnClickListener { onClick() }
            }
        }
        row.addView(badge(badgeRes, tone, 40))
        val col = LinearLayout(ctx).apply { orientation = LinearLayout.VERTICAL }
        col.addView(text(title, 15f, bold = true))
        sub?.let { col.addView(it) }
        // At large font sizes a status chip beside the text leaves it a sliver of width: stack it underneath.
        val stackChip = trailing is TextView && trailing !is android.widget.CompoundButton && ctx.resources.configuration.fontScale >= 1.5f
        if (stackChip && trailing != null) {
            col.addView(trailing, LinearLayout.LayoutParams(Ui.WRAP, Ui.WRAP).apply { topMargin = dp(6) })
        }
        row.addView(col, LinearLayout.LayoutParams(0, Ui.WRAP, 1f).apply { marginStart = dp(14); marginEnd = dp(8) })
        if (!stackChip) trailing?.let { row.addView(it) }
        if (onClick == null && trailing is Switch) asSwitchRow(row, trailing) else if (onClick != null) asButton(row)
        parent.addView(row, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        return row
    }

    fun divider(parent: LinearLayout) {
        val v = View(ctx).apply { setBackgroundColor(withAlpha(color(R.color.gb_line), 0xB0)) }
        parent.addView(v, LinearLayout.LayoutParams(Ui.MATCH, maxOf(1, dp(1) / 2)).apply { marginStart = dp(58) })
    }

    fun toggle(checked: Boolean, onChange: (Boolean) -> Unit): Switch = Switch(ctx).apply {
        // Pill toggle: ink track with an inverse thumb when on, quiet grey when off (drawn, so tint modes can't wash it out).
        fun pill(c: Int) = GradientDrawable().apply { setColor(c); cornerRadius = dp(15).toFloat(); setSize(dp(52), dp(30)) }
        // The thumb is a full-height disc whose 4dp ring matches the track, so it reads as inset without clipping.
        fun disc(c: Int, ring: Int) = GradientDrawable().apply {
            shape = GradientDrawable.OVAL; setColor(c); setStroke(dp(4), ring); setSize(dp(30), dp(30))
        }
        trackDrawable = StateListDrawable().apply {
            addState(intArrayOf(android.R.attr.state_checked), pill(color(R.color.gb_accent)))
            addState(intArrayOf(), pill(color(R.color.gb_line)))
        }
        thumbDrawable = StateListDrawable().apply {
            addState(intArrayOf(android.R.attr.state_checked), disc(color(R.color.gb_on_accent), color(R.color.gb_accent)))
            addState(intArrayOf(), disc(color(R.color.gb_muted), color(R.color.gb_line)))
        }
        showText = false
        isChecked = checked
        isSaveEnabled = false
        setOnCheckedChangeListener { _, c -> onChange(c) }
    }

    /** Square-ish quick action: icon, title, one status line. */
    fun tile(parent: LinearLayout, iconRes: Int, title: String, sub: String, enabled: Boolean, onClick: () -> Unit): TextView {
        val box = LinearLayout(ctx).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(18), dp(18), dp(18), dp(16))
            background = ripple(shape(color(R.color.gb_panel), Tok.R_TILE))
            isClickable = true
            isFocusable = true
            alpha = if (enabled) 1f else 0.6f
            setOnClickListener { onClick() }
            asButton(this)
        }
        box.addView(badge(iconRes, if (enabled) null else Tone.Muted, 48, solid = enabled))
        box.addView(text(title, 16f, bold = true).apply { setPadding(0, dp(12), 0, 0) })
        val subView = text(sub, 12.5f, Tone.Muted).apply { setPadding(0, dp(2), 0, 0) }
        box.addView(subView)
        parent.addView(box, LinearLayout.LayoutParams(0, Ui.WRAP, 1f).apply { marginStart = dp(if (parent.childCount > 0) 12 else 0) })
        return subView
    }
}

/**
 * Edge-to-edge scrolling screen: status/navigation bars get opaque scrims, content keeps a readable column
 * on wide screens. [root] is where screens add their views.
 */
class Screen(private val activity: Activity, private val kit: UiKit) {
    val root = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
    val scroll = ScrollView(activity)
    private val topScrim = View(activity).apply { setBackgroundColor(activity.getColor(R.color.gb_surface)) }
    private val bottomScrim = View(activity).apply { setBackgroundColor(activity.getColor(R.color.gb_surface)) }

    /** Fixed bar pinned under the scroll area (wizard navigation); null for plain screens. */
    val footer = LinearLayout(activity).apply {
        orientation = LinearLayout.HORIZONTAL
        visibility = View.GONE
        setBackgroundColor(activity.getColor(R.color.gb_surface))
    }

    fun install() {
        scroll.apply {
            clipToPadding = false
            isFillViewport = false
            addView(root, FrameLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
            setOnApplyWindowInsetsListener { v, insets -> applyInsets(v, insets) }
        }
        val column = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            addView(scroll, LinearLayout.LayoutParams(Ui.MATCH, 0, 1f))
            addView(footer, LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP))
        }
        val frame = FrameLayout(activity).apply {
            addView(column, FrameLayout.LayoutParams(Ui.MATCH, Ui.MATCH))
            addView(topScrim, FrameLayout.LayoutParams(Ui.MATCH, 0, Gravity.TOP))
            addView(bottomScrim, FrameLayout.LayoutParams(Ui.MATCH, 0, Gravity.BOTTOM))
        }
        activity.setContentView(frame)
        SystemBars.setup(activity) // after setContentView: the decor view (insets controller) exists only then
    }

    @Suppress("DEPRECATION")
    private fun applyInsets(v: View, insets: WindowInsets): WindowInsets {
        val base = kit.dp(16)
        val l: Int; val t: Int; val r: Int; val b: Int; val nav: Int
        if (Build.VERSION.SDK_INT >= 30) {
            val i = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            val ime = insets.getInsets(WindowInsets.Type.ime()).bottom
            l = i.left; t = i.top; r = i.right; b = maxOf(i.bottom, ime)
            nav = insets.getInsets(WindowInsets.Type.navigationBars()).bottom
        } else {
            l = insets.systemWindowInsetLeft; t = insets.systemWindowInsetTop
            r = insets.systemWindowInsetRight; b = insets.systemWindowInsetBottom
            nav = b
        }
        topScrim.layoutParams.height = t
        bottomScrim.layoutParams.height = nav
        topScrim.requestLayout(); bottomScrim.requestLayout()
        val avail = activity.resources.displayMetrics.widthPixels - l - r - 2 * base
        val extra = maxOf(0, (avail - kit.dp(MAX_CONTENT_DP)) / 2)
        // The footer keeps the same readable column as the content on tablets and in landscape.
        footer.setPadding(base + l + extra, kit.dp(8), base + r + extra, b + kit.dp(8))
        // With a footer the nav-bar inset belongs to the footer; without one it belongs to the scroll content.
        val bottomPad = if (footer.visibility == View.VISIBLE) base else base + b
        v.setPadding(base + l + extra, base + t, base + r + extra, bottomPad)
        return insets
    }

    fun requestInsets() { scroll.requestApplyInsets() }

    /** Name the page for TalkBack; a changed pane title is announced, so wizard steps are spoken on change. */
    fun announcePage(title: String) {
        if (Build.VERSION.SDK_INT >= 28) root.accessibilityPaneTitle = title
    }

    companion object { const val MAX_CONTENT_DP = 560 }
}

/**
 * Edge-to-edge window setup shared by every screen. Android 15+ enforces it for targetSdk 35+; this makes older
 * releases behave the same, lets content use display cutouts in landscape, and picks dark or light system-bar
 * icons from the current night mode so the clock and gesture handle stay legible in both themes.
 */
object SystemBars {
    @Suppress("DEPRECATION")
    fun setup(activity: Activity) {
        val w = activity.window
        val night = (activity.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES
        if (Build.VERSION.SDK_INT >= 30) {
            w.setDecorFitsSystemWindows(false)
            val mask = android.view.WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or
                android.view.WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS
            w.insetsController?.setSystemBarsAppearance(if (night) 0 else mask, mask)
        } else {
            var f = View.SYSTEM_UI_FLAG_LAYOUT_STABLE or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
            if (!night) {
                f = f or View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR or View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR
            }
            w.decorView.systemUiVisibility = f
        }
        if (Build.VERSION.SDK_INT >= 28) {
            w.attributes = w.attributes.also {
                it.layoutInDisplayCutoutMode = if (Build.VERSION.SDK_INT >= 30) {
                    android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
                } else {
                    android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
                }
            }
        }
        w.statusBarColor = Color.TRANSPARENT
        w.navigationBarColor = Color.TRANSPARENT
    }
}
