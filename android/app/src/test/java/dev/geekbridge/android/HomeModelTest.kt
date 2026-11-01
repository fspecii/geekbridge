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
    ) = HomeModel.hero(names, count, phase, connected, keep, service)

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
        assertEquals("Pair a computer first", HomeModel.tileSubtitle(0, emptyList()))
        assertEquals("To Desk", HomeModel.tileSubtitle(1, listOf("Desk")))
        assertEquals("Computer not connected", HomeModel.tileSubtitle(1, emptyList()))
    }
}
