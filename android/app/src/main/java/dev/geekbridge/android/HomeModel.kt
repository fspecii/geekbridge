// SPDX-License-Identifier: GPL-3.0-or-later
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
        s: Strings,
        pairedNames: List<String>,
        pairedCount: Int,
        phase: LinkPhase,
        connectedNames: List<String>,
        keepConnected: Boolean,
        serviceRunning: Boolean,
    ): HeroState {
        if (pairedCount <= 0) {
            return HeroState(
                s.get(R.string.hm_none_title),
                s.get(R.string.hm_none_sub),
                Tone.Muted, HeroAction.Pair,
            )
        }
        val first = pairedNames.firstOrNull() ?: s.get(R.string.hm_your_computer)
        if (connectedNames.isNotEmpty()) {
            val extra = connectedNames.size - 1
            return HeroState(
                if (extra > 0) s.get(R.string.hm_connected_more, connectedNames.first(), extra) else s.get(R.string.hm_connected, connectedNames.first()),
                s.get(R.string.hm_ready), Tone.Ok, HeroAction.None,
            )
        }
        return when (phase) {
            LinkPhase.Running -> HeroState(
                s.get(R.string.hm_waiting, first),
                s.get(R.string.hm_waiting_sub), Tone.Normal, HeroAction.None,
            )
            LinkPhase.Starting, LinkPhase.Stopping -> HeroState(s.get(R.string.hm_connecting), s.get(R.string.hm_one_moment), Tone.Muted, HeroAction.None)
            LinkPhase.Stopped -> when {
                !keepConnected -> HeroState(
                    s.get(R.string.hm_not_connected),
                    s.get(R.string.hm_bg_off), Tone.Muted, HeroAction.None,
                )
                !serviceRunning -> HeroState(
                    s.get(R.string.hm_not_connected),
                    s.get(R.string.hm_bg_not_running), Tone.Warn, HeroAction.None,
                )
                else -> HeroState(s.get(R.string.hm_connecting), s.get(R.string.hm_one_moment), Tone.Muted, HeroAction.None)
            }
        }
    }

    /** Short, honest subtitle for the Send file / Send clipboard tiles. */
    fun tileSubtitle(s: Strings, pairedCount: Int, connectedNames: List<String>): String = when {
        pairedCount <= 0 -> s.get(R.string.hm_pair_first)
        connectedNames.isNotEmpty() -> s.get(R.string.hm_to, connectedNames.first())
        else -> s.get(R.string.hm_not_connected_sub)
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
