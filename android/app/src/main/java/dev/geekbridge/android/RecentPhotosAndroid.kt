// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.ContentResolver
import android.content.ContentUris
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.provider.MediaStore
import android.util.Size
import java.io.ByteArrayOutputStream
import kotlin.math.max

/**
 * The phone's image library for [RecentPhotos]. Reads go through the media store with the permission the user gave
 * (all photos, or only the ones they picked on Android 14+); nothing is cached on disk and nothing is read until a
 * computer the user allowed asks. File names and pictures are never logged.
 */
class AndroidPhotoPort(private val ctx: Context, private val main: Handler) : PhotoPort {
    private val images: Uri = MediaStore.Images.Media.EXTERNAL_CONTENT_URI

    private fun uri(p: PhotoInfo): Uri = ContentUris.withAppendedId(images, p.mediaId)

    override fun access(): PhotoAccess = PhotoAccessRules.evaluate(Build.VERSION.SDK_INT) {
        ctx.checkSelfPermission(it) == android.content.pm.PackageManager.PERMISSION_GRANTED
    }

    override fun latest(max: Int): List<PhotoInfo> {
        if (max <= 0) return emptyList()
        val args = Bundle().apply {
            putString(ContentResolver.QUERY_ARG_SQL_SORT_ORDER, "${MediaStore.Images.Media.DATE_ADDED} DESC, ${MediaStore.Images.Media._ID} DESC")
            putInt(ContentResolver.QUERY_ARG_LIMIT, max)
        }
        val out = ArrayList<PhotoInfo>()
        try {
            ctx.contentResolver.query(images, arrayOf(
                    MediaStore.Images.Media._ID, MediaStore.Images.Media.DISPLAY_NAME, MediaStore.Images.Media.DATE_TAKEN,
                    MediaStore.Images.Media.DATE_ADDED, MediaStore.Images.Media.SIZE, MediaStore.Images.Media.WIDTH, MediaStore.Images.Media.HEIGHT,
                ), args, null)?.use { cur ->
                while (cur.moveToNext() && out.size < max) {
                    val taken = cur.getLong(2).takeIf { it > 0 } ?: (cur.getLong(3) * 1000)
                    out += PhotoInfo(
                        mediaId = cur.getLong(0),
                        name = cur.getString(1).orEmpty(),
                        takenMs = taken,
                        size = cur.getLong(4),
                        width = cur.getInt(5),
                        height = cur.getInt(6),
                    )
                }
            }
        } catch (_: SecurityException) {
            return emptyList() // permission withdrawn between the check and the query
        }
        return out
    }

    override fun thumbnail(p: PhotoInfo): PhotoThumb? {
        val bmp = try {
            if (Build.VERSION.SDK_INT >= 29) {
                ctx.contentResolver.loadThumbnail(uri(p), Size(PhotoRules.THUMB_EDGE, PhotoRules.THUMB_EDGE), null)
            } else {
                decodeSampled(uri(p))
            }
        } catch (_: Exception) {
            null
        } ?: return null
        try {
            var current = bmp
            // Halve the picture until it fits the byte cap at some quality (a noisy picture at 160 px rarely needs it).
            repeat(3) {
                val jpeg = PhotoRules.fit(PhotoRules.MAX_THUMB_BYTES) { q ->
                    ByteArrayOutputStream().also { current.compress(Bitmap.CompressFormat.JPEG, q, it) }.toByteArray()
                }
                if (jpeg != null) return PhotoThumb(current.width, current.height, jpeg)
                val next = Bitmap.createScaledBitmap(current, max(1, current.width / 2), max(1, current.height / 2), true)
                if (current !== bmp) current.recycle()
                current = next
            }
            if (current !== bmp) current.recycle()
            return null
        } finally {
            bmp.recycle()
        }
    }

    /** Before Android 10: read the bounds, then decode a power-of-two reduction. */
    private fun decodeSampled(u: Uri): Bitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        ctx.contentResolver.openInputStream(u)?.use { BitmapFactory.decodeStream(it, null, bounds) }
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
        var sample = 1
        while (max(bounds.outWidth, bounds.outHeight) / (sample * 2) >= PhotoRules.THUMB_EDGE) sample *= 2
        val opts = BitmapFactory.Options().apply { inSampleSize = sample }
        return ctx.contentResolver.openInputStream(u)?.use { BitmapFactory.decodeStream(it, null, opts) }
    }

    override fun exists(p: PhotoInfo): Boolean = try {
        ctx.contentResolver.query(uri(p), arrayOf(MediaStore.Images.Media._ID), null, null, null)?.use { it.moveToFirst() } == true
    } catch (_: Exception) {
        false
    }

    override fun sendFull(p: PhotoInfo, connectionId: Long): Boolean {
        // The share queue lives on the main thread. If another share is running it declines, and the computer's
        // request lapses after two minutes (docs/design/recent-photos.md).
        val u = uri(p)
        main.post { ShareCoordinator.get(ctx).sendFiles(connectionId, listOf(u)) }
        return true
    }
}
