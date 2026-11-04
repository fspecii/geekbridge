// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.app.AlertDialog

/** "Unlink <computer>?" in one place, so the wizard, Diagnostics and any later screen say and do the same thing. */
object UnlinkDialog {
    fun show(activity: Activity, controller: TransportController, p: PairedPeer) {
        val name = Bidi.isolate(p.name.ifEmpty { activity.getString(R.string.link_this_computer) })
        AlertDialog.Builder(activity)
            .setTitle(activity.getString(R.string.link_unpair_title, name))
            .setMessage(activity.getString(R.string.link_unpair_message))
            .setPositiveButton(R.string.link_unpair) { _, _ -> controller.unpair(p.deviceId) }
            .setNegativeButton(R.string.link_cancel, null)
            .show()
    }
}
