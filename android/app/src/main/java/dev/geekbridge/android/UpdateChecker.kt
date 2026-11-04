// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest

/**
 * Talks to GitHub (`/repos/<repo>/releases/latest`) with [HttpURLConnection] (https; plain http only to
 * loopback, for tests). One bounded request per check, a `User-Agent`, timeouts, a size cap, `If-None-Match`
 * with the stored ETag, no cookies, no identifiers. It only opens outbound connections: nothing listens.
 *
 * All methods block; call them off the main thread.
 */
class UpdateChecker(
    private val currentName: String,
    private val currentCode: Int,
    private val baseUrl: String = "https://api.github.com",
    private val repo: String = DEFAULT_REPO,
    private val allowLoopback: Boolean = false,
    private val channel: UpdateChannel = UpdateChannel.Stable,
    private val connectTimeoutMs: Int = 10_000,
    private val readTimeoutMs: Int = 15_000,
) {
    /** The previous 200 answer and its ETag. [extra] is the dev channel's `dev-manifest.json` text. */
    class Cache(val etag: String, val body: String, val extra: String? = null)

    /** Raised for every failure; [text] is safe to show and resolved to the user's language at the UI edge. */
    class UpdateException(val text: UpdateText) : Exception("update error ${text.id}") {
        constructor(id: Int, vararg args: Any) : this(UpdateText(id, *args))
    }

    /**
     * Fetches the latest release. [cache] is the previous 200 body with its ETag; on 304 it is reused.
     * [onFresh] receives a new (etag, body) pair to persist.
     */
    fun check(cache: Cache?, onFresh: (Cache) -> Unit = {}): UpdateCheckResult =
        if (channel == UpdateChannel.Dev) checkDev(cache, onFresh) else checkStable(cache, onFresh)

    private fun checkStable(cache: Cache?, onFresh: (Cache) -> Unit): UpdateCheckResult = try {
        val url = "${baseUrl.trimEnd('/')}/repos/$repo/releases/latest"
        val response = get(url, "application/vnd.github+json", MAX_API_BYTES, cache?.etag)
        val body = when (response.status) {
            200 -> {
                val text = String(response.body, Charsets.UTF_8)
                response.etag?.let { onFresh(Cache(it, text)) }
                text
            }
            304 -> cache?.body ?: throw UpdateException(R.string.upd_err_not_modified)
            // Checked before 404/403: a rate-limited 403 is not "no public release".
            403, 429 -> if (response.rateLimited || response.status == 429) {
                throw UpdateException(R.string.upd_err_rate_limit)
            } else {
                return probeReleaseList()
            }
            404 -> return probeReleaseList()
            else -> throw UpdateException(R.string.upd_err_http, response.status)
        }
        val release = try {
            UpdateModel.parseRelease(body)
        } catch (e: UpdateModel.ReleaseFormatException) {
            throw UpdateException(e.text)
        } catch (e: IllegalArgumentException) {
            throw UpdateException(R.string.upd_err_release_unclear)
        }
        UpdateModel.evaluate(release, currentName, currentCode, { name -> fetchSums(release, name) }, allowLoopback)
    } catch (e: UpdateException) {
        UpdateCheckResult.Failed(e.text)
    } catch (e: IOException) {
        UpdateCheckResult.Failed(UpdateText(R.string.upd_err_unreachable, Bounds.clip(e.javaClass.simpleName, 40)))
    }

    /**
     * The dev channel: `GET /repos/<repo>/releases/tags/dev` (conditional), then the small `dev-manifest.json`
     * asset (kept in [Cache.extra], so a 304 costs one request in total).
     */
    private fun checkDev(cache: Cache?, onFresh: (Cache) -> Unit): UpdateCheckResult {
        return try {
        val response = get("${baseUrl.trimEnd('/')}/repos/$repo/releases/tags/${UpdateModel.DEV_TAG}", "application/vnd.github+json", MAX_API_BYTES, cache?.etag)
        val body = when (response.status) {
            200 -> String(response.body, Charsets.UTF_8)
            304 -> cache?.body ?: throw UpdateException(R.string.upd_err_not_modified)
            403, 429 -> if (response.rateLimited || response.status == 429) {
                throw UpdateException(R.string.upd_err_rate_limit)
            } else {
                return probeReleaseList()
            }
            404 -> return probeReleaseList()
            else -> throw UpdateException(R.string.upd_err_http, response.status)
        }
        val release = try {
            UpdateModel.parseRelease(body)
        } catch (e: UpdateModel.ReleaseFormatException) {
            throw UpdateException(e.text)
        } catch (e: IllegalArgumentException) {
            throw UpdateException(R.string.upd_err_release_unclear)
        }
        if (release.draft || release.tag != UpdateModel.DEV_TAG) return UpdateCheckResult.UpToDate(null)
        var manifest = if (response.status == 304) cache?.extra else null
        if (manifest == null) {
            val asset = release.assets.firstOrNull { it.first == UpdateModel.DEV_MANIFEST && it.third <= UpdateModel.MAX_MANIFEST_BYTES }
                ?: throw UpdateException(R.string.upd_err_dev_manifest)
            if (!UpdateModel.urlAllowed(asset.second, allowLoopback)) throw UpdateException(R.string.upd_err_addr_not_trusted)
            val r = get(asset.second, "application/octet-stream", UpdateModel.MAX_MANIFEST_BYTES, null)
            if (r.status != 200) throw UpdateException(R.string.upd_err_http, r.status)
            manifest = String(r.body, Charsets.UTF_8)
        }
        val result = UpdateModel.evaluateDev(release, manifest, currentCode, { name -> fetchSums(release, name) }, allowLoopback)
        // Only a manifest that parsed is worth keeping behind the ETag.
        if (response.status == 200 && result !is UpdateCheckResult.Failed) response.etag?.let { onFresh(Cache(it, body, manifest)) }
        result
    } catch (e: UpdateException) {
        UpdateCheckResult.Failed(e.text)
    } catch (e: IOException) {
        UpdateCheckResult.Failed(UpdateText(R.string.upd_err_unreachable, Bounds.clip(e.javaClass.simpleName, 40)))
    }
    }

    /**
     * `releases/latest` answered 404/403. It never returns a pre-release, so a repository whose only release is
     * `v0.1.0-rc.1` answers 404 too. One more request tells that apart from a private repository: a readable
     * release list (even empty, or only pre-releases) means "nothing stable to offer" = up to date.
     */
    private fun probeReleaseList(): UpdateCheckResult {
        val r = get("${baseUrl.trimEnd('/')}/repos/$repo/releases?per_page=30", "application/vnd.github+json", MAX_API_BYTES, null)
        return when {
            r.status == 200 -> try {
                org.json.JSONArray(String(r.body, Charsets.UTF_8))
                UpdateCheckResult.UpToDate(null)
            } catch (_: org.json.JSONException) {
                throw UpdateException(R.string.upd_err_release_unclear)
            }
            r.rateLimited || r.status == 429 -> throw UpdateException(R.string.upd_err_rate_limit)
            r.status == 403 || r.status == 404 -> UpdateCheckResult.NoPublicRelease
            else -> throw UpdateException(R.string.upd_err_http, r.status)
        }
    }

    private fun fetchSums(release: UpdateModel.Release, apkName: String): String? {
        val sums = release.assets.firstOrNull { it.first.equals("SHA256SUMS", true) || it.first.equals("SHA256SUMS.txt", true) }
            ?: return null
        if (!UpdateModel.urlAllowed(sums.second, allowLoopback)) return null
        val r = get(sums.second, "application/octet-stream", MAX_SUMS_BYTES, null)
        if (r.status != 200) return null
        return UpdateModel.findChecksum(String(r.body, Charsets.UTF_8), apkName)
    }

    class Response(val status: Int, val etag: String?, val body: ByteArray, val rateLimited: Boolean = false)

    private class Head(val status: Int, val etag: String?, val rateLimited: Boolean)

    private fun get(startUrl: String, accept: String, maxBytes: Int, etag: String?): Response {
        val out = java.io.ByteArrayOutputStream()
        val head = request(startUrl, accept, etag, maxBytes.toLong()) { stream -> readBounded(stream, maxBytes.toLong()) { b, off, len -> out.write(b, off, len) } }
        return Response(head.status, head.etag, out.toByteArray(), head.rateLimited)
    }

    /**
     * Downloads the offer's APK into [dir] (created if needed), hashing while writing. Deletes the file and
     * throws unless the byte count and SHA-256 both match the release. Returns the verified file.
     */
    fun download(offer: UpdateOffer, dir: File, onProgress: (Long, Long) -> Unit = { _, _ -> }): File {
        val expected = offer.sha256?.lowercase() ?: throw UpdateException(R.string.upd_err_no_sha)
        if (!UpdateModel.isHex64(expected) || !UpdateModel.safeFileName(offer.apkName)) throw UpdateException(R.string.upd_err_bad_description)
        if (offer.apkSize !in 1..UpdateModel.MAX_APK_BYTES) throw UpdateException(R.string.upd_err_bad_size)
        if (!dir.isDirectory && !dir.mkdirs()) throw UpdateException(R.string.upd_err_no_folder)
        dir.listFiles()?.forEach { it.delete() } // one pending update at a time
        val file = File(dir, UPDATE_FILE_NAME)
        val digest = MessageDigest.getInstance("SHA-256")
        var written = 0L
        try {
            file.outputStream().use { sink ->
                val head = request(offer.apkUrl, "application/octet-stream", null, offer.apkSize) { stream ->
                    readBounded(stream, offer.apkSize) { b, off, len ->
                        digest.update(b, off, len)
                        sink.write(b, off, len)
                        written += len
                        onProgress(written, offer.apkSize)
                    }
                }
                if (head.status != 200) throw UpdateException(R.string.upd_err_http_download, head.status)
            }
            if (written != offer.apkSize) throw UpdateException(R.string.upd_err_wrong_size)
            val actual = digest.digest().joinToString("") { "%02x".format(it) }
            if (actual != expected) throw UpdateException(R.string.upd_err_sha_mismatch)
            return file
        } catch (e: Exception) {
            file.delete()
            throw if (e is UpdateException) e else UpdateException(if (e is IOException) R.string.upd_err_interrupted else R.string.upd_err_download_failed)
        }
    }

    /**
     * One GET with manual, host-checked redirects. [consume] reads the body of a 200; other bodies are
     * ignored. Returns the status, ETag and whether GitHub signalled a rate limit.
     *
     * No `Authorization` header is ever sent: an access token baked into an APK (or kept in app storage)
     * can be extracted by anyone who holds the app, so private-repository testing is desktop-only.
     */
    private fun request(
        startUrl: String,
        accept: String,
        etag: String?,
        maxBytes: Long,
        consume: (InputStream) -> Unit,
    ): Head {
        var url = startUrl
        repeat(MAX_REDIRECTS + 1) {
            if (!UpdateModel.urlAllowed(url, allowLoopback)) throw UpdateException(R.string.upd_err_untrusted_addr)
            val conn = URL(url).openConnection() as HttpURLConnection
            try {
                conn.instanceFollowRedirects = false
                conn.connectTimeout = connectTimeoutMs
                conn.readTimeout = readTimeoutMs
                conn.useCaches = false
                conn.requestMethod = "GET"
                conn.setRequestProperty("User-Agent", "geekbridge-android-updater/$currentName")
                conn.setRequestProperty("Accept", accept)
                conn.setRequestProperty("Accept-Encoding", "identity")
                conn.setRequestProperty("X-GitHub-Api-Version", "2022-11-28")
                if (etag != null) conn.setRequestProperty("If-None-Match", etag)
                val status = conn.responseCode
                if (status in 301..308 && status != 304 && status != 305 && status != 306) {
                    val location = conn.getHeaderField("Location") ?: throw UpdateException(R.string.upd_err_redirect_nolocation)
                    url = try {
                        URL(URL(url), location).toString() // absolute or same-origin relative; re-checked on the next hop
                    } catch (_: java.net.MalformedURLException) {
                        throw UpdateException(R.string.upd_err_redirect_bad)
                    }
                    return@repeat
                }
                if (status == 200) {
                    val declared = conn.contentLengthLong
                    if (declared > maxBytes) throw UpdateException(R.string.upd_err_too_big)
                    val enc = conn.contentEncoding
                    if (enc != null && !enc.equals("identity", true)) throw UpdateException(R.string.upd_err_encoding)
                    conn.inputStream.use(consume)
                }
                val rateLimited = conn.getHeaderField("Retry-After") != null || conn.getHeaderField("X-RateLimit-Remaining") == "0"
                return Head(status, conn.getHeaderField("ETag")?.takeIf { it.length <= 256 && it.all { c -> c in ' '..'~' } }, rateLimited)
            } finally {
                conn.disconnect()
            }
        }
        throw UpdateException(R.string.upd_err_redirects)
    }

    private fun readBounded(input: InputStream, max: Long, sink: (ByteArray, Int, Int) -> Unit) {
        val buf = ByteArray(32 * 1024)
        var total = 0L
        while (true) {
            val n = input.read(buf)
            if (n < 0) return
            total += n
            if (total > max) throw UpdateException(R.string.upd_err_too_big)
            sink(buf, 0, n)
        }
    }

    companion object {
        const val MAX_API_BYTES = 1024 * 1024
        const val MAX_SUMS_BYTES = 64 * 1024
        const val MAX_REDIRECTS = 5
        /** Set at build time with `-Pgb.releaseRepo=owner/name` (default `fspecii/geekbridge`). */
        val DEFAULT_REPO: String = BuildConfig.RELEASE_REPO
        const val UPDATE_FILE_NAME = "geekbridge-update.apk"

        /**
         * Debug builds only: the "updates" preference `debug_base_url` (an `http://127.0.0.1:PORT` mock release
         * server, set with `adb shell run-as`) replaces api.github.com so the update flow can be tested offline.
         * Release builds ignore it, and [allowLoopback] still refuses any non-loopback http URL.
         */
        fun channelOf(context: android.content.Context): UpdateChannel {
            val prefs = context.getSharedPreferences("updates", android.content.Context.MODE_PRIVATE)
            return UpdateChannel.parse(prefs.getString(KEY_CHANNEL, null)) ?: UpdateChannel.default(BuildConfig.DEBUG)
        }

        const val KEY_CHANNEL = "channel"

        fun forBuild(context: android.content.Context): UpdateChecker {
            val prefs = context.getSharedPreferences("updates", android.content.Context.MODE_PRIVATE)
            val channel = channelOf(context)
            val base = if (BuildConfig.DEBUG) prefs.getString("debug_base_url", null) else null
            // Debug builds only, like debug_base_url: `debug_repo` = owner/name of a (public) releases repo.
            val repo = (if (BuildConfig.DEBUG) prefs.getString("debug_repo", null) else null)
                ?.takeIf { UpdateModel.validRepo(it) } ?: DEFAULT_REPO
            return if (base != null && base.startsWith("http://127.0.0.1")) {
                UpdateChecker(BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE, baseUrl = base, repo = repo, allowLoopback = true, channel = channel)
            } else {
                UpdateChecker(BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE, repo = repo, channel = channel)
            }
        }
    }
}
