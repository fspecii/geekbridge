package dev.geekbridge.android

import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class WorkRunnerTest {
    private val runner = WorkRunner(postToMain = { it.run() })

    @After
    fun tearDown() = runner.shutdown()

    @Test
    fun deliversResult() {
        val latch = CountDownLatch(1)
        var got: Result<Int>? = null
        runner.submit("a", { 41 + 1 }) { got = it; latch.countDown() }
        assertTrue(latch.await(5, TimeUnit.SECONDS))
        assertEquals(42, got!!.getOrNull())
    }

    @Test
    fun deliversFailureAsResult() {
        val latch = CountDownLatch(1)
        var got: Result<Int>? = null
        runner.submit<Int>("a", { throw UnsatisfiedLinkError("x") }) { got = it; latch.countDown() }
        assertTrue(latch.await(5, TimeUnit.SECONDS))
        assertTrue(got!!.exceptionOrNull() is UnsatisfiedLinkError)
    }

    @Test
    fun newSubmissionToSameSlotCancelsAndSuppressesTheOldOne() {
        val delivered = CopyOnWriteArrayList<String>()
        val started = CountDownLatch(1)
        val second = CountDownLatch(1)
        runner.submit("slot", { t ->
            started.countDown()
            while (!t.isCancelled) Thread.sleep(2)
            "first"
        }) { delivered += it.getOrThrow() }
        assertTrue(started.await(5, TimeUnit.SECONDS))
        runner.submit("slot", { "second" }) { delivered += it.getOrThrow(); second.countDown() }
        assertTrue(second.await(5, TimeUnit.SECONDS))
        assertEquals(listOf("second"), delivered.toList())
    }

    @Test
    fun cancelAllSuppressesDeliveryAndSignalsListeners() {
        val started = CountDownLatch(1)
        val finished = CountDownLatch(1)
        val forwarded = CountDownLatch(1)
        var delivered = false
        runner.submit("slot", { t ->
            t.onCancel { forwarded.countDown() }
            started.countDown()
            while (!t.isCancelled) Thread.sleep(2)
            finished.countDown()
        }) { delivered = true }
        assertTrue(started.await(5, TimeUnit.SECONDS))
        runner.cancelAll()
        assertTrue(finished.await(5, TimeUnit.SECONDS))
        assertTrue(forwarded.await(5, TimeUnit.SECONDS))
        Thread.sleep(50)
        assertFalse(delivered)
    }

    @Test
    fun submitAfterShutdownIsDroppedQuietly() {
        runner.shutdown()
        var delivered = false
        runner.submit("a", { 1 }) { delivered = true }
        Thread.sleep(50)
        assertFalse(delivered)
    }

    @Test
    fun cancelTokenRunsLateListenerImmediately() {
        val t = CancelToken()
        t.cancel()
        var ran = false
        t.onCancel { ran = true }
        assertTrue(ran)
        assertTrue(t.isCancelled)
    }
}
