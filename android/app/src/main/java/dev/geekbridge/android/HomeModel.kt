package dev.geekbridge.android

enum class HeroAction { Pair, None }

data class HeroState(val title: String, val subtitle: String, val tone: Tone, val action: HeroAction)

/** The one-word link state shown in dot-matrix on the Home tile, with how many of the link dots light up (0..1). */
enum class LinkWord(val level: Float) { Online(1f), Waiting(0.5f), Linking(0.25f), Offline(0f), Unpaired(0f) }

/** What the home screen says about the computer link. Pure, so every state is unit-tested. */
object HomeModel {
    /** Same inputs as [hero]; the dot-matrix word and link level that go with it. */
    fun word(
        pairedCount: Int,
        phase: LinkPhase,
        connectedNames: List<String>,
        keepConnected: Boolean,
        serviceRunning: Boolean,
    ): LinkWord = when {
        pairedCount <= 0 -> LinkWord.Unpaired
        connectedNames.isNotEmpty() -> LinkWord.Online
        phase == LinkPhase.Running -> LinkWord.Waiting
        phase == LinkPhase.Starting || phase == LinkPhase.Stopping -> LinkWord.Linking
        keepConnected && serviceRunning -> LinkWord.Linking
        else -> LinkWord.Offline
    }


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

/** What is moving right now, for the Home tile: how many transfers, how many outgoing and the combined progress. */
data class TransferGlance(val count: Int, val outgoing: Int, val percent: Int?)

object TransferGlanceModel {
    /** Null when nothing is in flight. [percent] is bucketed to 5% so the screen is not rebuilt on every byte. */
    fun of(transfers: List<Transfer>): TransferGlance? {
        val live = transfers.filter { !it.state.finished && (it.kind == TransferKind.File) }
        if (live.isEmpty()) return null
        val sized = live.filter { (it.size ?: 0L) > 0 }
        val total = sized.sumOf { it.size ?: 0L }
        val done = sized.sumOf { it.bytes.coerceAtMost(it.size ?: 0L) }
        val pct = if (total > 0) ((done * 100 / total).toInt() / 5) * 5 else null
        return TransferGlance(live.size, live.count { it.direction == TransferDirection.Outgoing }, pct)
    }
}
