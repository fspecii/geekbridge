// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

/**
 * What a paired computer may make this phone text, counted per computer (its device id from the native core, never from
 * the packet). Texts can cost money and a program can send many; so a computer gets a small allowance per minute and per
 * hour, and may write to only a few different numbers per [PhoneDataLimits.SMS_RECIPIENT_WINDOW_MS]. A normal chat is one
 * number, many texts; a spray over numbers is what the recipient limit stops, even at a low rate.
 *
 * Lives as long as the app process, NOT per link: a computer that drops and reopens its connection gets no new allowance.
 * A refused request is not counted, so waiting really does restore the allowance. Memory is bounded ([MAX_COMPUTERS],
 * [MAX_RECIPIENTS_KEPT]); only digits of numbers are kept, never message text.
 */
class SmsGuard {
    enum class Verdict { Ok, Rate, Recipients }

    private class Book {
        val minute = ArrayDeque<Long>()
        val hour = ArrayDeque<Long>()
        val recipients = LinkedHashMap<String, Long>()
        var lastNotice = Long.MIN_VALUE / 2
        var lastUse = 0L
    }

    private val books = LinkedHashMap<String, Book>()

    @Synchronized
    fun admit(device: String, address: String, now: Long): Verdict {
        val b = book(device, now)
        while (b.minute.isNotEmpty() && now - b.minute.first() > 60_000) b.minute.removeFirst()
        while (b.hour.isNotEmpty() && now - b.hour.first() > 3_600_000) b.hour.removeFirst()
        val it = b.recipients.entries.iterator()
        while (it.hasNext()) if (now - it.next().value > PhoneDataLimits.SMS_RECIPIENT_WINDOW_MS) it.remove()
        if (b.minute.size >= PhoneDataLimits.SMS_SENDS_PER_MINUTE || b.hour.size >= PhoneDataLimits.SMS_SENDS_PER_HOUR) return Verdict.Rate
        val key = recipientKey(address)
        if (key !in b.recipients && b.recipients.size >= PhoneDataLimits.SMS_RECIPIENTS_PER_WINDOW) return Verdict.Recipients
        b.minute.addLast(now)
        b.hour.addLast(now)
        b.recipients.remove(key) // re-insert last so the oldest is the first to age out
        b.recipients[key] = now
        while (b.recipients.size > MAX_RECIPIENTS_KEPT) b.recipients.remove(b.recipients.keys.first())
        return Verdict.Ok
    }

    /** True at most once per [NOTICE_EVERY_MS] per computer: the owner is told, not spammed. */
    @Synchronized
    fun noticeDue(device: String, now: Long): Boolean {
        val b = book(device, now)
        if (now - b.lastNotice < NOTICE_EVERY_MS) return false
        b.lastNotice = now
        return true
    }

    @Synchronized
    fun trackedComputers(): Int = books.size

    private fun book(device: String, now: Long): Book {
        val id = device.ifEmpty { "?" }
        books[id]?.let { it.lastUse = now; return it }
        if (books.size >= MAX_COMPUTERS) {
            // Forget the computer idle the longest (more than any phone pairs with).
            books.entries.minByOrNull { it.value.lastUse }?.let { books.remove(it.key) }
        }
        return Book().also { it.lastUse = now; books[id] = it }
    }

    companion object {
        const val MAX_COMPUTERS = 16
        const val MAX_RECIPIENTS_KEPT = 32
        const val NOTICE_EVERY_MS = 10 * 60_000L

        /** The last nine digits: "+49 151 1234 5678" and "0151 12345678" are the same person. */
        fun recipientKey(address: String): String {
            val digits = address.filter { it in '0'..'9' }
            return if (digits.length > 9) digits.takeLast(9) else digits.ifEmpty { address.take(32) }
        }
    }
}
