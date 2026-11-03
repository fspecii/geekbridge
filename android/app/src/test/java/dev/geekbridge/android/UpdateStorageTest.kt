// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class UpdateStorageTest {
    @get:Rule val tmp = TemporaryFolder()

    @Test fun purgeRemovesOnlyFilesOlderThanTheLimit() {
        val dir = tmp.newFolder("updates")
        val old = File(dir, "geekbridge-update.apk").apply { writeBytes(ByteArray(10)); setLastModified(1_000) }
        val fresh = File(dir, "fresh.apk").apply { writeBytes(ByteArray(10)); setLastModified(9_000) }
        assertEquals(1, UpdateStorage.purge(dir, olderThanMs = 5_000, nowMs = 10_000))
        assertFalse(old.exists())
        assertTrue(fresh.exists())
    }

    @Test fun purgeWithZeroLimitClearsEverythingAndToleratesAMissingDirectory() {
        val dir = tmp.newFolder("updates2")
        File(dir, "a.apk").writeBytes(ByteArray(1))
        File(dir, "b.part").writeBytes(ByteArray(1))
        assertEquals(2, UpdateStorage.purge(dir, olderThanMs = 0, nowMs = System.currentTimeMillis() + 1))
        assertEquals(0, dir.list()!!.size)
        assertEquals(0, UpdateStorage.purge(File(tmp.root, "nope"), 0, 1))
    }
}
