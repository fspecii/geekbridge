// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ShortcutInfo
import android.content.pm.ShortcutManager
import android.graphics.drawable.Icon
import android.os.Build

/**
 * One "Send to <computer>" entry per linked computer in the Share sheet's top row (Android's sharing shortcuts, the
 * replacement for the deprecated ChooserTargetService). A shortcut carries only the computer's name and an id derived
 * from its device id: no address, key, count or file name. Android removes them with the app's data.
 */
object ShareShortcuts {
    const val CATEGORY = "dev.geekbridge.android.category.SEND_TO_COMPUTER"
    const val MAX = 4
    private const val PREFIX = "send:"

    class Entry(val id: String, val label: String)

    /** The shortcuts to publish for these paired (device id, name) computers, most recently listed first. */
    fun plan(peers: List<Pair<String, String>>): List<Entry> =
        peers.filter { it.first.isNotEmpty() }.distinctBy { it.first }.take(MAX)
            .map { Entry(PREFIX + it.first, it.second.ifBlank { "Computer" }.take(48)) }

    /** The computer a tapped shortcut stands for, or null for any other id. */
    fun deviceIdOf(shortcutId: String?): String? =
        shortcutId?.takeIf { it.startsWith(PREFIX) }?.substring(PREFIX.length)?.takeIf { it.isNotEmpty() }

    /** Ids to drop: published before, no longer wanted. */
    fun stale(published: Collection<String>, wanted: List<Entry>): List<String> =
        published.filter { it.startsWith(PREFIX) && wanted.none { w -> w.id == it } }

    private var last: List<Pair<String, String>>? = null

    /** Publishes [peers] (replacing the earlier set); an unchanged list does nothing. Never throws. */
    fun sync(ctx: Context, peers: List<Pair<String, String>>) {
        // Sharing shortcuts need long-lived shortcuts (Android 10); older systems keep the normal Share sheet list.
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
        val key = peers.take(MAX)
        synchronized(this) { if (key == last) return; last = key }
        try {
            val sm = ctx.getSystemService(ShortcutManager::class.java) ?: return
            val want = plan(peers)
            val gone = stale(sm.dynamicShortcuts.map { it.id }, want)
            if (want.isEmpty()) sm.removeAllDynamicShortcuts()
            else sm.setDynamicShortcuts(want.mapIndexed { rank, e -> build(ctx, e, rank) })
            if (gone.isNotEmpty() && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) sm.removeLongLivedShortcuts(gone)
        } catch (_: RuntimeException) {
            synchronized(this) { last = null } // rate-limited or the launcher is busy: try again at the next update
        }
    }

    @android.annotation.TargetApi(Build.VERSION_CODES.Q)
    private fun build(ctx: Context, e: Entry, rank: Int): ShortcutInfo {
        val target = ComponentName(ctx, ShareActivity::class.java)
        return ShortcutInfo.Builder(ctx, e.id)
            .setShortLabel(e.label)
            // Where there is room (the launcher's long-press list, a screen reader's description) say what it does.
            .setLongLabel(ctx.getString(R.string.share_shortcut_long, Bidi.isolate(e.label)))
            .setLongLived(true)
            .setRank(rank)
            .setActivity(target)
            .setCategories(setOf(CATEGORY))
            .setIcon(Icon.createWithResource(ctx, R.drawable.ic_computer))
            .setIntent(Intent(Intent.ACTION_SEND).setComponent(target))
            .build()
    }
}
