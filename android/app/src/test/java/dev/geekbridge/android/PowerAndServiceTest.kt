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
        assertEquals(Oem.Other, OemDetector.detect("Google", "google"))
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
    }

    @Test fun aPairingInProgressKeepsTheServiceEvenBeforeAnyComputerIsPaired() {
        assertTrue(ServicePolicy.shouldRun(0, true, pairing = true))
        assertTrue(ServicePolicy.shouldRun(0, false, pairing = true))
        assertFalse(ServicePolicy.shouldRun(0, true, pairing = false))
        assertFalse(ServicePolicy.shouldRun(1, false, pairing = false))
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

    @Test fun wifiLockModeFollowsTheApiLevel() {
        assertEquals(android.net.wifi.WifiManager.WIFI_MODE_FULL_HIGH_PERF, WifiLockPolicy.mode(28))
        assertEquals(android.net.wifi.WifiManager.WIFI_MODE_FULL_LOW_LATENCY, WifiLockPolicy.mode(29))
        assertEquals(android.net.wifi.WifiManager.WIFI_MODE_FULL_LOW_LATENCY, WifiLockPolicy.mode(35))
    }

    @Test fun review3GraceCheckKeepsLookingWhileAnExtendedGraceRuns() {
        // A second pairing prompt extends the grace after the first check was scheduled: the check that finds
        // grace left must schedule another, or an unpaired phone keeps the service (and its listener) forever.
        assertEquals(null, ServicePolicy.graceCheckDelayMs(pairedCount = 1, graceLeftMs = 5_000))
        assertEquals(null, ServicePolicy.graceCheckDelayMs(pairedCount = 0, graceLeftMs = 0))
        assertEquals(60_500L, ServicePolicy.graceCheckDelayMs(pairedCount = 0, graceLeftMs = 60_000))
    }
}
