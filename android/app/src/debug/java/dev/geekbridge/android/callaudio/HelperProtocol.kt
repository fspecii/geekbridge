// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import java.io.DataInputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream

/*
 * DEBUG BUILDS ONLY, EXPERIMENT (call audio over Wi-Fi, docs/design/call-audio-over-wifi.md).
 * Framing between the shell-uid helper and its client. Byte-for-byte twin of
 * experiments/callaudio-wifi/src/helper.rs; both are tested against the same vectors.
 *
 *   frame  = type:u8 flags:u8 len:u16be payload[len]   (len <= MAX_PAYLOAD)
 *   HELLO 1, PCM 2 (seq:u32be cap:u32be s16le[320]; flags 0 = down, 1 = up), STATUS 3, INJECT 4 (as PCM), CMD 5
 */
object HelperProtocol {
    const val HELLO = 1
    const val PCM = 2
    const val STATUS = 3
    const val INJECT = 4
    const val CMD = 5
    const val MAX_PAYLOAD = 4096
    const val FRAME_SAMPLES = 320
    const val RATE = 16_000
    const val PCM_PAYLOAD = 8 + FRAME_SAMPLES * 2

    sealed class Frame {
        data class Text(val type: Int, val text: String) : Frame()
        class Pcm(val type: Int, val up: Boolean, val seq: Int, val cap: Int, val samples: ShortArray) : Frame()
    }

    fun text(type: Int, s: String): ByteArray = frame(type, 0, s.toByteArray(Charsets.UTF_8))

    fun pcm(type: Int, up: Boolean, seq: Int, cap: Int, samples: ShortArray): ByteArray {
        require(samples.size == FRAME_SAMPLES) { "frame of ${samples.size} samples" }
        val p = ByteArray(PCM_PAYLOAD)
        putIntBe(p, 0, seq)
        putIntBe(p, 4, cap)
        for (i in samples.indices) {
            val v = samples[i].toInt()
            p[8 + 2 * i] = v.toByte()
            p[9 + 2 * i] = (v shr 8).toByte()
        }
        return frame(type, if (up) 1 else 0, p)
    }

    private fun putIntBe(b: ByteArray, at: Int, v: Int) {
        b[at] = (v ushr 24).toByte()
        b[at + 1] = (v ushr 16).toByte()
        b[at + 2] = (v ushr 8).toByte()
        b[at + 3] = v.toByte()
    }

    private fun frame(type: Int, flags: Int, payload: ByteArray): ByteArray {
        require(payload.size <= MAX_PAYLOAD) { "helper frame too large" }
        val out = ByteArray(4 + payload.size)
        out[0] = type.toByte()
        out[1] = flags.toByte()
        out[2] = (payload.size ushr 8).toByte()
        out[3] = payload.size.toByte()
        System.arraycopy(payload, 0, out, 4, payload.size)
        return out
    }

    /** Strict, like the Rust side: unknown type, odd flags, wrong PCM size or bad UTF-8 is an IOException. */
    fun decode(type: Int, flags: Int, payload: ByteArray): Frame {
        if (type != PCM && flags != 0) throw IOException("helper flags")
        return when (type) {
            HELLO, STATUS, CMD -> {
                val dec = Charsets.UTF_8.newDecoder()
                val s = try {
                    dec.decode(java.nio.ByteBuffer.wrap(payload)).toString()
                } catch (e: java.nio.charset.CharacterCodingException) {
                    throw IOException("helper text is not UTF-8")
                }
                Frame.Text(type, s)
            }
            PCM, INJECT -> {
                if (payload.size != PCM_PAYLOAD) throw IOException("helper PCM frame of the wrong size")
                if (flags > 1) throw IOException("helper PCM flags")
                val seq = intBe(payload, 0)
                val cap = intBe(payload, 4)
                val s = ShortArray(FRAME_SAMPLES) { i ->
                    ((payload[8 + 2 * i].toInt() and 0xFF) or (payload[9 + 2 * i].toInt() shl 8)).toShort()
                }
                Frame.Pcm(type, flags == 1, seq, cap, s)
            }
            else -> throw IOException("unknown helper frame type")
        }
    }

    private fun intBe(b: ByteArray, at: Int): Int =
        ((b[at].toInt() and 0xFF) shl 24) or ((b[at + 1].toInt() and 0xFF) shl 16) or
            ((b[at + 2].toInt() and 0xFF) shl 8) or (b[at + 3].toInt() and 0xFF)

    fun read(input: InputStream): Frame {
        val d = DataInputStream(input)
        val type = d.readUnsignedByte()
        val flags = d.readUnsignedByte()
        val len = d.readUnsignedShort()
        if (len > MAX_PAYLOAD) throw IOException("helper frame too large")
        val p = ByteArray(len)
        d.readFully(p)
        return decode(type, flags, p)
    }

    fun write(out: OutputStream, bytes: ByteArray) {
        synchronized(out) {
            out.write(bytes)
            out.flush()
        }
    }
}
