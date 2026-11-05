// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

import java.io.IOException
import java.io.InputStream
import java.io.OutputStream

/**
 * An in-memory byte pipe for tests. java.io.PipedInputStream remembers the last thread that wrote and throws
 * "Pipe broken" at the reader once that thread has died and the reader has waited about two seconds, which on a loaded
 * runner killed the connection reader after the helper's capture thread (the last writer) ended. This pipe has no
 * thread tracking: it ends only when a side is closed.
 */
class TestPipe(private val capacity: Int = 1 shl 20) {
    private val lock = Object()
    private val buf = java.io.ByteArrayOutputStream()
    private var head = ByteArray(0)
    private var pos = 0
    private var writerClosed = false
    private var readerClosed = false

    private fun pending() = head.size - pos + buf.size()

    val input = object : InputStream() {
        override fun read(): Int {
            val one = ByteArray(1)
            return if (read(one, 0, 1) < 0) -1 else one[0].toInt() and 0xff
        }

        override fun read(b: ByteArray, off: Int, len: Int): Int {
            if (len == 0) return 0
            synchronized(lock) {
                while (pending() == 0) {
                    if (writerClosed) return -1
                    if (readerClosed) throw IOException("closed")
                    lock.wait()
                }
                if (pos == head.size) {
                    head = buf.toByteArray()
                    buf.reset()
                    pos = 0
                }
                val n = minOf(len, head.size - pos)
                System.arraycopy(head, pos, b, off, n)
                pos += n
                lock.notifyAll()
                return n
            }
        }

        override fun close() {
            synchronized(lock) {
                readerClosed = true
                lock.notifyAll()
            }
        }
    }

    val output = object : OutputStream() {
        override fun write(b: Int) = write(byteArrayOf(b.toByte()), 0, 1)

        override fun write(b: ByteArray, off: Int, len: Int) {
            synchronized(lock) {
                var o = off
                var left = len
                while (left > 0) {
                    if (writerClosed || readerClosed) throw IOException("closed")
                    val room = capacity - pending()
                    if (room <= 0) {
                        lock.wait()
                        continue
                    }
                    val n = minOf(room, left)
                    buf.write(b, o, n)
                    o += n
                    left -= n
                    lock.notifyAll()
                }
            }
        }

        override fun close() {
            synchronized(lock) {
                writerClosed = true
                lock.notifyAll()
            }
        }
    }
}
