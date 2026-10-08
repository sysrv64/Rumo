// SPDX-License-Identifier: Apache-2.0
#![allow(non_snake_case)]

//! JNI entry points for video-clip decoding.
//!
//! These belong to `com.kerneldroid.rumo.data.RumoBridge` but live in the media
//! crate so the decode pipeline stays together. They are thin wrappers over
//! [`crate::video`]: [`probe_video_fd`] for metadata, [`open_video_fd`] for a
//! decodable source. Everything is synchronous and runs on the calling JNI
//! thread — like [`crate::audio_jni`] there is deliberately no
//! `pollster::block_on` and no async runtime here.
//!
//! # Handles and fd ownership
//!
//! `nativeVideoOpenFd` registers the source in a process-global registry keyed
//! by a monotonic id. Ids start at 1 so `0` stays the "open failed" sentinel;
//! `nativeVideoClose` removes and drops the entry, is idempotent, and treats an
//! unknown handle as a no-op.
//!
//! **The caller owns `fd`.** The Android backend duplicates the descriptor
//! inside `AMediaExtractor_setDataSourceFd`, so the Java side must keep it open
//! for as long as the handle lives and may close it once `nativeVideoClose` has
//! returned. The same contract as [`crate::audio_jni::open_audio_fd`].
//!
//! # Latency
//!
//! One `nativeVideoFrameAt` call decodes as far as needed to cover `time_ms`:
//! a cache hit when the time falls inside the frames still held by
//! [`crate::video::DecodingSource`] (a refcount bump — the cached pixels are
//! shared, not copied), otherwise a seek plus forward decodes. No cache is
//! added here — the video module already owns the frame cache, its byte budget
//! and the forward-decode policy. The Android backend bounds each of its own
//! decode pumps, so the call cannot block indefinitely.
//!
//! # Panics
//!
//! A panic crossing the FFI boundary is UB, so nothing here may unwind into the
//! JVM. The helpers below are panic-free by construction (no indexing, no
//! `unwrap`, a poisoned registry is reported as failure), and the three calls
//! into the decoding module — which parse container data and can in principle
//! panic on a malformed clip — are run inside [`catch_unwind`]. The JNI
//! string/array conversions additionally sit inside `EnvUnowned::with_env`,
//! which wraps its closure in its own `catch_unwind`.

use crate::video::yuv::YuvPlanes;
use crate::video::{VideoError, VideoFrame};
use std::collections::HashMap;
use std::os::fd::RawFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use jni::EnvUnowned;
use jni::errors::LogErrorAndDefault;
use jni::objects::{JClass, Reference as _};
use jni::sys::{jbyteArray, jint, jlong, jstring};

use crate::video::{VideoInfo, VideoSource, open_video_fd, probe_video_fd};

// ---------------------------------------------------------------------------
// JSON metadata
// ---------------------------------------------------------------------------

/// Metadata of `info` as the JSON object the Kotlin side parses:
///
/// ```json
/// {"width":1920,"height":1080,"durationMs":12345,"fps":29.97,"rotationDeg":90,"hasAudio":true}
/// ```
///
/// Every value is a number or a boolean, never a string, so no character ever
/// needs escaping. `fps` is the only field that could arrive non-finite from a
/// corrupt container; it is emitted as `0` in that case so the object stays
/// parseable. Other values are echoed verbatim (`0.0` fps prints as `0`).
pub(crate) fn video_info_json(info: &VideoInfo) -> String {
    let fps = if info.fps.is_finite() { info.fps } else { 0.0 };
    format!(
        "{{\"width\":{},\"height\":{},\"durationMs\":{},\"fps\":{},\"rotationDeg\":{},\"hasAudio\":{}}}",
        info.width, info.height, info.duration_ms, fps, info.rotation_deg, info.has_audio
    )
}

// ---------------------------------------------------------------------------
// Handle registry
// ---------------------------------------------------------------------------

/// A live decoder plus a `Send` marker.
///
/// `Box<dyn VideoSource>` is not `Send` (the Android backend holds raw
/// `AMediaExtractor`/`AMediaCodec` pointers), so the registry needs this promise
/// before it can live in a `static`.
///
/// SAFETY: every access goes through [`registry`], a mutex, so the decode calls
/// that touch those pointers are serialized; the Android backend registers no
/// async-notify callback and makes no cross-thread use of the codec, so the
/// pointers may be used from whichever thread happens to hold the lock. This
/// mirrors `rumo-export/src/jni.rs`, which does the same for its exporter.
struct SourceSlot(Box<dyn VideoSource>);

// SAFETY: see `SourceSlot`.
unsafe impl Send for SourceSlot {}

/// `handle -> source`; dropping a source releases its extractor and codec.
type SourceRegistry = Mutex<HashMap<i64, SourceSlot>>;

fn registry() -> &'static SourceRegistry {
    static REGISTRY: OnceLock<SourceRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Handle ids start at 1 so `0` stays a valid "open failed" sentinel.
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

/// Register `source` under a fresh handle and return it (never `0`).
///
/// Split out of [`open_source`] so the registry can be exercised without an
/// Android device, using [`crate::video::FakeVideoSource`].
pub(crate) fn register_source(source: Box<dyn VideoSource>) -> i64 {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    match registry().lock() {
        Ok(mut registry) => {
            registry.insert(handle, SourceSlot(source));
            handle
        }
        // A poisoned registry cannot be trusted: report "open failed".
        Err(_) => 0,
    }
}

/// Run `f` against the source registered under `handle`, if any.
fn with_source<T>(handle: i64, f: impl FnOnce(&mut Box<dyn VideoSource>) -> T) -> Option<T> {
    let mut registry = registry().lock().ok()?;
    registry.get_mut(&handle).map(|slot| f(&mut slot.0))
}

/// Metadata of the source under `handle`, or `None` for an unknown handle.
pub(crate) fn source_info(handle: i64) -> Option<VideoInfo> {
    with_source(handle, |source| source.info())
}

/// The RGBA8 bytes of the frame covering `time_ms`, or `None` for an unknown
/// handle or a failed decode. Never panics.
///
/// By handle: the answer aliases the frame still held in
/// [`crate::video::DecodingSource`]'s cache, so nothing on this path copies
/// pixels — it is a refcount bump out of the decode and back into the caller.
pub(crate) fn source_frame(handle: i64, time_ms: i64) -> Option<Arc<VideoFrame>> {
    // `with_source` yields `Option<T>` where `T` is the closure's own return, and
    // a failed decode is already an `Option`, hence the flatten.
    with_source(handle, |source| {
        catch_unwind(AssertUnwindSafe(|| source.frame_at(time_ms)))
            .ok()
            .and_then(Result::ok)
    })
    .flatten()
}

/// Answers "the planes of the frame covering `time_ms`, for this handle".
///
/// This is the seam between decoding and rendering: a decoded frame can reach
/// the GPU as the decoder's own YUV planes, and this is where a backend that
/// holds those planes says so. It is a plain function pointer rather than a
/// trait method on purpose — the two decode routes produce different things
/// (converted pixels versus foreign plane memory) and the registry that holds
/// them is not the renderer's to change from inside a decoder, so the contract
/// stays "given a handle, hand me planes".
///
/// Until a reader is installed **nothing changes**: [`frame_for_compositor`]
/// keeps converting, and the RGBA8 path stays the one that runs, so a decoder
/// that has not been wired up yet loses an optimisation rather than a frame.
/// A reader that answers `None` for one frame simply leaves that frame on the
/// converted path, which is per-frame and needs no fallback flag.
pub type PlanesReader = fn(handle: i64, time_ms: i64) -> Option<Arc<YuvPlanes>>;

fn planes_reader() -> &'static OnceLock<PlanesReader> {
    static READER: OnceLock<PlanesReader> = OnceLock::new();
    &READER
}

/// Install the reader that answers with planes.
///
/// Called once, by the backend that owns the decoded buffer. A second call is
/// ignored rather than replacing the reader, so a reader cannot change what an
/// already-open clip answers mid-playback.
pub fn set_planes_reader(reader: PlanesReader) {
    let _ = planes_reader().set(reader);
}

/// The planes of the frame covering `time_ms`, owned by the caller, or `None`
/// when this build has no reader or that frame could not be decoded.
///
/// The bytes are a copy the reader has to make anyway: plane memory the decoder
/// owns is only valid until the buffer is released, and the render thread
/// uploads them later, so the frame cannot borrow from the decoder. What is
/// *not* copied is any conversion — the payload is the decoder's planes and
/// their geometry, and the YCbCr → RGBA8 work happens on the GPU.
pub fn planes_for_compositor(handle: i64, time_ms: i64) -> Option<Arc<YuvPlanes>> {
    (*planes_reader().get()?)(handle, time_ms)
}

/// The planes of the frame covering `time_ms`, if this decoder produces them.
///
/// The read-only counterpart of the RGBA8 decode: same frame, same time, no
/// conversion. It is what makes the GPU path possible without re-decoding, so
/// it goes through the same registry and the same cache as everything else.
pub fn planes_for_frame(handle: i64, time_ms: i64) -> Option<Arc<YuvPlanes>> {
    // `source_frame` hands back the cached frame by `Arc`, so its planes come
    // out by clone — which for `Arc` is a refcount bump, not a copy of the
    // decoded buffer. That is the whole point of the plane path.
    source_frame(handle, time_ms).and_then(|frame| frame.planes().cloned())
}

/// Decode one frame as a loose `(width, height, rgba)` triple, for callers
/// outside this crate that need to composite a video clip.
///
/// `rumo-bridge` is the intended caller: it decodes here and hands the pixels
/// to the renderer's texture registry, so a video frame reaches the compositor
/// without a byte-array round trip through Java. `None` for an unknown handle,
/// a failed decode, or an image whose length disagrees with its dimensions.
pub fn frame_for_compositor(handle: i64, time_ms: i64) -> Option<(u32, u32, Vec<u8>)> {
    let frame = source_frame(handle, time_ms)?;
    // The frame's own size, not the probe's: see [`VideoFrame::width`]. The
    // length is still checked against *something*, because a buffer that does
    // not match its own dimensions means the conversion went wrong and the
    // texture would read past the end — but it is now compared with the frame
    // rather than with a header that can legitimately disagree.
    let expected = frame.width as usize * frame.height as usize * 4;
    let Some(rgba) = frame.rgba() else {
        return None;
    };
    if frame.width == 0 || frame.height == 0 || rgba.len() != expected {
        return None;
    }
    // The one copy left, and it is a property of the caller rather than of the
    // cache: `rumo_render::jni::upload_image` takes the buffer **by value**
    // (`Vec<u8>`) and stores it in its texture registry, so the pixels must be
    // handed over outright here. Everything upstream of this line is a handle —
    // the frame is wrapped once on its way into the cache — so this used to be
    // three whole-frame copies per request and is now one.
    Some((frame.width, frame.height, rgba.to_vec()))
}

/// Remove and drop the source under `handle`; its `fd` may be closed after this
/// returns. No-op for an unknown handle.
pub(crate) fn close_source(handle: i64) {
    let removed = match registry().lock() {
        Ok(mut registry) => registry.remove(&handle),
        Err(_) => return,
    };
    drop(removed);
}

// ---------------------------------------------------------------------------
// Non-JNI entry-point bodies (testable without a JVM)
// ---------------------------------------------------------------------------

/// Probe the clip in `fd` and return its JSON metadata, or `""` on failure.
///
/// `offset`/`length` select a sub-range of the descriptor (`length == -1` means
/// "to the end"). No decoder is opened and no handle is registered. The caller
/// keeps ownership of `fd`.
///
/// A failure answers with `{"ok":false,"error":"..."}` rather than an empty
/// string. An empty string and "the probe said nothing" look identical from
/// Kotlin, which is how a wrong argument here reached a device as a permanent
/// "will retry at render" toast with nothing in any log.
pub(crate) fn probe_json(fd: i32, offset: i64, length: i64) -> String {
    if fd < 0 {
        return probe_error_json("negative descriptor");
    }
    if let Err(e) = check_descriptor(fd) {
        return probe_error_json(&e.to_string());
    }
    match catch_unwind(AssertUnwindSafe(|| probe_video_fd(fd, offset, length))) {
        Ok(Ok(info)) => video_info_json(&info),
        Ok(Err(e)) => probe_error_json(&e.to_string()),
        Err(_) => probe_error_json("the probe panicked"),
    }
}

/// The descriptor half of the pair of reasons a clip will not open.
///
/// `AMediaExtractor_setDataSourceFd` needs a **seekable** descriptor and answers
/// `AMEDIA_ERROR_UNKNOWN` (-10000) when it does not get one. That status names
/// neither the descriptor nor the seekability, so the failure arrives looking
/// like a corrupt container and every attempt to fix it goes looking in the
/// wrong place — which is what happened here. Checking before the platform sees
/// the descriptor turns the only unanswerable status this API has into a
/// sentence.
///
/// Checked at the boundary so it runs on the host: `fd_is_seekable` answers for
/// a descriptor that is not a file (an invalid number is the cheapest negative
/// case std lets us make), and the refusal is therefore testable without a
/// device.
fn check_descriptor(fd: RawFd) -> Result<(), VideoError> {
    if fd < 0 {
        return Err(VideoError::Open("negative descriptor".into()));
    }
    if !crate::video::fd_is_seekable(fd) {
        return Err(VideoError::Open(
            "the descriptor cannot seek, and AMediaExtractor needs a seekable one; a \
             content:// provider that returns a pipe is the usual cause — open the file \
             by path instead"
                .into(),
        ));
    }
    Ok(())
}

/// A failed probe, carrying the reason. See [`probe_json`].
fn probe_error_json(reason: &str) -> String {
    // Escaped by hand rather than by a serializer: this crate has no JSON
    // dependency, and the only thing that needs escaping in an error from the
    // platform is a quote or a backslash.
    let mut escaped = String::with_capacity(reason.len() + 8);
    for ch in reason.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            c => escaped.push(c),
        }
    }
    format!("{{\"ok\":false,\"error\":\"{escaped}\"}}")
}

/// Open the clip in `fd` and register it. Returns a non-zero handle, or `0` on
/// failure (bad fd, undecodable container, poisoned registry). Never panics.
pub(crate) fn open_source(fd: i32, offset: i64, length: i64) -> i64 {
    if check_descriptor(fd).is_err() {
        return 0;
    }
    let opened = catch_unwind(AssertUnwindSafe(|| open_video_fd(fd, offset, length)))
        .ok()
        .and_then(Result::ok);
    match opened {
        Some(source) => register_source(source),
        None => 0,
    }
}

/// JSON metadata of the source under `handle`, or `""` for an unknown handle.
pub(crate) fn handle_info_json(handle: i64) -> String {
    match source_info(handle) {
        Some(info) => video_info_json(&info),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// JNI value conversion
// ---------------------------------------------------------------------------

/// Build a Java string from `text`.
///
/// Never throws: a JNI failure is logged and mapped to the default (null) by
/// [`LogErrorAndDefault`]. If the requested text cannot be materialised, the
/// empty string is attempted before giving up, so the result is null only when
/// the JVM cannot allocate even `""` (OOM with a pending exception).
fn to_java_string(mut env: EnvUnowned<'_>, text: &str) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        match env.new_string(text) {
            Ok(s) => Ok(s.as_raw() as jstring),
            Err(_) => Ok(env.new_string("")?.as_raw() as jstring),
        }
    })
    .resolve::<LogErrorAndDefault>()
}

/// Build a Java byte array from `bytes`; null when the JVM refuses (logged).
fn to_java_bytes(mut env: EnvUnowned<'_>, bytes: &[u8]) -> jbyteArray {
    env.with_env(|env| -> jni::errors::Result<jbyteArray> {
        Ok(env.byte_array_from_slice(bytes)?.as_raw() as jbyteArray)
    })
    .resolve::<LogErrorAndDefault>()
}

// ---------------------------------------------------------------------------
// JNI entry points
// ---------------------------------------------------------------------------

/// com.kerneldroid.rumo.data.RumoBridge.nativeVideoProbeFd (static via @JvmStatic).
///
/// Reads the metadata of the clip visible through `fd` without keeping a
/// decoder open. `offset`/`length` pick a sub-range (`-1` length means "to the
/// end"); the caller keeps ownership of `fd`. Returns the JSON metadata object,
/// or `""` on any failure. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoProbeFd(
    env: EnvUnowned<'_>,
    _class: JClass<'_>,
    fd: jint,
    offset: jlong,
    length: jlong,
) -> jstring {
    let json = probe_json(fd, offset, length);
    to_java_string(env, &json)
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeVideoOpenFd (static via @JvmStatic).
///
/// Opens a decoder for `fd` and registers it under a non-zero handle, or
/// returns `0` on failure. The caller owns `fd` and must keep it open until
/// `nativeVideoClose` has been called for the returned handle. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoOpenFd(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    fd: jint,
    offset: jlong,
    length: jlong,
) -> jlong {
    open_source(fd, offset, length)
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeVideoInfo (static via @JvmStatic).
///
/// JSON metadata of an open clip, or `""` for an unknown handle. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoInfo(
    env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jstring {
    let json = handle_info_json(handle);
    to_java_string(env, &json)
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeVideoFrameAt (static via @JvmStatic).
///
/// The frame covering `time_ms` (clamped to the clip) as `width * height * 4`
/// RGBA8 bytes, served from the decoder's cache when the time is still in it.
/// `time_ms` is in milliseconds from the start. Returns null for an unknown
/// handle or a failed decode. Never throws.
///
/// A codec can report an output size that disagrees with the container's track
/// format (padding without a crop key); rather than hand Java a buffer whose
/// length does not match the advertised frame, such a frame is reported as
/// null. Use [`frame_for_compositor`] when the caller wants the size too.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoFrameAt(
    env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    time_ms: jlong,
) -> jbyteArray {
    // The closure rather than `filter(consistent_frame_len)` directly: the frame
    // is an `Arc`, and the deref from `&Arc<VideoFrame>` to `&VideoFrame`
    // happens at the call inside, where coercion is applied.
    match source_frame(handle, time_ms).filter(|frame| consistent_frame_len(frame)) {
        Some(frame) => match frame.rgba() {
            Some(rgba) => to_java_bytes(env, &rgba),
            None => std::ptr::null_mut(),
        },
        None => std::ptr::null_mut(),
    }
}

/// `true` when the buffer matches the frame's **own** dimensions.
///
/// Measured against the frame rather than the probe, on purpose: the probe reads
/// the track header and the decoder reads the output buffer, and rotation or
/// cropping makes them disagree legitimately. Checking one against the other
/// turned that disagreement into "this clip has no picture", silently, for every
/// frame — and the frame's own size is the only length that actually has to hold,
/// because it is the buffer the texture will read.
fn consistent_frame_len(frame: &VideoFrame) -> bool {
    frame.width != 0
        && frame.height != 0
        && frame.rgba().is_some_and(|rgba| {
            rgba.len() == frame.width as usize * frame.height as usize * 4
        })
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeVideoClose (static via @JvmStatic).
///
/// Removes and drops the decoder; `fd` may be closed afterwards. Idempotent —
/// an unknown handle is a no-op. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoClose(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    close_source(handle);
}

#[cfg(test)]
mod tests {
    // The five `Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideo*` entry
    // points take an `EnvUnowned`, which only a live JVM invoking the symbol can
    // produce, so they are not called here — there is no JVM in this test
    // process. Everything they do except the JNI string/array conversion is
    // covered below through the same helpers they call; `to_java_string` and
    // `to_java_bytes` are one-call wrappers over `Env::new_string` /
    // `Env::byte_array_from_slice` and cannot be exercised without a JVM.
    use super::*;
    use crate::video::{FakeVideoSource, resolve_data_source_length};

    fn sample_info() -> VideoInfo {
        VideoInfo {
            width: 1920,
            height: 1080,
            duration_ms: 12_345,
            fps: 29.97,
            rotation_deg: 90,
            has_audio: true,
        }
    }

    /// A reader that answers with one synthetic planar frame, standing in for a
    /// backend that owns the decoded buffer.
    fn read_padded_i420(_handle: i64, _time_ms: i64) -> Option<Arc<YuvPlanes>> {
        let layout = crate::video::yuv::YuvLayout {
            stride: 8,
            slice_height: 6,
        };
        let bytes: Arc<[u8]> = vec![128u8; 8 * 6 + 2 * 4 * 3].into();
        YuvPlanes::new(
            crate::video::yuv::YuvFormat::I420,
            layout,
            4,
            4,
            crate::video::yuv::Matrix::Bt601,
            crate::video::yuv::Range::Limited,
            0,
            bytes,
        )
        .ok()
        .map(Arc::new)
    }

    /// The whole contract of the plane hand-across, without a device: no reader
    /// means no planes and the converted frame is still there, and installing a
    /// reader turns the same handle into planes.
    ///
    /// Both halves are in one test on purpose — `set_planes_reader` is a
    /// process-wide `OnceLock`, so a reader installed by one test would make a
    /// "no reader" assertion in another depend on test order.
    #[test]
    fn planes_are_optional_and_the_converted_frame_is_not() {
        assert!(
            planes_for_compositor(7, 0).is_none(),
            "with no reader installed the converted path stays the one that runs"
        );
        let handle = register_source(Box::new(FakeVideoSource::with_size(10.0, 2, 4, 4)));
        assert!(
            frame_for_compositor(handle, 0).is_some(),
            "the RGBA8 frame must not depend on the plane path"
        );
        set_planes_reader(read_padded_i420);
        let planes = planes_for_compositor(handle, 0).expect("planes after a reader");
        assert_eq!((planes.width(), planes.height()), (4, 4));
        assert_eq!(planes.bytes().len(), 8 * 6 + 2 * 4 * 3);
        assert_eq!(planes.rotation(), 0);
        close_source(handle);
        assert!(
            planes_for_compositor(handle, 0).is_some(),
            "handle lookup belongs to the reader, so a closed handle is its \
             business rather than this seam's"
        );
    }

    #[test]
    fn info_json_has_the_documented_shape() {
        assert_eq!(
            video_info_json(&sample_info()),
            "{\"width\":1920,\"height\":1080,\"durationMs\":12345,\"fps\":29.97,\"rotationDeg\":90,\"hasAudio\":true}"
        );
    }

    #[test]
    fn info_json_zero_fps_and_no_audio() {
        let info = VideoInfo {
            width: 4,
            height: 4,
            duration_ms: 0,
            fps: 0.0,
            rotation_deg: 0,
            has_audio: false,
        };
        assert_eq!(
            video_info_json(&info),
            "{\"width\":4,\"height\":4,\"durationMs\":0,\"fps\":0,\"rotationDeg\":0,\"hasAudio\":false}"
        );
        // 0.0 fps must survive as a number, not disappear or become a string.
        assert!(video_info_json(&info).contains("\"fps\":0,"));
    }

    #[test]
    fn info_json_keys_are_exact_and_need_no_escapes() {
        let json = video_info_json(&VideoInfo {
            width: 1,
            height: 2,
            duration_ms: 3,
            fps: 4.0,
            rotation_deg: 270,
            has_audio: true,
        });
        for key in [
            "\"width\"",
            "\"height\"",
            "\"durationMs\"",
            "\"fps\"",
            "\"rotationDeg\"",
            "\"hasAudio\"",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
        // Six keys, six colons: no other field sneaked in.
        assert_eq!(json.matches(':').count(), 6, "{json}");
        // Numbers and booleans only, so nothing is ever backslash-escaped.
        assert!(!json.contains('\\'), "unexpected escape in {json}");
        assert!(json.starts_with('{') && json.ends_with('}'), "{json}");
    }

    #[test]
    fn info_json_clamps_non_finite_fps() {
        let json = video_info_json(&VideoInfo {
            fps: f32::NAN,
            ..sample_info()
        });
        assert!(json.contains("\"fps\":0,"), "{json}");
        let json = video_info_json(&VideoInfo {
            fps: f32::INFINITY,
            ..sample_info()
        });
        assert!(json.contains("\"fps\":0,"), "{json}");
    }

    #[test]
    fn registry_register_lookup_remove_is_idempotent() {
        let handle = register_source(Box::new(FakeVideoSource::with_size(10.0, 5, 4, 4)));
        assert!(handle > 0, "register must not return the failure sentinel");

        let info = source_info(handle).expect("registered handle must resolve");
        assert_eq!(info.width, 4);
        assert_eq!(info.height, 4);
        assert_eq!(info.duration_ms, 500);
        assert_eq!(info.fps, 10.0);
        assert!(!info.has_audio);
        assert_eq!(handle_info_json(handle), video_info_json(&info));

        // 250 ms is covered by the fake's frame at 200 ms (first pixel == index).
        let frame = source_frame(handle, 250).expect("frame must decode");
        assert!(consistent_frame_len(&frame), "the frame must match itself");
        assert_eq!(frame.width, 4);
        assert_eq!(frame.height, 4);
        assert_eq!(frame.rgba().unwrap().len(), 4 * 4 * 4);
        assert_eq!(frame.rgba().unwrap()[0], 2);

        close_source(handle);
        assert!(source_info(handle).is_none());
        assert!(source_frame(handle, 0).is_none());
        assert_eq!(handle_info_json(handle), "");
        close_source(handle); // idempotent: an unknown handle is a no-op
    }

    #[test]
    fn handles_are_unique_and_never_zero() {
        let a = register_source(Box::new(FakeVideoSource::new(10.0, 2)));
        let b = register_source(Box::new(FakeVideoSource::new(10.0, 2)));
        assert!(a > 0 && b > 0, "handles must be positive");
        assert_ne!(a, b, "each registration needs its own handle");
        close_source(a);
        close_source(b);
    }

    #[test]
    fn unknown_handles_are_inert() {
        assert!(source_info(0).is_none());
        assert!(source_info(i64::MAX).is_none());
        assert!(source_info(-1).is_none());
        assert!(source_frame(0, 0).is_none());
        assert!(source_frame(i64::MAX, -5).is_none());
        assert_eq!(handle_info_json(0), "");
        // None of these may panic on a missing handle.
        close_source(0);
        close_source(i64::MAX);
    }

    /// The rule that cost four build cycles on a device: a non-positive declared
    /// length means "the rest of the descriptor" and is resolved from the
    /// descriptor's own size, because `FileSource` rewrites a negative length to
    /// zero and the extractor then sniffs nothing and answers -10000.
    #[test]
    fn a_non_positive_length_means_the_rest_of_the_descriptor() {
        assert_eq!(resolve_data_source_length(-1, 0, 124_576).unwrap(), 124_576);
        assert_eq!(resolve_data_source_length(0, 0, 124_576).unwrap(), 124_576);
        // A range that does not start at the beginning keeps its own tail: this
        // is the mdat of the clip that failed on the device.
        assert_eq!(resolve_data_source_length(-1, 3_216, 124_576).unwrap(), 121_360);
        // A caller that states a length is believed, but never past the end.
        assert_eq!(resolve_data_source_length(1_024, 0, 124_576).unwrap(), 1_024);
        assert!(resolve_data_source_length(999_999, 0, 124_576).is_err());
        // A range the descriptor cannot contain is refused by name rather than
        // handed over as an empty source.
        assert!(resolve_data_source_length(-1, 200_000, 124_576).is_err());
        assert!(resolve_data_source_length(-1, 0, 0).is_err());
    }

    /// The two reasons a clip cannot open, both named instead of collapsing into
    /// the platform's `AMEDIA_ERROR_UNKNOWN`. Runs on the host: an invalid
    /// descriptor number is the cheapest negative case there is.
    #[test]
    fn an_unseekable_descriptor_is_named_rather_than_left_to_the_platform() {
        // Not a file descriptor at all: the seek check fails, and the message
        // says so instead of letting the extractor answer -10000.
        let json = probe_json(i32::MAX - 1, 0, -1);
        assert!(
            json.contains("seek") || json.contains("negative"),
            "the probe must say the descriptor is the problem, got {json}"
        );
        assert_eq!(open_source(i32::MAX - 1, 0, -1), 0);

        // A real file *is* seekable, and a seekable descriptor must not be
        // refused — the check exists to name a cause, not to block valid input.
        let path = std::env::temp_dir().join("rumo-seek-probe.bin");
        std::fs::write(&path, [0u8; 32]).expect("write fixture");
        let file = std::fs::File::open(&path).expect("open fixture");
        use std::os::fd::AsRawFd;
        let fd = file.as_raw_fd();
        assert!(crate::video::fd_is_seekable(fd), "a file must be seekable");
        assert!(check_descriptor(fd).is_ok(), "a real file must be accepted");
        let _ = std::fs::remove_file(&path);
    }

    /// A bad descriptor is answered with the reason, not with silence — see
    /// [`probe_error_json`]. `open_source` has no channel for a reason, so it
    /// still answers `0` and the log has to come from the caller.
    #[test]
    fn failures_map_to_a_reason_or_a_zero() {
        for fd in [-1, i32::MIN] {
            let json = probe_json(fd, 0, -1);
            assert!(
                json.contains("\"ok\":false") && json.contains("negative descriptor"),
                "a rejected descriptor must say why, got {json}"
            );
            assert_eq!(open_source(fd, 0, -1), 0);
        }
        assert_eq!(probe_json(-1, 0, -1), probe_json(i32::MIN, 0, -1));

        #[cfg(not(target_os = "android"))]
        {
            // Descriptor 0 exists in the test process but is not a video clip,
            // so the probe answers with a reason rather than with nothing.
            let json = probe_json(0, 0, -1);
            assert!(json.contains("\"ok\":false"), "got {json}");
            assert_eq!(open_source(0, 0, -1), 0);
            assert_eq!(open_source(0, 1 << 20, 64), 0);
        }
    }
}
