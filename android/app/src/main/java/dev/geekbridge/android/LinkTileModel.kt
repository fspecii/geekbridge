// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/** What the Quick Settings "Computer link" tile shows; pure so it is unit-tested without Android. */
data class LinkTileView(val active: Boolean, val word: ConnWord, val computer: String?)

object LinkTileModel {
    /** Lit only while a computer is really connected (a ringing phone is connected too). */
    fun view(s: ConnectionState): LinkTileView =
        LinkTileView(s.word == ConnWord.Connected || s.word == ConnWord.Ringing, s.word, s.computer?.takeIf { it.isNotBlank() })

    /** "Connected · Desk"; just the word when there is no computer name to add. */
    fun subtitle(v: LinkTileView, wordText: String, join: (String, String) -> String): String =
        v.computer?.let { join(wordText, it) } ?: wordText

    /** A tap links a computer when none is paired, and otherwise opens the app to see the state. */
    fun tapPairs(v: LinkTileView): Boolean = v.word == ConnWord.Unpaired
}
