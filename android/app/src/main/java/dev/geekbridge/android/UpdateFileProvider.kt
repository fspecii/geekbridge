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
 * Hands the verified update APK to the system package installer, and nothing else. A minimal stand-in for
 * AndroidX `FileProvider` (the app has no AndroidX): not exported, read-only, one fixed path that maps to one
 * file in the app's private cache, readable only by whoever receives a URI grant from [UpdateSection].
 */
class UpdateFileProvider : ContentProvider() {
    override fun onCreate() = true

    override fun getType(uri: Uri): String? = if (uri.path == URI_PATH) APK_MIME else null

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        if (mode != "r" || uri.path != URI_PATH) throw FileNotFoundException("not available")
        val file = file(requireNotNull(context))
        if (!file.isFile) throw FileNotFoundException("no pending update")
        return ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
    }

    override fun query(uri: Uri, projection: Array<String>?, selection: String?, selectionArgs: Array<String>?, sortOrder: String?): Cursor? {
        if (uri.path != URI_PATH) return null
        val file = file(requireNotNull(context))
        if (!file.isFile) return null
        val columns = (projection ?: arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE))
            .filter { it == OpenableColumns.DISPLAY_NAME || it == OpenableColumns.SIZE }
            .toTypedArray()
        val row = columns.map { if (it == OpenableColumns.SIZE) file.length() else UpdateChecker.UPDATE_FILE_NAME }
        return MatrixCursor(columns, 1).apply { addRow(row) }
    }

    override fun insert(uri: Uri, values: ContentValues?): Uri? = throw UnsupportedOperationException()
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<String>?): Int =
        throw UnsupportedOperationException()
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<String>?): Int = throw UnsupportedOperationException()

    companion object {
        const val APK_MIME = "application/vnd.android.package-archive"
        private const val URI_PATH = "/update.apk"

        fun dir(context: Context) = File(context.cacheDir, "updates")
        fun file(context: Context) = File(dir(context), UpdateChecker.UPDATE_FILE_NAME)
        fun uri(context: Context): Uri =
            Uri.Builder().scheme("content").authority("${context.packageName}.updates").path(URI_PATH).build()
    }
}

/** Housekeeping for the one pending update APK in `cache/updates/`. */
object UpdateStorage {
    /** Removes files in [dir] last modified at least [olderThanMs] before [nowMs]; returns how many went. */
    fun purge(dir: File, olderThanMs: Long, nowMs: Long): Int {
        var removed = 0
        for (f in dir.listFiles().orEmpty()) {
            if (f.isFile && nowMs - f.lastModified() >= olderThanMs && f.delete()) removed++
        }
        return removed
    }

    /** An abandoned download (the card forgets its offer when the app restarts) is not kept for more than a day. */
    const val STALE_MS = 24L * 60 * 60 * 1000
}
