// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import java.io.File
import java.io.FileNotFoundException

/**
 * Serves the one image a computer sent to the Android clipboard, from the native core's private spool
 * directory. Not exported; an app that pastes the image gets a temporary read grant from the clip itself.
 * Only files named like the core's spool files are served, read-only.
 */
class ClipImageProvider : ContentProvider() {
    override fun onCreate() = true

    private fun fileFor(uri: Uri): File? {
        val ctx = context ?: return null
        val name = uri.lastPathSegment ?: return null
        if (!NAME.matches(name)) return null
        return File(File(ctx.filesDir, SPOOL_DIR), name).takeIf { it.isFile }
    }

    override fun getType(uri: Uri): String? {
        val f = fileFor(uri) ?: return null
        val head = ByteArray(8)
        val n = try {
            f.inputStream().use { it.read(head) }
        } catch (_: java.io.IOException) {
            return null
        }
        return when {
            n >= 8 && head[0] == 0x89.toByte() && head[1] == 'P'.code.toByte() -> "image/png"
            n >= 3 && head[0] == 0xFF.toByte() && head[1] == 0xD8.toByte() -> "image/jpeg"
            else -> null
        }
    }

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        val f = fileFor(uri)
        if (mode != "r" || f == null) throw FileNotFoundException("not available")
        return ParcelFileDescriptor.open(f, ParcelFileDescriptor.MODE_READ_ONLY)
    }

    override fun query(uri: Uri, projection: Array<String>?, selection: String?, selectionArgs: Array<String>?, sortOrder: String?): Cursor? {
        val f = fileFor(uri) ?: return null
        val columns = (projection ?: arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE))
            .filter { it == OpenableColumns.DISPLAY_NAME || it == OpenableColumns.SIZE }
            .toTypedArray()
        val row = columns.map { if (it == OpenableColumns.SIZE) f.length() else "clipboard-image" }
        return MatrixCursor(columns, 1).apply { addRow(row) }
    }

    override fun insert(uri: Uri, values: ContentValues?): Uri? = throw UnsupportedOperationException()
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<String>?): Int =
        throw UnsupportedOperationException()
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<String>?): Int = throw UnsupportedOperationException()

    companion object {
        /** Matches `<state_dir>/clipboard-cache` in the native core. */
        const val SPOOL_DIR = "clipboard-cache"
        private val NAME = Regex("in-[0-9]{1,12}\\.img")

        fun authority(ctx: Context) = ctx.packageName + ".clipimg"

        /** A content URI for a spool file path reported by the core; null if it is not one of its files. */
        fun uriFor(ctx: Context, path: String): Uri? {
            val f = File(path)
            val dir = File(ctx.filesDir, SPOOL_DIR)
            if (f.parentFile?.canonicalPath != dir.canonicalPath || !NAME.matches(f.name)) return null
            return Uri.Builder().scheme("content").authority(authority(ctx)).appendPath(f.name).build()
        }
    }
}
