// SPDX-License-Identifier: GPL-3.0-or-later
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
        assertEquals("Idle", Format.callState(TestStrings, CallState.Idle))
        assertEquals("Ringing (incoming call)", Format.callState(TestStrings, CallState.Ringing))
        assertEquals("In call", Format.callState(TestStrings, CallState.OffHook))
        assertTrue(Format.callState(TestStrings, null).startsWith("waiting"))
    }

    @Test
    fun formatsTimestampInGivenZone() {
        val millis = 1_700_000_000_000L // 2023-11-14T22:13:20Z
        assertEquals("22:13:20", Format.callChangedAt(TestStrings, millis, ZoneId.of("UTC")))
        assertEquals("23:13:20", Format.callChangedAt(TestStrings, millis, ZoneId.of("Europe/Paris")))
        assertEquals("none yet", Format.callChangedAt(TestStrings, null, ZoneId.of("UTC")))
    }

    @Test
    fun stateTextNeverCarriesANumber() {
        for (s in CallState.entries) assertFalse(Format.callState(TestStrings, s).any { it.isDigit() })
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
        assertFalse((r as MonitorStart.Failed).reason(TestStrings).contains("no permission")) // native text is not echoed
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

class CallListenerLedgerTest {
    private val lines = ArrayList<String>()
    private var now = 0L
    private val ledger = CallListenerLedger(clock = { now }, log = { lines.add(it) })

    @Test
    fun startsAsNeverAttempted() {
        assertFalse(ledger.isRegistered())
        assertTrue(ledger.lines().first().contains("not registered (never_attempted)"))
    }

    @Test
    fun recordsRegistrationCallbacksAndReports() {
        now = 1_000
        ledger.registered(ListenerStrategy.TelephonyCallback)
        ledger.raw(1)
        ledger.delivered(CallState.Ringing)
        ledger.reported("ringing -> sent queued=1")
        val text = ledger.lines().joinToString("\n")
        assertTrue(ledger.isRegistered())
        for (n in listOf("registered via TelephonyCallback", "merged state changes since the app started: 1; last ringing", "last ringing at", "ringing -> sent queued=1"))
            assertTrue("missing '$n' in:\n$text", n in text)
        assertTrue(lines.any { it == "callback #1 merged=ringing" })
    }

    @Test
    fun notRegisteredKeepsTheReasonAndTheAttemptTime() {
        now = 5_000
        ledger.registered(ListenerStrategy.TelephonyCallback)
        ledger.notRegistered("link_down")
        assertFalse(ledger.isRegistered())
        assertEquals(5_000L, ledger.lastAttemptAt())
        assertTrue(ledger.lines().first().contains("not registered (link_down)"))
    }

    @Test
    fun logLinesNeverCarryAnythingButCodes() {
        ledger.raw(7)
        ledger.notRegistered("runtime_exception:IllegalStateException")
        assertTrue(lines.contains("callback #1 merged=unknown(7)"))
    }

    @Test
    fun failedStartHasAClassOnlyCode() {
        assertEquals("security_exception", MonitorStart.Failed(R.string.call_state_refused).code)
        assertEquals("runtime_exception:IllegalStateException", MonitorStart.Failed(R.string.call_state_unavailable, "IllegalStateException").code)
    }

    @Test
    fun retriesAreSpaced() {
        assertTrue(ListenerRetry.due(null, 0))
        assertFalse(ListenerRetry.due(1_000, 1_000 + ListenerRetry.SPACING_MS - 1))
        assertTrue(ListenerRetry.due(1_000, 1_000 + ListenerRetry.SPACING_MS))
    }

    @Test
    fun monitorReportsEveryRawStateIncludingUnknownOnes() {
        val seen = ArrayList<Int>()
        var raw: ((Int) -> Unit)? = null
        val m = CallStateMonitor(
            { _, cb -> raw = cb; AutoCloseable { } }, Executor { it.run() }, { it.run() }, {}, onRawSeen = { seen.add(it) },
        )
        m.start()
        raw!!(1); raw!!(9)
        assertEquals(listOf(1, 9), seen)
    }
}

class CallSourceMergerTest {
    private val m = CallSourceMerger()

    @Test fun ringingWinsOverOffHookWhichWinsOverIdle() {
        assertEquals(0, m.update("default", 0))
        assertEquals(1, m.update("sub2/slot1", 1))
        assertNull("offhook on another source does not hide ringing", m.update("broadcast", 2))
        assertNull(m.update("default", 0))
        assertEquals(2, m.update("sub2/slot1", 0)) // offhook still held by the broadcast source
        assertEquals(0, m.update("broadcast", 0))
    }

    @Test fun idleOnlyWhenEverySourceIsIdle() {
        m.update("a", 1); m.update("b", 0)
        assertNull(m.update("b", 0))
        assertEquals(0, m.update("a", 0))
    }

    @Test fun anotherSourceSeeingTheRingingCallIsEnoughWhenTheDefaultStaysIdle() {
        assertEquals(0, m.update("default", 0))
        assertEquals(1, m.update("sub5/slot1", 1)) // the eSIM that is not the default
        assertEquals(0, m.update("sub5/slot1", 0))
    }

    @Test fun aRemovedSourceNoLongerCounts() {
        m.update("a", 1); m.update("b", 0)
        assertEquals(0, m.remove("a"))
        assertNull(m.remove("a"))
        assertNull(m.merged().takeIf { it != 0 })
    }

    @Test fun unknownRawStatesAreIgnored() {
        assertNull(m.update("a", 9))
        assertNull(m.merged())
    }

    @Test fun mergingRegistrarForwardsOnlyMergedChangesAndReportsEverySource() {
        val got = ArrayList<Int>()
        val seen = ArrayList<String>()
        var emit: ((String, Int) -> Unit)? = null
        var gone: ((String) -> Unit)? = null
        var closed = 0
        val reg = MergingCallRegistrar(
            { _, onSource, onGone -> emit = onSource; gone = onGone; AutoCloseable { closed++ } },
            { n, r -> seen.add("$n=$r") },
        )
        val h = reg.register(Executor { it.run() }) { got.add(it) }
        emit!!("default", 0); emit!!("sub3/slot1", 0); emit!!("sub3/slot1", 1); emit!!("default", 0); emit!!("sub3/slot1", 0)
        gone!!("sub3/slot1")
        assertEquals(listOf(0, 1, 0), got)
        assertEquals(5, seen.size)
        h.close()
        assertEquals(1, closed)
    }

    @Test fun broadcastExtrasMapToRawStates() {
        assertEquals(1, SimText.fromBroadcast("RINGING"))
        assertEquals(2, SimText.fromBroadcast("OFFHOOK"))
        assertEquals(0, SimText.fromBroadcast("IDLE"))
        assertNull(SimText.fromBroadcast(null))
        assertNull(SimText.fromBroadcast("ringing"))
    }

    @Test fun broadcastsAreKeptApartPerSlotSoOneSimsIdleDoesNotHideTheOthersRinging() {
        assertEquals("broadcast", SimText.broadcastSource(null))
        assertEquals("broadcast/slot1", SimText.broadcastSource(1))
        assertEquals("broadcast", SimText.broadcastSource(-1))
        assertEquals("broadcast", SimText.broadcastSource(99))
        val m = CallSourceMerger()
        assertEquals(1, m.update("broadcast/slot0", 1))
        assertNull(m.update("broadcast/slot1", 0))
        assertEquals(0, m.remove("broadcast/slot0")) // that SIM went away mid-ring
    }

    @Test fun subscriptionSummaryHasSlotsEsimFlagAndDefaultVoiceButNoIdentifiers() {
        val sims = listOf(ActiveSim(3, 0, false), ActiveSim(7, 1, true))
        assertEquals(
            "active subscriptions: 2 [sub3 slot 0 physical, sub7 slot 1 eSIM]; default voice subscription: sub3 (slot 0)",
            SimText.summary(sims, 3),
        )
        assertTrue(SimText.summary(sims, 9).contains("sub9 (not among the active ones)"))
        assertTrue(SimText.summary(emptyList(), -1).endsWith("default voice subscription: none"))
        assertEquals("sub7/slot1", sims[1].sourceName)
    }

    @Test fun ledgerShowsSourcesAndSubscriptions() {
        val lines = ArrayList<String>()
        val l = CallListenerLedger(clock = { 0L }, log = { lines.add(it) })
        l.subscriptions("active subscriptions: 1 [sub7 slot 1 eSIM]; default voice subscription: none")
        l.source("sub7/slot1", 1)
        l.source("default", 0)
        l.sourceGone("default")
        val text = l.lines().joinToString("\n")
        assertTrue("sub7/slot1=ringing" in text)
        assertTrue("active subscriptions: 1 [sub7 slot 1 eSIM]" in text)
        assertTrue(lines.contains("source sub7/slot1 reported ringing"))
        assertFalse("default=" in text)
    }
}
