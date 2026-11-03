// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SmsReadinessTest {
    private val G = PermissionStatus.Granted
    private fun all(s: PermissionStatus) = SmsPerm.entries.associateWith { s }
    private fun eval(
        on: Boolean = true, st: Map<SmsPerm, PermissionStatus> = all(G), restricted: Boolean = false, paired: Int = 1, allowed: Int = 1,
    ) = SmsReadinessLogic.evaluate(on, st, restricted, paired, allowed)

    @Test fun readyOnlyWhenSwitchPermissionsAndComputerAllAgree() {
        assertEquals(SmsStep.Ready, eval().step)
        assertEquals(SmsStep.TurnOn, eval(on = false).step)
        assertEquals(SmsStep.PairComputer, eval(paired = 0, allowed = 0).step)
        assertEquals(SmsStep.ChooseComputer, eval(paired = 2, allowed = 0).step)
    }

    @Test fun switchIsReportedBeforePermissions() {
        assertEquals(SmsStep.TurnOn, eval(on = false, st = all(PermissionStatus.NotAsked)).step)
    }

    @Test fun partialGrantNamesExactlyWhatIsMissing() {
        val st = mapOf(SmsPerm.Read to G, SmsPerm.Send to G, SmsPerm.Receive to PermissionStatus.NotAsked)
        val r = eval(st = st)
        assertEquals(SmsStep.Allow, r.step)
        assertEquals(listOf(SmsPerm.Receive), r.missing)
        assertFalse(r.ready)
        val onlyRead = mapOf(SmsPerm.Read to G, SmsPerm.Send to PermissionStatus.Denied, SmsPerm.Receive to PermissionStatus.Denied)
        assertEquals(listOf(SmsPerm.Send, SmsPerm.Receive), eval(st = onlyRead).missing)
        assertEquals(SmsStep.Allow, eval(st = onlyRead).step)
    }

    @Test fun permanentDenialPointsToSettingsOrToTheRestrictedSetting() {
        val blocked = mapOf(SmsPerm.Read to PermissionStatus.Blocked, SmsPerm.Send to G, SmsPerm.Receive to G)
        assertEquals(SmsStep.OpenSettings, eval(st = blocked).step)
        assertEquals(SmsStep.AllowRestricted, eval(st = blocked, restricted = true).step)
        // Restricted-capable but never asked yet: just ask.
        assertEquals(SmsStep.Allow, eval(st = all(PermissionStatus.NotAsked), restricted = true).step)
    }

    @Test fun everyStateHasPlainWordsAndNamesTheMissingPermission() {
        val st = mapOf(SmsPerm.Read to PermissionStatus.Blocked, SmsPerm.Send to G, SmsPerm.Receive to PermissionStatus.NotAsked)
        for (r in listOf(eval(on = false), eval(st = st), eval(st = st, restricted = true), eval(paired = 0), eval(allowed = 0), eval())) {
            assertTrue(SmsReadinessLogic.message(r, TestStrings).isNotBlank())
        }
        val m = SmsReadinessLogic.message(eval(st = st), TestStrings)
        assertTrue(m, "read your texts" in m && "see new texts" in m && "send texts" !in m)
        assertTrue("restricted" in SmsReadinessLogic.message(eval(st = st, restricted = true), TestStrings))
        assertEquals("On", SmsReadinessLogic.tileWord(eval(), TestStrings))
        assertEquals("Off", SmsReadinessLogic.tileWord(eval(on = false), TestStrings))
        assertEquals("Needs setup", SmsReadinessLogic.tileWord(eval(allowed = 0), TestStrings))
    }

    @Test fun gateRequiresAllThreePermissionsAndTheSwitch() {
        val s = PhoneDataSettings(sms = true)
        fun sms(r: Boolean, w: Boolean, v: Boolean) = PhoneDataGate.active(s, PhoneDataPermissions(readSms = r, sendSms = w, receiveSms = v)).sms
        assertTrue(sms(true, true, true))
        assertFalse(sms(true, false, true))
        assertFalse(sms(false, true, true))
        assertFalse(sms(true, true, false))
        assertFalse(PhoneDataGate.active(PhoneDataSettings(sms = false), PhoneDataPermissions(readSms = true, sendSms = true, receiveSms = true)).sms)
    }

    @Test fun grantingAfterTheLinkIsUpChangesTheDeclaredCapabilities() {
        val before = PhoneDataGate.active(PhoneDataSettings(sms = true), PhoneDataPermissions(readSms = true, sendSms = true))
        val after = PhoneDataGate.active(PhoneDataSettings(sms = true), PhoneDataPermissions(readSms = true, sendSms = true, receiveSms = true))
        assertTrue(CapabilityRedeclare.needed(before, after)) // reconnect to re-declare
        assertFalse(CapabilityRedeclare.needed(after, after))
        assertTrue(CapabilityRedeclare.needed(after, before)) // revoked later: withdrawn too
    }

    @Test fun perComputerGrantIsKeyedByThePairedDeviceId() {
        val g = ComputerGrants(migrated = true).set("dev-a", GrantFeature.Sms, true)
        assertTrue(g.allows("dev-a", GrantFeature.Sms))
        assertFalse(g.allows("dev-b", GrantFeature.Sms))
        assertEquals(setOf("dev-a"), g.permitsObject().getJSONArray("sms").let { a -> (0 until a.length()).map { a.getString(it) }.toSet() })
    }

    @Test fun restrictedSettingIsOnlyBlamedForSideloadedAndroid13Plus() {
        assertTrue(SmsRestriction.likely(33, true, null)) // adb
        assertTrue(SmsRestriction.likely(34, true, "com.google.android.packageinstaller"))
        assertFalse(SmsRestriction.likely(33, true, "com.android.vending"))
        assertFalse(SmsRestriction.likely(33, true, "com.sec.android.app.samsungapps"))
        assertFalse(SmsRestriction.likely(32, true, null))
        assertFalse(SmsRestriction.likely(33, false, null))
    }

    @Test fun permissionChangeRedeclaresAtOnceWhenLinkedAndAtLinkUpOtherwise() {
        val off = PhoneDataGate.active(PhoneDataSettings(sms = true), PhoneDataPermissions(readSms = true, sendSms = true))
        val on = PhoneDataGate.active(PhoneDataSettings(sms = true), PhoneDataPermissions(readSms = true, sendSms = true, receiveSms = true))
        assertFalse(off.sms)
        assertTrue(on.sms)
        assertEquals(CapabilityRedeclare.Action.Reconnect, CapabilityRedeclare.onChange(off, on, linkRunning = true))
        assertEquals(CapabilityRedeclare.Action.Observe, CapabilityRedeclare.onChange(on, on, linkRunning = true))
        assertEquals(CapabilityRedeclare.Action.None, CapabilityRedeclare.onChange(off, on, linkRunning = false))
        assertTrue(CapabilityRedeclare.driftAtLinkUp(off, on)) // answered while connecting
        assertFalse(CapabilityRedeclare.driftAtLinkUp(on, on))
    }

    @Test fun clipboardSwitchedOnOnALinkThatDidNotDeclareItReconnects() {
        assertTrue(CapabilityRedeclare.clipboardNeedsReconnect(declaredEnabled = false, nowEnabled = true, linkRunning = true))
        assertFalse(CapabilityRedeclare.clipboardNeedsReconnect(declaredEnabled = true, nowEnabled = true, linkRunning = true)) // already declared
        assertFalse(CapabilityRedeclare.clipboardNeedsReconnect(declaredEnabled = true, nowEnabled = false, linkRunning = true)) // off needs no new identity
        assertFalse(CapabilityRedeclare.clipboardNeedsReconnect(declaredEnabled = false, nowEnabled = true, linkRunning = false)) // next start declares it
    }

    @Test fun switchingTheSwitchOnWithoutPermissionsDeclaresNothingUntilGranted() {
        assertFalse(PhoneDataGate.active(PhoneDataSettings(sms = true), PhoneDataPermissions()).sms)
        assertFalse(PhoneDataGate.active(PhoneDataSettings(sms = true), PhoneDataPermissions(readSms = true, sendSms = true, receiveSms = false)).sms)
    }
}
