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

    @Test fun networkRestartIgnoresFirstCallbackAndSameNetwork() {
        assertFalse(ServicePolicy.shouldRestartOnNetwork(null, 5))
        assertFalse(ServicePolicy.shouldRestartOnNetwork(5, 5))
        assertTrue(ServicePolicy.shouldRestartOnNetwork(5, 6))
    }
}
