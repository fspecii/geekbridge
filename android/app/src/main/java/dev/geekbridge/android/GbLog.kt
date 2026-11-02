// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.util.Log

/**
 * The app's own diagnostic lines, all under one stable logcat tag (`adb logcat -s GB`): capability decisions and
 * link closes, each with its reason. Debug builds only: in a release build every call is dead code (R8 removes
 * it) and nothing is written. Messages carry reasons and counts, never names, numbers, addresses or content.
 */
object GbLog {
    const val TAG = "GB"

    /** Debug builds' recorder of the last lines/exceptions for the state dump; null in release. */
    @JvmField @Volatile var sink: ((String) -> Unit)? = null

    fun i(area: String, message: String) {
        if (!BuildConfig.DEBUG) return
        val line = "$area: $message"
        platform { Log.i(TAG, line) } // android.util.Log is a stub that throws in plain JVM unit tests
        sink?.invoke(line)
    }

    /** A caught exception: the class only, never its message (it could echo input). */
    fun caught(area: String, t: Throwable) {
        if (!BuildConfig.DEBUG) return
        val line = "$area: caught ${t.javaClass.name}"
        platform { Log.w(TAG, line) }
        sink?.invoke("exception $line")
    }

    private inline fun platform(write: () -> Unit) {
        try {
            write()
        } catch (_: RuntimeException) {
        }
    }
}
