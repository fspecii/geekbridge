// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Restores the background connection after a reboot or an app update, but only if a computer is paired. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val action = intent.action
        if (!handles(action)) return
        val app = context.applicationContext
        // File and preference reads stay off the main thread; goAsync keeps the process alive until they are done.
        val pending = goAsync()
        Thread({
            try {
                // The update is installed: its downloaded APK is dead weight (about 6 MB) in the cache.
                if (purgesDownloads(action)) {
                    UpdateStorage.purge(UpdateFileProvider.dir(app), 0, System.currentTimeMillis() + 1)
                }
                LinkService.sync(app) // does nothing unless AppState says a computer is paired
            } catch (_: RuntimeException) {
                // A failed restore must not crash the process; the app's own start repeats it.
            } finally {
                pending.finish()
            }
        }, "geekbridge-boot").start()
    }

    companion object {
        /** Only a finished boot or an app update restores the connection; anything else is ignored. */
        fun handles(action: String?): Boolean =
            action == Intent.ACTION_BOOT_COMPLETED || action == Intent.ACTION_MY_PACKAGE_REPLACED

        /** Only an update clears the downloaded APK; a plain boot keeps whatever is waiting. */
        fun purgesDownloads(action: String?): Boolean = action == Intent.ACTION_MY_PACKAGE_REPLACED
    }
}
