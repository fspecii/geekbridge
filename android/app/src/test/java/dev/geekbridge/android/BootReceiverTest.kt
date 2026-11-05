// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Intent
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class BootReceiverTest {
    @Test fun onlyBootAndUpdateRestoreTheConnection() {
        assertTrue(BootReceiver.handles(Intent.ACTION_BOOT_COMPLETED))
        assertTrue(BootReceiver.handles(Intent.ACTION_MY_PACKAGE_REPLACED))
        assertFalse(BootReceiver.handles(null))
        assertFalse(BootReceiver.handles("android.intent.action.BATTERY_LOW"))
        assertFalse(BootReceiver.handles("dev.geekbridge.android.BOOT_COMPLETED"))
    }

    @Test fun onlyAnUpdateClearsTheDownloadedApk() {
        assertTrue(BootReceiver.purgesDownloads(Intent.ACTION_MY_PACKAGE_REPLACED))
        assertFalse(BootReceiver.purgesDownloads(Intent.ACTION_BOOT_COMPLETED))
        assertFalse(BootReceiver.purgesDownloads(null))
    }
}
