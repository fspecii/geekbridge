// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.security.MessageDigest
import java.util.concurrent.ConcurrentHashMap
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/** The checker against a local mock "GitHub" (plain HTTP on loopback; production only talks https). */
class UpdateCheckerTest {
    @get:Rule val tmp = TemporaryFolder()

    /** One request/response exchange; `Connection: close`, so it is also how truncation is simulated. */
    class Ex(val path: String, val reqHeaders: Map<String, String>, private val out: java.io.OutputStream) {
        val responseHeaders = ArrayList<Pair<String, String>>()
        val responseBody: java.io.OutputStream get() = out
        private var sent = false

        /** length: bytes promised in Content-Length (-1 = none, 0 body-less). Body is written by the caller afterwards. */
        fun sendResponseHeaders(status: Int, length: Long) {
            sent = true
            val sb = StringBuilder("HTTP/1.1 $status X\r\n")
            responseHeaders.forEach { (k, v) -> sb.append("$k: $v\r\n") }
            sb.append("Content-Length: ${if (length < 0) 0 else length}\r\nConnection: close\r\n\r\n")
            out.write(sb.toString().toByteArray())
        }
    }

    private lateinit var listener: ServerSocket
    private val routes = ConcurrentHashMap<String, (Ex) -> Unit>()
    private val headers = ConcurrentHashMap<String, String>()
    private val hits = ConcurrentHashMap<String, Int>()
    private val base get() = "http://127.0.0.1:${listener.localPort}"

    @Before fun start() {
        listener = ServerSocket(0, 20, InetAddress.getLoopbackAddress())
        Thread {
            while (!listener.isClosed) {
                val conn = try { listener.accept() } catch (_: Exception) { return@Thread }
                Thread { handle(conn) }.apply { isDaemon = true }.start()
            }
        }.apply { isDaemon = true }.start()
    }

    private fun handle(conn: Socket) {
        conn.use {
            it.soTimeout = 5000
            val input = it.getInputStream().bufferedReader(Charsets.ISO_8859_1)
            val first = input.readLine() ?: return
            val path = first.split(' ')[1]
            val req = HashMap<String, String>()
            while (true) {
                val line = input.readLine() ?: return
                if (line.isEmpty()) break
                line.split(':', limit = 2).let { kv -> if (kv.size == 2) req[kv[0].lowercase()] = kv[1].trim() }
            }
            hits.merge(path, 1, Int::plus)
            req["user-agent"]?.let { v -> headers["ua:$path"] = v }
            req["if-none-match"]?.let { v -> headers["inm:$path"] = v }
            req["authorization"]?.let { v -> headers["auth:$path"] = v }
            val out = java.io.BufferedOutputStream(it.getOutputStream())
            val ex = Ex(path, req, out)
            try {
                val handler = routes[path]
                if (handler == null) ex.sendResponseHeaders(404, -1) else handler(ex)
                out.flush()
            } catch (_: java.io.IOException) {
            }
        }
    }

    @After fun stop() = listener.close()

    @Test fun prereleaseIsIgnored_andDowngradeIsNotOffered() {
        fun release(tag: String, pre: Boolean) = """{"tag_name":"$tag","prerelease":$pre,"html_url":"$base/rel","body":"",
            "assets":[{"name":"geekbridge-android.apk","size":10,"browser_download_url":"$base/dl/geekbridge-android.apk"}]}""".toByteArray()
        reply(latest, release("v0.9.0-rc.1", true))
        assertTrue(checker().check(null) is UpdateCheckResult.UpToDate)
        reply(latest, release("v0.9.0", true)) // flagged prerelease on the server even with a clean tag
        assertTrue(checker().check(null) is UpdateCheckResult.UpToDate)
        reply(latest, release("v0.9.0", false).decodeToString().replace("\"prerelease\":false", "\"draft\":true").toByteArray())
        assertTrue(checker().check(null) is UpdateCheckResult.UpToDate)
        reply(latest, release("v0.0.5", false)) // older than the installed 0.1.0
        assertTrue(checker().check(null) is UpdateCheckResult.UpToDate)
        reply(latest, release("v0.2.0", false)) // newer semver, but not newer than the installed versionCode
        val high = UpdateChecker("0.1.0", Int.MAX_VALUE, baseUrl = base, allowLoopback = true, readTimeoutMs = 3_000)
        assertTrue(high.check(null) is UpdateCheckResult.UpToDate)
    }

    private fun reply(path: String, body: ByteArray, status: Int = 200, vararg extra: Pair<String, String>) {
        routes[path] = { ex ->
            extra.forEach { (k, v) -> ex.responseHeaders.add(k to v) }
            ex.sendResponseHeaders(status, if (body.isEmpty()) -1 else body.size.toLong())
            if (body.isNotEmpty()) ex.responseBody.write(body)
        }
    }

    private fun sha(b: ByteArray) = MessageDigest.getInstance("SHA-256").digest(b).joinToString("") { "%02x".format(it) }
    private val latest = "/repos/fspecii/geekbridge/releases/latest"
    private fun checker() = UpdateChecker("0.1.0", 1_000, baseUrl = base, allowLoopback = true, readTimeoutMs = 3_000)

    private fun publish(apk: ByteArray, sums: String? = sha(apk) + "  geekbridge-android.apk\n", size: Int = apk.size) {
        val assets = StringBuilder(
            """{"name":"geekbridge-android.apk","size":$size,"browser_download_url":"$base/dl/geekbridge-android.apk"}""",
        )
        if (sums != null) assets.append(""",{"name":"SHA256SUMS","size":100,"browser_download_url":"$base/dl/SHA256SUMS"}""")
        val json = """{"tag_name":"v0.2.0","html_url":"$base/rel","body":"notes","assets":[$assets]}"""
        reply(latest, json.toByteArray(), 200, "ETag" to "\"e1\"")
        reply("/dl/geekbridge-android.apk", apk)
        if (sums != null) reply("/dl/SHA256SUMS", sums.toByteArray())
    }

    @Test fun checksDownloadsAndVerifies() {
        val apk = ByteArray(100_000) { (it % 251).toByte() }
        publish(apk)
        var cached: UpdateChecker.Cache? = null
        val result = checker().check(null) { cached = it }
        val offer = (result as UpdateCheckResult.Available).offer
        assertEquals(sha(apk), offer.sha256)
        assertEquals("\"e1\"", cached?.etag)
        assertTrue(headers["ua:$latest"]!!.startsWith("geekbridge-android-updater/0.1.0"))

        val dir = File(tmp.root, "updates")
        var last = 0L
        val file = checker().download(offer, dir) { done, _ -> last = done }
        assertEquals(apk.size.toLong(), last)
        assertTrue(file.readBytes().contentEquals(apk))
    }

    @Test fun conditionalRequestReusesCachedBody() {
        publish(ByteArray(10))
        val first = checker()
        var cache: UpdateChecker.Cache? = null
        first.check(null) { cache = it }
        routes[latest] = { ex -> ex.sendResponseHeaders(304, -1) }
        val again = checker().check(cache)
        assertTrue(again is UpdateCheckResult.Available)
        assertEquals("\"e1\"", headers["inm:$latest"])
        // 304 without a cached copy is an error, not a crash.
        assertTrue(checker().check(null) is UpdateCheckResult.Failed)
    }

    @Test fun hashMismatchDeletesTheFile() {
        val apk = ByteArray(5000) { 1 }
        publish(apk, sums = sha(byteArrayOf(9)) + "  geekbridge-android.apk\n")
        val offer = (checker().check(null) as UpdateCheckResult.Available).offer
        val dir = File(tmp.root, "u")
        try {
            checker().download(offer, dir)
            error("must fail")
        } catch (e: UpdateChecker.UpdateException) {
            assertTrue(e.text.text(TestStrings).contains("SHA-256"))
        }
        assertEquals(0, dir.listFiles()!!.size)
    }

    @Test fun missingChecksumMeansNoDownload() {
        publish(ByteArray(10), sums = null)
        val offer = (checker().check(null) as UpdateCheckResult.Available).offer
        assertNull(offer.sha256)
        try {
            checker().download(offer, File(tmp.root, "u"))
            error("must fail")
        } catch (e: UpdateChecker.UpdateException) {
            assertEquals(0, hits["/dl/geekbridge-android.apk"] ?: 0)
        }
    }

    @Test fun oversizedAndTruncatedDownloadsFail() {
        val apk = ByteArray(4096) { 2 }
        publish(apk)
        val offer = (checker().check(null) as UpdateCheckResult.Available).offer
        val dir = File(tmp.root, "u")
        // Server sends more than declared.
        reply("/dl/geekbridge-android.apk", ByteArray(100_000))
        assertFails { checker().download(offer, dir) }
        // Server sends less.
        reply("/dl/geekbridge-android.apk", ByteArray(4000) { 2 })
        assertFails { checker().download(offer, dir) }
        assertEquals(0, dir.listFiles()!!.size)
    }

    @Test fun malformedAndOversizedApiResponsesFail() {
        for (body in listOf("<html>no</html>".toByteArray(), """{"tag_name":"latest"}""".toByteArray(), ByteArray(2 * 1024 * 1024) { '{'.code.toByte() })) {
            reply(latest, body)
            assertTrue(checker().check(null) is UpdateCheckResult.Failed)
        }
        reply(latest, ByteArray(0), 500)
        assertTrue(checker().check(null) is UpdateCheckResult.Failed)
        reply(latest, ByteArray(0), 403, "X-RateLimit-Remaining" to "0")
        assertTrue((checker().check(null) as UpdateCheckResult.Failed).reason.text(TestStrings).contains("rate"))
        reply(latest, ByteArray(0), 429)
        assertTrue((checker().check(null) as UpdateCheckResult.Failed).reason.text(TestStrings).contains("rate"))
    }

    @Test fun privateOrEmptyRepositoryIsReportedAsNoPublicRelease() {
        reply(latest, """{"message":"Not Found"}""".toByteArray(), 404)
        assertEquals(UpdateCheckResult.NoPublicRelease, checker().check(null))
        reply(latest, ByteArray(0), 403) // plain refusal, not a rate limit
        assertEquals(UpdateCheckResult.NoPublicRelease, checker().check(null))
        assertTrue(UpdateCheckResult.NoPublicRelease.MESSAGE.text(TestStrings).contains("private or has no releases yet"))
    }

    @Test fun anotherRepositoryCanBeQueried_andNoAuthorizationIsEverSent() {
        val apk = ByteArray(10)
        val path = "/repos/acme/geekbridge-releases/releases/latest"
        val json = """{"tag_name":"v0.2.0","html_url":"$base/rel","assets":[{"name":"geekbridge-android.apk","size":10,"browser_download_url":"$base/dl/geekbridge-android.apk"}]}"""
        reply(path, json.toByteArray())
        val c = UpdateChecker("0.1.0", 1_000, baseUrl = base, repo = "acme/geekbridge-releases", allowLoopback = true)
        assertTrue(c.check(null) is UpdateCheckResult.Available)
        assertNull(headers["auth:$path"])
        assertTrue(UpdateModel.validRepo("acme/geekbridge-releases"))
        for (bad in listOf("", "acme", "a/b/c", "../x", "a/..", "a b/c", null)) assertFalse("$bad", UpdateModel.validRepo(bad))
        assertEquals(10, apk.size)
    }

    @Test fun redirectsToUntrustedHostsAreRefused() {
        publish(ByteArray(10))
        val offer = (checker().check(null) as UpdateCheckResult.Available).offer
        routes["/dl/geekbridge-android.apk"] = { ex ->
            ex.responseHeaders.add("Location" to "https://evil.example/a.apk")
            ex.sendResponseHeaders(302, -1)
        }
        assertFails { checker().download(offer, File(tmp.root, "u")) }
        assertFalse(File(tmp.root, "u/${UpdateChecker.UPDATE_FILE_NAME}").exists())
        // Same-origin redirect works.
        val apk = ByteArray(10)
        routes["/real"] = { ex -> ex.sendResponseHeaders(200, apk.size.toLong()); ex.responseBody.write(apk) }
        routes["/dl/geekbridge-android.apk"] = { ex ->
            ex.responseHeaders.add("Location" to "/real")
            ex.sendResponseHeaders(302, -1)
        }
        assertNotNull(checker().download(offer, File(tmp.root, "u")))
    }

    // ---- realistic GitHub payloads: the verbatim v0.1.0-rc.1 release entry (src/test/resources)

    private val list = "/repos/fspecii/geekbridge/releases?per_page=30"

    private fun rc1() = org.json.JSONObject(javaClass.getResource("/github_release_v0.1.0-rc.1.json")!!.readText())

    private fun checkerFor(name: String, code: Int) =
        UpdateChecker(name, code, baseUrl = base, allowLoopback = true, readTimeoutMs = 3_000)

    @Test fun onlyAPrereleasePublished_isUpToDate_notNoPublicRelease() {
        // GitHub's /releases/latest never returns a pre-release: 404 while the list shows v0.1.0-rc.1.
        reply(latest, """{"message":"Not Found"}""".toByteArray(), 404)
        reply(list, "[${rc1()}]".toByteArray())
        for ((name, code) in listOf("0.1.0-rc.1" to 10051, "0.0.9" to 9, "0.1.0-beta.2" to 10027)) {
            val r = checkerFor(name, code).check(null)
            assertTrue("$name: $r", r is UpdateCheckResult.UpToDate)
        }
        reply(list, "[]".toByteArray())
        assertTrue(checkerFor("0.1.0-rc.1", 10051).check(null) is UpdateCheckResult.UpToDate)
        // Nothing readable at all: private or misspelt.
        routes.remove(list)
        assertEquals(UpdateCheckResult.NoPublicRelease, checkerFor("0.1.0-rc.1", 10051).check(null))
        // A rate-limited list is a rate limit, not 'no public release'.
        reply(list, ByteArray(0), 403, "Retry-After" to "60")
        val limited = checkerFor("0.1.0-rc.1", 10051).check(null)
        assertTrue((limited as UpdateCheckResult.Failed).reason.text(TestStrings).contains("rate"))
    }

    private fun stableLikeRealRc1(apk: ByteArray, apkDigest: String?): ByteArray {
        val rel = rc1()
        rel.put("tag_name", "v0.1.0")
        rel.put("prerelease", false)
        rel.put("html_url", "$base/rel")
        val assets = rel.getJSONArray("assets")
        for (i in 0 until assets.length()) {
            val a = assets.getJSONObject(i)
            val name = a.getString("name")
            a.put("browser_download_url", "$base/dl/$name")
            if (name == "geekbridge-android.apk") {
                a.put("size", apk.size)
                if (apkDigest != null) a.put("digest", "sha256:$apkDigest") else a.remove("digest")
            }
        }
        return rel.toString().toByteArray()
    }

    @Test fun realisticStableReleaseOffersThePhoneApkOverRc1_andChecksGithubsDigest() {
        val apk = ByteArray(3000) { 3 }
        reply("/dl/geekbridge-android.apk", apk)
        reply("/dl/SHA256SUMS", (sha(apk) + "  geekbridge-android.apk\n" + sha(byteArrayOf(1)) + "  geekbridge-android-arm64.apk\n").toByteArray())
        reply(latest, stableLikeRealRc1(apk, sha(apk)), 200, "ETag" to "\"s1\"")
        // The phone runs rc.1: versionCode 10051 < 10099 for the final 0.1.0.
        val offer = (checkerFor("0.1.0-rc.1", 10051).check(null) as UpdateCheckResult.Available).offer
        assertEquals("geekbridge-android.apk", offer.apkName) // not the arm64 split
        assertEquals("0.1.0", offer.version)
        assertEquals(sha(apk), offer.sha256)
        assertNotNull(checkerFor("0.1.0-rc.1", 10051).download(offer, File(tmp.root, "u")))
        // 0.1.0 itself is current.
        assertTrue(checkerFor("0.1.0", 10099).check(null) is UpdateCheckResult.UpToDate)
        // GitHub's own digest disagrees with SHA256SUMS: nothing is offered.
        reply(latest, stableLikeRealRc1(apk, sha(byteArrayOf(7))))
        val bad = checkerFor("0.1.0-rc.1", 10051).check(null)
        assertTrue((bad as UpdateCheckResult.Failed).reason.text(TestStrings).contains("disagree"))
        // No digest at all (older assets): SHA256SUMS alone decides.
        reply(latest, stableLikeRealRc1(apk, null))
        assertTrue(checkerFor("0.1.0-rc.1", 10051).check(null) is UpdateCheckResult.Available)
    }

    @Test fun anInterruptedDownloadLeavesNothingAndARetryStartsCleanly() {
        val apk = ByteArray(40_000) { (it % 13).toByte() }
        publish(apk)
        val offer = (checker().check(null) as UpdateCheckResult.Available).offer
        val dir = File(tmp.root, "u")
        // The connection dies half way (no Range resume: the file is small and a half file is never kept).
        routes["/dl/geekbridge-android.apk"] = { ex ->
            ex.sendResponseHeaders(200, apk.size.toLong())
            ex.responseBody.write(apk, 0, 10_000)
        }
        assertFails { checker().download(offer, dir) }
        assertEquals(0, dir.listFiles()!!.size)
        reply("/dl/geekbridge-android.apk", apk)
        val file = checker().download(offer, dir)
        assertTrue(file.readBytes().contentEquals(apk))
        // A stale file from an earlier attempt never survives a new download.
        File(dir, "leftover.part").writeBytes(ByteArray(5))
        checker().download(offer, dir)
        assertEquals(listOf(UpdateChecker.UPDATE_FILE_NAME), dir.list()!!.toList())
    }

    @Test fun offlineIsAnErrorNotUpToDate() {
        val r = UpdateChecker("0.1.0", 10099, baseUrl = "http://127.0.0.1:1", allowLoopback = true, connectTimeoutMs = 500).check(null)
        assertTrue((r as UpdateCheckResult.Failed).reason.text(TestStrings).contains("reach"))
    }

    private fun assertFails(block: () -> Unit) {
        try {
            block()
            error("expected UpdateException")
        } catch (_: UpdateChecker.UpdateException) {
        }
    }
}
