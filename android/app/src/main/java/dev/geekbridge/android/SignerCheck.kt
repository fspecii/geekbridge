// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.pm.PackageManager
import android.content.pm.PackageInfo
import android.os.Build
import java.security.MessageDigest

/**
 * Android only installs an update signed by the same key as the installed app (or by a key whose v3 lineage
 * contains the installed key). Handing over an APK that fails that rule just ends in a cryptic system error,
 * so the updater compares the signer certificates itself and never shows "Install" for a mismatch.
 * Certificates are compared by SHA-256 of their DER bytes.
 */
object SignerCheck {
    /** [current]: certificates the APK is signed with now; [history]: v3 lineage (past certificates, plus current). */
    class Signers(val current: Set<String>, val history: Set<String> = current)

    sealed class Verdict {
        object Match : Verdict()
        class Mismatch(val installed: Set<String>, val offered: Set<String>) : Verdict()
        class Unreadable(val installedApp: Boolean) : Verdict()
        /** Signed by the right key but not this app (another package name), or not newer than the installed version. */
        class WrongApp(val packageName: String) : Verdict()
        class NotNewer(val offered: Long, val installed: Long) : Verdict()
    }

    /** Who a package file says it is. */
    class Identity(val packageName: String, val versionCode: Long)

    interface Source {
        fun installed(): Signers?
        fun archive(path: String): Signers?
        /** The package file's own package name and version code; `null` when unknown (the system still enforces both). */
        fun installedIdentity(): Identity? = null
        fun archiveIdentity(path: String): Identity? = null
    }

    fun compare(installed: Signers?, offered: Signers?): Verdict {
        if (installed == null || installed.current.isEmpty()) return Verdict.Unreadable(installedApp = true)
        if (offered == null || offered.current.isEmpty()) return Verdict.Unreadable(installedApp = false)
        if (installed.current == offered.current) return Verdict.Match
        // Key rotation: the update's lineage proves the installed certificate is an ancestor of its signer.
        if (offered.history.containsAll(installed.current)) return Verdict.Match
        return Verdict.Mismatch(installed.current, offered.current)
    }

    fun check(source: Source, apkPath: String): Verdict {
        val signer = compare(source.installed(), source.archive(apkPath))
        if (signer !is Verdict.Match) return signer
        // The manifest's and the release's words are not the file's: it must be this app and a newer version. Android
        // refuses anything else, but only after the user has been walked to the installer; say it here instead.
        return identity(source.installedIdentity(), source.archiveIdentity(apkPath))
    }

    fun identity(installed: Identity?, offered: Identity?): Verdict {
        if (installed == null || offered == null) return Verdict.Match
        if (offered.packageName != installed.packageName) return Verdict.WrongApp(offered.packageName)
        if (offered.versionCode <= installed.versionCode) return Verdict.NotNewer(offered.versionCode, installed.versionCode)
        return Verdict.Match
    }

    /** User-facing text for a blocking verdict; null for [Verdict.Match]. [debugBuild] is the running app's `BuildConfig.DEBUG`. */
    fun message(s: Strings, v: Verdict, debugBuild: Boolean): String? = when (v) {
        Verdict.Match -> null
        is Verdict.Unreadable ->
            s.get(if (v.installedApp) R.string.signer_unreadable_installed else R.string.signer_unreadable_download)
        is Verdict.WrongApp -> s.get(R.string.signer_wrong_app, Bounds.name(v.packageName, 60))
        is Verdict.NotNewer -> s.get(R.string.signer_not_newer, v.offered, v.installed)
        is Verdict.Mismatch ->
            if (debugBuild) {
                s.get(R.string.signer_debug_build)
            } else {
                s.get(R.string.signer_mismatch, short(v.offered), short(v.installed))
            }
    }

    private fun short(s: Set<String>) = s.first().take(12)

    fun sha256Hex(der: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(der).joinToString("") { "%02x".format(it) }

    /** Real implementation over [PackageManager] (`GET_SIGNING_CERTIFICATES` on API 28+, `GET_SIGNATURES` before). */
    class AndroidSource(private val pm: PackageManager, private val packageName: String) : Source {
        override fun installed(): Signers? = try {
            @Suppress("DEPRECATION")
            signersOf(pm.getPackageInfo(packageName, flags()))
        } catch (_: PackageManager.NameNotFoundException) {
            null
        }

        override fun archive(path: String): Signers? = try {
            @Suppress("DEPRECATION")
            pm.getPackageArchiveInfo(path, flags())?.let(::signersOf)
        } catch (_: RuntimeException) {
            null
        }

        override fun installedIdentity(): Identity? = try {
            @Suppress("DEPRECATION")
            pm.getPackageInfo(packageName, 0).let { Identity(it.packageName, codeOf(it)) }
        } catch (_: PackageManager.NameNotFoundException) {
            null
        }

        override fun archiveIdentity(path: String): Identity? = try {
            @Suppress("DEPRECATION")
            pm.getPackageArchiveInfo(path, 0)?.let { Identity(it.packageName, codeOf(it)) }
        } catch (_: RuntimeException) {
            null
        }

        @Suppress("DEPRECATION")
        private fun codeOf(i: PackageInfo): Long =
            if (Build.VERSION.SDK_INT >= 28) i.longVersionCode else i.versionCode.toLong()

        @Suppress("DEPRECATION")
        private fun flags(): Int =
            if (Build.VERSION.SDK_INT >= 28) PackageManager.GET_SIGNING_CERTIFICATES else PackageManager.GET_SIGNATURES

        @Suppress("DEPRECATION")
        private fun signersOf(info: PackageInfo): Signers? {
            if (Build.VERSION.SDK_INT >= 28) {
                val si = info.signingInfo ?: return null
                val current = si.apkContentsSigners.map { sha256Hex(it.toByteArray()) }.toSet()
                val history = if (si.hasPastSigningCertificates()) {
                    si.signingCertificateHistory.map { sha256Hex(it.toByteArray()) }.toSet() + current
                } else {
                    current
                }
                return Signers(current, history)
            }
            val current = info.signatures?.map { sha256Hex(it.toByteArray()) }?.toSet() ?: return null
            return Signers(current)
        }
    }
}
