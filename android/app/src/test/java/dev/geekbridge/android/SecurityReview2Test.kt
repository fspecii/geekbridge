// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Security review 2: per-computer opt-ins (S2-5), text-selection confirmation (S2-4), link confirmation (S2-9), caller number (S2-6). */
class SecurityReview2Test {
    private val laptop = "a".repeat(32)
    private val desktop = "b".repeat(32)

    // ---- S2-5

    @Test fun aNewlyPairedComputerHasNothingAllowed() {
        val g = ComputerGrants().set(laptop, GrantFeature.Sms, true)
        assertTrue(g.allows(laptop, GrantFeature.Sms))
        assertFalse(g.allows(desktop, GrantFeature.Sms))
        assertFalse(g.allows(laptop, GrantFeature.Contacts))
        assertEquals(setOf(laptop), g.devicesFor(GrantFeature.Sms))
    }

    @Test fun editingNeedsReadingAndWithdrawingIsPerFeature() {
        var g = ComputerGrants().set(laptop, GrantFeature.Calendar, true).set(laptop, GrantFeature.CalendarEdit, true)
        g = g.set(laptop, GrantFeature.Calendar, false)
        assertFalse(g.allows(laptop, GrantFeature.CalendarEdit))
        g = ComputerGrants().set(laptop, GrantFeature.Sms, true).set(desktop, GrantFeature.Sms, true).set(laptop, GrantFeature.Calls, true)
        g = g.clear(GrantFeature.Sms)
        assertTrue(g.devicesFor(GrantFeature.Sms).isEmpty())
        assertTrue(g.allows(laptop, GrantFeature.Calls))
    }

    @Test fun existingGlobalSettingsMigrateOnlyToTheComputersPairedNow() {
        val legacy = setOf(GrantFeature.Contacts, GrantFeature.Calls)
        val m = ComputerGrants.migrate(ComputerGrants(), legacy, setOf(laptop))
        assertTrue(m.migrated)
        assertTrue(m.allows(laptop, GrantFeature.Contacts) && m.allows(laptop, GrantFeature.Calls))
        assertFalse(m.allows(laptop, GrantFeature.Sms))
        // A computer paired afterwards gets nothing, and migration does not run twice.
        val later = ComputerGrants.migrate(m, legacy + GrantFeature.Sms, setOf(laptop, desktop))
        assertEquals(m, later)
        assertFalse(later.allows(desktop, GrantFeature.Contacts))
        // No computer paired at the time: nobody inherits it later either.
        val none = ComputerGrants.migrate(ComputerGrants(), legacy, emptySet())
        assertTrue(none.migrated && none.byDevice.isEmpty())
    }

    @Test fun unpairedComputersLoseTheirRights() {
        val g = ComputerGrants(migrated = true).set(laptop, GrantFeature.Sms, true).set(desktop, GrantFeature.Sms, true)
        assertEquals(setOf(desktop), g.keepOnly(setOf(desktop)).devicesFor(GrantFeature.Sms))
    }

    @Test fun grantsRoundTripAndUnreadableMeansNobody() {
        val g = ComputerGrants(migrated = true).set(laptop, GrantFeature.Sms, true).set(laptop, GrantFeature.Calls, true)
        assertEquals(g, ComputerGrants.fromJson(g.toJson()))
        assertEquals(ComputerGrants(), ComputerGrants.fromJson("{not json"))
        assertEquals(ComputerGrants(), ComputerGrants.fromJson(null))
    }

    @Test fun theCoreIsToldTheListOnStartAndOnChange() {
        val g = ComputerGrants().set(laptop, GrantFeature.Sms, true)
        val req = JSONObject(g.permitsRequest())
        assertEquals("transport_permits", req.getString("action"))
        val p = req.getJSONObject("permits")
        assertEquals(laptop, p.getJSONArray("sms").getString(0))
        assertEquals(0, p.getJSONArray("calls").length())
        // Every start request carries it, closed by default.
        val start = JSONObject(TransportProtocol.startRequest(StartOptions("/s", "n", true, null, false, false, false, permits = g)))
        assertEquals(laptop, start.getJSONObject("permits").getJSONArray("sms").getString(0))
        val closed = JSONObject(TransportProtocol.startRequest(StartOptions("/s", "n", true, null, false, false, false, permits = ComputerGrants())))
        assertEquals(0, closed.getJSONObject("permits").getJSONArray("sms").length())
    }

    @Test fun theSettingsLineSaysWhichComputerIsAllowed() {
        assertEquals("Allowed: Laptop, Desk", GrantText.summary(listOf("Laptop", "Desk"), 2, TestStrings))
        assertEquals("No computer allowed yet", GrantText.summary(emptyList(), 1, TestStrings))
        assertEquals("No computer linked yet", GrantText.summary(emptyList(), 0, TestStrings))
    }

    // ---- S2-4

    @Test fun textFromOutsideIsOnlyOfferedNotSent() {
        assertNull(SendConfirm.accept(null))
        assertNull(SendConfirm.accept("   \n"))
        assertEquals("rm -rf ~", SendConfirm.accept("rm -rf ~"))
        val shown = SendConfirm.preview("a\u0007b\nc" + "x".repeat(2000))
        assertFalse(shown.contains('\u0007'))
        assertTrue(shown.length <= SendConfirm.PREVIEW_CHARS + 1)
    }

    // ---- S2-9

    @Test fun theLinkConfirmationShowsTheFingerprintAndCleansTheName() {
        assertEquals("ABCD 1234 EF56 7890", QrConfirmText.fingerprintPrefix("abcd1234ef567890" + "0".repeat(48)))
        assertEquals("Laptop", QrConfirmText.safeName("Lap‮top​\n"))
        assertEquals("Bank Security", QrConfirmText.safeName("  Bank Security "))
        assertTrue(QrConfirmText.safeName("N".repeat(200)).length <= 41)
        val t = QrTarget("d".repeat(32), "x", "ab".repeat(32), "tok", listOf(QrAddress("192.168.1.5", 1716)))
        assertEquals("192.168.1.5:1716", QrConfirmText.address(t))
    }

    // ---- S2-6

    @Test fun onlyPlausiblePhoneNumbersAreAcceptedFromTheBroadcast() {
        assertTrue(CallRules.plausibleNumber("+49 151 1234567"))
        assertTrue(CallRules.plausibleNumber("(415) 555-0100"))
        assertFalse(CallRules.plausibleNumber("Call <b>bank</b>"))
        assertFalse(CallRules.plausibleNumber(""))
        assertFalse(CallRules.plausibleNumber("1".repeat(40)))
        assertFalse(CallRules.plausibleNumber(null))
        assertEquals("android.permission.READ_PRIVILEGED_PHONE_STATE", CallRules.PHONE_STATE_SENDER_PERMISSION)
    }
}
