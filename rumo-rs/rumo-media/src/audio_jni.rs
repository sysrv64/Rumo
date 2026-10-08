// SPDX-License-Identifier: Apache-2.0
#![allow(non_snake_case)]

//! JNI entry points for audio playback.
//!
//! These belong to `com.kerneldroid.rumo.data.RumoBridge` but live in the
//! media crate so the playback pipeline stays together. They are thin
//! wrappers over [`crate::audio::AudioPlayer`]; all blocking work happens
//! on the player's own control thread, never on the JNI thread (no
//! `pollster::block_on` here).
//!
//! Players are kept in a process-global registry keyed by a monotonic
//! handle id. A failed open yields handle `0`.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use jni::EnvUnowned;
use jni::objects::{JByteArray, JClass, Reference as _};
use jni::sys::{jdouble, jint, jlong, jstring};

use crate::audio::{AudioPlayer, decode_audio_file};

/// `handle -> player`; dropping a player stops and joins its stream.
type PlayerRegistry = Mutex<HashMap<i64, AudioPlayer>>;

fn registry() -> &'static PlayerRegistry {
    static REGISTRY: OnceLock<PlayerRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Handle ids start at 1 so `0` stays a valid "open failed" sentinel.
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

/// Run `f` against the player registered under `handle`, if any.
fn with_player<T>(handle: i64, f: impl FnOnce(&AudioPlayer) -> T) -> Option<T> {
    let registry = registry().lock().ok()?;
    registry.get(&handle).map(f)
}

/// Decode the audio visible through `fd` and start a paused player.
/// Returns the handle, or `0` on any failure (bad fd, decode, no device).
pub fn open_audio_fd(fd: i32) -> i64 {
    if fd < 0 {
        return 0;
    }
    let path = format!("/proc/self/fd/{fd}");
    let audio = match decode_audio_file(&path) {
        Ok(audio) => audio,
        Err(_) => return 0,
    };
    let player = match AudioPlayer::open(audio) {
        Ok(player) => player,
        Err(_) => return 0,
    };
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let Ok(mut registry) = registry().lock() else {
        return 0;
    };
    registry.insert(handle, player);
    handle
}

/// Duration of the player in seconds, or `-1.0` for an unknown handle.
pub fn player_duration(handle: i64) -> f64 {
    with_player(handle, AudioPlayer::duration_seconds).unwrap_or(-1.0)
}

/// Playback position in seconds, or `-1.0` for an unknown handle.
pub fn player_position(handle: i64) -> f64 {
    with_player(handle, AudioPlayer::position_seconds).unwrap_or(-1.0)
}

/// Start/resume the player. No-op for an unknown handle.
pub fn player_play(handle: i64) {
    let _ = with_player(handle, |player| player.play());
}

/// Pause the player. No-op for an unknown handle.
pub fn player_pause(handle: i64) {
    let _ = with_player(handle, |player| player.pause());
}

/// Seek the player to `seconds`. No-op for an unknown handle.
pub fn player_seek(handle: i64, seconds: f64) {
    let _ = with_player(handle, |player| player.seek(seconds));
}

/// Stop and drop the player. No-op for an unknown handle.
pub fn player_close(handle: i64) {
    let removed = match registry().lock() {
        Ok(mut registry) => registry.remove(&handle),
        Err(_) => return,
    };
    drop(removed);
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeAudioOpen (static via @JvmStatic).
///
/// `fd` is a file descriptor opened by Kotlin (read through
/// `/proc/self/fd/<fd>`). Returns a non-zero player handle, or `0` on
/// failure. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioOpen(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    fd: jint,
) -> jlong {
    open_audio_fd(fd) as jlong
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeAudioDuration (static via @JvmStatic).
///
/// Track duration in seconds, or `-1.0` for an unknown handle. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioDuration(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jdouble {
    player_duration(handle) as jdouble
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeAudioPlay (static via @JvmStatic).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioPlay(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    player_play(handle);
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeAudioPause (static via @JvmStatic).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioPause(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    player_pause(handle);
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeAudioSeek (static via @JvmStatic).
///
/// `seconds` past the end clamp to the end; negative values clamp to zero.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioSeek(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    seconds: jdouble,
) {
    player_seek(handle, seconds);
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeAudioPosition (static via @JvmStatic).
///
/// Playback position in seconds, or `-1.0` for an unknown handle. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioPosition(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jdouble {
    player_position(handle) as jdouble
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeAudioClose (static via @JvmStatic).
///
/// Stops and releases the player. No-op for an unknown handle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioClose(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    player_close(handle);
}

/// Minimal JSON string escaping: a decode error can quote a path or a codec
/// name, so the message must not be able to break the object. Local, because
/// this crate has no shared helper and the render crate's copy is private.
fn json_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if (c as u32) < 0x20 => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped
}

/// The failure answer for a decode error or a caught panic.
fn error_json(message: &str) -> String {
    format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(message))
}

/// A JSON-safe float: `NaN`/infinities are not valid JSON, so they become 0.
fn json_float(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// The JSON object the Kotlin side parses out of one beat analysis,
/// `{"ok":true,...}` or `{"ok":false,"error":...}`.
fn beats_json(bytes: &[u8]) -> String {
    let analysis = match crate::beats::analyze_audio_bytes(bytes) {
        Ok(analysis) => analysis,
        Err(error) => return error_json(&error.to_string()),
    };
    let bpm = match analysis.bpm {
        Some(bpm) => format!("{:.1}", json_float(bpm)),
        None => "null".to_string(),
    };
    let beats: Vec<String> = analysis
        .beats
        .iter()
        .map(|beat| {
            format!(
                "{{\"t\":{},\"strength\":{:.4}}}",
                beat.time_ms,
                json_float(beat.strength)
            )
        })
        .collect();
    format!(
        "{{\"ok\":true,\"bpm\":{},\"confidence\":\"{}\",\"sampleRate\":{},\"durationMs\":{},\"peak\":{:.6},\"beats\":[{}]}}",
        bpm,
        analysis.confidence.as_str(),
        analysis.sample_rate,
        analysis.duration_ms,
        json_float(analysis.peak),
        beats.join(",")
    )
}

/// The JSON object the Kotlin side parses out of one speech analysis,
/// `{"ok":true,...}` or `{"ok":false,"error":...}`.
fn speech_json(bytes: &[u8]) -> String {
    let analysis = match crate::vad::analyze_audio_bytes(bytes) {
        Ok(analysis) => analysis,
        Err(error) => return error_json(&error.to_string()),
    };
    let segments: Vec<String> = analysis
        .segments
        .iter()
        .map(|segment| {
            format!(
                "{{\"startMs\":{},\"endMs\":{}}}",
                segment.start_ms, segment.end_ms
            )
        })
        .collect();
    let silences: Vec<String> = analysis
        .silences
        .iter()
        .map(|silence| {
            format!(
                "{{\"startMs\":{},\"endMs\":{}}}",
                silence.start_ms, silence.end_ms
            )
        })
        .collect();
    let cut_points: Vec<String> = analysis
        .cut_points
        .iter()
        .map(|cut| cut.to_string())
        .collect();
    format!(
        "{{\"ok\":true,\"sampleRate\":{},\"durationMs\":{},\"onsetThreshold\":{:.2},\"offsetThreshold\":{:.2},\"speechRatio\":{:.4},\"segments\":[{}],\"silences\":[{}],\"cutPoints\":[{}]}}",
        analysis.sample_rate,
        analysis.duration_ms,
        json_float(analysis.onset_threshold),
        json_float(analysis.offset_threshold),
        json_float(analysis.speech_ratio),
        segments.join(","),
        silences.join(","),
        cut_points.join(",")
    )
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeAudioAnalyzeBeats (static via @JvmStatic).
///
/// Decodes the whole audio file in `bytes` and returns the beat analysis as
/// JSON. A decode failure is reported as `{"ok":false,"error":...}` and a panic
/// is caught and reported the same way, so a panic can never unwind into the
/// JVM. Never throws: a JNI-level failure (the JVM cannot materialise the
/// string) is logged and yields null, the same policy as the video metadata
/// entry points.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioAnalyzeBeats(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    bytes: JByteArray<'_>,
) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        let data = env.convert_byte_array(&bytes)?;
        let json = catch_unwind(AssertUnwindSafe(|| beats_json(&data)))
            .unwrap_or_else(|_| error_json("panic during beat analysis"));
        Ok(env.new_string(&json)?.as_raw() as jstring)
    })
    .resolve::<jni::errors::LogErrorAndDefault>()
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeAudioAnalyzeSpeech (static via @JvmStatic).
///
/// Decodes the whole audio file in `bytes` and returns the speech/silence
/// segmentation as JSON. A decode failure is reported as
/// `{"ok":false,"error":...}` and a panic is caught and reported the same way,
/// so a panic can never unwind into the JVM. Never throws: a JNI-level failure
/// is logged and yields null, the same policy as the beat analysis entry point.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioAnalyzeSpeech(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    bytes: JByteArray<'_>,
) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        let data = env.convert_byte_array(&bytes)?;
        let json = catch_unwind(AssertUnwindSafe(|| speech_json(&data)))
            .unwrap_or_else(|_| error_json("panic during speech analysis"));
        Ok(env.new_string(&json)?.as_raw() as jstring)
    })
    .resolve::<jni::errors::LogErrorAndDefault>()
}

/// Anchor that keeps the `#[no_mangle]` JNI symbols reachable when this
/// crate is linked as an rlib into the `rumo-bridge` cdylib, so
/// `--gc-sections` does not discard them before the Java linker sees them.
#[allow(dead_code)]
struct JniAnchor(*const ());

// SAFETY: the pointer is never read or dereferenced; it only pins code.
unsafe impl Sync for JniAnchor {}

#[used]
static AUDIO_JNI_ANCHORS: [JniAnchor; 9] = [
    JniAnchor(Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioOpen as *const ()),
    JniAnchor(Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioDuration as *const ()),
    JniAnchor(Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioPlay as *const ()),
    JniAnchor(Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioPause as *const ()),
    JniAnchor(Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioSeek as *const ()),
    JniAnchor(Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioPosition as *const ()),
    JniAnchor(Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioClose as *const ()),
    JniAnchor(Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioAnalyzeBeats as *const ()),
    JniAnchor(Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioAnalyzeSpeech as *const ()),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_handles_are_inert() {
        assert_eq!(player_duration(0), -1.0);
        assert_eq!(player_duration(i64::MAX), -1.0);
        assert_eq!(player_position(0), -1.0);
        // None of these may panic on a missing handle.
        player_play(0);
        player_pause(0);
        player_seek(0, 1.0);
        player_close(0);
    }

    #[test]
    fn open_bad_fd_returns_zero() {
        assert_eq!(open_audio_fd(-1), 0);
        assert_eq!(open_audio_fd(i32::MIN), 0);
        // No file is open at this descriptor in the test process.
        assert_eq!(open_audio_fd(4096), 0);
    }

    /// Minimal 44-byte PCM WAV of `samples` zero samples, 16-bit mono.
    fn silent_wav(sample_rate: u32, samples: u32) -> Vec<u8> {
        let data_len = samples * 2;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend(std::iter::repeat_n(0u8, data_len as usize));
        wav
    }

    #[test]
    fn speech_json_has_the_documented_shape() {
        let json = speech_json(&silent_wav(16_000, 16_000));
        assert!(json.contains("\"ok\":true"), "{json}");
        assert!(json.contains("\"sampleRate\":16000"), "{json}");
        assert!(json.contains("\"onsetThreshold\":0.60"), "{json}");
        assert!(json.contains("\"offsetThreshold\":0.40"), "{json}");
        assert!(json.contains("\"segments\":[]"), "{json}");
        assert!(json.contains("\"silences\":[{\"startMs\":0,\"endMs\":1000}]"), "{json}");
        assert!(json.contains("\"cutPoints\":[]"), "{json}");
    }

    #[test]
    fn speech_json_reports_errors_without_panicking() {
        let json = speech_json(b"not audio at all................");
        assert!(json.contains("\"ok\":false"), "{json}");
        assert!(json.contains("\"error\""), "{json}");
    }
}
