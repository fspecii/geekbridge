package dev.geekbridge.android

/** When the background connection service should hold sockets. Pure so the rule is unit-tested. */
object ServicePolicy {
    /** Never without a paired computer: an unpaired phone has nobody to stay connected to. */
    fun shouldRun(pairedCount: Int, keepConnected: Boolean): Boolean = pairedCount > 0 && keepConnected

    /** Network changes arrive in bursts; one restart per burst, and not on the very first callback. */
    fun shouldRestartOnNetwork(previousNetworkId: Long?, newNetworkId: Long): Boolean =
        previousNetworkId != null && previousNetworkId != newNetworkId

    const val RESTART_DEBOUNCE_MS = 3_000L

    /**
     * Runs [promote] (startForeground). On Android 12+ a restart from the background may be refused with an
     * exception; the service then has no right to keep running, so [stop] is called instead of crashing.
     * Returns true if it is now a foreground service.
     */
    fun promoteOrStop(promote: () -> Unit, stop: () -> Unit): Boolean = try {
        promote()
        true
    } catch (_: RuntimeException) {
        stop()
        false
    }
}
