// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PrefetchPoolTest {
    private class Handle : AutoCloseable {
        val closed = AtomicInteger()
        override fun close() { closed.incrementAndGet() }
    }

    private fun waitFor(what: String, cond: () -> Boolean) {
        val end = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (!cond()) { check(System.nanoTime() < end) { "timed out: $what" }; Thread.sleep(5) }
    }

    @Test fun slowFirstItemDoesNotDelayTheOthers() {
        val pool = newPrefetchPool()
        try {
            val release = CountDownLatch(1)
            val slow = EarlyOpen { release.await(5, TimeUnit.SECONDS); Handle() }
            val rest = List(3) { EarlyOpen { Handle() } }
            val all = listOf(slow) + rest
            prefetchAll(all, pool)
            waitFor("items 2-4 settle") { rest.all { it.isSettled() } }
            assertFalse(slow.isSettled())
            assertFalse(allSettled(all))
            release.countDown()
            waitFor("item 1 settles") { slow.isSettled() }
            assertTrue(allSettled(all))
        } finally { pool.shutdownNow() }
    }

    @Test fun throwingOpenSettlesOnlyItsOwnItemAndTakeReportsIt() {
        val bad = EarlyOpen<Handle> { throw SecurityException("grant revoked") }
        val good = EarlyOpen { Handle() }
        val pool = newPrefetchPool()
        try {
            prefetchAll(listOf(bad, good), pool)
            waitFor("both settle") { bad.isSettled() && good.isSettled() }
        } finally { pool.shutdownNow() }
        try { bad.take(); throw AssertionError("expected SecurityException") } catch (_: SecurityException) { }
        good.take()
    }

    @Test fun discardAfterPrefetchClosesHeldHandles() {
        val handles = List(3) { Handle() }
        val sources = handles.map { h -> EarlyOpen { h } }
        val pool = newPrefetchPool()
        try {
            prefetchAll(sources, pool)
            waitFor("settle") { allSettled(sources) }
        } finally { pool.shutdownNow() }
        sources.forEach { it.discard() }
        handles.forEach { assertEquals(1, it.closed.get()) }
    }

    @Test fun allSettledIgnoresTextItemsAndNeedsEveryFile() {
        val a = EarlyOpen { Handle() }
        val b = EarlyOpen { Handle() }
        assertTrue(allSettled(listOf(null)))
        assertFalse(allSettled(listOf(a, null, b)))
        a.prefetch()
        assertFalse(allSettled(listOf(a, null, b)))
        b.prefetch()
        assertTrue(allSettled(listOf(a, null, b)))
    }

    @Test fun poolIsBoundedDaemonAndNamed() {
        val pool = newPrefetchPool()
        try {
            val started = CountDownLatch(PREFETCH_THREADS)
            val hold = CountDownLatch(1)
            val names = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()
            val daemon = AtomicInteger()
            repeat(PREFETCH_THREADS + 2) {
                pool.execute {
                    names.add(Thread.currentThread().name); if (Thread.currentThread().isDaemon) daemon.incrementAndGet()
                    started.countDown(); hold.await(5, TimeUnit.SECONDS)
                }
            }
            assertTrue(started.await(5, TimeUnit.SECONDS))
            Thread.sleep(50)
            assertEquals(PREFETCH_THREADS, names.size)
            assertTrue(names.all { it.startsWith("geekbridge-share-open-") })
            hold.countDown()
        } finally { pool.shutdownNow() }
    }
}
