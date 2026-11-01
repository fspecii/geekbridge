package dev.geekbridge.android

/** When the background connection service should hold sockets. Pure so the rule is unit-tested. */
object ServicePolicy {
    /** Never without a paired computer: an unpaired phone has nobody to stay connected to. */
    fun shouldRun(pairedCount: Int, keepConnected: Boolean): Boolean = pairedCount > 0 && keepConnected

    /** Network changes arrive in bursts; one restart per burst, and not on the very first callback. */
    fun shouldRestartOnNetwork(previousNetworkId: Long?, newNetworkId: Long): Boolean =
        previousNetworkId != null && previousNetworkId != newNetworkId

    const val RESTART_DEBOUNCE_MS = 3_000L
}
