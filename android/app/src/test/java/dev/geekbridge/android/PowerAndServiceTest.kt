// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PowerAndServiceTest {
    @Test fun oemDetection() {
        assertEquals(Oem.Xiaomi, OemDetector.detect("Xiaomi", "Redmi"))
        assertEquals(Oem.Xiaomi, OemDetector.detect("Xiaomi", "POCO"))
        assertEquals(Oem.Samsung, OemDetector.detect("samsung", "samsung"))
        assertEquals(Oem.Huawei, OemDetector.detect("HUAWEI", "HONOR"))
        assertEquals(Oem.OnePlus, OemDetector.detect("OnePlus", null))
        assertEquals(Oem.Oppo, OemDetector.detect("realme", "realme"))
        assertEquals(Oem.Vivo, OemDetector.detect("vivo", "iQOO"))
        assertEquals(Oem.Pixel, OemDetector.detect("Google", "google"))
        assertEquals(Oem.Other, OemDetector.detect(null, null))
    }

    @Test fun everyKnownOemHasIntentsAndGuidance() {
        for (o in Oem.entries) {
            if (o == Oem.Other) {
                assertTrue(OemIntents.candidates(o).isEmpty())
                assertTrue(OemGuides.stepsRes(o) == null)
            } else if (!o.hasExtraSwitch) {
                // Nothing OS: no vendor screen exists, so every launch falls back to app settings; the guide still explains.
                assertTrue(OemIntents.candidates(o).isEmpty())
                assertTrue("$o guide", OemGuides.stepsRes(o) != null)
            } else {
                assertTrue("$o intents", OemIntents.candidates(o).isNotEmpty())
                assertTrue("$o guide", OemGuides.stepsRes(o) != null)
                assertTrue(OemIntents.candidates(o).all { it.pkg != null && it.cls != null })
            }
        }
    }

    @Test fun batteryStateClassification() {
        val c = PowerRules::classify
        assertEquals(BatteryState.Unrestricted, c(false, true, 10))
        assertEquals(BatteryState.Optimised, c(false, false, 10))
        assertEquals(BatteryState.Optimised, c(false, false, null))
        // The user's "Restricted" mode wins over everything, even a Doze exemption.
        assertEquals(BatteryState.BackgroundRestricted, c(true, true, 5))
        // `am set-standby-bucket restricted` (or Android doing it to a rarely used app) is a restriction ...
        assertEquals(BatteryState.BackgroundRestricted, c(false, false, PowerRules.BUCKET_RESTRICTED))
        // ... but a Doze-exempt app is not held back by its bucket, and "rare" is only the ordinary optimisation.
        assertEquals(BatteryState.Unrestricted, c(false, true, PowerRules.BUCKET_RESTRICTED))
        assertEquals(BatteryState.Optimised, c(false, false, 40))
    }

    @Test fun samsungAndPixelGuidesNameTheRealSwitches() {
        val res = java.io.File("src/main/res/values/gb_help.xml").readText()
        fun array(name: String) = Regex("<string-array name=\"$name\">(.*?)</string-array>", RegexOption.DOT_MATCHES_ALL).find(res)!!.groupValues[1]
        val samsung = array("power_steps_samsung")
        for (word in listOf("Never sleeping apps", "Put unused apps to sleep", "Adaptive battery", "Unrestricted")) assertTrue(word, word in samsung)
        val pixel = array("power_steps_pixel")
        for (word in listOf("Unrestricted", "Adaptive battery", "Restricted")) assertTrue(word, word in pixel)
        assertTrue("Autostart" in array("power_steps_xiaomi"))
        assertEquals(R.array.power_steps_pixel, OemGuides.stepsRes(Oem.Pixel))
    }

    @Test fun dozeWindowsAndTheEndOfDozePingAtOnce() {
        assertTrue(DozeRules.pingNow(idle = false))
        assertFalse(DozeRules.pingNow(idle = true))
    }

    @Test fun launcherStopsAtFirstScreenThatOpens() {
        val tried = ArrayList<String>()
        val fallbackCalls = intArrayOf(0)
        val specs = listOf(IntentSpec(pkg = "a", cls = "A"), IntentSpec(pkg = "b", cls = "B"), IntentSpec(pkg = "c", cls = "C"))
        val r = OemLauncher.launch(specs, { tried += it.pkg!!; it.pkg == "b" }) { fallbackCalls[0]++ }
        assertEquals(LaunchResult.Opened, r)
        assertEquals(listOf("a", "b"), tried)
        assertEquals(0, fallbackCalls[0])
    }

    @Test fun launcherFallsBackToAppSettings() {
        val fallbackCalls = intArrayOf(0)
        val r = OemLauncher.launch(listOf(IntentSpec(pkg = "a", cls = "A")), { false }) { fallbackCalls[0]++ }
        assertEquals(LaunchResult.OpenedFallback, r)
        assertEquals(1, fallbackCalls[0])
        assertEquals(LaunchResult.OpenedFallback, OemLauncher.launch(emptyList(), { true }) { fallbackCalls[0]++ })
        assertEquals(2, fallbackCalls[0])
    }

    @Test fun serviceRunsOnlyWithPairedComputerAndOptIn() {
        assertFalse(ServicePolicy.shouldRun(0, true))
        assertFalse(ServicePolicy.shouldRun(2, false))
        assertTrue(ServicePolicy.shouldRun(1, true))
        // A share in progress keeps the process alive even with "Stay connected" off (Android may kill a background
        // process with no foreground service mid-transfer), but never without a paired computer.
        assertTrue(ServicePolicy.shouldRun(1, false, sharing = true))
        assertFalse(ServicePolicy.shouldRun(0, false, sharing = true))
    }

    @Test fun aPairingInProgressKeepsTheServiceEvenBeforeAnyComputerIsPaired() {
        assertTrue(ServicePolicy.shouldRun(0, true, pairing = true))
        assertTrue(ServicePolicy.shouldRun(0, false, pairing = true))
        assertFalse(ServicePolicy.shouldRun(0, true, pairing = false))
        assertFalse(ServicePolicy.shouldRun(1, false, pairing = false))
    }

    @Test fun wifiTrackingRestartsOnlyForAnotherWifiNetworkAndNudgesOnTheFirst() {
        assertEquals(NetworkAction.Nudge, ServicePolicy.onWifiAvailable(null, 5))
        assertEquals(NetworkAction.None, ServicePolicy.onWifiAvailable(5, 5))
        assertEquals(NetworkAction.Restart, ServicePolicy.onWifiAvailable(5, 6))
        assertTrue(ScreenRules.nudgeOnScreenOn)
    }

    @Test fun networkRestartIgnoresFirstCallbackAndSameNetwork() {
        assertFalse(ServicePolicy.shouldRestartOnNetwork(null, 5))
        assertFalse(ServicePolicy.shouldRestartOnNetwork(5, 5))
        assertTrue(ServicePolicy.shouldRestartOnNetwork(5, 6))
    }

    @Test fun wifiLockOnlyWhilePairedAndLinkedOrJustLost() {
        assertEquals(false, WifiLockPolicy.wanted(0, true, null)) // nobody paired: never
        assertEquals(false, WifiLockPolicy.wanted(1, false, null)) // idle without a link: not held
        assertEquals(true, WifiLockPolicy.wanted(1, true, null))
        assertEquals(true, WifiLockPolicy.wanted(2, false, 30_000L)) // redial window after a loss
        assertEquals(false, WifiLockPolicy.wanted(1, false, WifiLockPolicy.LINGER_MS))
    }

    @Test fun wifiLockModesFollowTheApiLevel() {
        val hp = android.net.wifi.WifiManager.WIFI_MODE_FULL_HIGH_PERF
        val ll = android.net.wifi.WifiManager.WIFI_MODE_FULL_LOW_LATENCY
        assertEquals(listOf(hp), WifiLockPolicy.modes(26))
        assertEquals(listOf(hp), WifiLockPolicy.modes(28))
        assertEquals(listOf(hp, ll), WifiLockPolicy.modes(29))
        assertEquals(listOf(hp, ll), WifiLockPolicy.modes(35))
    }

    @Test fun wifiBindChoosesTheOnlyUsableWifiAddress() {
        assertEquals("192.168.1.20", WifiBindPolicy.choose(listOf(listOf("192.168.1.20"))))
        assertEquals("192.168.1.20", WifiBindPolicy.choose(listOf(listOf("192.168.1.20", "192.168.1.20"))))
        assertEquals("10.0.0.7", WifiBindPolicy.choose(listOf(listOf("169.254.3.4", "10.0.0.7")))) // link-local ignored
        assertEquals(null, WifiBindPolicy.choose(emptyList())) // no Wi-Fi: unbound
        assertEquals(null, WifiBindPolicy.choose(listOf(emptyList()))) // IPv6-only Wi-Fi
        assertEquals(null, WifiBindPolicy.choose(listOf(listOf("192.168.1.20"), listOf("10.0.0.7")))) // ambiguous
        assertEquals(null, WifiBindPolicy.choose(listOf(listOf("192.168.1.20", "192.168.2.20"))))
    }

    @Test fun wifiBindRejectsAddressesThatCannotBeLeftFrom() {
        for (bad in listOf("", "0.0.0.0", "127.0.0.1", "224.0.0.1", "255.255.255.255", "169.254.1.1", "300.1.1.1", "fe80::1", "1.2.3", "a.b.c.d"))
            assertFalse(bad, WifiBindPolicy.usable(bad))
        for (good in listOf("192.168.0.2", "10.1.2.3", "172.16.5.5", "100.64.0.9")) assertTrue(good, WifiBindPolicy.usable(good))
    }

    @Test fun wifiBindIsPushedOnceFirstAndThenOnlyOnChange() {
        assertTrue(WifiBindPolicy.shouldPush(first = true, last = null, now = null)) // the core outlives the service
        assertFalse(WifiBindPolicy.shouldPush(first = false, last = "10.0.0.7", now = "10.0.0.7"))
        assertTrue(WifiBindPolicy.shouldPush(first = false, last = "10.0.0.7", now = "10.0.0.8"))
        assertTrue(WifiBindPolicy.shouldPush(first = false, last = "10.0.0.7", now = null)) // Wi-Fi lost: clear
        assertFalse(WifiBindPolicy.shouldPush(first = false, last = null, now = null))
    }

    @Test fun review3GraceCheckKeepsLookingWhileAnExtendedGraceRuns() {
        // A second pairing prompt extends the grace after the first check was scheduled: the check that finds
        // grace left must schedule another, or an unpaired phone keeps the service (and its listener) forever.
        assertEquals(null, ServicePolicy.graceCheckDelayMs(pairedCount = 1, graceLeftMs = 5_000))
        assertEquals(null, ServicePolicy.graceCheckDelayMs(pairedCount = 0, graceLeftMs = 0))
        assertEquals(60_500L, ServicePolicy.graceCheckDelayMs(pairedCount = 0, graceLeftMs = 60_000))
    }
}
