// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.nio.ByteBuffer
import java.util.Base64
import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class QrScanTest {
    private fun s(w: Int, h: Int) = QrFrames.Size(w, h)

    @Test fun theLargestSizeTheCoreAcceptsIsChosen() {
        val sizes = listOf(s(1920, 1080), s(1280, 720), s(960, 720), s(800, 600), s(640, 480), s(176, 144))
        assertEquals(s(960, 720), QrFrames.pick(sizes))
        // Wider but fewer pixels loses to the larger picture.
        assertEquals(s(960, 540), QrFrames.pick(listOf(s(1280, 720), s(960, 540), s(640, 360))))
        assertNull(QrFrames.pick(listOf(s(1920, 1080), s(3840, 2160))))
        assertNull(QrFrames.pick(listOf(s(160, 120))))
        assertNull(QrFrames.pick(emptyList()))
    }

    @Test fun paddedRowsAreCopiedWithoutThePadding() {
        // 4 x 3 picture, rows padded to 6 bytes with 0xEE.
        val rows = listOf(byteArrayOf(1, 2, 3, 4), byteArrayOf(5, 6, 7, 8), byteArrayOf(9, 10, 11, 12))
        val padded = rows.flatMap { it.toList() + listOf(0xEE.toByte(), 0xEE.toByte()) }.dropLast(2).toByteArray()
        val out = QrFrames.lumaOf(ByteBuffer.wrap(padded), 6, 4, 3)
        assertArrayEquals(byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12), out)
    }

    @Test(expected = IllegalArgumentException::class) fun aStrideSmallerThanTheWidthIsRefused() {
        QrFrames.lumaOf(ByteBuffer.allocate(100), 3, 4, 3)
    }

    @Test fun theDecodeRequestCarriesTheFrameAsBase64() {
        val luma = ByteArray(40 * 40) { (it % 251).toByte() }
        val o = JSONObject(QrFrames.decodeRequest(luma, 40, 40))
        assertEquals(1, o.getInt("version"))
        assertEquals("pairing_decode_qr", o.getString("action"))
        assertEquals(40, o.getInt("width"))
        assertEquals(40, o.getInt("height"))
        assertArrayEquals(luma, Base64.getDecoder().decode(o.getString("luma")))
        // A full-size frame stays inside what the client lets through to the core.
        val big = QrFrames.decodeRequest(ByteArray(QrFrames.MAX_PIXELS), 960, 720)
        assertTrue(big.length <= Limits.MAX_DATA_REQUEST_CHARS)
    }

    @Test fun onlyAPairingLinkEndsTheScan() {
        val link = "geekbridge://pair?v=1&id=x"
        assertEquals(QrFrames.Found.Link(link), QrFrames.classify(" $link "))
        assertEquals(QrFrames.Found.NotOurs, QrFrames.classify("https://example.com"))
        assertEquals(QrFrames.Found.NotOurs, QrFrames.classify("geekbridge://other?x=1"))
    }

    @Test fun theCoreAnswerIsReadDefensively() {
        assertEquals("abc", QrFrames.textOf(JSONObject().put("text", "abc")))
        assertNull(QrFrames.textOf(JSONObject()))
        assertNull(QrFrames.textOf(JSONObject().put("text", "")))
        assertNull(QrFrames.textOf("abc"))
        assertNull(QrFrames.textOf(null))
    }

    @Test fun cameraPermissionLeadsToTheRightScreen() {
        assertEquals(ScanState.Starting, ScanRules.forPermission(granted = true, askedBefore = true, canAsk = false))
        assertEquals(ScanState.NeedPermission, ScanRules.forPermission(false, askedBefore = false, canAsk = false))
        assertEquals(ScanState.NeedPermission, ScanRules.forPermission(false, askedBefore = true, canAsk = true))
        assertEquals(ScanState.PermissionBlocked, ScanRules.forPermission(false, askedBefore = true, canAsk = false))
    }

    @Test fun theScanScreenExplainsItselfAndTheTokenIsNeverInATarget() {
        for (id in listOf(R.string.scan_hint, R.string.scan_perm_body, R.string.scan_blocked_body, R.string.scan_nocamera, R.string.scan_not_ours)) {
            assertTrue(TestStrings.get(id).length > 20)
        }
        // The hint says what happens to the picture.
        assertTrue(TestStrings.get(R.string.scan_hint).contains("nothing is recorded or sent", ignoreCase = true))
        val t = QrTarget("a".repeat(32), "Desk", "cd".repeat(32), "00112233445566778899aabbccddeeff", listOf(QrAddress("10.0.0.2", 1716)))
        assertTrue(!t.toString().contains("00112233"))
    }
}
