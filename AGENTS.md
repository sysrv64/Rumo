# AGENTS.md

How to work in this repository. Written for contributors who are AI agents, but
it is the same file a human contributor should read first.

Read this before changing anything. Most of what follows is not style preference
— it is a constraint that cost somebody a day to discover.

## What this is

An Android video editor and motion-design tool. A Rust workspace does the
rendering; Kotlin owns the interface and the project state. See `README.md` for
the layout.

The assistant is not in this repository: it lives in `AI-Engines` and arrives as
the `ai-engines/` submodule. Everything under `ai-engines/` is edited there, in
its own checkout; a change to it is a change to that repository, and it is
picked up here as a new submodule revision.

Two rules that are not negotiable and are enforced by review:

- **The engine is pure Rust.** No `cc`, no `cmake`, no C or C++ dependency.
  Verify with
  `cargo tree --target aarch64-linux-android -p rumo_bridge` — it must not
  contain `cc` or `cmake`. Only `*-sys` and `ndk` bindings are allowed.
- **Kotlin does not draw.** If you find yourself computing pixels, geometry or
  timing in Kotlin, the work belongs in the engine.

One design worth knowing before you touch layers: **the background is a layer.**
It is an ordinary `SHAPE` layer whose shape is `Frame`, which the engine draws to
cover the target instead of placing it inside (`preview_shape_draws` special-cases
it, and `frame_covers_the_target_and_ignores_placement` pins that down). That is
what gives the background effects, opacity, keyframes and a row in the layer list
without any of it being written twice — it used to be only the colour the frame
was cleared with, and could take none of them. Its length is derived from the
project rather than chosen, and `setBackground` moves the clear colour and the
layer together, because a transparent layer over an opaque clear shows nothing.

## Working in this repository

### Git identity

Set your own name and email in your local configuration before committing. Use
the identity you actually own — a per-commit identity is not something to
invent, and a maintainer's address must never be used as a default.

Do not commit `target/`, `build/`, `.gradle/`, or `app/src/main/jniLibs/` —
the native library is produced by a build script and does not belong in history.

### Comments

**All comments in this repository are in English**, in both Rust and Kotlin. The
policy used to be "Rust in English, Kotlin in Russian"; it was reversed when the
project was published, because the audience no longer reads Russian. Do not add
comments in another language.

Comments explain **why**, not what. The code already says what it does. A good
comment names the alternative that was rejected and what it would have cost, or
records a constraint that is invisible from the code — an API's real behaviour, a
platform quirk, a bug that was already paid for once.

Do not add comments that restate the next line, and do not leave commented-out
code or TODOs for work you have finished. Do not delete an existing comment you
do not understand: in this codebase those are usually load-bearing.

### Documentation

The project keeps a design record — why each decision was made, in the order
they were made — but it is **not published**, so it is not in this repository.
What that changes for you: a decision that is not visible in the diff has
nowhere else to live, so it belongs in the code. A comment that names the
alternative you rejected and what it would have cost is the only record a reader
of this repository will have, and the commits are the second. Do not assume a
reader can find the reasoning somewhere else.

### Strings and languages

The interface is offered in English, Russian and Chinese, and the language is
chosen in Appearance. **Any text a user reads comes from a string resource.**
Hardcoding it means it stays in whatever language it was written in while the
rest of the screen changes around it — which is exactly how a language switch
ends up half-working.

- One file per area, mirrored across the three locale folders:
  `values/strings_<area>.xml`, `values-ru/…`, `values-zh/…`. Android merges them,
  so a new area needs no registration anywhere.
- **Translate only what a person reads.** Comparison keys, persisted values, JSON
  fields, provider and model ids, URLs, HTTP headers, routes, preference keys and
  tool-schema text must stay as they are. Localizing one of those breaks logic
  silently, and the failure looks like a bug in something else entirely.
- Where a string is both displayed *and* compared, do not externalize it. Leave
  it and say so — an untranslated message is a small cost, a broken comparison is
  a large one. `ShopUpdates.NO_RELEASE` is the worked example.
- Language names are shown in their own script (`English`, `Русский`, `中文`) and
  live in the default file with `translatable="false"`, because a picker that
  translated them would be unusable to the person who needs it.
- Placeholders are positional (`%1$s`, `%2$d`). Never glue sentence fragments
  together: an order that reads correctly in English is usually wrong in Chinese.
- Counts use `<plurals>`; Russian needs `one/few/many/other`, Chinese only
  `other`.
- Comments in the locale files are **English too**, matching the default file
  they mirror. A note about *why* a string exists is for whoever maintains the
  translation, and the locale folder is a copy of a file whose notes are already
  written — keeping them in the same language as the default file means a
  contributor can read either one. Do not translate the comments.
- Chinese is two scripts and one option: the picker offers 中文, and the script
  follows the phone (`AppLanguage.locale`), resolving to `values-zh` or
  `values-b+zh+Hant`. Adding a language to the picker is not the same as adding a
  script, and a Traditional reader should not have to know which of two entries
  is theirs.

The chosen language is applied by wrapping the composition in a context carrying
the locale (`Context.withAppLanguage` in `ui/AppLanguage.kt`), not by
`recreate()`. Restarting the activity would discard the open project, which
lives in the composition's `remember`.

## Where builds happen

On GitHub, in CI. Not on a workstation and not on a phone, which is where this
was built until now and is not where it is built from here on.

`ci.yml` runs the test gate and builds a debug APK on every push to `master` and
every pull request. `release.yml` builds and signs a release on a `v*` tag and has
no `pull_request` trigger at all, which is the only thing that keeps the signing
key and write access out of reach of a pull request — do not add one.

A build you ran locally is still not what CI will do: do not assume a toolchain
on the machine you are reading this on, and check the run rather than your
command line.

What the workflows set up, because none of it lives in the repository:

- The Android SDK and NDK, with `ANDROID_SDK_ROOT` and `ANDROID_NDK_HOME`
  exported. Gradle does not find them by itself on a bare runner, and `cargo ndk`
  never inherits Gradle's SDK location.
- The Rust toolchain `rust-toolchain.toml` pins, with the
  `aarch64-linux-android` target added.

The `.so` is built by hand rather than by Gradle — the reason is under [The
APK](#the-apk) — so the order matters: cargo, then the strip, then the APK.

Both repositories are public, so the `ai-engines/` submodule is read like any
other public clone and no token is involved. Making `AI-Engines` private again
breaks the checkout in both workflows, and that failure reads as "repository not
found" rather than as a missing credential.

### What the old build hosts imposed, and does not apply any more

This used to be built on arm64 Linux containers with very little RAM, and most
of the operational folklore in this file came from that: `--no-daemon`, `-j1`,
one build at a time. A hosted runner has enough memory for none of it to be
necessary, and `-j2` for the test gate is enough. The constraints were real, so
they are recorded rather than deleted: if a build is ever run on a small machine
again, they come back with it, and the link step of the native library is the
first thing to fail.

## Rust (`rumo-rs/`)

```sh
cargo test --workspace -j2
```

This is the gate. GPU and hardware tests are marked `#[ignore]` on purpose:
there is no adapter on a build host, and running one produces `SIGILL` rather
than a useful failure.

Other invariants:

- **No `pollster::block_on` on a JNI thread.** GPU initialisation runs on a
  dedicated worker thread with a timeout, and the JNI call returns an error code.
  Blocking the JNI thread deadlocks the app instead of failing.
- **The engine is strictly RGBA8 internally**, with no channel shuffling. The
  single place a channel order is rearranged is the legacy `Bitmap` boundary,
  which needs `0xAARRGGBB`.
- **New JNI goes in its own crate** — `rumo-render/src/jni.rs`,
  `rumo-media/src/audio_jni.rs`, `rumo-export/src/jni.rs` — **plus a `#[used]`
  link-table entry in `rumo-bridge/src/lib.rs`.** Without the entry the symbol
  never reaches the cdylib, and the failure appears at runtime, not at link time.

### Building the `.so` (arm64)

`cargo ndk` fails at the link step without the sysroot libraries passed
explicitly. This recipe works:

```sh
cd rumo-rs
export ANDROID_SDK_ROOT=/path/to/android-sdk
export ANDROID_NDK_HOME=$ANDROID_SDK_ROOT/ndk/30.0.14904198
NDK=$ANDROID_NDK_HOME
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-L $NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/26 -L $NDK/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/21/lib/linux/aarch64"
cargo ndk -t arm64-v8a -o ../app/src/main/jniLibs build --lib -j1
```

Do **not** use `cargo build --target aarch64-linux-android` with `--sysroot` in
`RUSTFLAGS`; it breaks the std search with `E0463`.

#### Check JNI symbols by name, not by presence

`nm -D … | grep Foo` proves a symbol exists, not that it is named the way Java
looks for it. A typo in the class name (`RumboBridge` for `RumoBridge`) produces
a symbol nobody asks for, and an `UnsatisfiedLinkError` at runtime from a build
that looked verified. Derive the expected names from the Kotlin and look for
exactly those:

```sh
python3 - <<'EOF'
import re, subprocess
kt = open("app/src/main/java/com/kerneldroid/rumo/data/RumoBridge.kt", encoding="utf-8").read()
pkg = re.search(r'^package\s+([\w.]+)', kt, re.M).group(1)
prefix = "Java_" + pkg.replace(".", "_") + "_RumoBridge_"
have = set(re.findall(r'\b(Java_\w+)',
    subprocess.run(["nm", "-D", "app/src/main/jniLibs/arm64-v8a/librumo_bridge.so"],
                   capture_output=True, text=True).stdout))
externs = re.findall(r'external fun (\w+)', kt)
missing = [e for e in externs if prefix + e not in have]
print("missing:", missing if missing else "(none)")
EOF
```

#### Strip the library after building

A `dev`-profile `.so` is about 230 MB of DWARF and 34 MB of symbol tables on top
of roughly 38 MB of code. AGP does run `strip` — but only with a `strip` tool it
can execute, and on an arm64 host the NDK's `llvm-strip` is an x86_64 binary that
dies with `SIGILL`, so it strips nothing and the library is packaged verbatim at
300 MB instead of 70. A hosted runner is x86_64 and strips as it should.

Stripping here anyway is deliberate: it makes the packaged size the same on both
kinds of machine, and it is the difference between an APK that is worth
downloading and one that is not. Run it after linking:

```sh
SO=../app/src/main/jniLibs/arm64-v8a/librumo_bridge.so
for c in "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip" \
         llvm-strip aarch64-linux-gnu-strip; do
  command -v "$c" >/dev/null 2>&1 && "$c" --version >/dev/null 2>&1 || continue
  "$c" --strip-debug "$SO" && break
done
```

The `--version` probe is not decoration: the NDK's own `llvm-strip` has the same
host architecture as the rest of the toolchain, so on a machine where it cannot
run the probe rules it out *before* it takes the build down.

`--strip-debug` removes DWARF and keeps `.symtab`, so a backtrace still names
functions. `--strip-unneeded` removes those too (another ~33 MB), but then the
stack shows only addresses. Measured on aarch64: 296 298 792 bytes before,
70 294 744 after.

## The APK

```sh
sh gradlew :app:assembleDebug -x cargoBuild --console=plain --no-daemon
```

`-x cargoBuild` is deliberate: the `.so` is built by the recipe above, and
letting Gradle rebuild it compiles the whole Rust graph a second time under a
different fingerprint. On the machines this used to be built on that took about
an hour; on a runner it is faster and still wasted work, and it is what the CI
workflow must avoid doing twice. The artifact is
`app/build/outputs/apk/debug/app-debug.apk`.

If the APK is far larger than the library plus the dex files, it has dead space
inside the zip from an incremental rewrite in place. Delete
`app/build/outputs/apk/debug/app-debug.apk` and
`app/build/intermediates/packaged_manifests` and rebuild; compare the file size
against the sum of compressed entry sizes to confirm.

## Verifying your work

Say what you checked and what you did not. Specifically:

- A build that compiles is not a feature that works. A hosted runner has no
  device and no GPU, so nothing visual has been observed by anything that ran
  there, and a green workflow will not change that.
- Do not describe untested code as working, and do not report a green build as
  verification of behaviour.
- When you cannot verify something, say so in the commit message and in the pull
  request. The repository's history is full of "not verified" notes; keep that
  habit.
