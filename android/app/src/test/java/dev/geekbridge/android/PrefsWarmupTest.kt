// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PrefsWarmupTest {
    private val src = File("src/main/java/dev/geekbridge/android")

    @Test fun noDuplicates() = assertEquals(PrefsWarmup.FILES.size, PrefsWarmup.FILES.toSet().size)

    /** Every preference file named in the sources is warmed (the migration meta file is read before the worker runs). */
    @Test fun everyFileOpenedInTheSourcesIsWarmed() {
        assertTrue("run from android/app", src.isDirectory)
        val named = Regex("""(?:private )?const val (?:FILE|PREFS) = "([A-Za-z_]+)"""")
        val literal = Regex("""getSharedPreferences\("([A-Za-z_]+)"""")
        val found = sortedSetOf<String>()
        for (f in src.listFiles { x -> x.extension == "kt" }!!) {
            val t = f.readText()
            if (f.name == "StateMigrations.kt") continue
            named.findAll(t).forEach { found += it.groupValues[1] }
            literal.findAll(t).forEach { found += it.groupValues[1] }
        }
        val missing = found.filter { it !in PrefsWarmup.FILES }
        assertTrue("not in PrefsWarmup.FILES: $missing", missing.isEmpty())
    }
}
