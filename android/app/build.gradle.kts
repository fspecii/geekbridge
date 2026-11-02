import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// Single version source: [workspace.package] version in the repository's Cargo.toml. The release workflow
// tags that exact version (vX.Y.Z[-pre]). versionCode must strictly increase for every release or Android
// refuses the update, so it is derived, never hand-edited (same formula as AppVersion.toVersionCode in
// UpdateModel.kt and scripts/version-code.sh; documented in docs/builds.md):
//   versionCode = (major*10_000 + minor*100 + patch) * 100 + slot
//   slot: 99 final; prerelease alpha.N 0..24, beta.N 25..49, rc.N 50..74 (N capped at 24), so rc < final.
val workspaceVersion: String = run {
    val cargo = rootProject.projectDir.resolve("../Cargo.toml")
    val text = cargo.readText()
    val section = text.substringAfter("[workspace.package]", "")
    Regex("""(?m)^version\s*=\s*"([^"]+)"""").find(section)?.groupValues?.get(1)
        ?: throw GradleException("No [workspace.package] version in ${cargo.canonicalPath}")
}
val workspaceVersionCode: Int = run {
    val m = Regex("""^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$""").matchEntire(workspaceVersion)
        ?: throw GradleException("Workspace version '$workspaceVersion' is not major.minor.patch[-pre]")
    val (major, minor, patch, pre) = m.destructured
    require(major.toInt() <= 214 && minor.toInt() <= 99 && patch.toInt() <= 99) { "version component out of range" }
    val ids = pre.split('.')
    val slot = if (pre.isEmpty()) 99 else {
        val base = when (ids[0].lowercase()) { "beta" -> 25; "rc" -> 50; else -> 0 }
        base + (ids.getOrNull(1)?.toLongOrNull() ?: 0L).coerceIn(0L, 24L).toInt()
    }
    (major.toInt() * 10_000 + minor.toInt() * 100 + patch.toInt()) * 100 + slot
}

val packagedAbis: List<String> =
    (findProperty("gb.abis") as String?)?.split(",")?.map { it.trim() }?.filter { it.isNotEmpty() }
        ?: listOf("arm64-v8a", "x86_64")

android {
    namespace = "dev.geekbridge.android"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.geekbridge.android"
        minSdk = 26
        targetSdk = 36
        versionCode = workspaceVersionCode
        versionName = workspaceVersion
        // Contract: phone and emulator only.
        // -Pgb.abis=arm64-v8a builds a phone-only APK (about half the size); default is phone + emulator.
        ndk { abiFilters += packagedAbis }
        // Where the updater looks for releases (owner/name). -Pgb.releaseRepo=acme/geekbridge-releases points
        // a build at a separate public releases-only repo; validated again at runtime (UpdateModel.validRepo).
        val releaseRepo = (findProperty("gb.releaseRepo") as String?)?.trim()?.takeIf { it.isNotEmpty() } ?: "fspecii/geekbridge"
        require(Regex("[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+").matches(releaseRepo)) { "gb.releaseRepo must be owner/name" }
        buildConfigField("String", "RELEASE_REPO", "\"$releaseRepo\"")
    }

    buildTypes {
        release {
            // R8 + resource shrinking (docs/android-perf.md). Keep rules: proguard-rules.pro.
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures { buildConfig = true }

    lint {
        abortOnError = true
        warningsAsErrors = false
    }
}

kotlin {
    compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    // android.jar's org.json is a stub that throws in JVM unit tests; use the real one there.
    testImplementation("org.json:json:20250517")
}

// ---------------------------------------------------------------------------------------------
// Native core guard. The APK must never ship without the Rust core: the app would then run but
// every validation would report "unavailable". Fail packaging instead, with an actionable message.
// Unit tests and lint do not need the libraries and do not depend on this task.
// ---------------------------------------------------------------------------------------------
val nativeAbis = mapOf("arm64-v8a" to 183, "x86_64" to 62).filterKeys { it in packagedAbis } // ELF e_machine values
val nativeLibName = "libgeekbridge_android.so"
val jniSymbol = "Java_dev_geekbridge_android_NativeBridge_nativeRequest"
val jniLibsDir = layout.projectDirectory.dir("src/main/jniLibs")

val verifyNativeLibs by tasks.registering {
    group = "verification"
    description = "Fails if libgeekbridge_android.so is missing, not ELF for its ABI, or lacks the JNI symbol."
    val libs = nativeAbis.keys.associateWith { jniLibsDir.file("$it/$nativeLibName").asFile }
    inputs.files(libs.values).optional()
    doLast {
        val problems = mutableListOf<String>()
        for ((abi, machine) in nativeAbis) {
            val file = libs.getValue(abi)
            if (!file.isFile || file.length() < 64) {
                problems += "missing: ${file.relativeTo(projectDir)}"
                continue
            }
            val bytes = file.readBytes()
            val isElf = bytes[0] == 0x7f.toByte() && bytes[1] == 'E'.code.toByte() &&
                bytes[2] == 'L'.code.toByte() && bytes[3] == 'F'.code.toByte()
            val gotMachine = (bytes[18].toInt() and 0xff) or ((bytes[19].toInt() and 0xff) shl 8)
            when {
                !isElf -> problems += "not an ELF file: ${file.relativeTo(projectDir)}"
                gotMachine != machine ->
                    problems += "wrong architecture for $abi (e_machine=$gotMachine, want $machine)"
                !containsAscii(bytes, jniSymbol) ->
                    problems += "$abi library does not export $jniSymbol"
            }
        }
        if (problems.isNotEmpty()) {
            throw GradleException(
                "GeekBridge native core is not packaged; refusing to build an APK without it.\n" +
                    problems.joinToString("\n") { "  - $it" } +
                    "\nBuild crates/bridge-android for arm64-v8a and x86_64 and copy the libraries to " +
                    "android/app/src/main/jniLibs/<abi>/$nativeLibName (scripts/build-android.sh does this)."
            )
        }
    }
}

fun containsAscii(haystack: ByteArray, needle: String): Boolean {
    val n = needle.toByteArray(Charsets.US_ASCII)
    outer@ for (i in 0..haystack.size - n.size) {
        for (j in n.indices) if (haystack[i + j] != n[j]) continue@outer
        return true
    }
    return false
}

// Everything that packages JNI libraries (debug/release APK and bundle) requires the check.
tasks.configureEach {
    if (name.startsWith("merge") && name.endsWith("JniLibFolders") && !name.contains("AndroidTest")) {
        dependsOn(verifyNativeLibs)
    }
}
