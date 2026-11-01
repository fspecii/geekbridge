package dev.geekbridge.android

import android.os.Handler
import android.os.Looper

/** The process-wide transport controller, created without capturing any Activity. */
object LinkRuntime {
    private val core: CoreClient by lazy { CoreClient.native() }
    private val main = Handler(Looper.getMainLooper())

    private var observing = false

    /** Also remembers the paired computers (count and names) whenever a readable status arrives. */
    @Synchronized
    fun controller(): TransportController {
        val c = TransportController.get({ core.transportAction(it) }, { main.post(it) })
        if (!observing) {
            observing = true
            c.observers += {
                ClipboardRuntime.onLinkChanged(c.phase == LinkPhase.Running)
                val s = c.snapshot
                if (s != null && s.isRunning) AppState.recordPaired(s.paired, readable = !s.pairedError)
            }
        }
        return c
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
    )
}
