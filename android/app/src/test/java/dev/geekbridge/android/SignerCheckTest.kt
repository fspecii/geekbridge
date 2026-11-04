// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SignerCheckTest {
    private fun s(vararg c: String) = SignerCheck.Signers(c.toSet())
    private class Fake(val installed: SignerCheck.Signers?, val archive: SignerCheck.Signers?) : SignerCheck.Source {
        var asked: String? = null
        override fun installed() = installed
        override fun archive(path: String): SignerCheck.Signers? { asked = path; return archive }
    }

    /** Security review 7 (S7-8): the file must be this app and newer, whatever the release page and manifest say. */
    @Test fun aFileForAnotherAppOrNotNewerIsBlockedBeforeTheInstaller() {
        val me = SignerCheck.Identity("dev.geekbridge.android", 20200)
        assertTrue(SignerCheck.identity(me, SignerCheck.Identity("dev.geekbridge.android", 20201)) is SignerCheck.Verdict.Match)
        assertTrue(SignerCheck.identity(me, SignerCheck.Identity("com.evil.app", 99999)) is SignerCheck.Verdict.WrongApp)
        assertTrue(SignerCheck.identity(me, SignerCheck.Identity("dev.geekbridge.android", 20200)) is SignerCheck.Verdict.NotNewer)
        assertTrue(SignerCheck.identity(me, SignerCheck.Identity("dev.geekbridge.android", 100)) is SignerCheck.Verdict.NotNewer)
        // Unknown identity is left to the system, which enforces both.
        assertTrue(SignerCheck.identity(me, null) is SignerCheck.Verdict.Match)
        // check() applies it after the signer comparison.
        val src = object : SignerCheck.Source {
            override fun installed() = s("aa")
            override fun archive(path: String) = s("aa")
            override fun installedIdentity() = me
            override fun archiveIdentity(path: String) = SignerCheck.Identity("com.evil.app", 99999)
        }
        assertTrue(SignerCheck.check(src, "x.apk") is SignerCheck.Verdict.WrongApp)
    }

    @Test fun sameCertificateMatches() {
        assertTrue(SignerCheck.compare(s("aa"), s("aa")) is SignerCheck.Verdict.Match)
        assertTrue(SignerCheck.compare(s("aa", "bb"), s("bb", "aa")) is SignerCheck.Verdict.Match)
    }

    @Test fun differentCertificateIsBlocked() {
        val v = SignerCheck.compare(s("aa"), s("bb"))
        assertTrue(v is SignerCheck.Verdict.Mismatch)
        assertTrue(SignerCheck.compare(s("aa", "cc"), s("aa")) is SignerCheck.Verdict.Mismatch) // fewer signers than installed
    }

    @Test fun rotatedKeyWithLineageContainingTheInstalledKeyMatches() {
        val offered = SignerCheck.Signers(current = setOf("new"), history = setOf("old", "new"))
        assertTrue(SignerCheck.compare(s("old"), offered) is SignerCheck.Verdict.Match)
        // Lineage that does not contain the installed key does not help.
        assertTrue(SignerCheck.compare(s("other"), offered) is SignerCheck.Verdict.Mismatch)
    }

    @Test fun unreadableSignersFailClosed() {
        assertTrue(SignerCheck.compare(null, s("aa")) is SignerCheck.Verdict.Unreadable)
        assertTrue(SignerCheck.compare(s("aa"), null) is SignerCheck.Verdict.Unreadable)
        assertTrue(SignerCheck.compare(s("aa"), SignerCheck.Signers(emptySet())) is SignerCheck.Verdict.Unreadable)
    }

    @Test fun checkAsksTheSourceAboutTheDownloadedFile() {
        val fake = Fake(s("aa"), s("aa"))
        assertTrue(SignerCheck.check(fake, "/x/update.apk") is SignerCheck.Verdict.Match)
        assertEquals("/x/update.apk", fake.asked)
    }

    @Test fun messagesExplainTheMismatch() {
        val mismatch = SignerCheck.compare(s("a".repeat(64)), s("b".repeat(64)))
        assertNull(SignerCheck.message(TestStrings, SignerCheck.Verdict.Match, false))
        val debug = SignerCheck.message(TestStrings, mismatch, debugBuild = true)!!
        assertTrue(debug, debug.contains("debug-signed") && debug.contains("uninstall"))
        val release = SignerCheck.message(TestStrings, mismatch, debugBuild = false)!!
        assertTrue(release, release.contains("aaaaaaaaaaaa") && release.contains("bbbbbbbbbbbb"))
        assertNotNull(SignerCheck.message(TestStrings, SignerCheck.Verdict.Unreadable(installedApp = false), false))
    }

    @Test fun certificateDigestIsSha256Hex() {
        assertEquals("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", SignerCheck.sha256Hex("abc".toByteArray()))
    }
}
