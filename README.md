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

Early. The engine is covered by unit tests, but the app has not been verified on
a physical device by the maintainers, and GPU tests are marked `#[ignore]`
because the build hosts have no adapter. Treat rendering output as unverified
until you have run it yourself.

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

The engine is pure Rust with no C or C++ dependencies. That is a hard rule, not
a preference: `cargo tree --target aarch64-linux-android -p rumo_bridge` must not
contain `cc` or `cmake`. Only `*-sys` and `ndk` bindings are allowed.

```sh
cd rumo-rs
cargo test --workspace -j2
```

The Android library, then the APK. `cargo-ndk` needs the NDK path exported by
hand, and needs the sysroot and unwinder directories passed explicitly or it
fails at the link step:

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

`-x cargoBuild` is deliberate: Gradle would otherwise recompile the whole Rust
graph under a different fingerprint and fail at the same link step. The artifact
is `app/build/outputs/apk/debug/app-debug.apk`.

**Strip the library before packaging.** A `dev`-profile `.so` carries roughly
230 MB of DWARF and 34 MB of symbol tables on top of about 38 MB of code. AGP
runs `strip` itself, but its `llvm-strip` is an x86_64 binary and dies with
`SIGILL` on an arm64 host, so the library is packaged verbatim and the APK is
300 MB instead of 70. The Gradle `cargoBuild` task strips it; when you build the
`.so` by hand as above, strip it yourself:

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
