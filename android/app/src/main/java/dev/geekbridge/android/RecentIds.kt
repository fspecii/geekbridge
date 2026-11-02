// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/** A set of ids that forgets the oldest once it holds more than [max]; for "have I seen this" bookkeeping that must not grow for the life of the process. */
class RecentIds(private val max: Int) {
    private val ids = LinkedHashSet<Long>()

    /** True if [id] was not already present. */
    fun add(id: Long): Boolean {
        if (!ids.add(id)) return false
        while (ids.size > max) ids.remove(ids.first())
        return true
    }

    operator fun contains(id: Long): Boolean = id in ids
    operator fun plusAssign(id: Long) { add(id) }
    val size: Int get() = ids.size
}
