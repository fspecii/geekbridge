package dev.geekbridge.android

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import java.io.File

/** A destination for one incoming file, open for writing. Close [pfd] right after `share_accept` returns. */
class SavedFile(val name: String, val uri: Uri?, val file: File?, val pfd: ParcelFileDescriptor)

/**
 * Where received files go. API 29+: a pending `MediaStore.Downloads` row under `Download/GeekBridge` (no storage
 * permission). API 26-28 have no such collection and shared storage needs a broad permission we do not
 * request, so files go to the app's own external Downloads folder instead.
 */
object ReceivedFiles {
    const val FOLDER = "GeekBridge"

    fun create(ctx: Context, rawName: String): SavedFile {
        val name = NameRules.sanitize(rawName)
        return if (Build.VERSION.SDK_INT >= 29) {
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, name)
                put(MediaStore.Downloads.MIME_TYPE, NameRules.mimeFor(name))
                put(MediaStore.Downloads.RELATIVE_PATH, "${Environment.DIRECTORY_DOWNLOADS}/$FOLDER")
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
            val uri = ctx.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                ?: throw java.io.IOException("could not create the download entry")
            try {
                val pfd = ctx.contentResolver.openFileDescriptor(uri, "w") ?: throw java.io.IOException("could not open the entry")
                SavedFile(name, uri, null, pfd)
            } catch (e: Exception) {
                ctx.contentResolver.delete(uri, null, null)
                throw e
            }
        } else {
            val dir = File(ctx.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS), FOLDER)
            if (!dir.isDirectory && !dir.mkdirs()) throw java.io.IOException("could not create the folder")
            val unique = NameRules.unique(name) { File(dir, it).exists() }
            val f = File(dir, unique)
            if (!f.createNewFile()) throw java.io.IOException("file exists")
            SavedFile(unique, null, f, ParcelFileDescriptor.open(f, ParcelFileDescriptor.MODE_WRITE_ONLY or ParcelFileDescriptor.MODE_TRUNCATE))
        }
    }

    fun publish(ctx: Context, s: SavedFile) {
        if (s.uri != null) {
            val v = ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }
            ctx.contentResolver.update(s.uri, v, null, null)
        }
    }

    fun discard(ctx: Context, s: SavedFile) {
        if (s.uri != null) ctx.contentResolver.delete(s.uri, null, null) else s.file?.delete()
    }
}
