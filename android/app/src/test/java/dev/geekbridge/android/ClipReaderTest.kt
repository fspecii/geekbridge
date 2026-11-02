// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ClipReaderTest {
    private fun raw(uri: String? = "content://evil/1", declared: String? = "image/png", text: CharSequence? = null) =
        RawClip(text, null, uri, declared, sensitive = false, timestamp = 0)

    private val pool = Executors.newCachedThreadPool { Thread(it).apply { isDaemon = true } }

    @Test fun hungProviderCostsOnlyTheTimeoutAndFallsBackToDeclaredType() {
        val started = CountDownLatch(1)
        val cancelled = CountDownLatch(1)
        val probe = MimeProbe { _, token ->
            started.countDown()
            token.onCancel { cancelled.countDown() }
            Thread.sleep(60_000) // a provider that never answers
            "image/png"
        }
        val t0 = System.nanoTime()
        val snap = ClipReader.snapshot(raw(), probe, nowMs = 1000, timeoutMs = 150, executor = pool)
        val elapsedMs = (System.nanoTime() - t0) / 1_000_000
        assertTrue("took ${elapsedMs}ms", elapsedMs < 2000)
        assertTrue(started.await(1, TimeUnit.SECONDS))
        assertTrue("provider was told to cancel", cancelled.await(1, TimeUnit.SECONDS))
        assertEquals(ClipKind.Image, snap?.kind)
        assertEquals("image/png", snap?.imageMime)
    }

    @Test fun hungProviderWithNonImageDeclaredTypeYieldsNothing() {
        val probe = MimeProbe { _, _ -> Thread.sleep(60_000); "image/png" }
        assertNull(ClipReader.snapshot(raw(declared = "application/zip"), probe, 1000, timeoutMs = 100, executor = pool))
    }

    @Test fun providerIsNeverTouchedOnTheCallingThread() {
        var probeThread: Thread? = null
        val probe = MimeProbe { _, _ -> probeThread = Thread.currentThread(); "image/jpeg" }
        val snap = ClipReader.snapshot(raw(declared = null), probe, 1000, timeoutMs = 1000, executor = pool)
        assertEquals("image/jpeg", snap?.imageMime)
        assertNotEquals(Thread.currentThread(), probeThread)
    }

    @Test fun throwingProviderFallsBackToDeclaredType() {
        val probe = MimeProbe { _, _ -> throw SecurityException("no") }
        assertEquals("image/png", ClipReader.snapshot(raw(), probe, 1000, executor = pool)?.imageMime)
    }

    @Test fun textClipsNeverCallTheProvider() {
        val probe = MimeProbe { _, _ -> error("must not be called") }
        val snap = ClipReader.snapshot(raw(uri = null, text = "hello"), probe, 1000, executor = pool)
        assertEquals(ClipKind.Text, snap?.kind)
    }

    @Test fun boundedCallReturnsNullOnTimeoutAndValueOtherwise() {
        assertEquals(7, BoundedCall.run(1000, pool) { 7 })
        assertNull(BoundedCall.run(50, pool) { Thread.sleep(60_000); 1 })
    }
}
