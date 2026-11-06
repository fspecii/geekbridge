// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The idle link service keeps no thread that only a rare feature needs (docs/android-perf.md, "Threads"). */
class IdleThreadsTest {
    @Test fun smsStatusThreadStartsOnFirstSend() {
        val dir = File("src/main/java/dev/geekbridge/android")
        assertTrue("run from android/app", dir.isDirectory)
        val t = File(dir, "PhoneDataAndroid.kt").readText()
        assertTrue(t.contains("private val callbacks by lazy"))
        assertFalse(Regex("""val callbacks = Handler\(\s*android\.os\.HandlerThread""").containsMatchIn(t))
    }
}
