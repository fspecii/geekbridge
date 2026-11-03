// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONArray
import org.json.JSONObject

/** How much of the photo library Android lets this app see. */
enum class PhotoAccess { None, Partial, Full }

/**
 * Which Android permissions give access to images, by version (docs/design/recent-photos.md):
 * 34+ asks for READ_MEDIA_IMAGES and READ_MEDIA_VISUAL_USER_SELECTED together, so the user may share only the photos
 * they pick (partial access); 33 has READ_MEDIA_IMAGES; older versions have the broad storage permission.
 */
object PhotoAccessRules {
    const val READ_MEDIA_IMAGES = "android.permission.READ_MEDIA_IMAGES"
    const val READ_MEDIA_VISUAL_USER_SELECTED = "android.permission.READ_MEDIA_VISUAL_USER_SELECTED"
    const val READ_EXTERNAL_STORAGE = "android.permission.READ_EXTERNAL_STORAGE"

    fun requestPermissions(sdk: Int): Array<String> = when {
        sdk >= 34 -> arrayOf(READ_MEDIA_IMAGES, READ_MEDIA_VISUAL_USER_SELECTED)
        sdk >= 33 -> arrayOf(READ_MEDIA_IMAGES)
        else -> arrayOf(READ_EXTERNAL_STORAGE)
    }

    fun evaluate(sdk: Int, has: (String) -> Boolean): PhotoAccess = when {
        sdk >= 33 -> when {
            has(READ_MEDIA_IMAGES) -> PhotoAccess.Full
            sdk >= 34 && has(READ_MEDIA_VISUAL_USER_SELECTED) -> PhotoAccess.Partial
            else -> PhotoAccess.None
        }
        else -> if (has(READ_EXTERNAL_STORAGE)) PhotoAccess.Full else PhotoAccess.None
    }
}

/** One picture as the media library describes it. [name] is shown, never used as a path. */
data class PhotoInfo(
    val mediaId: Long,
    val name: String,
    val takenMs: Long,
    val size: Long,
    val width: Int,
    val height: Int,
)

/** A thumbnail ready to send: a small JPEG and its pixel size. */
class PhotoThumb(val width: Int, val height: Int, val jpeg: ByteArray)

/** The media library behind a small interface, so the rules are tested without Android. */
interface PhotoPort {
    fun access(): PhotoAccess
    /** Newest images first, at most [max] (what the library lets this app see). */
    fun latest(max: Int): List<PhotoInfo>
    /** A small JPEG of the picture, or null if it cannot be read any more. */
    fun thumbnail(p: PhotoInfo): PhotoThumb?
    /** Still there (and still visible to the app)? */
    fun exists(p: PhotoInfo): Boolean
    /** Starts sending the whole file to [connectionId] over the ordinary share channel. False if it cannot start. */
    fun sendFull(p: PhotoInfo, connectionId: Long): Boolean
}

object PhotoRules {
    const val REQUEST_KIND = "geekbridge.photos.request"
    const val LIST_KIND = "geekbridge.photos.list"
    const val THUMB_KIND = "geekbridge.photos.thumb"

    const val MAX_ITEMS = 24
    const val MAX_ID = 32
    const val MAX_NAME = 100
    const val MAX_THUMB_BYTES = 24 * 1024
    const val THUMB_EDGE = 160
    const val MAX_PHOTO_BYTES = 512L * 1024 * 1024
    const val MAX_DIMENSION = 100_000
    /** Ids remembered (about four lists); older ones answer "gone". */
    const val MAX_IDS = 96

    sealed class Ask {
        data class List(val limit: Int) : Ask()
        data class Thumb(val id: String) : Ask()
        data class Get(val id: String) : Ask()
    }

    fun validId(s: String): Boolean = s.isNotEmpty() && s.length <= MAX_ID && s.all { it.isLetterOrDigit() && it.code < 128 || it == '_' || it == '-' }

    /** Strict, like the computer's parser: anything else is ignored. */
    fun parseRequest(b: JSONObject): Ask? = when (b.opt("op") as? String) {
        "list" -> (b.opt("limit") as? Int)?.takeIf { it in 1..MAX_ITEMS }?.let { Ask.List(it) }
        "thumb" -> (b.opt("id") as? String)?.takeIf(::validId)?.let { Ask.Thumb(it) }
        "get" -> (b.opt("id") as? String)?.takeIf(::validId)?.let { Ask.Get(it) }
        else -> null
    }

    private fun isBidi(c: Char) = c == '\u061C' || c == '\u200E' || c == '\u200F' || c == '\u2028' || c == '\u2029' || c in '\u202A'..'\u202E' || c in '\u2066'..'\u2069'

    /**
     * Only a picture whose name is plain text of at most [MAX_NAME] characters is listed: the computer matches the file
     * the phone later sends by this very name, so what is shown and what is sent must be the same string.
     */
    fun listable(p: PhotoInfo): Boolean =
        p.name.isNotBlank() && p.name == p.name.trim() && p.name.codePointCount(0, p.name.length) <= MAX_NAME &&
            p.name.none { Character.isISOControl(it) || isBidi(it) } &&
            p.size in 1..MAX_PHOTO_BYTES && p.width in 1..MAX_DIMENSION && p.height in 1..MAX_DIMENSION && p.takenMs >= 0

    fun listBody(items: List<Pair<String, PhotoInfo>>, partial: Boolean): JSONObject = JSONObject()
        .put(
            "items",
            JSONArray().also { a ->
                for ((id, p) in items.take(MAX_ITEMS)) {
                    a.put(JSONObject().put("id", id).put("name", p.name).put("taken", p.takenMs).put("size", p.size).put("w", p.width).put("h", p.height))
                }
            },
        )
        .put("partial", partial)

    fun thumbBody(id: String, t: PhotoThumb): JSONObject = JSONObject()
        .put("id", id).put("w", t.width).put("h", t.height)
        .put("jpeg", java.util.Base64.getEncoder().encodeToString(t.jpeg))

    fun goneBody(id: String): JSONObject = JSONObject().put("id", id).put("gone", true)

    /**
     * Compresses with falling quality until the picture fits [maxBytes]; null if even the lowest quality does not
     * (the caller then shrinks the picture and tries again). [compress] takes a JPEG quality of 1..100.
     */
    fun fit(maxBytes: Int, qualities: List<Int> = listOf(75, 60, 45, 30), compress: (Int) -> ByteArray): ByteArray? {
        for (q in qualities) {
            val b = compress(q)
            if (b.size <= maxBytes) return b
        }
        return null
    }
}

/**
 * Opaque ids the computer is given: "p1", "p2", ... never reused, bounded, oldest forgotten first. An id belongs to
 * the connection it was handed to: another computer (even an allowed one) cannot ask for a picture it was not shown.
 */
class PhotoIds(private val max: Int = PhotoRules.MAX_IDS) {
    private val byId = LinkedHashMap<String, Pair<Long, PhotoInfo>>()
    private var counter = 0L

    @Synchronized
    fun assign(photos: List<PhotoInfo>, connectionId: Long = 0): List<Pair<String, PhotoInfo>> {
        val out = photos.map { p -> "p${++counter}" to p }
        for ((id, p) in out) byId[id] = connectionId to p
        while (byId.size > max) byId.remove(byId.keys.first())
        return out
    }

    @Synchronized fun get(id: String, connectionId: Long = 0): PhotoInfo? = byId[id]?.takeIf { it.first == connectionId }?.second

    @Synchronized fun clear() = byId.clear()
}

/** A token bucket: [burst] at once, then one more every [everyMs]. */
class Throttle(private val burst: Int, private val everyMs: Long) {
    private var tokens = burst.toDouble()
    private var last = Long.MIN_VALUE

    fun allow(nowMs: Long): Boolean {
        if (last != Long.MIN_VALUE) tokens = minOf(burst.toDouble(), tokens + (nowMs - last).coerceAtLeast(0) / everyMs.toDouble())
        last = nowMs
        if (tokens < 1.0) return false
        tokens -= 1.0
        return true
    }
}

/**
 * Answers a computer's recent-photos requests: a short list of the newest pictures, a small thumbnail per picture
 * when asked, and (only on "get") the whole file over the share channel. Nothing is read unless a computer that the
 * user allowed asked, and only ids listed to that very computer can be asked for. A computer that asks faster than a
 * person browsing could (thousands of thumbnails, lists in a loop) is ignored, so it cannot burn the battery or hold
 * the engine. File names and pictures are never logged.
 *
 * [send] delivers a packet to one computer's connection and tells whether the core took it.
 */
class RecentPhotos(
    private val port: PhotoPort,
    private val send: (kind: String, body: JSONObject, connectionId: Long) -> Boolean,
    private val enabled: () -> Boolean,
    private val ids: PhotoIds = PhotoIds(),
    private val clock: () -> Long = System::currentTimeMillis,
) {
    private class Limits {
        val list = Throttle(LIST_BURST, LIST_EVERY_MS)
        val thumb = Throttle(THUMB_BURST, THUMB_EVERY_MS)
        val get = Throttle(GET_BURST, GET_EVERY_MS)
    }

    private val limits = HashMap<Long, Limits>()

    @Synchronized
    fun reset() {
        ids.clear()
        limits.clear()
    }

    @Synchronized
    fun onRequest(connectionId: Long, body: JSONObject) {
        if (!enabled()) return
        val access = port.access()
        if (access == PhotoAccess.None) return
        val ask = PhotoRules.parseRequest(body) ?: return
        if (limits.size > MAX_TRACKED && connectionId !in limits) limits.clear()
        val lim = limits.getOrPut(connectionId) { Limits() }
        val now = clock()
        when (ask) {
            is PhotoRules.Ask.List -> if (lim.list.allow(now)) list(connectionId, ask.limit, access)
            is PhotoRules.Ask.Thumb -> if (lim.thumb.allow(now)) thumb(connectionId, ask.id)
            is PhotoRules.Ask.Get -> if (lim.get.allow(now)) get(connectionId, ask.id)
        }
    }

    private fun list(connectionId: Long, limit: Int, access: PhotoAccess) {
        // A few more are read than asked for: a picture with an odd name is skipped, not replaced by a gap.
        val photos = port.latest(limit * 2).filter(PhotoRules::listable).take(limit)
        send(PhotoRules.LIST_KIND, PhotoRules.listBody(ids.assign(photos, connectionId), access == PhotoAccess.Partial), connectionId)
    }

    private fun thumb(connectionId: Long, id: String) {
        val p = ids.get(id, connectionId)
        val t = p?.let { port.thumbnail(it) }
        val body = if (t == null || t.jpeg.size > PhotoRules.MAX_THUMB_BYTES || t.width !in 1..256 || t.height !in 1..256) PhotoRules.goneBody(id) else PhotoRules.thumbBody(id, t)
        send(PhotoRules.THUMB_KIND, body, connectionId)
    }

    private fun get(connectionId: Long, id: String) {
        val p = ids.get(id, connectionId) ?: return
        if (!port.exists(p)) return
        port.sendFull(p, connectionId)
    }

    companion object {
        // A person opening the tab asks for one list and up to 24 thumbnails (the computer itself waits 3 s between
        // lists). These allow several of those in a row and then slow to a pace no person reaches.
        const val LIST_BURST = 4
        const val LIST_EVERY_MS = 2_000L
        const val THUMB_BURST = 60
        const val THUMB_EVERY_MS = 125L
        const val GET_BURST = 3
        const val GET_EVERY_MS = 5_000L
        const val MAX_TRACKED = 16
    }
}

/** What stands between the user and "the computer shows my recent photos", in the order to fix it. */
enum class PhotosStep { Ready, ReadyPartial, TurnOn, GrantAccess, PairComputer, ChooseComputer }

object PhotosReadiness {
    fun evaluate(switchOn: Boolean, access: PhotoAccess, pairedCount: Int, allowedCount: Int): PhotosStep = when {
        !switchOn -> PhotosStep.TurnOn
        access == PhotoAccess.None -> PhotosStep.GrantAccess
        pairedCount == 0 -> PhotosStep.PairComputer
        allowedCount == 0 -> PhotosStep.ChooseComputer
        access == PhotoAccess.Partial -> PhotosStep.ReadyPartial
        else -> PhotosStep.Ready
    }

    fun message(step: PhotosStep, s: Strings): String = s.get(
        when (step) {
            PhotosStep.Ready -> R.string.photos_state_ready
            PhotosStep.ReadyPartial -> R.string.photos_state_partial
            PhotosStep.TurnOn -> R.string.photos_state_turn_on
            PhotosStep.GrantAccess -> R.string.photos_state_access
            PhotosStep.PairComputer -> R.string.photos_state_pair
            PhotosStep.ChooseComputer -> R.string.photos_state_choose
        },
    )
}
