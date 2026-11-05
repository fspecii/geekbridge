// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class UpdateNoticePolicyTest {
    private val h = 60L * 60 * 1000
    private val min = 60L * 1000
    private val day = 24 * h
    private val start = 1_700_000_000_000L
    private val un = NoticeNet.Unmetered

    private fun due(last: Long, attempt: Long, now: Long, net: NoticeNet = un, enabled: Boolean = true) =
        UpdateNoticePolicy.due(enabled, net, last, attempt, now)

    @Test fun neverCheckedIsDueOnceOnline() {
        assertTrue(due(0, 0, start))
        assertFalse(due(0, 0, start, net = NoticeNet.None))
    }

    @Test fun twentyFourHourGapAfterASuccess() {
        assertFalse(due(start, start, start + day - 1))
        assertTrue(due(start, start, start + day))
        assertFalse(due(start, start, start + 12 * h))
    }

    @Test fun backoffIsFifteenMinutesAfterAFailure() {
        val last = start - 3 * day // old success, then a failed attempt at `start`
        assertFalse(due(last, start, start + 15 * min - 1))
        assertTrue(due(last, start, start + 15 * min))
        assertEquals(15 * min, UpdateNoticePolicy.RETRY_MS)
    }

    @Test fun aFailureDoesNotShortenTheDailyGapAfterASuccess() {
        // success at start, a later failed attempt cannot happen before the gap, but the gap still rules
        assertFalse(due(start, start - 1, start + 23 * h))
    }

    @Test fun disabledSettingNeverChecks() {
        assertFalse(due(0, 0, start, enabled = false))
        assertFalse(due(start - 10 * day, 0, start, enabled = false))
    }

    @Test fun offlineNeverChecks() {
        assertFalse(due(start - 10 * day, 0, start, net = NoticeNet.None))
    }

    @Test fun meteredWaitsAWeekSinceTheLastGoodAnswer() {
        assertFalse(due(start - 2 * day, 0, start, net = NoticeNet.Metered))
        assertFalse(due(start - 7 * day + 1, 0, start, net = NoticeNet.Metered))
        assertTrue(due(start - 7 * day, 0, start, net = NoticeNet.Metered))
        assertTrue(due(start - 2 * day, 0, start, net = un))
    }

    @Test fun clockMovingBackwardsCountsAsDue() {
        assertTrue(due(start + 5 * day, start + 5 * day, start))
        assertTrue(due(start - 3 * day, start + 5 * day, start))
    }

    private fun offer(version: String, dev: Boolean = false) =
        UpdateOffer(version, "v$version", "", "", "a.apk", "https://github.com/x/a.apk", 1, "0".repeat(64), dev)

    @Test fun stableNeverOffersAPrereleaseOrAnOlderBuild() {
        val cur = AppVersion.parse("0.3.1")!!.toVersionCode()!!
        assertTrue(UpdateNoticePolicy.accept(offer("0.3.2"), UpdateChannel.Stable, cur))
        assertFalse(UpdateNoticePolicy.accept(offer("0.4.0-rc.1"), UpdateChannel.Stable, cur))
        assertFalse(UpdateNoticePolicy.accept(offer("0.3.1"), UpdateChannel.Stable, cur))
        assertFalse(UpdateNoticePolicy.accept(offer("0.3.0"), UpdateChannel.Stable, cur))
        assertFalse(UpdateNoticePolicy.accept(offer("junk"), UpdateChannel.Stable, cur))
        assertFalse(UpdateNoticePolicy.accept(offer("dev-abcdef123456", dev = true), UpdateChannel.Stable, cur))
    }

    @Test fun devOffersOnlyOnTheDevChannel() {
        assertTrue(UpdateNoticePolicy.accept(offer("dev-abcdef123456", dev = true), UpdateChannel.Dev, 20_100))
        assertFalse(UpdateNoticePolicy.accept(offer("0.3.2"), UpdateChannel.Dev, 20_100))
    }

    @Test fun chipNeedsTheSettingTheSameBuildAndTheSameChannel() {
        fun chip(enabled: Boolean = true, v: String? = "0.3.2", built: Int = 30_199, ch: String? = "stable", cur: Int = 30_199, channel: UpdateChannel = UpdateChannel.Stable) =
            UpdateNoticePolicy.chipShown(enabled, v, built, ch, cur, channel)
        assertTrue(chip())
        assertFalse(chip(enabled = false))
        assertFalse(chip(v = null))
        assertFalse(chip(v = ""))
        assertFalse(chip(cur = 30_299)) // installed since: gone
        assertFalse(chip(channel = UpdateChannel.Dev))
        assertFalse(chip(ch = null))
    }

    @Test fun oneNotificationPerVersionFromTheBackgroundOnly() {
        assertTrue(UpdateNoticePolicy.shouldNotify(true, true, "0.3.2", null))
        assertFalse(UpdateNoticePolicy.shouldNotify(true, true, "0.3.2", "0.3.2"))
        assertTrue(UpdateNoticePolicy.shouldNotify(true, true, "0.3.3", "0.3.2"))
        assertFalse(UpdateNoticePolicy.shouldNotify(false, true, "0.3.2", null)) // off by default
        assertFalse(UpdateNoticePolicy.shouldNotify(true, false, "0.3.2", null)) // the user is looking at the chip
    }
}
