// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.view.View
import android.view.ViewGroup

/**
 * What TalkBack says for the dot-matrix screens. Everything here is plain strings in and out, so the wording is
 * unit-tested on the JVM; the views only attach the result.
 */
object A11yLabels {
    /**
     * The real words behind a run of dot-matrix text. Dots are never read: the string that was drawn is spoken,
     * with numerals padded for the display ("07") spoken as numbers ("7"), and stray whitespace removed.
     */
    fun spoken(text: CharSequence): String {
        val t = text.toString().trim().replace(Regex("\\s+"), " ")
        return if (t.length in 2..4 && t.all { it in '0'..'9' }) t.toInt().toString() else t
    }

    /** One sentence-style description for the Home status tile: "Link: Online. Connected to X. Details." */
    fun hero(caption: String, word: String, title: String, subtitle: String): String =
        listOf("${caption.trim()}: ${spoken(word)}", title, subtitle)
            .map { it.trim().trimEnd('.', ',', ':') }
            .filter { it.isNotEmpty() }
            .joinToString(". ", postfix = ".")

    /** A feature tile reads "name, state"; a tile that needs the user also says so in words, not only a red dot. */
    fun tile(title: String, word: String, needsAttention: Boolean, attention: String): String =
        "${title.trim()}, ${spoken(word)}" + if (needsAttention) ". $attention" else ""

    /**
     * The child path (indices from [root]) of the view that currently holds TalkBack focus, so a screen that
     * rebuilds its views can put the focus back instead of dropping the user at the top.
     */
    fun focusPath(root: ViewGroup): List<Int>? {
        fun walk(group: ViewGroup, path: List<Int>): List<Int>? {
            for (i in 0 until group.childCount) {
                val child = group.getChildAt(i)
                if (child.isAccessibilityFocused) return path + i
                if (child is ViewGroup) walk(child, path + i)?.let { return it }
            }
            return null
        }
        return walk(root, emptyList())
    }

    /** Re-focus the node at [path] (as far as the rebuilt tree still reaches); no-op when [path] is null. */
    @android.annotation.SuppressLint("AccessibilityFocus") // restores the user's own focus after a rebuild; never moves it elsewhere
    fun restoreFocus(root: ViewGroup, path: List<Int>?) {
        if (path == null) return
        var view: View = root
        for (i in path) {
            val group = view as? ViewGroup ?: break
            view = group.getChildAt(minOf(i, group.childCount - 1)) ?: break
        }
        if (view !== root) view.post { view.performAccessibilityAction(android.view.accessibility.AccessibilityNodeInfo.ACTION_ACCESSIBILITY_FOCUS, null) }
    }
}
