// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CallAudioModelTest {
    @Test fun runtimePermissionIsNeededFromAndroid12Only() {
        assertFalse(CallAudioModel.needsRuntimePermission(30))
        assertTrue(CallAudioModel.needsRuntimePermission(31))
        assertTrue(CallAudioModel.needsRuntimePermission(36))
    }

    @Test fun noRadioWinsOverEverything() {
        assertEquals(HfpPhase.NoBluetooth, CallAudioModel.preflight(false, 36, false, false))
    }

    @Test fun androidTwelveAsksBeforeReadingConnectedDevices() {
        assertEquals(HfpPhase.NeedsPermission, CallAudioModel.preflight(true, 33, false, true))
        // Older Android: the install-time permission is enough, so the check runs straight away.
        assertNull(CallAudioModel.preflight(true, 30, false, true))
    }

    @Test fun bluetoothOffIsReportedOnceAllowed() {
        assertEquals(HfpPhase.BluetoothOff, CallAudioModel.preflight(true, 36, true, false))
        assertNull(CallAudioModel.preflight(true, 36, true, true))
    }

    @Test fun connectedDevicesBecomeTheAnswer() {
        assertEquals(HfpPhase.Unknown, CallAudioModel.result(null).phase)
        assertEquals(HfpPhase.NoneConnected, CallAudioModel.result(emptyList()).phase)
        val v = CallAudioModel.result(listOf(" Work laptop ", null))
        assertEquals(HfpPhase.Connected, v.phase)
        assertEquals(listOf("Work laptop", ""), v.names)
    }
}
