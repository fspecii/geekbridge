// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Restores the background connection after a reboot or an app update, but only if a computer is paired. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        // The update is installed: its downloaded APK is dead weight (about 6 MB) in the cache.
        if (intent.action == Intent.ACTION_MY_PACKAGE_REPLACED) {
            UpdateStorage.purge(UpdateFileProvider.dir(context), 0, System.currentTimeMillis() + 1)
        }
        if (intent.action == Intent.ACTION_BOOT_COMPLETED || intent.action == Intent.ACTION_MY_PACKAGE_REPLACED) {
            LinkService.sync(context) // does nothing unless AppState says a computer is paired
        }
    }
}
