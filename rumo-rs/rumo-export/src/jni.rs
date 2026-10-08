// SPDX-License-Identifier: Apache-2.0

//! JNI entry points for the MP4 export pipeline.
//!
//! Registered in `com.kerneldroid.rumo.data.RumoBridge` — see the Kotlin
//! declarations in the project notes. All work is synchronous and runs on
//! the calling JNI thread: there is deliberately no `block_on` and no
//! async runtime here. A registry keyed by handle guards the live exporters.

use crate::audio::AudioSource;
use crate::backend::PlatformBackend;
use crate::error::ExportError;
use crate::exporter::{Exporter, ExporterConfig};
use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{
    JClass, JFloatArray, JIntArray, JLongArray, JString, Reference as _,
};
use jni::sys::{jfloat, jint, jlong, jstring};
use rumo_media::AudioError;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{LazyLock, Mutex};

/// `nativeExportWriteFrame` / `nativeExportEnd` success.
pub const EXPORT_OK: jint = 0;
/// Unknown/expired handle.
pub const EXPORT_ERR_HANDLE: jint = -1;
/// Bad argument (frame size, dimensions, fps, ...).
pub const EXPORT_ERR_ARG: jint = -2;
/// Encoder/muxer failure.
pub const EXPORT_ERR_ENCODE: jint = -3;
/// `nativeExportAudioTrack` found no audio in the source: a silent file is a
/// valid project state, **not** a failure (docs/11 §11.5), so it is deliberately
/// a positive code and never lands in `nativeExportLastError`.
pub const EXPORT_AUDIO_NO_TRACK: jint = 1;
/// `nativeExportWriteFrameGpu` could not produce the frame on the GPU.
///
/// Positive like [`EXPORT_AUDIO_NO_TRACK`], and for the same reason: this is not
/// a failure of the export. The caller renders that one frame through the CPU
/// path (`nativeRenderPreviewEx` + `nativeExportWriteFrameArgb`) and the export
/// continues; the reason is in the diagnostics log, not in
/// `nativeExportLastError`.
/// `nativeExportWriteFrameGpu` could not produce the frame on the GPU.
///
/// A real failure since §12.4 deleted the CPU export route: there is no second
/// way to encode a frame, so this is reported like any other and the export
/// stops with the reason in `nativeExportLastError`. It used to be a positive
/// code telling the caller to fall back; there is nothing left to fall back to.
pub const EXPORT_ERR_GPU: jint = -4;

/// A live exporter plus its scratch buffers and a `Send` marker.
///
/// SAFETY: every access goes through `EXPORTERS`, which serializes all JNI
/// calls; the Android backend never registers a MediaCodec async-notify
/// callback, so the underlying codec has no thread affinity of its own.
struct ExportSlot {
    exporter: Exporter<PlatformBackend>,
}

// SAFETY: see `ExportSlot` docs — access is mutex-serialized.
unsafe impl Send for ExportSlot {}

static EXPORTERS: LazyLock<Mutex<HashMap<jlong, ExportSlot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

fn bad() -> jni::errors::Error {
    jni::errors::Error::JniCall(jni::errors::JniError::Unknown)
}

fn last_error_slot() -> &'static Mutex<String> {
    static LAST: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
    &LAST
}

/// Code and text of the last recorded export failure, so a per-frame failure
/// loop records one entry instead of one per frame.
fn last_failure_slot() -> &'static Mutex<(&'static str, String)> {
    static LAST: LazyLock<Mutex<(&'static str, String)>> =
        LazyLock::new(|| Mutex::new(("", String::new())));
    &LAST
}

/// Record an export failure: the reason the user needs, in the single place the
/// UI reads (`nativeExportLastError`) and the diagnostics log file picks up.
///
/// Every failure here used to collapse into `map_err(|_| bad())` or
/// `EXPORT_ERR_ENCODE`, and the Kotlin caller then swallowed even that — which
/// is the only reason a failed export ever surfaced as the guess
/// "Export failed (engine unavailable?)".
fn export_failed(code: &'static str, text: impl Into<String>) {
    let text = text.into();
    *last_error_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = text.clone();
    // A frame that keeps failing would otherwise append one entry per frame and
    // push everything else out of the bounded ring.
    let repeat = {
        let mut last = last_failure_slot()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let repeat = last.0 == code && last.1 == text;
        *last = (code, text.clone());
        repeat
    };
    if !repeat {
        rumo_render::diag::error(code, text);
    }
}

/// Start of a new export: log it and clear the previous run's error, so
/// `nativeExportLastError` always describes the run the user just watched.
fn export_started(text: impl Into<String>) {
    *last_error_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = String::new();
    *last_failure_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = ("", String::new());
    rumo_render::diag::info("export", text);
}

/// A successful later step. Deliberately does **not** clear the error: a frame
/// that failed mid-export must still be reported even when the finalize that
/// follows it succeeds.
fn export_ok(text: impl Into<String>) {
    rumo_render::diag::info("export", text);
}

/// Log an event that is *not* an export failure.
///
/// One undecodable audio source must not turn into "Export failed" in the UI,
/// but it must not vanish either: this is the difference between
/// [`export_failed`] and the diagnostics log (docs/11 §11.5).
fn export_warn(code: &'static str, text: impl Into<String>) {
    rumo_render::diag::error(code, text);
}

/// `RumoBridge.nativeExportLastError(): String`
///
/// The reason the most recent export step failed, or `""` when the last one
/// succeeded. Never null, never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeExportLastError(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
) -> jstring {
    let text = last_error_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    env.with_env(|env| -> jni::errors::Result<jstring> {
        Ok(env.new_string(&text)?.as_raw() as jstring)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeExportBegin(outPath, width, height, fps, bitrate)`
///
/// Opens an H.264/MP4 exporter and returns a positive handle, or `0` after
/// throwing `java/lang/RuntimeException`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeExportBegin(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    out_path: JString<'_>,
    width: jint,
    height: jint,
    fps: jfloat,
    bitrate: jint,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        let out_path = out_path.try_to_string(env)?;
        if width <= 0 || height <= 0 || !(fps > 0.0) || bitrate <= 0 {
            export_failed(
                "export_bad_arg",
                format!("rejected config: {width}x{height}, fps={fps}, bitrate={bitrate}"),
            );
            return Err(bad());
        }
        let config = ExporterConfig::new(
            out_path,
            width as u32,
            height as u32,
            fps as f64,
            bitrate as u32,
        );
        let exporter = match Exporter::new(config) {
            Ok(exporter) => exporter,
            Err(err) => {
                // The real reason: encoder creation, unsupported resolution,
                // muxer setup — the text the UI must show instead of a guess.
                export_failed("export_begin", format!("{err}"));
                return Err(bad());
            }
        };
        export_started(format!(
            "encoder opened: {width}x{height} @{fps}fps {bitrate}bps"
        ));
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        EXPORTERS.lock().map_err(|_| bad())?.insert(
            handle,
            ExportSlot { exporter },
        );
        Ok(handle)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// Decode the source behind `fd` into interleaved PCM.
///
/// `symphonia` goes first: it covers the audio files a project imports and the
/// AAC/M4A inside an MP4, and it is the same decoder the player uses. A clip in
/// a codec it does not demux (AMR, AC-3, ...) is then handed to the platform —
/// `MediaExtractor` knows every codec the device supports, which is the audio
/// track `rumo-media` selects next to the video one.
#[cfg(not(target_os = "android"))]
fn decode_source_audio(fd: jint) -> Result<rumo_media::DecodedAudio, AudioError> {
    let path = format!("/proc/self/fd/{fd}");
    rumo_media::decode_audio_file(&path)
}

/// Decode the source behind `fd`; see the host variant for the policy.
#[cfg(target_os = "android")]
fn decode_source_audio(fd: jint) -> Result<rumo_media::DecodedAudio, AudioError> {
    match decode_source_audio_symphonia(fd) {
        Ok(audio) => Ok(audio),
        // "symphonia cannot decode this" covers both "not audio" and "codec I do
        // not know"; the platform tells the two apart.
        Err(err @ AudioError::Unsupported(_)) => {
            match rumo_media::video::decode_audio_track_fd(fd, 0, -1) {
                Ok(audio) => Ok(audio),
                Err(rumo_media::VideoError::NoAudioTrack) => Err(err),
                Err(other) => Err(AudioError::Decode(format!("platform decode: {other}"))),
            }
        }
        Err(err) => Err(err),
    }
}

#[cfg(target_os = "android")]
fn decode_source_audio_symphonia(fd: jint) -> Result<rumo_media::DecodedAudio, AudioError> {
    let path = format!("/proc/self/fd/{fd}");
    rumo_media::decode_audio_file(&path)
}

/// `RumoBridge.nativeExportAudioTrack(handle, fd, startMs, durationMs, gain)`
///
/// Decode the source behind `fd` (a file descriptor Kotlin opened on the media
/// file; read through `/proc/self/fd/<fd>`), mix it into the export's single
/// audio track with its own `startMs`/`gain`, and encode the result to AAC right
/// away (docs/11 §11.5).
///
/// Return codes: [`EXPORT_OK`] mixed, [`EXPORT_AUDIO_NO_TRACK`] the file has no
/// audio (ordinary, and never a failure), [`EXPORT_ERR_ARG`] a bad argument,
/// [`EXPORT_ERR_ENCODE`] decode/mix/encode failed — which skips this source and
/// leaves the export running, [`EXPORT_ERR_HANDLE`] unknown handle. Never
/// throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeExportAudioTrack(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    fd: jint,
    start_ms: jlong,
    duration_ms: jlong,
    gain: jfloat,
) -> jint {
    if fd < 0 || duration_ms < 0 || !gain.is_finite() || gain < 0.0 {
        export_failed(
            "export_audio_arg",
            format!("rejected audio source: fd={fd}, durationMs={duration_ms}, gain={gain}"),
        );
        return EXPORT_ERR_ARG;
    }
    // The registry mutex already serializes every export JNI call, so holding it
    // across the decode costs nothing the frame path was not waiting for anyway.
    let Ok(mut guard) = EXPORTERS.lock() else {
        return EXPORT_ERR_HANDLE;
    };
    let Some(slot) = guard.get_mut(&handle) else {
        export_failed("export_audio", format!("unknown export handle {handle}"));
        return EXPORT_ERR_HANDLE;
    };
    let audio = match decode_source_audio(fd) {
        Ok(audio) => audio,
        Err(AudioError::Unsupported(what)) => {
            export_ok(format!("source fd {fd} has no usable audio track: {what}"));
            return EXPORT_AUDIO_NO_TRACK;
        }
        Err(err) => {
            export_warn(
                "export_audio_source",
                format!("skipped audio source fd {fd}: {err}"),
            );
            return EXPORT_ERR_ENCODE;
        }
    };
    let source = AudioSource::new(audio, start_ms, duration_ms, gain);
    match slot.exporter.add_audio_source(source) {
        Ok(true) => {
            export_ok(format!(
                "audio source mixed: fd={fd} at {start_ms}ms, gain={gain}"
            ));
            EXPORT_OK
        }
        Ok(false) => {
            export_ok(format!("audio source fd {fd} contributed no frames"));
            EXPORT_AUDIO_NO_TRACK
        }
        Err(err) => {
            // The source is skipped, not the export: the video track and every
            // other sound must still make it into the file.
            export_warn(
                "export_audio_source",
                format!("skipped audio source fd {fd}: {err}"),
            );
            EXPORT_ERR_ENCODE
        }
    }
}

/// Turn an encode result into a JNI code, recording the reason either way.
fn encode_code(result: crate::error::Result<()>) -> jint {
    match result {
        Ok(()) => EXPORT_OK,
        Err(err @ (ExportError::InvalidFrameLen { .. } | ExportError::InvalidConfig(_))) => {
            export_failed("export_write", format!("{err}"));
            EXPORT_ERR_ARG
        }
        Err(err) => {
            export_failed("export_write", format!("{err}"));
            EXPORT_ERR_ENCODE
        }
    }
}

/// `RumoBridge.nativeExportWriteFrameGpu(handle, width, height, bgArgb, …scene
/// arrays…, timeMs, ptsUs)`
///
/// Render one frame of the extended scene **and encode it**, with the frame
/// never leaving the GPU: the scene is composited into an offscreen target, a
/// compute pass packs that target into NV12, and the bytes go straight to the
/// encoder (docs/12 §12.3). The arguments are exactly those of
/// `nativeRenderPreviewEx`, plus the handle and the timestamp, and both entries
/// read them through the same reader — the exported frame is the frame the
/// preview shows.
///
/// Return codes: [`EXPORT_OK`], [`EXPORT_ERR_ARG`] a bad argument or an
/// unreadable scene, [`EXPORT_ERR_HANDLE`] unknown handle, [`EXPORT_ERR_ENCODE`]
/// encoder failure, and [`EXPORT_GPU_UNAVAILABLE`] — the GPU could not produce
/// the frame, which is the caller's cue to use the CPU path for this one frame.
/// Never throws.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeExportWriteFrameGpu(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    width: jint,
    height: jint,
    bg_argb: jint,
    ordinals: JIntArray<'_>,
    argbs: JIntArray<'_>,
    dxs: JFloatArray<'_>,
    dys: JFloatArray<'_>,
    rotations: JFloatArray<'_>,
    alphas: JFloatArray<'_>,
    shape_scales: JFloatArray<'_>,
    text_handles: JLongArray<'_>,
    text_x: JFloatArray<'_>,
    text_y: JFloatArray<'_>,
    text_argb: JIntArray<'_>,
    text_alpha: JFloatArray<'_>,
    text_rot: JFloatArray<'_>,
    tex_ids: JLongArray<'_>,
    tex_x: JFloatArray<'_>,
    tex_y: JFloatArray<'_>,
    tex_w: JFloatArray<'_>,
    tex_h: JFloatArray<'_>,
    tex_alpha: JFloatArray<'_>,
    layer_starts: JLongArray<'_>,
    layer_durations: JLongArray<'_>,
    text_starts: JLongArray<'_>,
    text_durations: JLongArray<'_>,
    tex_starts: JLongArray<'_>,
    tex_durations: JLongArray<'_>,
    text_scales: JFloatArray<'_>,
    // Global draw order, one value per draw in each group. Without these the
    // export would draw in the engine's old group order (shapes, then text,
    // then pictures) while the preview honours the layer list — the exported
    // video would disagree with what the user arranged on screen.
    shape_orders: JIntArray<'_>,
    text_orders: JIntArray<'_>,
    tex_orders: JIntArray<'_>,
    effects_json: JString<'_>,
    time_ms: jlong,
    pts_us: jlong,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        let bad = || jni::errors::Error::JniCall(jni::errors::JniError::Unknown);
        if width <= 0 || height <= 0 {
            export_failed(
                "export_write_gpu",
                format!("rejected frame size {width}x{height}"),
            );
            return Ok(EXPORT_ERR_ARG);
        }
        let scene = match rumo_render::jni::read_ex_scene_ordered(
            env,
            width as u32,
            height as u32,
            bg_argb as u32,
            time_ms,
            &ordinals,
            &argbs,
            &dxs,
            &dys,
            &rotations,
            &alphas,
            &shape_scales,
            &text_handles,
            &text_x,
            &text_y,
            &text_argb,
            &text_alpha,
            &text_rot,
            &tex_ids,
            &tex_x,
            &tex_y,
            &tex_w,
            &tex_h,
            &tex_alpha,
            &layer_starts,
            &layer_durations,
            &text_starts,
            &text_durations,
            &tex_starts,
            &tex_durations,
            &text_scales,
            Some(&shape_orders),
            Some(&text_orders),
            Some(&tex_orders),
            &effects_json,
        ) {
            Ok(scene) => scene,
            Err(err) => {
                export_failed("export_write_gpu", format!("cannot read the scene: {err}"));
                return Ok(EXPORT_ERR_ARG);
            }
        };
        let request =
            rumo_render::jni::ex_frame_request(width as u32, height as u32, &scene, time_ms);
        // The frame is rendered and packed before the registry is locked: that
        // mutex serializes every export call, and holding it across a GPU frame
        // would make an unrelated `nativeExportLastError` wait for the render.
        let nv12 = match rumo_render::gpu_worker::render_nv12(
            request,
            crate::nv12::BT601_FULL,
            rumo_render::gpu_worker::DEFAULT_TIMEOUT,
        ) {
            Ok(nv12) => nv12,
            Err(why) => {
                // A real failure now: the CPU route this used to hand the frame
                // to is gone (§12.4), so there is no second way to encode. The
                // reason goes to `nativeExportLastError` through the same path
                // as every other failure, and the export stops instead of
                // producing a file with a different renderer's frames in it.
                export_failed("export_gpu_nv12", why);
                return Ok(EXPORT_ERR_GPU);
            }
        };
        let mut guard = EXPORTERS.lock().map_err(|_| bad())?;
        let Some(slot) = guard.get_mut(&handle) else {
            export_failed("export_write", format!("unknown export handle {handle}"));
            return Ok(EXPORT_ERR_HANDLE);
        };
        let pts = if pts_us < 0 {
            slot.exporter.next_pts_us()
        } else {
            pts_us
        };
        Ok(encode_code(slot.exporter.write_frame_nv12(&nv12, pts)))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `RumoBridge.nativeExportEnd(handle)`
///
/// Signals EOS, drains the encoder and finalizes the MP4. Removes the
/// handle from the registry. Returns [`EXPORT_OK`] or a negative code.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeExportEnd(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jint {
    env.with_env(|_env| -> jni::errors::Result<jint> {
        let mut guard = EXPORTERS.lock().map_err(|_| bad())?;
        let Some(slot) = guard.remove(&handle) else {
            export_failed("export_end", format!("unknown export handle {handle}"));
            return Ok(EXPORT_ERR_HANDLE);
        };
        Ok(match slot.exporter.finish() {
            Ok(()) => {
                export_ok("MP4 finalized");
                EXPORT_OK
            }
            Err(err) => {
                export_failed("export_end", format!("{err}"));
                EXPORT_ERR_ENCODE
            }
        })
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
