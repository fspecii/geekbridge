// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LinkTileAndReconnectTest {
    private val join = { w: String, n: String -> "$w · $n" }

    @Test fun tileIsLitOnlyWhileConnected() {
        assertTrue(LinkTileModel.view(ConnectionState(ConnWord.Connected, "Desk")).active)
        assertTrue(LinkTileModel.view(ConnectionState(ConnWord.Ringing, "Desk")).active)
        assertFalse(LinkTileModel.view(ConnectionState(ConnWord.Offline, "Desk")).active)
        assertFalse(LinkTileModel.view(ConnectionState(ConnWord.Unpaired, null)).active)
    }

    @Test fun subtitleAddsTheComputerNameWhenThereIsOne() {
        assertEquals("Connected · Desk", LinkTileModel.subtitle(LinkTileModel.view(ConnectionState(ConnWord.Connected, "Desk")), "Connected", join))
        assertEquals("Not linked", LinkTileModel.subtitle(LinkTileModel.view(ConnectionState(ConnWord.Unpaired, null)), "Not linked", join))
        assertEquals("Offline", LinkTileModel.subtitle(LinkTileModel.view(ConnectionState(ConnWord.Offline, "  ")), "Offline", join))
    }

    @Test fun onlyAnUnlinkedPhoneIsSentToPairing() {
        assertTrue(LinkTileModel.tapPairs(LinkTileModel.view(ConnectionState(ConnWord.Unpaired, null))))
        assertFalse(LinkTileModel.tapPairs(LinkTileModel.view(ConnectionState(ConnWord.Offline, "Desk"))))
        assertFalse(LinkTileModel.tapPairs(LinkTileModel.view(ConnectionState(ConnWord.Connected, "Desk"))))
    }

    @Test fun reconnectNoticeNeedsAWantedConnectionARefusalAndNotifications() {
        assertTrue(ReconnectPolicy.shouldShow(wantService = true, startRefused = true, notificationsEnabled = true))
        assertFalse(ReconnectPolicy.shouldShow(wantService = false, startRefused = true, notificationsEnabled = true))
        assertFalse(ReconnectPolicy.shouldShow(wantService = true, startRefused = false, notificationsEnabled = true))
        assertFalse(ReconnectPolicy.shouldShow(wantService = true, startRefused = true, notificationsEnabled = false))
    }
}
