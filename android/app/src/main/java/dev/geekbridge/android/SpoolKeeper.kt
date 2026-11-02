// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/** Remembers the one received-image file kept for the clipboard and deletes it as soon as nothing refers to it. */
class SpoolKeeper(private val delete: (String) -> Unit) {
    private var current: String? = null

    /** An item was put on the clipboard; [imagePath] is its spooled image, if it has one. */
    fun received(imagePath: String?) {
        current?.takeIf { it != imagePath }?.let(delete)
        current = imagePath
    }

    /** Nothing refers to the image any more (a text arrived, the setting went off, the link stopped). */
    fun clear() = received(null)
}
