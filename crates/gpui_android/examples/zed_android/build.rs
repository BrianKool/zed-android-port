//! Build-time guard for the bootstrap-package-name invariant.
//!
//! Termux's bootstrap binaries hardcode `/data/data/<package>/files/...` in
//! every ELF's `DT_RUNPATH` and every shell shebang. Rebuilding the
//! bootstrap with `TERMUX_APP_PACKAGE=com.zdroid.b` bakes that exact
//! string into ~3 GB of binaries. If `applicationId` in
//! `android/app/build.gradle.kts` ever drifts from this constant, every
//! binary in the rebuilt bootstrap fails at runtime with `dlopen failed:
//! library "/data/data/com.termux/..." not found` â€” an afternoon of
//! head-scratching by the time it shows up in logcat.
//!
//! Make the mismatch un-compileable: read the gradle file, assert it
//! contains `applicationId = "<our package>"`. Only checks the gradle file
//! exists and contains the literal â€” does NOT pretend to be a kts parser.
//! AGP and we agree on the substring or we both panic.

const BOOTSTRAP_PACKAGE_NAME: &str = "com.zdroid.b";

fn main() {
    let gradle_path = "android/app/build.gradle.kts";
    println!("cargo:rerun-if-changed={gradle_path}");

    let gradle = match std::fs::read_to_string(gradle_path) {
        Ok(s) => s,
        Err(err) => {
            // Don't hard-fail on missing gradle (some `cargo` flows like
            // `cargo doc` or out-of-tree builds may not have it laid out
            // the same way); just warn so a CI run that builds the APK
            // still catches drift.
            println!(
                "cargo:warning=build.rs: could not read {gradle_path} ({err}); \
                 skipping applicationId assertion"
            );
            return;
        }
    };

    let needle = format!("applicationId = \"{BOOTSTRAP_PACKAGE_NAME}\"");
    assert!(
        gradle.contains(&needle),
        "applicationId in {gradle_path} must match BOOTSTRAP_PACKAGE_NAME ({BOOTSTRAP_PACKAGE_NAME}). \
         The bundled Termux bootstrap is rebuilt with this exact package name baked into every \
         binary's DT_RUNPATH and shebangs; a mismatch breaks every spawned process at runtime."
    );

    // Surface the constant to runtime code as well, in case any consumer
    // wants to log or assert against it. Available via env!.
    println!("cargo:rustc-env=BOOTSTRAP_PACKAGE_NAME={BOOTSTRAP_PACKAGE_NAME}");

    // Check that the standalone askpass-helper binary has been staged
    // into APK assets. The helper lives in `askpass-helper/` and is
    // built independently via `cargo ndk -t arm64-v8a -P 26 build
    // --release` from that subdir, then copied to assets/. If a
    // contributor forgets that step, ssh password prompts will silently
    // SIGABRT under SELinux untrusted_app_27 (current_exe() falls back
    // to /system/bin/app_process64). Surface the gap loudly at build
    // time instead of waiting for the runtime symptom.
    let askpass_asset = "android/app/src/main/assets/zed-askpass-helper";
    println!("cargo:rerun-if-changed={askpass_asset}");
    if !std::path::Path::new(askpass_asset).is_file() {
        println!(
            "cargo:warning=build.rs: askpass helper missing at {askpass_asset}. \
             Build it via:\n  \
             cd askpass-helper && \\\n    \
             ANDROID_NDK_HOME=$ANDROID_NDK_HOME cargo ndk -t arm64-v8a -P 26 build --release && \\\n    \
             cp target/aarch64-linux-android/release/zed-askpass-helper ../{askpass_asset}\n  \
             Without it, SSH password / passphrase prompts will SIGABRT under SELinux untrusted_app_27."
        );
    }
}
