// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/**
 * Bidirectional-text isolation for values a sentence embeds but does not control: a computer's self-chosen name, a
 * phone number, an app name. Without it a right-to-left name can reorder the words and digits around it (a number
 * beside an Arabic name reads backwards). Pure Unicode controls, so it works the same in JVM tests and on a phone;
 * the format characters are invisible and are not stripped by [CallRules.cleanComputer] because they are added after.
 */
object Bidi {
    private const val FSI = '⁨' // first-strong isolate: direction follows the first letter of the value
    private const val LRI = '⁦' // left-to-right isolate
    private const val PDI = '⁩' // pop directional isolate

    /** A name or free text: its own direction, no effect on the surrounding sentence. */
    fun isolate(text: String): String = "$FSI$text$PDI"

    /** A phone number or code: always left-to-right, even inside an Arabic or Hebrew sentence. */
    fun ltr(text: String): String = "$LRI$text$PDI"
}
