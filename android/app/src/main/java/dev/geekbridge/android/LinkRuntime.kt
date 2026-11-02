// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.os.Handler
import android.os.Looper

/** The process-wide transport controller, created without capturing any Activity. */
object LinkRuntime {
    private val core: CoreClient by lazy { CoreClient.native() }
    private val main = Handler(Looper.getMainLooper())

    private var observing = false
    private val waiter = WaiterSlot()

    /** Also remembers the paired computers (count and names) whenever a readable status arrives. */
    @Synchronized
    fun controller(): TransportController {
        val c = TransportController.get({ core.transportAction(it) }, { main.post(it) })
        if (!observing) {
            observing = true
            c.observers += {
                ClipboardRuntime.onLinkChanged(c.phase == LinkPhase.Running)
                CallControlRuntime.onLinkChanged(c.phase == LinkPhase.Running)
                PhoneDataRuntime.onLinkChanged(c.phase == LinkPhase.Running)
                if (c.phase == LinkPhase.Running) startWaiter(c)
                val s = c.snapshot
                if (s != null && s.prompts.isNotEmpty()) AppState.pairingPromptSeen()
                if (s != null && s.isRunning) AppState.recordPaired(s.paired, readable = !s.pairedError)
                WidgetUpdater.onLink(s?.connections.orEmpty().filter { it.trusted }.map { it.peer.name.ifEmpty { "Computer" } })
            }
        }
        return c
    }

    /**
     * Nothing polls the status while the app is closed, so the core wakes this thread when something happens: a
     * computer that unpaired this phone reaches [AppState.recordPaired], which stops the background service.
     */
    private fun startWaiter(c: TransportController) {
        if (!waiter.claim()) return
        Thread({
            try {
                waitLoop(c)
            } finally {
                waiter.leaveUnless { false }
            }
        }, "geekbridge-link-wait").apply { isDaemon = true; start() }
    }

    private fun waitLoop(c: TransportController) {
        var since = 0L
        while (true) {
            val o = core.transportAction(TransportProtocol.waitRequest(since))
            // Not running any more; the next link start claims the slot again.
            val seq = (o as? CoreOutcome.Ok)?.let { TransportProtocol.parseWait(it.result) } ?: return
            if (seq != since) {
                since = seq
                main.post { c.eventsUpTo(seq) }
            }
        }
    }

    fun startOptions(stateDir: String, deviceName: String, discoveryListen: Boolean, announce: Boolean) = StartOptions(
        stateDir = stateDir,
        deviceName = deviceName,
        listen = true,
        listenPort = null,
        discoveryListen = discoveryListen,
        discoveryAnnounce = announce,
        allowLoopback = false,
        clipboard = ClipboardRuntime.settings,
        calls = CallControlRuntime.settings,
        data = PhoneDataRuntime.startFeatures(),
    )
}
