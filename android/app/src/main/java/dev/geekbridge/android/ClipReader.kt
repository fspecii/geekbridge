// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/**
 * Runs [work] on a worker and waits at most [timeoutMs]. On timeout the [CancelToken] is cancelled (so
 * callers can forward it to a CancellationSignal or close a descriptor) and null is returned; a thrown
 * exception also yields null. The calling thread is never blocked longer than the timeout, whatever the
 * provider on the other end does.
 */
object BoundedCall {
    private val pool: ExecutorService = Executors.newCachedThreadPool { r ->
        Thread(r, "geekbridge-provider").apply { isDaemon = true }
    }

    fun <T> run(timeoutMs: Long, executor: ExecutorService = pool, work: (CancelToken) -> T): T? {
        val token = CancelToken()
        val future = try {
            executor.submit<T> { work(token) }
        } catch (_: java.util.concurrent.RejectedExecutionException) {
            return null
        }
        return try {
            future.get(timeoutMs, TimeUnit.MILLISECONDS)
        } catch (_: TimeoutException) {
            token.cancel()
            future.cancel(true)
            null
        } catch (_: InterruptedException) {
            token.cancel()
            future.cancel(true)
            Thread.currentThread().interrupt()
            null
        } catch (_: java.util.concurrent.ExecutionException) {
            null
        }
    }
}

/** The plain fields of the first clipboard item, copied from ClipData without touching any provider. */
class RawClip(
    val text: CharSequence?,
    val html: String?,
    val uri: String?,
    val declaredMime: String?,
    val sensitive: Boolean,
    val timestamp: Long,
)

/** Asks the provider behind a clipboard URI what it holds (ContentResolver.getType); may block. */
fun interface MimeProbe {
    fun typeOf(uri: String, cancel: CancelToken): String?
}

/** Turns a [RawClip] into a [ClipSnapshot]; every provider access is bounded by [PROBE_TIMEOUT_MS]. */
object ClipReader {
    const val PROBE_TIMEOUT_MS = 2000L
    private const val BIG = ClipboardLimits.MAX_TEXT_BYTES + 1

    fun snapshot(raw: RawClip, probe: MimeProbe, nowMs: Long, timeoutMs: Long = PROBE_TIMEOUT_MS, executor: ExecutorService? = null): ClipSnapshot? {
        val uri = raw.uri
        val imageMime = if (uri != null) {
            // A slow or hung provider costs at most the timeout, then the type the clipboard declared is used.
            val probed = if (executor != null) BoundedCall.run(timeoutMs, executor) { probe.typeOf(uri, it) } else BoundedCall.run(timeoutMs) { probe.typeOf(uri, it) }
            ClipClassifier.imageMimeOf(probed ?: raw.declaredMime)
        } else {
            null
        }
        // A hostile app could put megabytes in the clipboard; keep only enough to know it is too big.
        val text = raw.text?.let { if (it.length > BIG) it.subSequence(0, BIG).toString() else it.toString() }
        val html = if (imageMime == null) raw.html?.let { if (it.length > BIG) it.substring(0, BIG) else it } else null
        val kind = ClipClassifier.classify(text, html, imageMime) ?: return null
        val copiedAt = raw.timestamp.takeIf { it > 0 } ?: nowMs
        return ClipSnapshot(kind, text, html, if (kind == ClipKind.Image) uri else null, imageMime, copiedAt, nowMs, raw.sensitive)
    }
}
