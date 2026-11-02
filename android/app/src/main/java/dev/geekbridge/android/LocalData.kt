// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.ContentResolver
import android.content.ContentUris
import android.content.Context
import android.os.Bundle
import android.os.CancellationSignal
import android.os.OperationCanceledException
import android.provider.CalendarContract
import android.provider.ContactsContract

data class ContactPreview(val name: String, val number: String?)

data class AgendaEvent(
    val title: String,
    val beginMillis: Long,
    val endMillis: Long,
    val allDay: Boolean,
    val location: String?,
)

sealed interface LoadResult<out T> {
    /** [truncated] is true when the provider had more rows than the preview bound. */
    data class Loaded<T>(val items: List<T>, val truncated: Boolean) : LoadResult<T>

    data class Failed(val reasonId: Int, val detail: String? = null) : LoadResult<Nothing> {
        fun reason(s: Strings): String = if (detail == null) s.get(reasonId) else s.get(reasonId, detail)
    }

    data object Cancelled : LoadResult<Nothing>
}

/**
 * Read-only, bounded previews straight from Android's providers. Nothing here is cached, logged,
 * written to disk or sent anywhere. Call only after the user granted the matching permission and
 * only from a worker thread.
 */
object LocalProviders {
    const val MAX_CONTACTS = 25
    const val MAX_EVENTS = 25
    const val AGENDA_DAYS = 7
    private const val MAX_CONTACT_ROWS_SCANNED = 150

    fun loadContacts(context: Context, signal: CancellationSignal): LoadResult<ContactPreview> {
        val args = Bundle().apply {
            putString(ContentResolver.QUERY_ARG_SQL_SORT_ORDER, "${ContactsContract.CommonDataKinds.Phone.DISPLAY_NAME} ASC")
            putInt(ContentResolver.QUERY_ARG_LIMIT, MAX_CONTACT_ROWS_SCANNED)
        }
        val projection = arrayOf(
            ContactsContract.CommonDataKinds.Phone.CONTACT_ID,
            ContactsContract.CommonDataKinds.Phone.DISPLAY_NAME,
            ContactsContract.CommonDataKinds.Phone.NUMBER,
        )
        return guarded {
            val out = ArrayList<ContactPreview>(MAX_CONTACTS)
            val seen = HashSet<Long>()
            var truncated = false
            val cursor = context.contentResolver.query(
                ContactsContract.CommonDataKinds.Phone.CONTENT_URI, projection, args, signal,
            ) ?: return@guarded LoadResult.Failed(R.string.local_contacts_unavailable)
            cursor.use {
                var scanned = 0
                while (it.moveToNext()) {
                    signal.throwIfCanceled()
                    if (++scanned > MAX_CONTACT_ROWS_SCANNED) {
                        truncated = true
                        break
                    }
                    val id = it.getLong(0)
                    if (!seen.add(id)) continue
                    if (out.size >= MAX_CONTACTS) {
                        truncated = true
                        break
                    }
                    out += ContactPreview(
                        name = Bounds.clip(it.getString(1), Limits.PREVIEW_FIELD_CHARS).ifEmpty { "(no name)" },
                        number = Bounds.clip(it.getString(2), 32).ifEmpty { null },
                    )
                }
            }
            LoadResult.Loaded(out, truncated)
        }
    }

    fun loadAgenda(context: Context, nowMillis: Long, signal: CancellationSignal): LoadResult<AgendaEvent> {
        val end = nowMillis + AGENDA_DAYS * 24L * 60 * 60 * 1000
        val uri = CalendarContract.Instances.CONTENT_URI.buildUpon().also {
            ContentUris.appendId(it, nowMillis)
            ContentUris.appendId(it, end)
        }.build()
        val projection = arrayOf(
            CalendarContract.Instances.TITLE,
            CalendarContract.Instances.BEGIN,
            CalendarContract.Instances.END,
            CalendarContract.Instances.ALL_DAY,
            CalendarContract.Instances.EVENT_LOCATION,
        )
        val args = Bundle().apply {
            putString(ContentResolver.QUERY_ARG_SQL_SORT_ORDER, "${CalendarContract.Instances.BEGIN} ASC")
            putInt(ContentResolver.QUERY_ARG_LIMIT, MAX_EVENTS + 1)
        }
        return guarded {
            val out = ArrayList<AgendaEvent>(MAX_EVENTS)
            var truncated = false
            val cursor = context.contentResolver.query(uri, projection, args, signal)
                ?: return@guarded LoadResult.Failed(R.string.local_calendar_unavailable)
            cursor.use {
                while (it.moveToNext()) {
                    signal.throwIfCanceled()
                    if (out.size >= MAX_EVENTS) {
                        truncated = true
                        break
                    }
                    out += AgendaEvent(
                        title = Bounds.clip(it.getString(0), Limits.PREVIEW_FIELD_CHARS).ifEmpty { "(no title)" },
                        beginMillis = it.getLong(1),
                        endMillis = it.getLong(2),
                        allDay = it.getInt(3) != 0,
                        location = Bounds.clip(it.getString(4), Limits.PREVIEW_FIELD_CHARS).ifEmpty { null },
                    )
                }
            }
            LoadResult.Loaded(out, truncated)
        }
    }

    // Failure reasons never include exception messages (they could echo provider data).
    private inline fun <T> guarded(block: () -> LoadResult<T>): LoadResult<T> = try {
        block()
    } catch (e: OperationCanceledException) {
        LoadResult.Cancelled
    } catch (e: SecurityException) {
        LoadResult.Failed(R.string.local_access_refused)
    } catch (e: RuntimeException) {
        LoadResult.Failed(R.string.local_reading_failed, e.javaClass.simpleName)
    }
}
