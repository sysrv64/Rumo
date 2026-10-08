import java.util.Properties

/**
 * Strip debug information from the built `.so`.
 *
 * In the `dev` profile this is 232 MB of DWARF and 34 MB of symbol tables on a 318 MB
 * library whose code itself is about 38 MB. AGP calls `strip` itself, but its
 * `llvm-strip` from the NDK is built for x86_64 and crashes with `SIGILL` on aarch64, so
 * the library travels into the APK as it is — 300 MB instead of 70.
 *
 * `--strip-debug` keeps `.symtab`, so a backtrace shows function names.
 * `--strip-unneeded` would take that away too (about 33 MB more), but then the stack is nothing but
 * addresses. The choice and the measurements: `docs/18-strip.md`.
 *
 * Tools from `PATH` are tried first and only then from the NDK: the NDK's
 * `llvm-strip` has the same host architecture as the whole toolchain, and on a machine where
 * it does not run, the `--version` probe rules it out before it can bring down the
 * build.
 */
val stripDebugInfo: String = """
    SO=../app/src/main/jniLibs/arm64-v8a/librumo_bridge.so
    if [ -f "${'$'}SO" ]; then
      for c in llvm-strip aarch64-linux-gnu-strip strip \
               "${'$'}ANDROID_SDK_ROOT"/ndk/*/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip; do
        command -v "${'$'}c" >/dev/null 2>&1 && "${'$'}c" --version >/dev/null 2>&1 || continue
        "${'$'}c" --strip-debug "${'$'}SO" && echo "cargoBuild: stripped debug info from ${'$'}SO" && break
      done
    fi
""".trimIndent()

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "com.kerneldroid.rumo"
    // core-ktx 1.19.0 requires compileSdk >= 37 to link.
    compileSdk = 37

    defaultConfig {
        applicationId = "com.kerneldroid.rumo"
        minSdk = 26
        // targetSdk = compileSdk: the app declares that it works by the rules of
        // the current platform. The difference between them is a way to tell the system
        // "apply last year's rules to me", and it is only needed by those who
        // have not yet fixed what those rules broke.
        targetSdk = 37
        versionCode = 1
        versionName = "0.1"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
        debug {
            isMinifyEnabled = false
            ndk {
                abiFilters += "arm64-v8a"
            }
        }
    }
    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlin {
        compilerOptions {
            jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
        }
    }
    buildFeatures {
        compose = true
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    // Icons.Rounded.* beyond the small core set (Home/School/Movie/Dashboard/...).
    implementation(libs.androidx.compose.material.icons.extended)
    // NavHost routes: home/tutorials/projects/templates/editor.
    implementation(libs.androidx.navigation.compose)
    // Dynamic seed schemes (system Material You -> kolor seed -> static fallback).
    implementation(libs.material.kolor)
    // Required only so the manifest theme parent (Theme.Material3.*) resolves at link time.
    implementation(libs.material)
    // Markdown in the assistant's replies: headings, lists, tables, code. A custom
    // parser is hundreds of lines and still without tables; the library takes
    // `org.jetbrains:markdown` and renders into Compose.
    implementation(libs.markdown.renderer)
    implementation(libs.markdown.renderer.m3)
    // The assistant: its own repository (mounted at ai-engines/), one engine
    // inside it. The app reaches it only through `RumiHost`, which it implements.
    implementation(project(":ai-engines"))
}

// Builds librumo_bridge.so into app/src/main/jniLibs via cargo-ndk.
// Never-fail shell wrapper: machines without cargo/NDK still get a Gradle build.
tasks.register<Exec>("cargoBuild") {
    // The release graph is served by cargoBuildRelease: the debug task is skipped,
    // otherwise two profiles write to one jniLibs/*.so and the last one wins.
    onlyIf { gradle.startParameter.taskNames.none { it.contains("Release", ignoreCase = true) } }
    workingDir = file("../rumo-rs")
    // cargo-ndk doesn't inherit Gradle's SDK location: export it explicitly.
    // (local.properties is gitignored; env fallback for CI machines.)
    val sdkDir: String = run {
        val p = Properties()
        rootProject.file("local.properties").takeIf { it.exists() }?.inputStream()?.use { p.load(it) }
        p.getProperty("sdk.dir") ?: System.getenv("ANDROID_SDK_ROOT") ?: System.getenv("ANDROID_HOME") ?: ""
    }
    if (sdkDir.isNotEmpty()) {
        environment("ANDROID_SDK_ROOT", sdkDir)
        environment("ANDROID_HOME", sdkDir)
    }
    // 1) cargo-ndk (the path for x86_64 CI/dev machines).
    // 2) fallback for aarch64 hosts: a direct cargo build with the system clang-21
    //    (NDK prebuilts are linux-x86_64 only) with the NDK sysroot + libunwind.a.
    // Never fails the build: without a fresh .so Gradle takes the existing jniLibs.
    commandLine(
        "sh",
        "-c",
        """
        export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-L ${'$'}ANDROID_SDK_ROOT/ndk/29.0.14206865/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/21/lib/linux/aarch64 -L ${'$'}ANDROID_SDK_ROOT/ndk/30.0.14904198/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/21/lib/linux/aarch64"
        cargo ndk -t arm64-v8a -o ../app/src/main/jniLibs build --lib || {
          NDK_VER="$(ls -d "${'$'}ANDROID_SDK_ROOT"/ndk/* 2>/dev/null | sort -V | tail -n 1)"
          SYSROOT="${'$'}NDK_VER/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
          UNWIND="${'$'}NDK_VER/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/21/lib/linux/aarch64"
          export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="clang-21"
          export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-Clink-args=--target=aarch64-linux-android21 --sysroot=${'$'}SYSROOT -L ${'$'}UNWIND"
          cargo build --target aarch64-linux-android --lib && mkdir -p ../app/src/main/jniLibs/arm64-v8a && cp target/aarch64-linux-android/debug/librumo_bridge.so ../app/src/main/jniLibs/arm64-v8a/ || true
        } || echo 'cargoBuild: no fresh .so, continuing with existing jniLibs'
        ${stripDebugInfo}
        exit 0
        """.trimIndent(),
    )
}

// Strict release Rust build: fails loudly, never silently continues.
// Differences from cargoBuild (debug): --release args, NO '|| true'/'|| echo'
// silencers, NO trailing 'exit 0'. cargo-ndk failure falls back to a direct
// cargo release build; if that also yields nothing fresh -> exit 1.
tasks.register<Exec>("cargoBuildRelease") {
    workingDir = file("../rumo-rs")
    // Same SDK export as cargoBuild: cargo-ndk doesn't inherit Gradle's SDK location.
    val sdkDir: String = run {
        val p = Properties()
        rootProject.file("local.properties").takeIf { it.exists() }?.inputStream()?.use { p.load(it) }
        p.getProperty("sdk.dir") ?: System.getenv("ANDROID_SDK_ROOT") ?: System.getenv("ANDROID_HOME") ?: ""
    }
    if (sdkDir.isNotEmpty()) {
        environment("ANDROID_SDK_ROOT", sdkDir)
        environment("ANDROID_HOME", sdkDir)
    }
    commandLine(
        "sh",
        "-c",
        """
        export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-L ${'$'}ANDROID_SDK_ROOT/ndk/29.0.14206865/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/21/lib/linux/aarch64 -L ${'$'}ANDROID_SDK_ROOT/ndk/30.0.14904198/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/21/lib/linux/aarch64"
        if cargo ndk -t arm64-v8a -o ../app/src/main/jniLibs build --lib --release; then
          echo 'cargoBuildRelease: cargo-ndk release OK'
        else
          echo 'cargoBuildRelease: cargo-ndk failed, trying direct cargo fallback (release)'
          NDK_VER="$(ls -d "${'$'}ANDROID_SDK_ROOT"/ndk/* 2>/dev/null | sort -V | tail -n 1)"
          SYSROOT="${'$'}NDK_VER/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
          UNWIND="${'$'}NDK_VER/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/21/lib/linux/aarch64"
          export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="clang-21"
          export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-Clink-args=--target=aarch64-linux-android21 --sysroot=${'$'}SYSROOT -L ${'$'}UNWIND"
          if ! cargo build --target aarch64-linux-android --release --lib; then
            echo 'cargoBuildRelease: FAILED: neither cargo-ndk nor direct cargo build produced a release .so' >&2
            exit 1
          fi
          mkdir -p ../app/src/main/jniLibs/arm64-v8a
          cp target/aarch64-linux-android/release/librumo_bridge.so ../app/src/main/jniLibs/arm64-v8a/
        fi
        if [ ! -f ../app/src/main/jniLibs/arm64-v8a/librumo_bridge.so ]; then
          echo 'cargoBuildRelease: FAILED: librumo_bridge.so missing after release build' >&2
          exit 1
        fi
        [ ../app/src/main/jniLibs/arm64-v8a/librumo_bridge.so -nt rumo-bridge/src/lib.rs ] || { echo 'cargoBuildRelease: FAILED: stale .so (librumo_bridge.so not newer than rumo-bridge/src/lib.rs)' >&2; exit 1; }
        ${stripDebugInfo}
        """.trimIndent(),
    )
}

tasks.named("preBuild") {
    dependsOn("cargoBuild")
}

// preReleaseBuild IS created in this AGP (9.5.0-alpha02, visible in
// ':app:tasks --all'), but only AFTER this script is evaluated, so a direct
// tasks.named("preReleaseBuild") fails configuration with "not found".
// matching+configureEach fires for late-added tasks, hence it is used.
// (Fallback assembleRelease-hook not needed; this is the recorded choice.)
tasks.matching { it.name == "preReleaseBuild" }.configureEach {
    dependsOn("cargoBuildRelease")
}
