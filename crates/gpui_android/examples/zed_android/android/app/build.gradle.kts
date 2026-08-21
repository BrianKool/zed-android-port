import java.util.Properties

plugins {
    id("com.android.application")
    kotlin("android")
}

val zdroidApplicationId = "com.zdroid"
val forbiddenLegacyApplicationIds = listOf("com.zdroid.b")

// Release signing config. `signing.properties` and `release.keystore` are
// gitignored: contributors who clone the repo can still build the debug
// variant, but a release build that produces a signed APK requires the
// keystore + properties to be present locally (or provided by CI via
// env-var injected secrets, future work).
val signingPropsFile = file("signing.properties")
val signingProps = Properties().apply {
    if (signingPropsFile.exists()) {
        signingPropsFile.inputStream().use { load(it) }
    }
}
val hasReleaseSigning = signingPropsFile.exists()
val requiredSigningProperties = listOf("storeFile", "storePassword", "keyAlias", "keyPassword")
val hasCompleteReleaseSigning = hasReleaseSigning && requiredSigningProperties.all {
    !signingProps.getProperty(it).isNullOrBlank()
}

// Cargo build scripts (notably wasmtime-c-api-impl) invoke `cmake`
// directly. Android Studio installs CMake inside the SDK but does not add it
// to the Gradle daemon's PATH, so a truly clean build used to fail whenever a
// previously cached native artifact was absent. Resolve the SDK the same ways
// Android tooling does and publish its newest bundled CMake to every Cargo
// task, keeping local and CI builds reproducible.
val localProperties = Properties().apply {
    val propertiesFile = rootProject.file("local.properties")
    if (propertiesFile.exists()) {
        propertiesFile.inputStream().use { load(it) }
    }
}
val androidSdkDir = sequenceOf(
    System.getenv("ANDROID_SDK_ROOT"),
    System.getenv("ANDROID_HOME"),
    localProperties.getProperty("sdk.dir"),
)
    .filterNotNull()
    .map(::file)
    .firstOrNull { it.isDirectory }
val sdkCmakeBin = androidSdkDir
    ?.resolve("cmake")
    ?.listFiles()
    ?.filter { it.isDirectory }
    ?.maxByOrNull { it.name }
    ?.resolve("bin")
    ?.takeIf { it.resolve("cmake.exe").isFile || it.resolve("cmake").isFile }
val cargoBuildPath = sdkCmakeBin?.let {
    "${it.absolutePath}${File.pathSeparator}${System.getenv("PATH").orEmpty()}"
}

// Main Rust library bundling.
//
// Android Gradle only packages files already present under jniLibs; it does
// not know that libzed_android.so is produced by Cargo. Always invoke Cargo
// before preBuild (Cargo itself remains incremental), then stage the resulting
// library. This prevents an APK from combining fresh Kotlin with a stale Rust
// JNI contract.
val zedAndroidDir = file("../..").canonicalFile
val zedAndroidLib = file("${zedAndroidDir}/target/aarch64-linux-android/release/libzed_android.so")
val stagedZedAndroidLib = file("src/main/jniLibs/arm64-v8a/libzed_android.so")

tasks.register<Exec>("buildZedAndroidLib") {
    description = "Build the main Zdroid Rust cdylib via cargo-ndk."
    group = "build setup"

    workingDir(zedAndroidDir)
    commandLine(
        "cargo",
        "ndk",
        "-t",
        "arm64-v8a",
        "-P",
        "26",
        "build",
        "--release",
    )
    providers.environmentVariable("ANDROID_NDK_HOME").orNull?.let { ndk ->
        environment("ANDROID_NDK_HOME", ndk)
    }
    cargoBuildPath?.let { environment("PATH", it) }

    outputs.file(zedAndroidLib)
    outputs.upToDateWhen { false }
}

tasks.register<Copy>("stageZedAndroidLib") {
    description = "Stage the freshly-built Rust cdylib into Android jniLibs."
    group = "build setup"

    dependsOn("buildZedAndroidLib")
    from(zedAndroidLib)
    into(stagedZedAndroidLib.parentFile)
    rename { stagedZedAndroidLib.name }
    inputs.file(zedAndroidLib)
    outputs.file(stagedZedAndroidLib)
}

tasks.matching { it.name == "preBuild" }.configureEach {
    dependsOn("stageZedAndroidLib")
}

// Bootstrap-zip distribution.
//
// Pre-Phase-6: the APK bundled bootstrap-aarch64.zip (~239 MB) as an
// asset, downloaded by a `downloadBootstrap` Gradle task from a
// GitHub release at build time, then extracted at first launch.
//
// Phase 6 moved the download to runtime, owned by
// `BootstrapAdapter::install` (see `crates/zdroid_runtime/src/
// adapters/bootstrap_install.rs`). The APK no longer bundles the
// zip; the Bootstrap adapter pulls it directly from
// `https://github.com/<release_repo>/releases/latest` when the user
// selects Bootstrap and the on-disk version sentinel doesn't match.
//
// Chroot users (Magisk + zd-spawnd module) never trigger the
// bootstrap install. Saves ~239 MB of fat for the majority case.

// zd-exec bundling.
//
// `zd-exec` is the Rust spawn wrapper (in crates/zdroid_runtime/src/
// bin/zd-exec.rs) the editor invokes as `terminal.shell` for chroot
// mode, and as the symlink target for `$PREFIX/zd-runtime/<name>`
// in chroot+other modes. It MUST be in the APK so fresh installs
// have it. Without bundling, end users hit
// `failed to spawn $PREFIX/bin/zd-exec â€” no such file or directory`
// the first time they open the integrated terminal in chroot mode.
//
// Build flow:
//   1. `buildZdExec` runs `cargo ndk â€¦ build --release -p
//      zdroid_runtime --bin zd-exec` from the workspace root, with
//      $ANDROID_NDK_HOME pointed at the same NDK the lib build uses.
//   2. The resulting ELF at
//      `target/aarch64-linux-android/release/zd-exec` is copied to
//      `app/src/main/assets/zd-exec`.
//   3. `preBuild` depends on it, so gradle picks the asset up during
//      `mergeAssets` before APK packaging.
//
// Rust-side counterpart: `gpui_android::zd_exec_install::ensure_installed`
// reads this asset at boot and extracts to `$PREFIX/bin/zd-exec` with
// 0755 perms when missing or out of date.
val workspaceRoot = file("../../../../../..").canonicalFile
val zdExecBin = file("${workspaceRoot}/target/aarch64-linux-android/release/zd-exec")
val zdExecAsset = file("src/main/assets/zd-exec")
val zdExecSrc = fileTree("${workspaceRoot}/crates/zdroid_runtime/src") {
    include("**/*.rs")
}

tasks.register<Exec>("buildZdExec") {
    description = "Build zd-exec via cargo-ndk and stage into assets/."
    group = "build setup"

    workingDir(workspaceRoot)
    commandLine(
        "cargo",
        "ndk",
        "-t",
        "arm64-v8a",
        "-P",
        "26",
        "build",
        "--release",
        "-p",
        "zdroid_runtime",
        "--bin",
        "zd-exec",
    )

    // Honor whatever the developer's `cargo ndk` lib build uses.
    // ANDROID_NDK_HOME has to be the same NDK or the prebuilts mismatch.
    providers.environmentVariable("ANDROID_NDK_HOME").orNull?.let { ndk ->
        environment("ANDROID_NDK_HOME", ndk)
    }
    cargoBuildPath?.let { environment("PATH", it) }

    inputs.files(zdExecSrc)
    inputs.file("${workspaceRoot}/crates/zdroid_runtime/Cargo.toml")
    outputs.file(zdExecBin)
}

tasks.register<Copy>("stageZdExecAsset") {
    description = "Copy the freshly-built zd-exec into assets/ for APK packaging."
    group = "build setup"

    dependsOn("buildZdExec")
    from(zdExecBin)
    into(zdExecAsset.parentFile)
    rename { zdExecAsset.name }
    // Don't bother re-running on every gradle invocation when the
    // source binary hasn't changed.
    inputs.file(zdExecBin)
    outputs.file(zdExecAsset)
}

tasks.matching { it.name == "preBuild" }.configureEach {
    dependsOn("stageZdExecAsset")
}

// zed-askpass-helper bundling.
//
// `zed-askpass-helper` is the SSH_ASKPASS relay binary that ssh
// (running inside the Kali chroot) exec's to prompt the user for a
// password via Zed's in-process modal. The helper MUST be statically
// linked: ssh inside the chroot can't resolve bionic's dynamic linker
// (`/system/bin/linker64` lives outside the chroot fs), so a
// dynamically-linked build fails exec with the shell's misleading
// "not found".
//
// The helper crate at `examples/zed_android/askpass-helper/` is its
// own workspace and ships a `.cargo/config.toml` setting
// `target-feature=+crt-static` for `aarch64-linux-android`. By running
// `cargo ndk` with `workingDir` set to that crate's directory, gradle
// guarantees the static-link config is in scope for every APK build
// â€” no chance of a contributor accidentally shipping a dynamic
// binary by building from the wrong cwd or hand-editing the asset.
val askpassHelperDir = file("${workspaceRoot}/crates/gpui_android/examples/zed_android/askpass-helper")
val askpassHelperBin = file("${askpassHelperDir}/target/aarch64-linux-android/release/zed-askpass-helper")
val askpassHelperAsset = file("src/main/assets/zed-askpass-helper")
val askpassHelperSrc = fileTree("${askpassHelperDir}/src") {
    include("**/*.rs")
}

tasks.register<Exec>("buildAskpassHelper") {
    description = "Build zed-askpass-helper (static aarch64) via cargo-ndk and stage into assets/."
    group = "build setup"

    workingDir(askpassHelperDir)
    commandLine(
        "cargo",
        "ndk",
        "-t",
        "arm64-v8a",
        "-P",
        "26",
        "build",
        "--release",
    )

    providers.environmentVariable("ANDROID_NDK_HOME").orNull?.let { ndk ->
        environment("ANDROID_NDK_HOME", ndk)
    }
    cargoBuildPath?.let { environment("PATH", it) }

    inputs.files(askpassHelperSrc)
    inputs.file("${askpassHelperDir}/Cargo.toml")
    inputs.file("${askpassHelperDir}/.cargo/config.toml")
    outputs.file(askpassHelperBin)
}

tasks.register<Copy>("stageAskpassHelperAsset") {
    description = "Copy the freshly-built zed-askpass-helper into assets/ for APK packaging."
    group = "build setup"

    dependsOn("buildAskpassHelper")
    from(askpassHelperBin)
    into(askpassHelperAsset.parentFile)
    rename { askpassHelperAsset.name }
    inputs.file(askpassHelperBin)
    outputs.file(askpassHelperAsset)
}

tasks.matching { it.name == "preBuild" }.configureEach {
    dependsOn("stageAskpassHelperAsset")
}

val runtimeContractSources = fileTree("${workspaceRoot}/crates/gpui_android/native/zd-runtime") {
    include("zd-exec", "zd-runtime-hook", "zd-runtime-sync", "zd-runtime.conf.example")
}

tasks.register("verifyZdroidRuntimeContract") {
    description = "Reject stale package paths or JNI signatures before packaging the APK."
    group = "verification"

    dependsOn("stageZedAndroidLib", "stageZdExecAsset", "stageAskpassHelperAsset")
    inputs.files(stagedZedAndroidLib, zdExecAsset, runtimeContractSources)

    doLast {
        val contractFiles = listOf(stagedZedAndroidLib, zdExecAsset) + runtimeContractSources.files
        contractFiles.forEach { contractFile ->
            val contents = contractFile.readBytes().toString(Charsets.ISO_8859_1)
            forbiddenLegacyApplicationIds.forEach { forbidden ->
                check(!contents.contains(forbidden)) {
                    "Stale Android package '$forbidden' found in ${contractFile.path}"
                }
            }
        }

        val nativeContents = stagedZedAndroidLib.readBytes().toString(Charsets.ISO_8859_1)
        check(nativeContents.contains("/data/data/$zdroidApplicationId/files/usr")) {
            "Main Rust library does not contain the expected $zdroidApplicationId runtime prefix"
        }
        check(nativeContents.contains("launchOpenTree") && nativeContents.contains("(Z)V")) {
            "Main Rust library does not contain the current launchOpenTree(boolean) JNI contract"
        }
    }
}

tasks.matching { it.name == "preBuild" }.configureEach {
    dependsOn("verifyZdroidRuntimeContract")
}

android {
    namespace = zdroidApplicationId
    compileSdk = 35

    // Pin the NDK explicitly so reproducibility doesn't depend on whatever
    // `sdkmanager --list_installed` happens to surface. Bionic's
    // `forkpty()` is in API 23+, so any NDK â‰¥ r21 is sufficient; we use r27
    // because that's the one we shipped L1 with and `+fp16` codegen
    // (gemm-f16) wants a recent toolchain.
    ndkVersion = "27.0.12077973"

    defaultConfig {
        applicationId = zdroidApplicationId
        // minSdk = 26 enforces bionic â‰¥ Oreo. `forkpty()` is on the symbol
        // table from API 23, but cpal/livekit transitive crates require
        // libaaudio which is API 26.
        minSdk = 26
        // targetSdk = 28 is the linchpin of the bundled Termux runtime:
        // SELinux puts us in the `untrusted_app_27` domain where
        // `execute_no_trans` on `app_data_file` is permitted, so we can
        // execve $PREFIX/bin/* directly. Pinning > 28 lands in
        // `untrusted_app_all` / numbered higher domains where exec is
        // denied â€” the entire L2 plan stops working. Skipping Play Store
        // eligibility is the explicit trade.
        targetSdk = 28
        versionCode = 141
        versionName = "1.1.5"
        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }

    // Don't deflate bootstrap-aarch64.zip during APK packaging â€” it's
    // already a deflated zip, and re-deflating it (a) wastes APK size
    // (b) forces AAssetManager to decompress at runtime, which prevents
    // the bootstrap extractor from using the mmap-able buffer path.
    androidResources {
        noCompress += listOf("zip")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    // `targetSdk = 28` is intentional (see the comment in defaultConfig). It
    // pins us in the SELinux `untrusted_app_27` domain so the bundled Termux
    // runtime can `execve` $PREFIX/bin/*. AGP's `lintVitalRelease` task
    // flags this as `ExpiredTargetSdkVersion` and refuses to assemble the
    // release APK. We're not Play-Store eligible by design â€” disable that
    // single rule rather than bumping the SDK and breaking exec.
    lint {
        disable += "ExpiredTargetSdkVersion"
    }

    signingConfigs {
        if (hasCompleteReleaseSigning) {
            create("release") {
                storeFile = file(signingProps.getProperty("storeFile"))
                storePassword = signingProps.getProperty("storePassword")
                keyAlias = signingProps.getProperty("keyAlias")
                keyPassword = signingProps.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        getByName("debug") {
            isMinifyEnabled = false
        }
        getByName("release") {
            isMinifyEnabled = false
            if (hasCompleteReleaseSigning) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }
}

val verifyReleaseSigning by tasks.registering {
    description = "Fail official release builds unless every signing secret is configured."
    group = "verification"
    doLast {
        check(hasCompleteReleaseSigning) {
            val missing = if (!hasReleaseSigning) {
                "signing.properties"
            } else {
                requiredSigningProperties
                    .filter { signingProps.getProperty(it).isNullOrBlank() }
                    .joinToString()
            }
            "Release signing is incomplete (missing: $missing). Use assembleDebug for local unsigned work."
        }
        val keyStore = file(signingProps.getProperty("storeFile"))
        check(keyStore.isFile) { "Release keystore does not exist: ${keyStore.path}" }
    }
}

tasks.matching {
    it.name == "assembleRelease" || it.name == "bundleRelease" || it.name == "packageRelease"
}.configureEach {
    dependsOn(verifyReleaseSigning)
}

dependencies {
    implementation("androidx.games:games-activity:3.0.5")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.core:core-ktx:1.13.1")
    // ActivityResultLauncher / ActivityResultContracts for SAF picker.
    implementation("androidx.activity:activity-ktx:1.9.3")
}
