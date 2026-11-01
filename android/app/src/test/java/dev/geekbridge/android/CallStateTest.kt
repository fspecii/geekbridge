package dev.geekbridge.android

import java.time.ZoneId
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CallStateMappingTest {
    @Test
    fun mapsTelephonyConstants() {
        assertEquals(CallState.Idle, CallStateMapping.fromRaw(0))
        assertEquals(CallState.Ringing, CallStateMapping.fromRaw(1))
        assertEquals(CallState.OffHook, CallStateMapping.fromRaw(2))
    }

    @Test
    fun unknownValuesAreIgnored() {
        for (raw in listOf(-1, 3, 99, Int.MAX_VALUE, Int.MIN_VALUE)) assertNull(CallStateMapping.fromRaw(raw))
    }

    @Test
    fun rawConstantsMatchPlatform() {
        // android.jar constants are inlined at compile time, so this runs on the JVM.
        assertEquals(android.telephony.TelephonyManager.CALL_STATE_IDLE, CallStateMapping.RAW_IDLE)
        assertEquals(android.telephony.TelephonyManager.CALL_STATE_RINGING, CallStateMapping.RAW_RINGING)
        assertEquals(android.telephony.TelephonyManager.CALL_STATE_OFFHOOK, CallStateMapping.RAW_OFFHOOK)
    }
}

class ListenerStrategyTest {
    @Test
    fun picksCallbackOnApi31AndUp() {
        for (sdk in listOf(31, 33, 34, 36, 40)) assertEquals(ListenerStrategy.TelephonyCallback, ListenerStrategy.forSdk(sdk))
    }

    @Test
    fun picksListenerWithExecutorOnApi29And30() {
        for (sdk in listOf(29, 30)) assertEquals(ListenerStrategy.ListenerWithExecutor, ListenerStrategy.forSdk(sdk))
    }

    @Test
    fun fallsBackToMainLooperListenerBelow29() {
        for (sdk in listOf(26, 27, 28)) assertEquals(ListenerStrategy.ListenerOnMainLooper, ListenerStrategy.forSdk(sdk))
    }
}

class CallStateTrackerTest {
    @Test
    fun recordsChangesAndIgnoresRepeats() {
        val t = CallStateTracker()
        assertNull(t.state)
        assertNull(t.changedAtMillis)
        assertTrue(t.update(CallState.Idle, 100))
        assertFalse(t.update(CallState.Idle, 200))
        assertEquals(100L, t.changedAtMillis)
        assertTrue(t.update(CallState.Ringing, 300))
        assertEquals(CallState.Ringing, t.state)
        assertEquals(300L, t.changedAtMillis)
    }

    @Test
    fun resetForgetsEverything() {
        val t = CallStateTracker()
        t.update(CallState.OffHook, 5)
        t.reset()
        assertNull(t.state)
        assertNull(t.changedAtMillis)
    }
}

class CallStateFormatTest {
    @Test
    fun namesStates() {
        assertEquals("Idle", Format.callState(CallState.Idle))
        assertEquals("Ringing (incoming call)", Format.callState(CallState.Ringing))
        assertEquals("In call", Format.callState(CallState.OffHook))
        assertTrue(Format.callState(null).startsWith("waiting"))
    }

    @Test
    fun formatsTimestampInGivenZone() {
        val millis = 1_700_000_000_000L // 2023-11-14T22:13:20Z
        assertEquals("22:13:20", Format.callChangedAt(millis, ZoneId.of("UTC")))
        assertEquals("23:13:20", Format.callChangedAt(millis, ZoneId.of("Europe/Paris")))
        assertEquals("none yet", Format.callChangedAt(null, ZoneId.of("UTC")))
    }

    @Test
    fun stateTextNeverCarriesANumber() {
        for (s in CallState.entries) assertFalse(Format.callState(s).any { it.isDigit() })
    }
}

class CallStateMonitorTest {
    private class FakeRegistrar : CallStateRegistrar {
        var registered = 0
        var closed = 0
        var onRaw: ((Int) -> Unit)? = null
        var failWith: RuntimeException? = null

        override fun register(executor: Executor, onRaw: (Int) -> Unit): AutoCloseable {
            failWith?.let { throw it }
            registered++
            this.onRaw = onRaw
            return AutoCloseable { closed++; this.onRaw = null }
        }
    }

    private val queue = ArrayDeque<Runnable>()
    private val seen = ArrayList<CallState>()
    private val registrar = FakeRegistrar()
    private val monitor = CallStateMonitor(registrar, Executor { it.run() }, { queue.add(it) }, { seen.add(it) })

    private fun drain() {
        while (queue.isNotEmpty()) queue.removeFirst().run()
    }

    @Test
    fun deliversMappedStatesOnTheMainQueue() {
        assertEquals(MonitorStart.Started, monitor.start())
        registrar.onRaw!!(1)
        assertTrue("must not be delivered before the main queue runs", seen.isEmpty())
        registrar.onRaw!!(2)
        registrar.onRaw!!(0)
        drain()
        assertEquals(listOf(CallState.Ringing, CallState.OffHook, CallState.Idle), seen)
    }

    @Test
    fun ignoresUnknownRawStates() {
        monitor.start()
        registrar.onRaw!!(42)
        drain()
        assertTrue(seen.isEmpty())
    }

    @Test
    fun startIsIdempotent() {
        assertEquals(MonitorStart.Started, monitor.start())
        assertEquals(MonitorStart.AlreadyRunning, monitor.start())
        assertEquals(1, registrar.registered)
    }

    @Test
    fun stopUnregistersExactlyOnceAndDropsQueuedReports() {
        monitor.start()
        registrar.onRaw!!(1)
        monitor.stop()
        monitor.stop()
        drain()
        assertEquals(1, registrar.closed)
        assertFalse(monitor.isRunning)
        assertTrue("report queued before stop must not be delivered", seen.isEmpty())
    }

    @Test
    fun restartRegistersAgainAndIgnoresReportsFromTheOldRegistration() {
        monitor.start()
        val old = registrar.onRaw!!
        monitor.stop()
        monitor.start()
        old(1)
        registrar.onRaw!!(2)
        drain()
        assertEquals(2, registrar.registered)
        assertEquals(listOf(CallState.OffHook), seen)
    }

    @Test
    fun registrationFailureIsReportedAndLeavesNothingRunning() {
        registrar.failWith = SecurityException("no permission")
        val r = monitor.start()
        assertTrue(r is MonitorStart.Failed)
        assertFalse(monitor.isRunning)
        assertFalse((r as MonitorStart.Failed).reason.contains("no permission")) // native text is not echoed
        registrar.failWith = null
        assertEquals(MonitorStart.Started, monitor.start())
    }

    @Test
    fun runtimeFailureIsReported() {
        registrar.failWith = IllegalStateException("boom")
        assertTrue(monitor.start() is MonitorStart.Failed)
    }

    @Test
    fun failingUnregisterDoesNotThrow() {
        val bad = CallStateMonitor(
            { _, _ -> AutoCloseable { throw IllegalStateException("gone") } },
            Executor { it.run() }, { it.run() }, {},
        )
        bad.start()
        bad.stop()
        assertFalse(bad.isRunning)
    }
}

class CallStateExecutorTest {
    @Test
    fun queueIsBoundedAndKeepsTheNewestReport() {
        val ex = CallStateExecutor.create()
        try {
            val gate = CountDownLatch(1)
            val started = CountDownLatch(1)
            val ran = java.util.Collections.synchronizedList(ArrayList<Int>())
            ex.execute { started.countDown(); gate.await(5, TimeUnit.SECONDS) }
            assertTrue(started.await(5, TimeUnit.SECONDS))
            val total = CallStateExecutor.QUEUE_CAPACITY * 4
            for (i in 1..total) ex.execute { ran.add(i) }
            val pool = ex as java.util.concurrent.ThreadPoolExecutor
            assertTrue(pool.queue.size <= CallStateExecutor.QUEUE_CAPACITY)
            assertEquals(1, pool.maximumPoolSize)
            gate.countDown()
            ex.shutdown()
            assertTrue(ex.awaitTermination(5, TimeUnit.SECONDS))
            assertEquals(total, ran.last())
            assertTrue(ran.size <= CallStateExecutor.QUEUE_CAPACITY)
        } finally {
            ex.shutdownNow()
        }
    }
}
