// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class HomeModelTest {
    private fun hero(
        names: List<String> = listOf("Desk"),
        count: Int = names.size,
        phase: LinkPhase = LinkPhase.Stopped,
        connected: List<String> = emptyList(),
        keep: Boolean = true,
        service: Boolean = false,
    ) = HomeModel.hero(TestStrings, names, count, phase, connected, keep, service)

    @Test fun unpairedAsksToPair() {
        val h = hero(names = emptyList())
        assertEquals(HeroAction.Pair, h.action)
        assertEquals("No computer paired", h.title)
    }

    @Test fun connectedNamesTheComputer() {
        val h = hero(phase = LinkPhase.Running, connected = listOf("Desk"))
        assertEquals("Connected to Desk", h.title)
        assertEquals(Tone.Ok, h.tone)
        assertEquals("Connected to Desk +1", hero(connected = listOf("Desk", "Lap")).title)
    }

    @Test fun runningButNotConnectedWaits() {
        assertEquals("Waiting for Desk", hero(phase = LinkPhase.Running).title)
    }

    @Test fun neverClaimsConnectedWithoutATrustedConnection() {
        for (p in LinkPhase.entries) assertTrue(!hero(phase = p).title.startsWith("Connected"))
    }

    @Test fun stoppedExplainsWhy() {
        assertTrue(hero(keep = false).subtitle.contains("off"))
        assertEquals(Tone.Warn, hero(keep = true, service = false).tone)
        assertEquals("Connecting…", hero(keep = true, service = true).title)
    }

    @Test fun tileSubtitles() {
        assertEquals("Pair a computer first", HomeModel.tileSubtitle(TestStrings, 0, emptyList()))
        assertEquals("To Desk", HomeModel.tileSubtitle(TestStrings, 1, listOf("Desk")))
        assertEquals("Computer not connected", HomeModel.tileSubtitle(TestStrings, 1, emptyList()))
    }

    private fun word(
        count: Int = 1,
        phase: LinkPhase = LinkPhase.Stopped,
        connected: List<String> = emptyList(),
        keep: Boolean = true,
        service: Boolean = false,
    ) = HomeModel.word(count, phase, connected, keep, service)

    @Test fun linkWordFollowsTheHeroStates() {
        assertEquals(LinkWord.Unpaired, word(count = 0))
        assertEquals(LinkWord.Online, word(phase = LinkPhase.Running, connected = listOf("Desk")))
        assertEquals(LinkWord.Waiting, word(phase = LinkPhase.Running))
        assertEquals(LinkWord.Linking, word(phase = LinkPhase.Starting))
        assertEquals(LinkWord.Linking, word(keep = true, service = true))
        assertEquals(LinkWord.Offline, word(keep = false))
        assertEquals(LinkWord.Offline, word(keep = true, service = false))
        assertEquals(1f, LinkWord.Online.level, 0f)
        assertEquals(0f, LinkWord.Offline.level, 0f)
    }

    private fun transfer(dir: TransferDirection, size: Long?, bytes: Long, state: TransferState, kind: TransferKind = TransferKind.File) =
        Transfer(1, 1, "d", "Desk", dir, kind, "a.bin", size, bytes, state, null, null)

    @Test fun transferGlanceSumsLiveFilesAndBucketsPercent() {
        assertEquals(null, TransferGlanceModel.of(emptyList()))
        assertEquals(null, TransferGlanceModel.of(listOf(transfer(TransferDirection.Outgoing, 100, 100, TransferState.Done))))
        val g = TransferGlanceModel.of(
            listOf(
                transfer(TransferDirection.Outgoing, 100, 37, TransferState.Active),
                transfer(TransferDirection.Incoming, 100, 50, TransferState.Active),
            ),
        )!!
        assertEquals(2, g.count)
        assertEquals(1, g.outgoing)
        assertEquals(40, g.percent) // (37+50)/200 = 43% -> 40 bucket
        assertEquals(null, TransferGlanceModel.of(listOf(transfer(TransferDirection.Outgoing, null, 5, TransferState.Waiting)))!!.percent)
    }
}
