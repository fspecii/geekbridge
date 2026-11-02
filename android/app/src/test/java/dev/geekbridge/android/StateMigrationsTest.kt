// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** In-memory preferences files; loads the golden fixtures in resources/state-v1. */
private class Prefs {
    val files = HashMap<String, HashMap<String, Any>>()
    fun kv(name: String) = object : KeyValues {
        val m = files.getOrPut(name) { HashMap() }
        override fun getString(key: String) = m[key] as? String
        override fun putString(key: String, value: String?) { if (value == null) m.remove(key) else m[key] = value }
        override fun getInt(key: String, default: Int) = (m[key] as? Number)?.toInt() ?: default
        override fun putInt(key: String, value: Int) { m[key] = value }
    }
    val open: (String) -> KeyValues = { kv(it) }
    fun snapshot() = files.mapValues { it.value.toMap() }

    companion object {
        fun fixtures(): Prefs {
            val p = Prefs()
            for (name in listOf("geekbridge_state", "geekbridge_clipboard", "geekbridge_calls", "geekbridge_phonedata", "geekbridge_grants", "share_policy", "geekbridge_device_name", "updates")) {
                val text = Prefs::class.java.getResource("/state-v1/$name.json")!!.readText()
                val o = JSONObject(text)
                val m = p.files.getOrPut(name) { HashMap() }
                for (k in o.keys()) m[k] = when (val v = o.get(k)) {
                    is JSONArray -> (0 until v.length()).map { v.getString(it) }.toSet()
                    else -> v
                }
            }
            return p
        }
    }
}

class StateMigrationsTest {
    private val desk = "0123456789abcdef0123456789abcdef"

    @Test fun `v1 fixtures have no version and load unchanged after the entry point runs`() {
        val p = Prefs.fixtures()
        val before = p.snapshot()
        val o = StateMigrations.run(p.open)
        assertEquals(1, o.from); assertEquals(1, o.to); assertFalse(o.readOnly); assertNull(o.message)
        assertEquals(1, p.kv("gb_state").getInt(StateMigrations.KEY, 0))
        // Nothing but the new version file changed.
        assertEquals(before, p.snapshot() - "gb_state")
    }

    @Test fun `pairing, per-computer opt-ins, clipboard and device name survive the migration`() {
        val p = Prefs.fixtures()
        StateMigrations.run(p.open)
        val app = p.kv("geekbridge_state")
        assertEquals(1, app.getInt("paired_count", 0))
        assertTrue((app.getString("paired_peers") ?: "").startsWith(desk))
        val grants = ComputerGrants.fromJson(BackedBlob(p.kv("geekbridge_grants"), "grants", BackedBlob::jsonObject).read())
        assertTrue(grants.migrated)
        assertTrue(grants.allows(desk, GrantFeature.Contacts)); assertTrue(grants.allows(desk, GrantFeature.Calendar))
        assertFalse(grants.allows(desk, GrantFeature.Sms)); assertFalse(grants.allows("f".repeat(32), GrantFeature.Contacts))
        assertEquals("Ana Pixel", p.files["geekbridge_device_name"]!!["name"])
        assertEquals(true, p.files["geekbridge_clipboard"]!!["enabled"])
        assertEquals(setOf("1", "7"), p.files["geekbridge_phonedata"]!!["calendar_ids"])
        assertEquals(true, p.files["share_policy"]!!["auto_accept:$desk"])
    }

    @Test fun `the ledger blob in the fixture is readable by the current code`() {
        val p = Prefs.fixtures()
        val ledger = OpLedger(BackedBlob(p.kv("geekbridge_phonedata"), "calendar_ops", BackedBlob::jsonArray).let { b ->
            object : BlobStore { override fun read() = b.read(); override fun write(text: String) = b.write(text) }
        })
        assertEquals("applied", ledger.outcome("op-aaaa1111")?.state)
    }

    @Test fun `a fake v2 migration runs in order, saves progress and is idempotent`() {
        val p = Prefs.fixtures()
        val ran = ArrayList<String>()
        val steps = listOf(
            StateMigrations.Step(1, "1to2") { open -> ran += "1to2"; open("geekbridge_clipboard").putString("mode", "text") },
            StateMigrations.Step(2, "2to3") { open -> ran += "2to3"; open("geekbridge_clipboard").putString("mode", open("geekbridge_clipboard").getString("mode") + "+3") },
        )
        val o = StateMigrations.run(p.open, steps, current = 3)
        assertEquals(listOf("1to2", "2to3"), ran); assertEquals(3, o.to)
        assertEquals("text+3", p.kv("geekbridge_clipboard").getString("mode"))
        StateMigrations.run(p.open, steps, current = 3)
        assertEquals(listOf("1to2", "2to3"), ran) // second run is a no-op
    }

    @Test fun `a crash mid-migration resumes from the last saved step`() {
        val p = Prefs.fixtures()
        var fail = true
        val ran = ArrayList<String>()
        val steps = listOf(
            StateMigrations.Step(1, "a") { ran += "a" },
            StateMigrations.Step(2, "b") { ran += "b"; if (fail) error("boom") },
        )
        val first = StateMigrations.run(p.open, steps, 3)
        assertTrue(first.readOnly); assertEquals(2, p.kv("gb_state").getInt(StateMigrations.KEY, 0))
        fail = false
        val second = StateMigrations.run(p.open, steps, 3)
        assertFalse(second.readOnly); assertEquals(listOf("a", "b", "b"), ran)
    }

    @Test fun `data from a newer version is neither migrated nor overwritten`() {
        val p = Prefs.fixtures()
        p.kv("gb_state").putInt(StateMigrations.KEY, 9)
        val before = p.snapshot()
        var touched = false
        val o = StateMigrations.run(p.open, listOf(StateMigrations.Step(1, "x") { touched = true }), current = 2)
        assertTrue(o.readOnly); assertFalse(touched); assertTrue(o.message!!.contains("newer"))
        assertEquals(before, p.snapshot())
        // Blob writers keep off while read-only.
        val b = BackedBlob(p.kv("geekbridge_grants"), "grants", BackedBlob::jsonObject, readOnly = { o.readOnly })
        b.write("{}")
        assertEquals(before["geekbridge_grants"], p.snapshot()["geekbridge_grants"])
    }

    @Test fun `corrupt blob falls back to the backup, then to empty, and never throws`() {
        val kv = Prefs().kv("f")
        val notes = ArrayList<String>()
        val b = BackedBlob(kv, "grants", BackedBlob::jsonObject, readOnly = { false }, note = { notes += it })
        assertEquals("", b.read()); assertTrue(notes.isEmpty()) // missing is normal
        b.write("""{"v":1}"""); b.write("""{"v":2}""")
        assertEquals("""{"v":1}""", kv.getString("grants.bak"))
        kv.putString("grants", """{"v":2""") // truncated
        assertEquals("""{"v":1}""", b.read()); assertEquals(1, notes.size)
        // A bad value is never promoted into the backup.
        b.write("""{"v":3}""")
        assertEquals("""{"v":3}""", b.read())
        kv.putString("grants", ""); kv.putString("grants.bak", "garbage")
        assertEquals("", b.read()); assertEquals(2, notes.size)
        assertEquals(ComputerGrants(), ComputerGrants.fromJson(b.read().ifEmpty { null }))
    }

    @Test fun `corrupt ledger blob starts empty instead of crashing`() {
        val kv = Prefs().kv("f")
        kv.putString("calendar_ops", "[{broken")
        val b = BackedBlob(kv, "calendar_ops", BackedBlob::jsonArray, readOnly = { false }, note = {})
        assertFalse(OpLedger(object : BlobStore { override fun read() = b.read(); override fun write(text: String) = b.write(text) }).known("op-x"))
    }
}
