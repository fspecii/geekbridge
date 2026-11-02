// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test

/** A preferences file as SharedPreferences behaves: reading a key as the wrong type throws. */
private class FakePrefs(val m: HashMap<String, Any> = HashMap()) : KeyValues {
    override fun getString(key: String) = m[key] as? String
    override fun putString(key: String, value: String?) { if (value == null) m.remove(key) else m[key] = value }
    override fun getInt(key: String, default: Int): Int = if (m.containsKey(key)) m[key] as Int else default
    override fun putInt(key: String, value: Int) { m[key] = value }
}

class WhatsNewModelTest {
    private val repoRoot = File("../..").canonicalFile

    // ---- version-change detection

    @Test fun freshInstallRecordsWithoutShowing() {
        assertEquals(WhatsNewDecision.Record, WhatsNewModel.decide(null, 20099, hadPriorUse = false))
    }

    @Test fun updateFromABuildThatNeverRecordedAVersionShows() {
        assertEquals(WhatsNewDecision.Show, WhatsNewModel.decide(null, 20099, hadPriorUse = true))
    }

    @Test fun upgradeShowsAndSameVersionDoesNot() {
        assertEquals(WhatsNewDecision.Show, WhatsNewModel.decide(10099, 20099, true))
        assertEquals(WhatsNewDecision.Show, WhatsNewModel.decide(10050, 10099, true)) // rc -> final
        assertEquals(WhatsNewDecision.Same, WhatsNewModel.decide(20099, 20099, true))
    }

    @Test fun downgradeIsSilentAndRearmsTheNextUpgrade() {
        assertEquals(WhatsNewDecision.Record, WhatsNewModel.decide(20099, 10099, true))
        val store = WhatsNewStore(FakePrefs())
        store.write(20099)
        store.write(10099) // what Record does after the downgrade
        assertEquals(WhatsNewDecision.Show, WhatsNewModel.decide(store.read().lastSeen, 20099, true))
    }

    @Test fun corruptStateRecordsSilently() {
        for (bad in listOf<Any>("seven", 0, -3)) {
            val p = FakePrefs(hashMapOf(WhatsNewStore.KEY_LAST to bad))
            val s = WhatsNewStore(p).read()
            assertTrue("$bad", s.damaged || s.lastSeen == null)
            assertEquals(WhatsNewDecision.Record, WhatsNewModel.decide(s.lastSeen, 20099, true, s.damaged))
        }
        // repaired by the next write
        val p = FakePrefs(hashMapOf(WhatsNewStore.KEY_LAST to "seven"))
        WhatsNewStore(p).write(20099)
        assertEquals(20099, WhatsNewStore(p).read().lastSeen)
    }

    @Test fun storeRoundTripsAndStampsItsVersion() {
        val p = FakePrefs()
        val s = WhatsNewStore(p)
        assertNull(s.read().lastSeen)
        s.write(10099)
        assertEquals(10099, s.read().lastSeen)
        assertEquals(WhatsNewStore.STATE_VERSION, p.m[WhatsNewStore.KEY_STATE])
    }

    @Test fun stateFromANewerFormatIsNeverRewritten() {
        val p = FakePrefs(hashMapOf(WhatsNewStore.KEY_STATE to 9, WhatsNewStore.KEY_LAST to 99999))
        val s = WhatsNewStore(p)
        assertTrue(s.read().readOnly)
        assertEquals(WhatsNewDecision.Record, WhatsNewModel.decide(s.read().lastSeen, 20099, true, s.read().damaged))
        s.write(20099)
        assertEquals(99999, p.m[WhatsNewStore.KEY_LAST])
        assertEquals(9, p.m[WhatsNewStore.KEY_STATE])
    }

    // ---- parsing and sanitising

    private fun json(version: String, vararg lines: String) =
        """{"version":"$version","fallback":false,"highlights":[${lines.joinToString(",") { "\"" + it.replace("\\", "\\\\").replace("\"", "\\\"") + "\"" }}]}"""

    @Test fun parseKeepsAtMostSixCleanLines() {
        val n = WhatsNewModel.parse(TestStrings, json("0.2.0", *Array(10) { "line $it" }), "0.2.0")
        assertEquals("0.2.0", n.version)
        assertEquals((0 until 6).map { "line $it" }, n.highlights)
    }

    @Test fun missingMalformedOrStaleAssetFallsBack() {
        val fb = listOf("See the release notes")
        assertEquals(fb, WhatsNewModel.parse(TestStrings, null, "0.2.0").highlights)
        assertEquals(fb, WhatsNewModel.parse(TestStrings, "not json", "0.2.0").highlights)
        assertEquals(fb, WhatsNewModel.parse(TestStrings, "{}", "0.2.0").highlights)
        assertEquals(fb, WhatsNewModel.parse(TestStrings, json("0.2.0"), "0.2.0").highlights)
        assertEquals(fb, WhatsNewModel.parse(TestStrings, json("0.1.0", "old text"), "0.2.0").highlights) // asset for another version
        assertEquals(fb, WhatsNewModel.parse(TestStrings, "x".repeat(20_000), "0.2.0").highlights)
    }

    @Test fun sanitiseRemovesMarkdownMarkupAndControlCharacters() {
        assertEquals(
            "Bold code link img tag 31m ok",
            WhatsNewModel.sanitise("**Bold** `code` [link](https://x) ![img](y.png) <b>tag</b>\u0007\u001b[31m ok\t\n"),
        )
        assertEquals("head quote cell esc", WhatsNewModel.sanitise("# head > quote | cell \\ esc"))
        assertEquals("abc", WhatsNewModel.sanitise("a\u202eb\u200bc")) // bidi override, zero-width space
        assertEquals("", WhatsNewModel.sanitise("\u0000\u0001 \n"))
    }

    @Test fun sanitiseCapsVeryLongLines() {
        val cut = WhatsNewModel.sanitise("word ".repeat(200))
        assertTrue(cut.length <= WhatsNewModel.MAX_LINE_CHARS && cut.endsWith("\u2026"))
        assertTrue(WhatsNewModel.sanitise("x".repeat(100_000)).length <= WhatsNewModel.MAX_LINE_CHARS)
    }

    @Test fun releaseUrlUsesTheConfiguredRepoAndAValidVersion() {
        assertEquals("https://github.com/fspecii/geekbridge/releases/tag/v0.2.0", WhatsNewModel.releaseUrl("fspecii/geekbridge", "0.2.0"))
        assertEquals("https://github.com/acme/rel/releases/tag/v1.0.0-rc.1", WhatsNewModel.releaseUrl("acme/rel", "1.0.0-rc.1"))
        assertNull(WhatsNewModel.releaseUrl("not a repo", "0.2.0"))
        assertNull(WhatsNewModel.releaseUrl("a/b", "../../x"))
        assertTrue(UpdateModel.urlAllowed(WhatsNewModel.releaseUrl("fspecii/geekbridge", "0.2.0")!!))
    }

    // ---- the checked-in asset cannot drift from CHANGELOG.md

    @Test fun theBundledAssetDescribesThisVersion() {
        val cargo = File(repoRoot, "Cargo.toml").readText().substringAfter("[workspace.package]")
        val version = Regex("""(?m)^version\s*=\s*"([^"]+)"""").find(cargo)!!.groupValues[1]
        val asset = File("src/main/assets/${WhatsNewModel.ASSET}").readText()
        val n = WhatsNewModel.parse(TestStrings, asset, version)
        assertTrue(n.highlights.size in 1..WhatsNewModel.MAX_HIGHLIGHTS)
        assertFalse("asset is stale for $version: run scripts/gen-whats-new.sh", n.highlights == listOf("See the release notes") && !asset.contains("\"fallback\": true"))
        // every line already is what sanitise would produce
        for (l in n.highlights) assertEquals(l, WhatsNewModel.sanitise(l))
    }

    @Test fun theGeneratorSaysTheCheckedInFilesAreCurrent() {
        val script = File(repoRoot, "scripts/gen-whats-new.sh")
        assumeTrue("needs bash and the repository scripts", script.isFile && File("/bin/bash").exists())
        val p = ProcessBuilder("/bin/bash", script.path, "--check").directory(repoRoot).redirectErrorStream(true).start()
        val out = p.inputStream.bufferedReader().readText()
        assertEquals("generated what's-new files are stale: $out", 0, p.waitFor())
    }
}
