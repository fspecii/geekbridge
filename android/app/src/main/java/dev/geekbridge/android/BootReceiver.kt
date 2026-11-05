// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Restores the background connection after a reboot or an app update, but only if a computer is paired. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val action = intent.action
        if (action != Intent.ACTION_BOOT_COMPLETED && action != Intent.ACTION_MY_PACKAGE_REPLACED) return
        val app = context.applicationContext
        // File and preference reads stay off the main thread; goAsync keeps the process alive until they are done.
        val pending = goAsync()
        Thread({
            try {
                // The update is installed: its downloaded APK is dead weight (about 6 MB) in the cache.
                if (action == Intent.ACTION_MY_PACKAGE_REPLACED) {
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
}
