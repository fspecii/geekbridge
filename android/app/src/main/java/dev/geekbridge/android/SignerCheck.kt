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
        class Unreadable(val what: String) : Verdict()
    }

    interface Source {
        fun installed(): Signers?
        fun archive(path: String): Signers?
    }

    fun compare(installed: Signers?, offered: Signers?): Verdict {
        if (installed == null || installed.current.isEmpty()) return Verdict.Unreadable("the installed app")
        if (offered == null || offered.current.isEmpty()) return Verdict.Unreadable("the downloaded update")
        if (installed.current == offered.current) return Verdict.Match
        // Key rotation: the update's lineage proves the installed certificate is an ancestor of its signer.
        if (offered.history.containsAll(installed.current)) return Verdict.Match
        return Verdict.Mismatch(installed.current, offered.current)
    }

    fun check(source: Source, apkPath: String): Verdict = compare(source.installed(), source.archive(apkPath))

    /** User-facing text for a blocking verdict; null for [Verdict.Match]. [debugBuild] is the running app's `BuildConfig.DEBUG`. */
    fun message(v: Verdict, debugBuild: Boolean): String? = when (v) {
        Verdict.Match -> null
        is Verdict.Unreadable ->
            "Couldn't read the signing certificate of ${v.what}, so the update was not offered. Download the APK from the release page instead."
        is Verdict.Mismatch ->
            if (debugBuild) {
                "This is a developer (debug-signed) build. The release is signed with the release key, which Android won't accept over it. " +
                    "To switch, uninstall this build once and install the release APK; app data is lost."
            } else {
                "The update is signed with a different key (${short(v.offered)}) than the installed app (${short(v.installed)}). " +
                    "Android would refuse it, so it was not offered. Uninstall and reinstall the release APK to switch keys; app data is lost."
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
