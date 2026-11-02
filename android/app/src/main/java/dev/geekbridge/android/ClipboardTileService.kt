// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.Intent
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService

/** Quick Settings tile: "Send clipboard". One tap gives GeekBridge focus long enough to read and send it. */
class ClipboardTileService : TileService() {
    override fun onTileAdded() {
        ClipboardRuntime.init(this)
        ClipboardRuntime.tileAdded = true
    }

    override fun onTileRemoved() {
        ClipboardRuntime.init(this)
        ClipboardRuntime.tileAdded = false
    }

    override fun onStartListening() {
        ClipboardRuntime.init(this)
        ClipboardRuntime.tileAdded = true // it is only asked to listen while it is in Quick Settings
        qsTile?.apply {
            state = if (ClipboardRuntime.settings.enabled) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
            label = getString(R.string.tile_label)
            if (Build.VERSION.SDK_INT >= 29) {
                subtitle = if (ClipboardRuntime.settings.enabled) getString(R.string.tile_on) else getString(R.string.tile_off)
            }
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
        val intent = ClipboardFloatingActivity.intent(this)
        if (Build.VERSION.SDK_INT >= 34) {
            startActivityAndCollapse(PendingIntent.getActivity(this, 0, intent, PendingIntent.FLAG_IMMUTABLE))
        } else {
            @Suppress("DEPRECATION")
            startActivityAndCollapse(intent)
        }
    }
}
