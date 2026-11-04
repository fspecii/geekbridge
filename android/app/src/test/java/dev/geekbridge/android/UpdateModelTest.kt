// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class UpdateModelTest {
    private fun v(s: String) = AppVersion.parse(s) ?: error("$s should parse")

    @Test
    fun parsesTagsAndRejectsMalformed() {
        assertEquals("1.2.3", v("v1.2.3").toString())
        assertEquals("1.2.3", v("1.2.3+build.5").toString())
        assertEquals("1.0.0-rc.1", v("1.0.0-rc.1").toString())
        for (bad in listOf("", "v", "1", "1.2", "1.2.3.4", "01.2.3", "1.2.x", "1.2.3-", "1.2.3-a..b", "1.2.3+", "latest", "1.2.3-01", "9".repeat(30) + ".0.0")) {
            assertNull("'$bad' must not parse", AppVersion.parse(bad))
        }
        assertNull(AppVersion.parse(null))
    }

    @Test
    fun ordersBySemverPrecedence() {
        val order = listOf("0.9.9", "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1", "1.1.0", "2.0.0", "10.0.0")
        for ((a, b) in order.zipWithNext()) assertTrue("$a < $b", v(a) < v(b))
        assertEquals(v("1.0.0+a"), v("v1.0.0+b"))
        assertTrue(v("0.10.0") > v("0.9.0"))
    }

    @Test
    fun versionCodeFormula() {
        val expected = listOf(
            "0.1.0" to 10_099, "0.1.1" to 10_199, "0.2.0-rc.1" to 20_051, "0.2.0" to 20_099,
            "1.0.0-alpha.1" to 1_000_001, "1.0.0-beta.2" to 1_000_027, "1.0.0-rc.1" to 1_000_051, "1.0.0" to 1_000_099,
        )
        for ((v, code) in expected) assertEquals(v, code, v(v).toVersionCode())
        // Strictly increasing in semver order, so rc < final and Android always accepts the upgrade.
        val order = listOf("0.1.0", "0.1.1", "0.2.0-rc.1", "0.2.0-rc.2", "0.2.0", "1.0.0-alpha.1", "1.0.0-beta.1", "1.0.0-rc.1", "1.0.0", "1.0.1", "1.1.0")
        for ((a, b) in order.zipWithNext()) assertTrue("$a < $b", v(a).toVersionCode()!! < v(b).toVersionCode()!!)
        assertNull(v("1.100.0").toVersionCode())
        assertNull(v("215.0.0").toVersionCode())
        assertTrue(v("214.99.99").toVersionCode()!! > 0)
    }

    @Test
    fun buildVersionComesFromCargoWorkspace() {
        val cargo = File("../../Cargo.toml").readText()
        val section = cargo.substringAfter("[workspace.package]")
        val version = Regex("""(?m)^version\s*=\s*"([^"]+)"""").find(section)!!.groupValues[1]
        assertEquals(version, BuildConfig.VERSION_NAME)
        if (BuildConfig.DEBUG) {
            // Debug builds are dev builds: DEV_CODE_BASE + commit count (a build without git has count 0).
            // (-Pgb.formulaVersionCode=1 keeps the formula code, used for debug-signed prerelease APKs and e2e rehearsals.)
            assertTrue(
                "debug code ${BuildConfig.VERSION_CODE}",
                BuildConfig.VERSION_CODE >= UpdateModel.DEV_CODE_BASE || BuildConfig.VERSION_CODE == v(version).toVersionCode(),
            )
        } else {
            assertEquals(v(version).toVersionCode(), BuildConfig.VERSION_CODE)
        }
    }

    @Test
    fun autoCheckIsDueOnlyAfterTwelveHours() {
        val h = 3_600_000L
        assertTrue(UpdateModel.autoCheckDue(0, 5 * h))
        assertFalse(UpdateModel.autoCheckDue(10 * h, 10 * h + 11 * h))
        assertTrue(UpdateModel.autoCheckDue(10 * h, 10 * h + 12 * h))
        assertTrue("clock moved back", UpdateModel.autoCheckDue(10 * h, 9 * h))
    }

    @Test
    fun urlPolicy() {
        assertTrue(UpdateModel.urlAllowed("https://github.com/fspecii/geekbridge/releases/download/v1/a.apk"))
        assertTrue(UpdateModel.urlAllowed("https://objects.githubusercontent.com/x"))
        assertTrue(UpdateModel.urlAllowed("https://release-assets.githubusercontent.com/x"))
        assertTrue(UpdateModel.urlAllowed("https://github.com:443/x"))
        assertFalse(UpdateModel.urlAllowed("https://github.com:8443/x"))
        assertFalse(UpdateModel.urlAllowed("https://raw.githubusercontent.com/x"))
        assertFalse(UpdateModel.urlAllowed("https://gist.githubusercontent.com/x"))
        assertFalse(UpdateModel.urlAllowed("http://github.com/x"))
        assertFalse(UpdateModel.urlAllowed("https://evilgithub.com/x"))
        assertFalse(UpdateModel.urlAllowed("https://github.com.evil.example/x"))
        assertFalse(UpdateModel.urlAllowed("https://user@github.com/x"))
        assertFalse(UpdateModel.urlAllowed("http://127.0.0.1:8080/x"))
        assertTrue(UpdateModel.urlAllowed("http://127.0.0.1:8080/x", allowLoopback = true))
        assertFalse(UpdateModel.urlAllowed("not a url"))
    }

    @Test
    fun checksumLookup() {
        val h1 = "a".repeat(64)
        val h2 = "B".repeat(64)
        val listing = "$h1  one.apk\n$h2 *two.apk\nnot a line\n"
        assertEquals(h1, UpdateModel.findChecksum(listing, "one.apk"))
        assertEquals("b".repeat(64), UpdateModel.findChecksum(listing, "two.apk"))
        assertNull(UpdateModel.findChecksum(listing, "three.apk"))
        assertNull(UpdateModel.findChecksum("$h1  x.apk\n${"c".repeat(64)}  x.apk\n", "x.apk"))
        assertNull(UpdateModel.findChecksum("abc  x.apk", "x.apk"))
    }

    private fun json(tag: String, extraAssets: String = "", prerelease: Boolean = false) = """
        {"tag_name":"$tag","html_url":"https://github.com/fspecii/geekbridge/releases/tag/$tag","prerelease":$prerelease,
         "body":"Fixes\u0007 things","assets":[
           {"name":"geekbridge-linux-x86_64.tar.gz","browser_download_url":"https://github.com/x/l.tgz","size":10},
           {"name":"geekbridge-android.apk","browser_download_url":"https://github.com/fspecii/geekbridge/releases/download/$tag/geekbridge-android.apk","size":2048}
           $extraAssets]}
    """.trimIndent()

    private val sums = { name: String -> if (name == "geekbridge-android.apk") "f".repeat(64) else null }

    @Test
    fun offersNewerReleaseWithApkAndChecksum() {
        val r = UpdateModel.evaluate(UpdateModel.parseRelease(json("v0.2.0")), "0.1.0", 1_000, sums)
        val offer = (r as UpdateCheckResult.Available).offer
        assertEquals("0.2.0", offer.version)
        assertEquals("geekbridge-android.apk", offer.apkName)
        assertEquals(2048, offer.apkSize)
        assertEquals("f".repeat(64), offer.sha256)
        assertEquals("Fixes things", offer.notes)
    }

    @Test
    fun stableV010IsOfferedOverRc1AndLaterRcIsNot() {
        // rc.1 is versionCode 10051, final 0.1.0 is 10099.
        val offer = UpdateModel.evaluate(UpdateModel.parseRelease(json("v0.1.0")), "0.1.0-rc.1", 10_051, sums)
        assertEquals("0.1.0", (offer as UpdateCheckResult.Available).offer.version)
        val rc2 = UpdateModel.evaluate(UpdateModel.parseRelease(json("v0.1.0-rc.2")), "0.1.0-rc.1", 10_051, sums)
        assertTrue(rc2 is UpdateCheckResult.UpToDate)
    }

    @Test
    fun sameOlderPrereleaseAndSkewedVersionCodesAreNotOffered() {
        fun eval(tag: String, name: String = "0.1.0", code: Int = 1_000) =
            UpdateModel.evaluate(UpdateModel.parseRelease(json(tag)), name, code, sums)
        assertTrue(eval("v0.1.0") is UpdateCheckResult.UpToDate)
        assertTrue(eval("v0.0.9") is UpdateCheckResult.UpToDate)
        assertTrue(eval("v0.2.0-rc.1") is UpdateCheckResult.UpToDate)
        // Semver says newer but the installed build already has a higher version code: not offered.
        assertTrue(eval("v0.2.0", code = 9_999_999) is UpdateCheckResult.UpToDate)
        assertTrue(eval("banana") is UpdateCheckResult.Failed)
        assertTrue(eval("v0.2.0", name = "dev") is UpdateCheckResult.Failed)
    }

    @Test
    fun missingApkAndMalformedJsonFail() {
        val noApk = """{"tag_name":"v0.2.0","assets":[{"name":"x.zip","browser_download_url":"https://github.com/x","size":1}]}"""
        assertTrue(UpdateModel.evaluate(UpdateModel.parseRelease(noApk), "0.1.0", 1_000, sums) is UpdateCheckResult.Failed)
        for (bad in listOf("", "<html>", "{}", """{"tag_name":""}""")) {
            try {
                UpdateModel.parseRelease(bad)
                error("should have thrown for: $bad")
            } catch (_: IllegalArgumentException) {
            }
        }
    }

    @Test
    fun draftsAndPrereleasesAreUpToDateNotErrors() {
        for (flag in listOf("draft", "prerelease")) {
            val r = UpdateModel.parseRelease("""{"tag_name":"v9.0.0","$flag":true,"assets":[]}""")
            assertTrue(r.unpublished)
            assertTrue(UpdateModel.evaluate(r, "0.1.0", 10099, sums) is UpdateCheckResult.UpToDate)
        }
    }

    @Test
    fun untrustedApkUrlIsRefusedAndMissingChecksumIsReported() {
        val evil = """{"tag_name":"v0.2.0","assets":[{"name":"geekbridge-android.apk","browser_download_url":"https://evil.example/a.apk","size":10}]}"""
        assertTrue(UpdateModel.evaluate(UpdateModel.parseRelease(evil), "0.1.0", 1_000, sums) is UpdateCheckResult.Failed)
        val r = UpdateModel.evaluate(UpdateModel.parseRelease(json("v0.2.0")), "0.1.0", 1_000, { _ -> null })
        assertNull((r as UpdateCheckResult.Available).offer.sha256)
    }

    @Test
    fun apkPickerRejectsUnsafeOrOversizedNames() {
        assertNull(UpdateModel.pickApk(listOf("../a.apk" to 10L, "b.apk" to 0L, "c.apk" to UpdateModel.MAX_APK_BYTES + 1, "d.zip" to 5L)))
        assertEquals("geekbridge-android.apk", UpdateModel.pickApk(listOf("z.apk" to 5L, "geekbridge-android.apk" to 5L))!!.first)
        assertNotNull(UpdateModel.pickApk(listOf("a.apk" to 5L)))
    }

    /** Security review 7 (S7-9): release notes and the manifest's time come from the release page. */
    @Test fun notesAndBuiltAtLoseDirectionOverridesAndZeroWidthCharacters() {
        assertEquals("safe exe.txt x", UpdateModel.cleanNotes("safe \u202Eexe.txt\u200B \u2066x\u2069"))
        val m = UpdateModel.parseDevManifest("""{"commit":"0123456789abcdef","versionCode":20161,"builtAt":"2026\u202e-11-04\u200b"}""")
        assertEquals("2026-11-04", m.builtAt)
    }
}
