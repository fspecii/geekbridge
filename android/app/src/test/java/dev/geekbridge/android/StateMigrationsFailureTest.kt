// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.After
import org.junit.Test

/**
 * Failure paths of the saved-data upgrade: a missing or throwing step must leave the data exactly as it was and put
 * the app in read-only mode (so a half-upgraded or newer file is never overwritten), and the one-generation blob
 * backup must never be replaced by something damaged.
 */
class StateMigrationsFailureTest {
    private class Mem : KeyValues {
        val m = HashMap<String, Any>()
        override fun getString(key: String) = m[key] as? String
        override fun putString(key: String, value: String?) { if (value == null) m.remove(key) else m[key] = value }
        override fun getInt(key: String, default: Int) = (m[key] as? Number)?.toInt() ?: default
        override fun putInt(key: String, value: Int) { m[key] = value }
    }

    /** `StateMigrations.last` is process-wide: leave it in the normal state for whichever test class runs next. */
    @After fun resetReadOnly() { StateMigrations.run(opener(HashMap()), steps = emptyList(), current = 1) }

    private fun opener(files: MutableMap<String, Mem>): (String) -> KeyValues = { files.getOrPut(it) { Mem() } }

    @Test fun `a version gap with no step is reported and nothing is rewritten`() {
        val files = HashMap<String, Mem>()
        val meta = opener(files)(StateMigrations.FILE)
        meta.putInt(StateMigrations.KEY, 1)
        files.getOrPut("data") { Mem() }.putString("pairs", "[\"a\"]")
        val out = StateMigrations.run(opener(files), steps = emptyList(), current = 3)
        assertTrue(out.readOnly)
        assertEquals(1, out.from)
        assertEquals(1, out.to)
        assertTrue(out.message!!.contains("could not be upgraded"))
        assertEquals("[\"a\"]", files["data"]!!.getString("pairs"))
        assertEquals("stored version stays at 1", 1, meta.getInt(StateMigrations.KEY, 0))
        assertTrue(StateMigrations.readOnly)
        assertEquals(out.message, StateMigrations.diagnostics())
    }

    @Test fun `a step that throws stops the chain at the last completed version`() {
        val files = HashMap<String, Mem>()
        val ran = ArrayList<String>()
        val steps = listOf(
            StateMigrations.Step(1, "one") { ran += "one" },
            StateMigrations.Step(2, "two") { error("disk full") },
            StateMigrations.Step(3, "three") { ran += "three" },
        )
        val out = StateMigrations.run(opener(files), steps = steps, current = 4)
        assertEquals(listOf("one"), ran)
        assertTrue(out.readOnly)
        assertEquals(listOf("one"), out.applied)
        assertEquals(2, out.to)
        assertTrue(out.message!!.contains("disk full"))
        assertEquals("progress up to the failure is kept so the next start resumes there", 2, files[StateMigrations.FILE]!!.getInt(StateMigrations.KEY, 0))

        // A second start with the step fixed resumes at version 2, not at 1, and finishes.
        val fixed = listOf(steps[0], StateMigrations.Step(2, "two") { ran += "two" }, steps[2])
        val again = StateMigrations.run(opener(files), steps = fixed, current = 4)
        assertFalse(again.readOnly)
        assertEquals(listOf("two", "three"), again.applied)
        assertEquals(4, files[StateMigrations.FILE]!!.getInt(StateMigrations.KEY, 0))
        assertNull(again.message)
        assertFalse(StateMigrations.readOnly)
    }

    @Test fun `a stored version below one is treated as version one`() {
        val files = HashMap<String, Mem>()
        files.getOrPut(StateMigrations.FILE) { Mem() }.putInt(StateMigrations.KEY, -7)
        val out = StateMigrations.run(opener(files), steps = emptyList(), current = 1)
        assertEquals(1, out.from)
        assertFalse(out.readOnly)
        assertEquals(1, files[StateMigrations.FILE]!!.getInt(StateMigrations.KEY, 0))
    }

    @Test fun `a blob write keeps the last good value as backup and never backs up a damaged or empty one`() {
        val kv = Mem()
        val b = BackedBlob(kv, "k", BackedBlob::jsonObject, readOnly = { false }, note = {})
        b.write("{\"v\":1}")
        b.write("{\"v\":2}")
        assertEquals("{\"v\":1}", kv.getString("k.bak"))
        // Damaged main: the next write must not displace the good backup with it.
        kv.putString("k", "{ truncated")
        b.write("{\"v\":3}")
        assertEquals("{\"v\":1}", kv.getString("k.bak"))
        // Empty main is not worth backing up either.
        kv.putString("k", "")
        b.write("{\"v\":4}")
        assertEquals("{\"v\":1}", kv.getString("k.bak"))
        assertEquals("{\"v\":4}", b.read())
    }

    @Test fun `a read-only blob ignores writes entirely and reports what it restored`() {
        val kv = Mem()
        val notes = ArrayList<String>()
        var readOnly = false
        val b = BackedBlob(kv, "k", BackedBlob::jsonArray, readOnly = { readOnly }, note = { notes += it })
        b.write("[1]")
        readOnly = true
        b.write("[2]")
        assertEquals("[1]", kv.getString("k"))
        assertNull(kv.getString("k.bak"))

        readOnly = false
        kv.putString("k", "[oops")
        kv.putString("k.bak", "[1]")
        assertEquals("[1]", b.read())
        assertTrue(notes.single().contains("restored the previous copy"))
        kv.putString("k.bak", "also broken")
        assertEquals("", b.read())
        assertTrue(notes.last().contains("no backup"))
        kv.putString("k", "")
        assertEquals("", b.read())
        kv.putString("k", "[]") // valid and empty list is fine and needs no note
        val before = notes.size
        assertEquals("[]", b.read())
        assertEquals(before, notes.size)
    }
}
