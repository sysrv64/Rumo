# Rumo

A video editor and motion-design tool for Android. A Rust engine underneath, a
Material 3 Expressive interface on top.

**Ru**st + **Mo**tion.

## What it is

Rumo is a FOSS motion-design editor: keyframed animation, shapes, effects, text
and audio on a phone-sized timeline. The rendering path — shapes, curves,
tessellation, compositing, media decode, text layout and MP4 export — is a Rust
workspace driven over JNI. Kotlin owns the interface and the project state; it
does not draw.

## Status

Early. It has been run on one device and nowhere else: the author's own phone,
one vendor's build of Android, one GPU. Other phones, other GPUs and other
vendors' ROMs are untested, and testing them is what it needs next — a report
from a device that behaves differently is the most useful thing right now.

The engine is covered by unit tests, but the GPU tests are marked `#[ignore]`
because CI has no adapter to run them on. A green workflow therefore says that
the code compiles and that the CPU-side tests pass; it does not say that anything
renders.

## Layout

| Path | What it is |
| --- | --- |
| `rumo-rs/` | The engine: a Rust workspace, six crates |
| `app/` | The Android application: Kotlin + Jetpack Compose |
| `AGENTS.md` | How to work in this repository — read it before changing anything |

### The Rust crates

| Crate | Role |
| --- | --- |
| `rumo-core` | The project model, `.rumo` serialisation, timing |
| `rumo-render` | Shapes, curves, tessellation, compositing, the GPU path |
| `rumo-media` | Video decode, audio decode and playback, YUV handling |
| `rumo-text` | Text layout and glyph meshes |
| `rumo-export` | Encoding, the MP4 writer, the export pipeline |
| `rumo-bridge` | The JNI surface — the only crate the app links |

## Requirements

- Android 8.0 (API 26) and up, arm64-v8a
- `compileSdk` 37, `targetSdk` 37
- Rust 1.97 or newer, with the `aarch64-linux-android` target
- Android NDK 30.0.14904198

## Building

Builds run in **GitHub Actions**, not on a workstation or a phone.
`.github/workflows/ci.yml` runs everything below on a push and on a pull request;
what follows is what it runs, and what to run by hand. Neither the SDK nor the
Rust target is in the repository, so a runner installs both.

The engine is pure Rust with no C or C++ dependencies. That is a hard rule, not
a preference: `cargo tree --target aarch64-linux-android -p rumo_bridge` must not
contain `cc` or `cmake`. Only `*-sys` and `ndk` bindings are allowed.

```sh
cd rumo-rs
cargo test --workspace -j2
```

The Android library, then the APK. `cargo-ndk` does not inherit Gradle's SDK
location, and it needs the sysroot and unwinder directories passed explicitly or
it fails at the link step:

```sh
cd rumo-rs
export ANDROID_SDK_ROOT=/path/to/android-sdk
export ANDROID_NDK_HOME=$ANDROID_SDK_ROOT/ndk/30.0.14904198
NDK=$ANDROID_NDK_HOME
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-L $NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/26 -L $NDK/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/21/lib/linux/aarch64"
cargo ndk -t arm64-v8a -o ../app/src/main/jniLibs build --lib -j1

cd ..
sh gradlew :app:assembleDebug -x cargoBuild --console=plain --no-daemon
```

`-x cargoBuild` is deliberate: the `.so` was just built, and Gradle would
otherwise compile the whole Rust graph a second time under a different
fingerprint. The order in the workflow is the same: cargo, strip, APK. The
artifact is `app/build/outputs/apk/debug/app-debug.apk`.

**Strip the library before packaging.** A `dev`-profile `.so` carries roughly
230 MB of DWARF and 34 MB of symbol tables on top of about 38 MB of code. AGP
does strip native libraries, but only with a `strip` tool it can execute; on an
arm64 host the NDK's `llvm-strip` is an x86_64 binary that dies with `SIGILL`,
so nothing is stripped and the APK is 300 MB instead of 70. A runner is x86_64
and strips properly. Doing it explicitly anyway makes the packaged size the same
on either kind of machine. The Gradle `cargoBuild` task strips it; when the
`.so` is built by hand as above, strip it yourself:

```sh
SO=../app/src/main/jniLibs/arm64-v8a/librumo_bridge.so
llvm-strip --strip-debug "$SO"     # or aarch64-linux-gnu-strip
```

`--strip-debug` drops DWARF and keeps `.symtab`, so native backtraces still name
functions.

## Languages

The interface is offered in English, Russian and Chinese, chosen in
Appearance → Language and switched without restarting. Every string a user reads
comes from a resource; `values-ru/` and `values-zh/` mirror `values/` file for
file. See [`AGENTS.md`](AGENTS.md) for the rule about what must *not* be
translated — identifiers, persisted values and anything compared later stay as
they are, because localizing one of those breaks logic silently.

## The assistant

Rumo ships an assistant, Rumi, that drives the editor through its own tools — it
builds, adjusts and renders, and can only act through what is in its tool list.
It is provider-agnostic: OpenCode Go and Zen, Google Gemini, Anthropic, OpenAI,
or any OpenAI-compatible endpoint you point it at. Credentials are API keys you
mint with the provider, stored per provider.

It can also generate media — speech, images and video — through Gemini, OpenAI
and BytePlus ModelArk. Those tools are optional and appear to the model only once
you have configured a key for the matching kind, so an unconfigured install
offers the model nothing it cannot do.

**Services are chosen for terms-of-service compliance, not for convenience.**
ElevenLabs is not embedded: its ElevenAPI terms forbid calling the API with a key
from a mobile application. fal.ai, Replicate, Runway, Luma and Kling are not
embedded either, for the same reason. Reach any of them through your own proxy as
a custom service, where the key and the responsibility are yours.

The assistant's own code lives in the `AI-Engines` repository and is pulled in
as the `ai-engines/` submodule; `AGENTS.md` describes how the two connect.

## Licence

Split, because the two halves are used differently:

| Part | Licence |
| --- | --- |
| `app/` — Kotlin, resources, Gradle scripts | GPL-3.0-or-later |
| `app/…/EaseUi.kt` — a port of the Rust easing | Apache-2.0, the one exception |
| `rumo-rs/` — the Rust workspace | Apache-2.0 |
| The APK as a whole | GPL-3.0-or-later (combined work) |

`or-later` rather than `only`: the terms can then move to a later GPL version
without asking everyone who ever touched the tree. The single Apache file is
explained in [`LICENSE`](LICENSE) — it is left permissive because a derivative of
Apache code may be relicensed to GPL but never back.

The full texts are in [`LICENSE`](LICENSE) and
[`rumo-rs/LICENSE-APACHE`](rumo-rs/LICENSE-APACHE); [`NOTICE`](NOTICE) says
which is which.

Copyright (C) 2026 Kerneldroid.
