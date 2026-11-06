// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** A computer allowed the battery on the phone gets the level only after the core knows the permit; the reporter says what it did. */
class BatteryGrantPushTest {
    private val desk = "0123456789abcdef0123456789abcdef"

    @Test fun thePermitReachesTheCoreBeforeTheLevelIsPushed() {
        val steps = ArrayList<String>()
        val g = ComputerGrants().set(desk, GrantFeature.Battery, true)
        GrantsPublish.run(g, batteryChanged = true, applyPermits = { req ->
            val battery = JSONObject(req).getJSONObject("permits").getJSONArray("battery")
            steps += "permits:" + (0 until battery.length()).joinToString(",") { battery.getString(it) }
        }, pushBattery = { steps += "push" })
        assertEquals(listOf("permits:$desk", "push"), steps)
    }

    @Test fun otherChangesSendPermitsOnlyAndNeverPushTheLevel() {
        val steps = ArrayList<String>()
        val g = ComputerGrants().set(desk, GrantFeature.Sms, true)
        GrantsPublish.run(g, batteryChanged = false, applyPermits = { steps += "permits" }, pushBattery = { steps += "push" })
        assertEquals(listOf("permits"), steps)
    }

    @Test fun everySendAndRefusalLeavesOneLineAndTheLevelIsNeverInIt() {
        var core: CoreOutcome = CoreOutcome.Ok(JSONObject().put("queued", 1))
        var on = true
        var level: BatteryReading? = BatteryReading(61, false, false)
        val notes = ArrayList<String>()
        val r = BatteryReporter(read = { level }, core = { core }, enabled = { on }, later = { _, _ -> }, note = { notes += it })
        r.sendNow(4)
        core = CoreOutcome.Rejected("not_connected", "x")
        r.sendNow(4)
        core = CoreOutcome.Ok(JSONObject().put("queued", 1))
        r.sendNow()
        level = null
        r.sendNow(4)
        level = BatteryReading(61, false, false)
        on = false
        r.sendNow(4)
        assertEquals(
            listOf(
                "reply on connection 4: queued",
                "reply on connection 4: refused (not_connected)",
                "sent to all allowed computers: queued",
                "asked for the battery, not sent: the system gave no level",
                "asked for the battery, not sent: sharing is off or the link is not running",
            ),
            notes,
        )
        assertTrue(notes.none { it.contains("61") })
    }
}
