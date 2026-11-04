// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.net.Uri

/**
 * The "send me the download link" text for someone who has the phone app but not the computer app yet.
 * Public information only: the releases page and three install lines. It never contains a pairing link, a
 * token, a device name or any address, so it is safe to put in any mail or chat.
 */
object ComputerInvite {
    const val RELEASES_URL = "https://github.com/fspecii/geekbridge/releases/latest"

    /** The address without the scheme: short enough to type into a computer's address bar. */
    const val SHORT_URL = "github.com/fspecii/geekbridge/releases/latest"

    fun subject(s: Strings): String = s.get(R.string.invite_subject)

    fun body(s: Strings): String = listOf(
        s.get(R.string.invite_intro),
        RELEASES_URL,
        "",
    ).plus(lines(s).mapIndexed { i, l -> "${i + 1}. $l" }).joinToString("\n")

    /** The three short install lines, without numbers. */
    fun lines(s: Strings): List<String> =
        listOf(s.get(R.string.invite_line1), s.get(R.string.invite_line2), s.get(R.string.invite_line3))

    /** `mailto:` with subject and body percent-encoded, no recipient: the mail app lets the user pick one. */
    fun mailto(s: Strings): Uri =
        Uri.parse("mailto:?subject=" + Uri.encode(subject(s)) + "&body=" + Uri.encode(body(s)))

    /** The same mailto as text (for unit tests, which cannot use android.net.Uri). */
    fun mailtoText(s: Strings): String =
        "mailto:?subject=" + java.net.URLEncoder.encode(subject(s), "UTF-8").replace("+", "%20") +
            "&body=" + java.net.URLEncoder.encode(body(s), "UTF-8").replace("+", "%20")
}
