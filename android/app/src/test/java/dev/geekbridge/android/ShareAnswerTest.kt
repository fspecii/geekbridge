// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The computer's answer to a share (`geekbridge.share.result`): parsing the snapshot and the words on the Share screen. */
class ShareAnswerTest {
    private fun entry(id: Long = 7, kind: String = "file", outcome: String = "receive_off", reason: String? = "not_allowed") =
        JSONObject().put("id", id).put("connection_id", 2).put("kind", kind).put("outcome", outcome).apply { if (reason != null) put("reason", reason) }

    private fun answer(outcome: ShareOutcome, isText: Boolean = false, reason: String? = null) = ShareAnswer(7, isText, 2, outcome, reason)

    private fun transfer(state: TransferState) =
        Transfer(7, 2, "dev", "Desk", TransferDirection.Outgoing, TransferKind.File, "a.pdf", 1000, 0, state, null, null)

    @Test fun parsesEveryOutcomeAndDropsWhatIsNotExactlyUnderstood() {
        val ok = ShareProtocol.parseAnswers(
            JSONArray().put(entry(1, "file", "accepted")).put(entry(2, "file", "declined", "user")).put(entry(3, "file", "receive_off"))
                .put(entry(4, "file", "too_large")).put(entry(5, "file", "busy")).put(entry(6, "text", "saved")).put(entry(8, "text", "link_saved")),
        )
        assertEquals(
            listOf(ShareOutcome.Accepted, ShareOutcome.Declined, ShareOutcome.ReceiveOff, ShareOutcome.TooLarge, ShareOutcome.Busy, ShareOutcome.Saved, ShareOutcome.LinkSaved),
            ok.map { it.outcome },
        )
        assertEquals(listOf(false, false, false, false, false, true, true), ok.map { it.isText })
        assertEquals("user", ok[1].reason)
        val bad = JSONArray()
            .put(entry(outcome = "exploded")).put(entry(kind = "video")).put(entry(id = 0)).put(entry(id = -3))
            .put(entry(kind = "file", outcome = "saved")).put(entry(kind = "text", outcome = "accepted"))
            .put(JSONObject().put("id", "x").put("connection_id", 1).put("kind", "file").put("outcome", "busy"))
            .put("not an object").put(entry(id = 9, reason = "Not A Code!"))
        val kept = ShareProtocol.parseAnswers(bad)
        assertEquals(listOf(9L), kept.map { it.id })
        assertNull(kept[0].reason)
        assertTrue(ShareProtocol.parseAnswers(null).isEmpty())
        assertEquals(ShareLimits.MAX_TRANSFERS, ShareProtocol.parseAnswers(JSONArray().apply { repeat(500) { put(entry(it + 1L)) } }).size)
    }

    @Test fun anOldSnapshotWithoutAnswersParsesAsNone() {
        val snap = TransportProtocol.parseSnapshot(JSONObject().put("state", "stopped"))
        assertTrue(snap!!.shareAnswers.isEmpty())
    }

    @Test fun refusalsSayWhatHappenedInPlainWords() {
        val s = TestStrings
        val line = { a: ShareAnswer, t: Transfer? -> ShareAnswerText.line("a.pdf", a.isText, t, a, false, false, 0, s) }
        assertEquals(
            "a.pdf — the computer is not set to receive files from this phone (on the computer: Share files > Allow this phone to send me files)" to Tone.Warn,
            line(answer(ShareOutcome.ReceiveOff), transfer(TransferState.Declined)),
        )
        assertEquals("a.pdf — the computer declined it" to Tone.Warn, line(answer(ShareOutcome.Declined, reason = "user"), transfer(TransferState.Declined)))
        assertEquals("a.pdf — the computer did not answer in time" to Tone.Warn, line(answer(ShareOutcome.Declined, reason = "expired"), transfer(TransferState.Declined)))
        assertEquals("a.pdf — the computer refused it: too large" to Tone.Warn, line(answer(ShareOutcome.TooLarge), transfer(TransferState.Declined)))
        assertEquals("a.pdf — the computer is busy with other transfers; try again in a moment" to Tone.Warn, line(answer(ShareOutcome.Busy), transfer(TransferState.Declined)))
        assertTrue(line(answer(ShareOutcome.ReceiveOff, isText = true), null)!!.first.contains("text and links"))
    }

    @Test fun textAndLinksAreAcknowledgedAsKeptNeverAsOpened() {
        val s = TestStrings
        val saved = ShareAnswerText.line("text", true, null, answer(ShareOutcome.Saved, true), true, true, 100, s)!!
        assertEquals("text — received by the computer (shown there, nothing is opened)" to Tone.Ok, saved)
        val link = ShareAnswerText.line("link", true, null, answer(ShareOutcome.LinkSaved, true), true, true, 100, s)!!
        assertEquals("link — received by the computer as a link (shown there, not opened)" to Tone.Ok, link)
        assertTrue(link.first.contains("not opened"))
    }

    @Test fun acceptedYieldsToProgressOnceBytesMove() {
        val s = TestStrings
        val a = answer(ShareOutcome.Accepted)
        assertEquals("a.pdf — the computer accepted it" to Tone.Normal, ShareAnswerText.line("a.pdf", false, transfer(TransferState.Waiting), a, false, false, 0, s))
        assertNull(ShareAnswerText.line("a.pdf", false, transfer(TransferState.Active), a, false, false, 0, s))
        assertNull(ShareAnswerText.line("a.pdf", false, transfer(TransferState.Done), a, false, false, 0, s))
        // A failure after "accepted" is told by the progress line (failed ...), not by a stale "accepted".
        assertNull(ShareAnswerText.line("a.pdf", false, transfer(TransferState.Failed), a, false, false, 0, s))
    }

    @Test fun anOldComputerKeepsTodaysWording() {
        val s = TestStrings
        // No answer and none expected: the caller falls back to the earlier hint ("waiting ... Allow this phone ...").
        assertNull(ShareAnswerText.line("a.pdf", false, transfer(TransferState.Waiting), null, false, false, 0, s))
        assertNull(ShareAnswerText.line("text", true, null, null, true, false, 100, s))
        assertTrue(s.get(R.string.share_state_waiting).contains("Allow this phone"))
    }

    @Test fun aTextWaitsBrieflyForTheAnswerThenFallsBackToSent() {
        val s = TestStrings
        val waiting = ShareAnswerText.line("text", true, null, null, true, true, 1_000, s)!!
        assertEquals("text — sent, waiting for the computer's answer…" to Tone.Muted, waiting)
        assertNull(ShareAnswerText.line("text", true, null, null, true, true, ShareAnswerText.TEXT_WAIT_MS + 1, s))
        assertTrue(ShareAnswerText.textStillWaiting(true, false, 1_000))
        assertFalse(ShareAnswerText.textStillWaiting(true, true, 1_000))
        assertFalse(ShareAnswerText.textStillWaiting(false, false, 1_000))
        assertFalse(ShareAnswerText.textStillWaiting(true, false, ShareAnswerText.TEXT_WAIT_MS + 1))
    }
}
