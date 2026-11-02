// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.util.Locale
import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

/** Semantic version (semver 2.0 precedence; build metadata ignored). Mirrors `bridge-update`'s parser. */
class AppVersion private constructor(
    val major: Long,
    val minor: Long,
    val patch: Long,
    private val pre: List<String>,
) : Comparable<AppVersion> {
    val isPrerelease: Boolean get() = pre.isNotEmpty()

    /**
     * Same formula as `android/app/build.gradle.kts` and `scripts/version-code.sh` (docs/builds.md):
     * `(major*10_000 + minor*100 + patch) * 100 + slot`; slot 99 for a final release, else
     * alpha 0..24, beta 25..49, rc 50..74 by the number after the label (capped), so rc < final.
     */
    fun toVersionCode(): Int? {
        if (major > 214 || minor > 99 || patch > 99) return null
        val slot = if (pre.isEmpty()) 99 else {
            val base = when (pre[0].lowercase(Locale.ROOT)) { "beta" -> 25; "rc" -> 50; else -> 0 }
            base + (pre.getOrNull(1)?.toLongOrNull() ?: 0L).coerceIn(0L, 24L).toInt()
        }
        return ((major * 10_000 + minor * 100 + patch) * 100 + slot).toInt()
    }

    override fun compareTo(other: AppVersion): Int {
        compareValues(major, other.major).let { if (it != 0) return it }
        compareValues(minor, other.minor).let { if (it != 0) return it }
        compareValues(patch, other.patch).let { if (it != 0) return it }
        if (pre.isEmpty() || other.pre.isEmpty()) {
            return when {
                pre.isEmpty() && other.pre.isEmpty() -> 0
                pre.isEmpty() -> 1
                else -> -1
            }
        }
        for (i in 0 until minOf(pre.size, other.pre.size)) {
            val a = pre[i]
            val b = other.pre[i]
            val an = a.toLongOrNull()
            val bn = b.toLongOrNull()
            val c = when {
                an != null && bn != null -> an.compareTo(bn)
                an != null -> -1
                bn != null -> 1
                else -> a.compareTo(b)
            }
            if (c != 0) return c
        }
        return pre.size.compareTo(other.pre.size)
    }

    override fun equals(other: Any?) = other is AppVersion && compareTo(other) == 0
    override fun hashCode() = listOf(major, minor, patch, pre).hashCode()
    override fun toString() = "$major.$minor.$patch" + if (pre.isEmpty()) "" else "-" + pre.joinToString(".")

    companion object {
        fun parse(text: String?): AppVersion? {
            var t = text?.trim() ?: return null
            if (t.isEmpty() || t.length > 64) return null
            if (t[0] == 'v' || t[0] == 'V') t = t.substring(1)
            val plus = t.indexOf('+')
            if (plus >= 0) {
                val build = t.substring(plus + 1)
                if (build.isEmpty() || !build.split('.').all { id -> id.isNotEmpty() && id.all(::alnumOrDash) }) return null
                t = t.substring(0, plus)
            }
            val dash = t.indexOf('-')
            val core = if (dash >= 0) t.substring(0, dash) else t
            val preText = if (dash >= 0) t.substring(dash + 1) else null
            val nums = core.split('.')
            if (nums.size != 3) return null
            val (ma, mi, pa) = nums.map(::number)
            if (ma == null || mi == null || pa == null) return null
            val pre = if (preText == null) {
                emptyList()
            } else {
                val ids = preText.split('.')
                if (ids.any { id -> id.isEmpty() || !id.all(::alnumOrDash) }) return null
                if (ids.any { id -> id.all { it in '0'..'9' } && number(id) == null }) return null
                ids
            }
            return AppVersion(ma, mi, pa, pre)
        }

        private fun alnumOrDash(c: Char) = c in '0'..'9' || c in 'a'..'z' || c in 'A'..'Z' || c == '-'

        /** Digits only, no leading zero, fits comfortably in a Long. */
        private fun number(s: String): Long? {
            if (s.isEmpty() || s.length > 18 || !s.all { it in '0'..'9' }) return null
            if (s.length > 1 && s[0] == '0') return null
            return s.toLong()
        }
    }
}

/** A newer release this build can install, with everything needed to verify it. */
data class UpdateOffer(
    val version: String,
    val tag: String,
    val notes: String,
    val releaseUrl: String,
    val apkName: String,
    val apkUrl: String,
    val apkSize: Long,
    val sha256: String?,
)

sealed class UpdateCheckResult {
    data class UpToDate(val latest: String?) : UpdateCheckResult()
    data class Available(val offer: UpdateOffer) : UpdateCheckResult()
    data class Failed(val reason: String) : UpdateCheckResult()

    /** GitHub answered 404/403 for the release list: private repository, or nothing published yet. */
    data object NoPublicRelease : UpdateCheckResult() {
        const val MESSAGE = "No public release found: the project repository may be private or has no releases yet."
    }
}

object UpdateModel {
    const val MAX_NOTES_CHARS = 2000
    const val MAX_APK_BYTES = 200L * 1024 * 1024
    const val CHECK_INTERVAL_MS = 12L * 60 * 60 * 1000

    /** Automatic check on app open: at most once per 12 hours (clock moving backwards also counts as due). */
    fun autoCheckDue(lastCheckMs: Long, nowMs: Long): Boolean =
        lastCheckMs <= 0 || nowMs < lastCheckMs || nowMs - lastCheckMs >= CHECK_INTERVAL_MS

    /** `owner/name` as GitHub spells it; anything else never reaches an API URL. */
    fun validRepo(repo: String?): Boolean {
        val parts = repo?.split('/') ?: return false
        return parts.size == 2 && parts.all { p ->
            p.isNotEmpty() && p.length <= 100 && p != "." && p != ".." &&
                p.all { it in '0'..'9' || it in 'a'..'z' || it in 'A'..'Z' || it == '-' || it == '_' || it == '.' }
        }
    }

    fun isHex64(s: String) = s.length == 64 && s.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }

    /** Exact hosts a release asset may be served from; `raw.`/`gist.` etc. host user content. */
    private val RELEASE_HOSTS = setOf(
        "github.com",
        "api.github.com",
        "objects.githubusercontent.com",
        "release-assets.githubusercontent.com",
        "github-releases.githubusercontent.com",
    )

    /** Only https GitHub hosts are ever contacted (loopback http only when [allowLoopback], for tests). */
    fun urlAllowed(url: String, allowLoopback: Boolean = false): Boolean {
        val uri = try {
            java.net.URI(url)
        } catch (_: Exception) {
            return false
        }
        val host = uri.host?.lowercase(Locale.ROOT) ?: return false
        if (uri.userInfo != null) return false
        if (uri.scheme == "https") {
            return (uri.port == -1 || uri.port == 443) && host in RELEASE_HOSTS
        }
        return allowLoopback && uri.scheme == "http" && (host == "127.0.0.1" || host == "localhost")
    }

    /** `<hex>  <name>` / `<hex> *<name>` lookup; null unless every matching line agrees. */
    fun findChecksum(listing: String, name: String): String? {
        var found: String? = null
        for (raw in listing.lineSequence()) {
            val line = raw.trim()
            val split = line.indexOfFirst { it.isWhitespace() }
            if (split <= 0) continue
            val digest = line.substring(0, split)
            var rest = line.substring(split).trim().trimStart('*')
            if (rest.startsWith("./")) rest = rest.substring(2)
            if (rest != name || !isHex64(digest)) continue
            val d = digest.lowercase(Locale.ROOT)
            if (found != null && found != d) return null
            found = d
        }
        return found
    }

    fun safeFileName(name: String) =
        name.isNotEmpty() && name.length <= 128 && !name.startsWith(".") && !name.contains("..") &&
            name.all { it.isLetterOrDigit() && it.code < 128 || it == '.' || it == '-' || it == '_' || it == '+' }

    fun cleanNotes(text: String?): String {
        val kept = (text ?: "").filter { !Character.isISOControl(it) || it == '\n' }.trim()
        return if (kept.length > MAX_NOTES_CHARS) kept.substring(0, MAX_NOTES_CHARS - 1) + "…" else kept
    }

    /** The Android package among the release assets: `*.apk`, preferring `geekbridge-android.apk`. */
    fun pickApk(assets: List<Pair<String, Long>>): Pair<String, Long>? =
        assets.filter { (n, size) -> n.lowercase(Locale.ROOT).endsWith(".apk") && safeFileName(n) && size in 1..MAX_APK_BYTES }
            .minWithOrNull(compareBy({ if (it.first == "geekbridge-android.apk") 0 else 1 }, { it.first }))

    /** Parsed `releases/latest` payload; throws [IllegalArgumentException] for anything malformed. */
    class Release(val tag: String, val htmlUrl: String, val body: String, val assets: List<Triple<String, String, Long>>)

    fun parseRelease(json: String): Release {
        val o = try {
            JSONObject(json)
        } catch (_: JSONException) {
            throw IllegalArgumentException("The release information is not understood.")
        }
        val tag = o.optString("tag_name", "")
        require(tag.isNotEmpty() && tag.length <= 64) { "The release has no usable tag." }
        require(!o.optBoolean("draft", false) && !o.optBoolean("prerelease", false)) { "Not a published release." }
        val arr: JSONArray = o.optJSONArray("assets") ?: JSONArray()
        require(arr.length() <= 200) { "The release lists too many files." }
        val assets = ArrayList<Triple<String, String, Long>>()
        for (i in 0 until arr.length()) {
            val a = arr.optJSONObject(i) ?: continue
            assets += Triple(a.optString("name", ""), a.optString("browser_download_url", ""), a.optLong("size", 0))
        }
        return Release(tag, o.optString("html_url", ""), o.optString("body", ""), assets)
    }

    /**
     * Decides what to offer. A release is newer only when its semver is greater **and** the version code
     * derived from it is greater than the installed one; prereleases are never offered.
     */
    fun evaluate(
        release: Release,
        currentName: String,
        currentCode: Int,
        sums: (String) -> String?,
        allowLoopback: Boolean = false,
    ): UpdateCheckResult {
        val remote = AppVersion.parse(release.tag) ?: return UpdateCheckResult.Failed("The release tag is not a version.")
        val local = AppVersion.parse(currentName) ?: return UpdateCheckResult.Failed("This app's version is not recognised.")
        if (remote.isPrerelease) return UpdateCheckResult.UpToDate(remote.toString())
        val code = remote.toVersionCode()
        if (remote <= local || code == null || code <= currentCode) return UpdateCheckResult.UpToDate(remote.toString())
        val apk = pickApk(release.assets.map { it.first to it.third })
            ?: return UpdateCheckResult.Failed("Version $remote is out, but no Android package is published.")
        val url = release.assets.first { it.first == apk.first }.second
        if (!urlAllowed(url, allowLoopback)) return UpdateCheckResult.Failed("The download address is not trusted.")
        val page = if (urlAllowed(release.htmlUrl, allowLoopback)) release.htmlUrl else ""
        return UpdateCheckResult.Available(
            UpdateOffer(remote.toString(), release.tag, cleanNotes(release.body), page, apk.first, url, apk.second, sums(apk.first)),
        )
    }
}
