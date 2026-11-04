// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import android.content.Context
import android.content.Intent
import dev.geekbridge.android.DebugHooks
import dev.geekbridge.android.R

/** DEBUG BUILDS ONLY. Adds the Settings > Experimental row and starts the runtime if the user left the switch on. */
object CallWifiBoot {
    fun install(ctx: Context) {
        CallWifiRuntime.init(ctx)
        DebugHooks.experimentalRow = { activity, kit, parent ->
            kit.row(
                parent, R.drawable.ic_call, activity.getString(R.string.cw_row_title), activity.getString(R.string.cw_row_sub), null,
            ) { activity.startActivity(Intent(activity, CallWifiActivity::class.java)) }
        }
    }
}
