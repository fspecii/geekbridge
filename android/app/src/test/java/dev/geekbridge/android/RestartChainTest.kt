// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import dev.geekbridge.android.ServicePolicy.RestartStep
import org.junit.Assert.assertEquals
import org.junit.Test

class RestartChainTest {
    @Test fun firstStepStopsARunningLinkAndStartsAStoppedOne() {
        assertEquals(RestartStep.Stop, ServicePolicy.restartStep(0, LinkPhase.Running))
        assertEquals(RestartStep.Start, ServicePolicy.restartStep(0, LinkPhase.Stopped))
        assertEquals(RestartStep.Wait, ServicePolicy.restartStep(0, LinkPhase.Stopping))
    }

    // The chain stopped the old link; a link that is Running on a retry was started by someone else after that
    // (capability re-declare, user start) on the new network already. Stopping it would cut the new link.
    @Test fun aRetryNeverStopsALinkThatCameUpAfterTheChainStoppedTheOldOne() {
        assertEquals(RestartStep.Done, ServicePolicy.restartStep(1, LinkPhase.Running))
        assertEquals(RestartStep.Done, ServicePolicy.restartStep(5, LinkPhase.Running))
        assertEquals(RestartStep.Start, ServicePolicy.restartStep(1, LinkPhase.Stopped))
        assertEquals(RestartStep.Wait, ServicePolicy.restartStep(1, LinkPhase.Starting))
    }
}
