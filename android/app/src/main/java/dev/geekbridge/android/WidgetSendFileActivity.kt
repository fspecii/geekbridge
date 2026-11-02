// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.Intent
import android.os.Bundle
import android.widget.Toast

/**
 * "Send file" from the Quick actions widget: the same flow as Home's Send file (system file picker, then
 * [ShareActivity] sends to the paired computer). Not exported; reached only through the widget's immutable
 * PendingIntent. Transparent, and gone from recents once it has handed over.
 */
class WidgetSendFileActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppState.init(this)
        if (savedInstanceState != null) return // the picker is already open (rotation / process restore)
        if (AppState.pairedCount <= 0) {
            startActivity(SetupActivity.intent(this, step = StepId.Pair))
            finish()
            return
        }
        val pick = Intent(Intent.ACTION_OPEN_DOCUMENT)
            .addCategory(Intent.CATEGORY_OPENABLE).setType("*/*").putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        try {
            @Suppress("DEPRECATION")
            startActivityForResult(pick, REQ_PICK)
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(this, R.string.home_no_file_picker, Toast.LENGTH_SHORT).show()
            finish()
        }
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_PICK && resultCode == RESULT_OK && data != null) {
            val send = Intent(this, ShareActivity::class.java).setAction(Intent.ACTION_SEND_MULTIPLE)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            val clip = data.clipData
            if (clip != null) send.clipData = clip else data.data?.let { send.clipData = ClipData.newRawUri("files", it) }
            startActivity(send)
        }
        finish()
    }

    private companion object {
        const val REQ_PICK = 72
    }
}
