// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

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
import org.junit.Test

/** The Dev update channel against a mock rolling `dev` prerelease (plain HTTP on loopback). */
class UpdateDevChannelTest {
    private lateinit var listener: ServerSocket
    private val routes = ConcurrentHashMap<String, Pair<Int, ByteArray>>()
    private val hits = ConcurrentHashMap<String, Int>()
    private val inm = ConcurrentHashMap<String, String>()
    private val base get() = "http://127.0.0.1:${listener.localPort}"
    private val devPath = "/repos/fspecii/geekbridge/releases/tags/dev"
    private val commit = "89abcdef0123456789abcdef0123456789abcdef"

    @Before fun start() {
        listener = ServerSocket(0, 20, InetAddress.getLoopbackAddress())
        Thread {
            while (!listener.isClosed) {
                val conn = try { listener.accept() } catch (_: Exception) { return@Thread }
                Thread { handle(conn) }.apply { isDaemon = true }.start()
            }
        }.apply { isDaemon = true }.start()
    }

    @After fun stop() = listener.close()

    private fun handle(conn: Socket) {
        conn.use {
        it.soTimeout = 5000
        val input = it.getInputStream().bufferedReader(Charsets.ISO_8859_1)
        val path = (input.readLine() ?: return).split(' ')[1]
        while (true) {
            val line = input.readLine() ?: return
            if (line.isEmpty()) break
            if (line.lowercase().startsWith("if-none-match:")) inm[path] = line.substringAfter(':').trim()
        }
        hits.merge(path, 1, Int::plus)
        val (status, body) = routes[path] ?: (404 to ByteArray(0))
        val etag = if (status == 200 && path == devPath) "ETag: \"d1\"\r\n" else ""
        it.getOutputStream().apply {
            write("HTTP/1.1 $status X\r\n${etag}Content-Length: ${body.size}\r\nConnection: close\r\n\r\n".toByteArray())
            write(body)
            flush()
        }
        }
    }

    private fun sha(b: ByteArray) = MessageDigest.getInstance("SHA-256").digest(b).joinToString("") { "%02x".format(it) }

    private fun manifest(code: Int) =
        """{"commit":"$commit","versionCode":$code,"builtAt":"2026-11-04T10:00:00Z","notes":"Fixes the call popup"}"""

    private fun publish(apk: ByteArray, code: Int = 20200, sums: Boolean = true, prerelease: Boolean = true) {
        val assets = StringBuilder(
            """{"name":"geekbridge-debug.apk","size":${apk.size},"browser_download_url":"$base/dl/geekbridge-debug.apk"},""" +
                """{"name":"geekbridge-linux-x86_64.tar.gz","size":5,"browser_download_url":"$base/dl/linux.tgz"},""" +
                """{"name":"dev-manifest.json","size":150,"browser_download_url":"$base/dl/dev-manifest.json"}""",
        )
        if (sums) assets.append(""",{"name":"SHA256SUMS","size":100,"browser_download_url":"$base/dl/SHA256SUMS"}""")
        routes[devPath] = 200 to
            """{"tag_name":"dev","prerelease":$prerelease,"html_url":"$base/rel","body":"Rolling","assets":[$assets]}""".toByteArray()
        routes["/dl/dev-manifest.json"] = 200 to manifest(code).toByteArray()
        routes["/dl/geekbridge-debug.apk"] = 200 to apk
        if (sums) routes["/dl/SHA256SUMS"] = 200 to (sha(apk) + "  geekbridge-debug.apk\n").toByteArray()
    }

    private fun checker(code: Int) = UpdateChecker(
        "0.1.0", code, baseUrl = base, allowLoopback = true, readTimeoutMs = 3_000, channel = UpdateChannel.Dev,
    )

    @Test fun newerDevBuildIsOfferedDownloadedAndVerified() {
        val apk = ByteArray(40_000) { (it % 241).toByte() }
        publish(apk)
        var cached: UpdateChecker.Cache? = null
        val result = checker(20161).check(null) { cached = it }
        val offer = (result as UpdateCheckResult.Available).offer
        assertTrue(offer.dev)
        assertEquals("dev-89abcdef0123", offer.version)
        assertEquals("geekbridge-debug.apk", offer.apkName)
        assertEquals(sha(apk), offer.sha256)
        assertTrue(offer.notes.contains("Fixes the call popup"))
        assertTrue(offer.notes.contains("2026-11-04T10:00:00Z"))
        assertEquals("\"d1\"", cached?.etag)
        assertNotNull(cached?.extra)
        assertNull("the stable endpoint is never asked", hits["/repos/fspecii/geekbridge/releases/latest"])
        val dir = java.io.File.createTempFile("upd", "").let { f -> f.delete(); f.apply { mkdirs() } }
        val file = checker(20161).download(offer, dir)
        assertTrue(file.readBytes().contentEquals(apk))
        dir.deleteRecursively()
    }

    @Test fun sameOrOlderDevBuildIsUpToDate() {
        publish(ByteArray(10))
        assertTrue(checker(20200).check(null) is UpdateCheckResult.UpToDate)
        assertTrue(checker(20201).check(null) is UpdateCheckResult.UpToDate)
        // The old debug build (10099) is older than every dev build.
        assertTrue(checker(10099).check(null) is UpdateCheckResult.Available)
    }

    @Test fun etagRevalidationReusesTheManifest() {
        publish(ByteArray(10))
        var cached: UpdateChecker.Cache? = null
        checker(20100).check(null) { cached = it }
        assertEquals(1, hits["/dl/dev-manifest.json"])
        routes[devPath] = 304 to ByteArray(0)
        val r = checker(20100).check(cached)
        assertTrue(r is UpdateCheckResult.Available)
        assertEquals("\"d1\"", inm[devPath])
        assertEquals("manifest not fetched again on 304", 1, hits["/dl/dev-manifest.json"])
    }

    @Test fun missingChecksumMeansNoDownload() {
        publish(ByteArray(10), sums = false)
        val offer = (checker(20100).check(null) as UpdateCheckResult.Available).offer
        assertNull(offer.sha256)
    }

    @Test fun malformedManifestIsAFailureNotAnOffer() {
        publish(ByteArray(10))
        routes["/dl/dev-manifest.json"] = 200 to """{"commit":"nope","versionCode":20300}""".toByteArray()
        assertTrue(checker(20100).check(null) is UpdateCheckResult.Failed)
        routes["/dl/dev-manifest.json"] = 200 to """{"commit":"$commit","versionCode":5}""".toByteArray()
        assertTrue(checker(20100).check(null) is UpdateCheckResult.Failed)
    }

    @Test fun noDevReleaseIsNotAnError() {
        routes["/repos/fspecii/geekbridge/releases?per_page=30"] = 200 to "[]".toByteArray()
        assertTrue(checker(20100).check(null) is UpdateCheckResult.UpToDate)
    }

    @Test fun aDifferentTagOrDraftIsIgnored() {
        publish(ByteArray(10))
        routes[devPath] = 200 to """{"tag_name":"v9.9.9","html_url":"$base/rel","assets":[]}""".toByteArray()
        assertTrue(checker(20100).check(null) is UpdateCheckResult.UpToDate)
        routes[devPath] = 200 to """{"tag_name":"dev","draft":true,"html_url":"$base/rel","assets":[]}""".toByteArray()
        assertTrue(checker(20100).check(null) is UpdateCheckResult.UpToDate)
    }

    @Test fun offlineIsAnErrorNotUpToDate() {
        val r = UpdateChecker("0.1.0", 20100, baseUrl = "http://127.0.0.1:1", allowLoopback = true, connectTimeoutMs = 500, channel = UpdateChannel.Dev).check(null)
        assertTrue(r is UpdateCheckResult.Failed)
    }

    @Test fun channelDefaultsAndParsing() {
        assertEquals(UpdateChannel.Dev, UpdateChannel.default(debugBuild = true))
        assertEquals(UpdateChannel.Stable, UpdateChannel.default(debugBuild = false))
        assertEquals(UpdateChannel.Dev, UpdateChannel.parse("dev"))
        assertNull(UpdateChannel.parse("nightly"))
        assertFalse(UpdateModel.DEV_CODE_BASE <= 10099)
    }
}
