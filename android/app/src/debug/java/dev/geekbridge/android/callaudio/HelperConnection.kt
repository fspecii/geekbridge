// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import java.io.IOException
import java.io.InputStream
import java.io.OutputStream

/*
 * DEBUG BUILDS ONLY, EXPERIMENT. The app's end of the connection to the shell-uid helper (the app is the client).
 * No Android types, so a JVM test drives it with the real HelperSession over pipes. Audio passes through to the
 * listener; nothing is stored or logged.
 */
class HelperConnection(
    input: InputStream,
    private val output: OutputStream,
    private val token: String,
    /** The HELLO frame when the caller already read it (to check the token); null = it is still in the stream. */
    helloRead: String? = null,
) {
    interface Listener {
        fun onPcm(samples: ShortArray, cap: Int)
        fun onStatus(json: String)
        fun onClosed()
    }

    private val reader = Thread({ pump(input) }, "gb-callwifi-helper")

    @Volatile var listener: Listener? = null
    @Volatile var hello: String? = helloRead
        private set
    @Volatile var closed = false
        private set

    fun start() {
        reader.isDaemon = true
        reader.start()
    }

    private fun pump(input: InputStream) {
        try {
            while (true) {
                when (val f = HelperProtocol.read(input)) {
                    is HelperProtocol.Frame.Text -> when (f.type) {
                        HelperProtocol.HELLO -> hello = f.text
                        HelperProtocol.STATUS -> listener?.onStatus(f.text)
                    }
                    is HelperProtocol.Frame.Pcm -> if (f.type == HelperProtocol.PCM && !f.up) listener?.onPcm(f.samples, f.cap)
                }
            }
        } catch (_: IOException) {
        } finally {
            closed = true
            listener?.onClosed()
        }
    }

    /** One fixed command. Keys and values are `[A-Za-z0-9_]` only (the helper's reader accepts nothing else). */
    fun command(cmd: String, vararg args: Pair<String, String>) {
        val sb = StringBuilder("{\"cmd\":\"").append(safe(cmd)).append("\",\"token\":\"").append(safe(token)).append('"')
        for ((k, v) in args) sb.append(",\"").append(safe(k)).append("\":\"").append(safe(v)).append('"')
        sb.append('}')
        try {
            HelperProtocol.write(output, HelperProtocol.text(HelperProtocol.CMD, sb.toString()))
        } catch (_: IOException) {
            closed = true
        }
    }

    /** The computer's voice for the helper's injection route (speak back). */
    fun inject(seq: Int, samples: ShortArray) {
        try {
            HelperProtocol.write(output, HelperProtocol.pcm(HelperProtocol.INJECT, true, seq, 0, samples))
        } catch (_: IOException) {
            closed = true
        }
    }

    fun close() {
        closed = true
        try {
            output.close()
        } catch (_: IOException) {
        }
    }

    private fun safe(s: String) = s.filter { it.isLetterOrDigit() || it == '_' }.take(64)
}
