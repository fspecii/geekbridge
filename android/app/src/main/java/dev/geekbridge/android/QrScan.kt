// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.nio.ByteBuffer
import java.util.Base64
import org.json.JSONObject

/**
 * Pure helpers of the in-app QR scanner: which camera size to use, how a camera frame becomes the brightness bytes
 * the Rust core reads, and what the core's answer means. No Android classes, so all of it is unit-tested on the JVM.
 *
 * The picture is decoded by the native core (`pairing_decode_qr`, the same `rqrr` the computer's tests use), so the
 * APK carries no scanner library and no Play services. The frame is never stored or sent anywhere.
 */
object QrFrames {
    /** The core refuses frames over this many pixels (docs/android-contract.md). */
    const val MAX_PIXELS = 960 * 720
    private const val MIN_WIDTH = 320

    data class Size(val width: Int, val height: Int) {
        val pixels: Int get() = width * height
    }

    /** The largest size the core accepts. Preview and analysis use the same size, so no resampling is ever needed. */
    fun pick(sizes: Collection<Size>): Size? =
        sizes.filter { it.pixels <= MAX_PIXELS && maxOf(it.width, it.height) >= MIN_WIDTH }
            .maxWithOrNull(compareBy<Size>({ it.pixels }, { it.width }))

    /** The first plane of a YUV_420_888 image is the brightness; rows may be padded to [rowStride]. */
    fun lumaOf(plane: ByteBuffer, rowStride: Int, width: Int, height: Int): ByteArray {
        require(rowStride >= width) { "row stride smaller than the width" }
        val out = ByteArray(width * height)
        val src = plane.duplicate()
        for (y in 0 until height) {
            src.position(y * rowStride)
            src.get(out, y * width, width)
        }
        return out
    }

    fun decodeRequest(luma: ByteArray, width: Int, height: Int): String = JSONObject()
        .put("version", CoreProtocol.VERSION)
        .put("action", "pairing_decode_qr")
        .put("luma", Base64.getEncoder().encodeToString(luma))
        .put("width", width)
        .put("height", height)
        .toString()

    /** The text the core found in the picture, or null (no code, or not a plain text result). */
    fun textOf(result: Any?): String? = (result as? JSONObject)?.optString("text")?.takeIf { it.isNotEmpty() }

    /** What the scanner does with decoded text: only a GeekBridge pairing link ends the scan; anything else is a hint. */
    sealed interface Found {
        data class Link(val text: String) : Found
        data object NotOurs : Found
    }

    fun classify(text: String): Found = QrProtocol.linkText(text)?.let { Found.Link(it) } ?: Found.NotOurs
}

/** What the scanner screen is doing, decided from the permission and the camera. */
enum class ScanState { Starting, Scanning, NeedPermission, PermissionBlocked, NoCamera, CoreMissing }

object ScanRules {
    /** Which state a permission answer leads to. [canAsk] is false once the system will not show its prompt again. */
    fun forPermission(granted: Boolean, askedBefore: Boolean, canAsk: Boolean): ScanState = when {
        granted -> ScanState.Starting
        !askedBefore -> ScanState.NeedPermission
        canAsk -> ScanState.NeedPermission
        else -> ScanState.PermissionBlocked
    }
}
