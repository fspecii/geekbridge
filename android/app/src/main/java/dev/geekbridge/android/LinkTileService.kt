// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.Intent
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService

/**
 * Quick Settings tile: whether this phone is connected to the computer. Read-only state from the same source the
 * Connection widget uses; nothing runs while the tile is not on screen, and no data leaves the phone.
 */
class LinkTileService : TileService() {
    override fun onStartListening() {
        AppState.init(this)
        val v = LinkTileModel.view(WidgetUpdater.connectionNow())
        qsTile?.apply {
            state = if (v.active) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
            label = getString(R.string.tile_link_label)
            val word = getString(
                when (v.word) {
                    ConnWord.Connected -> R.string.widget_word_connected
                    ConnWord.Ringing -> R.string.widget_word_ringing
                    ConnWord.Offline -> R.string.widget_word_offline
                    ConnWord.Unpaired -> R.string.widget_word_unpaired
                },
            )
            val sub = LinkTileModel.subtitle(v, word) { w, name -> getString(R.string.tile_link_sub, w, name) }
            if (Build.VERSION.SDK_INT >= 29) subtitle = sub
            // Spoken with a comma: a screen reader reads the "·" of the visible subtitle as "middle dot".
            val spoken = LinkTileModel.subtitle(v, word) { w, name -> getString(R.string.tile_link_sub_spoken, w, name) }
            contentDescription = getString(R.string.tile_link_a11y, spoken)
            updateTile()
        }
    }

    override fun onClick() {
        val go = { launch() }
        if (isLocked) unlockAndRun(go) else go()
    }

    // The Intent overload only throws on apps targeting 34+ *and running* on 34+; there we use the PendingIntent.
    @SuppressLint("StartActivityAndCollapseDeprecated")
    private fun launch() {
        AppState.init(this)
        val pair = LinkTileModel.tapPairs(LinkTileModel.view(WidgetUpdater.connectionNow()))
        val intent = (if (pair) SetupActivity.intent(this, step = StepId.Pair) else Intent(this, HomeActivity::class.java))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        if (Build.VERSION.SDK_INT >= 34) {
            startActivityAndCollapse(PendingIntent.getActivity(this, 0, intent, PendingIntent.FLAG_IMMUTABLE))
        } else {
            @Suppress("DEPRECATION")
            startActivityAndCollapse(intent)
        }
    }
}
