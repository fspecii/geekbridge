package dev.geekbridge.android

enum class HeroAction { Pair, None }

data class HeroState(val title: String, val subtitle: String, val tone: Tone, val action: HeroAction)

/** What the home screen says about the computer link. Pure, so every state is unit-tested. */
object HomeModel {
    fun hero(
        pairedNames: List<String>,
        pairedCount: Int,
        phase: LinkPhase,
        connectedNames: List<String>,
        keepConnected: Boolean,
        serviceRunning: Boolean,
    ): HeroState {
        if (pairedCount <= 0) {
            return HeroState(
                "No computer paired",
                "Pair this phone with your computer to send files and the clipboard between them.",
                Tone.Muted, HeroAction.Pair,
            )
        }
        val first = pairedNames.firstOrNull() ?: "your computer"
        if (connectedNames.isNotEmpty()) {
            val extra = connectedNames.size - 1
            return HeroState(
                "Connected to ${connectedNames.first()}" + if (extra > 0) " +$extra" else "",
                "Ready to send and receive.", Tone.Ok, HeroAction.None,
            )
        }
        return when (phase) {
            LinkPhase.Running -> HeroState(
                "Waiting for $first",
                "Open GeekBridge on your computer, on the same Wi-Fi network.", Tone.Normal, HeroAction.None,
            )
            LinkPhase.Starting, LinkPhase.Stopping -> HeroState("Connecting…", "One moment.", Tone.Muted, HeroAction.None)
            LinkPhase.Stopped -> when {
                !keepConnected -> HeroState(
                    "Not connected",
                    "Staying connected in the background is off. The phone connects while this app is open.", Tone.Muted, HeroAction.None,
                )
                !serviceRunning -> HeroState(
                    "Not connected",
                    "The background connection is not running. Check battery settings, then reopen the app.", Tone.Warn, HeroAction.None,
                )
                else -> HeroState("Connecting…", "One moment.", Tone.Muted, HeroAction.None)
            }
        }
    }

    /** Short, honest subtitle for the Send file / Send clipboard tiles. */
    fun tileSubtitle(pairedCount: Int, connectedNames: List<String>): String = when {
        pairedCount <= 0 -> "Pair a computer first"
        connectedNames.isNotEmpty() -> "To ${connectedNames.first()}"
        else -> "Computer not connected"
    }
}
