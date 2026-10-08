// SPDX-License-Identifier: Apache-2.0

//! Process-wide render diagnostics: a bounded ring of what the GPU path did,
//! and a truthful record of which path actually produced the last frame.
//!
//! # Why this exists
//!
//! The compositor can fall back from the GPU to the CPU reference for a dozen
//! different reasons — no adapter, device-request rejection, a swapchain that
//! refuses to configure, a driver that rejects one effect's pipeline, a worker
//! timeout. Historically every one of those was silent: the caller got `None`
//! or a negative `ENGINE_ERR_*` and the user saw a small `CPU` badge with no
//! explanation, on hardware that is perfectly capable of rendering on the GPU.
//!
//! So every fallible step of GPU setup and every engine-path failure records an
//! entry here, and [`json`] is exposed to the app verbatim so the UI can show
//! the reason. The rule for contributors: **if you add a path that can fall
//! back to the CPU, record why.**

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Maximum retained entries; older ones are dropped from the front.
pub const MAX_ENTRIES: usize = 256;

/// Severity of one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Normal progress, including the successful GPU setup steps.
    Info,
    /// Something was skipped or degraded but rendering continued.
    Warn,
    /// A step failed; the frame came from a slower or different path.
    Error,
}

impl Level {
    /// Stable string used in the JSON report.
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// One recorded event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Monotonic sequence number, so the UI can tell entries apart.
    pub seq: u64,
    /// Severity.
    pub level: Level,
    /// Stable machine-readable code (never localised).
    pub code: &'static str,
    /// Human-readable detail, usually an error string straight from wgpu.
    pub text: String,
}

/// Everything the UI needs, as a plain structure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// `true` when the last completed frame was produced on the GPU.
    pub path_gpu: bool,
    /// Adapter name, empty when no GPU device was created.
    pub adapter: String,
    /// Graphics backend (`"Vulkan"`, `"Gl"`, …), empty when unknown.
    pub backend: String,
    /// One-line summary of what the user should look at.
    pub hint: String,
    /// Effect kinds whose shader pipeline could not be built.
    pub rejected_effects: Vec<String>,
    /// Entries, oldest first.
    pub entries: Vec<Entry>,
}

struct Inner {
    entries: VecDeque<Entry>,
    seq: u64,
    adapter: String,
    backend: String,
    hint: String,
    rejected_effects: Vec<String>,
    /// Code of the last CPU-fallback reason, so a per-frame fallback does not
    /// flood the ring with 60 identical entries per second.
    last_fallback_code: &'static str,
    /// True while the device is present, so `set_adapter` is recorded once.
    adapter_recorded: bool,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            seq: 0,
            adapter: String::new(),
            backend: String::new(),
            hint: String::new(),
            rejected_effects: Vec::new(),
            last_fallback_code: "",
            adapter_recorded: false,
        }
    }
}

fn inner() -> &'static Mutex<Inner> {
    static INNER: OnceLock<Mutex<Inner>> = OnceLock::new();
    INNER.get_or_init(|| Mutex::new(Inner::default()))
}

fn last_error_slot() -> &'static Mutex<String> {
    static LAST: OnceLock<Mutex<String>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(String::new()))
}

static PATH_GPU: AtomicBool = AtomicBool::new(false);
static ERROR_COUNT: AtomicU64 = AtomicU64::new(0);
/// Whether the Surface engine produced its last frame on the GPU.
static ENGINE_OK: AtomicBool = AtomicBool::new(false);
/// Whether the offscreen preview produced its last frame on the GPU.
static PREVIEW_OK: AtomicBool = AtomicBool::new(false);

fn with_inner<R>(f: impl FnOnce(&mut Inner) -> R) -> R {
    let mut guard = inner().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

/// Append one entry.
pub fn record(level: Level, code: &'static str, text: impl Into<String>) {
    let text = text.into();
    with_inner(|state| {
        state.seq += 1;
        let seq = state.seq;
        state.entries.push_back(Entry {
            seq,
            level,
            code,
            text: text.clone(),
        });
        while state.entries.len() > MAX_ENTRIES {
            state.entries.pop_front();
        }
    });
    // Mirror to the platform log so a `logcat` user sees the same story.
    match level {
        Level::Info => log_info(code, &text),
        Level::Warn => log_warn(code, &text),
        Level::Error => log_error(code, &text),
    }
}

/// [`record`] at [`Level::Info`].
pub fn info(code: &'static str, text: impl Into<String>) {
    record(Level::Info, code, text);
}

/// [`record`] at [`Level::Warn`].
pub fn warn(code: &'static str, text: impl Into<String>) {
    record(Level::Warn, code, text);
}

/// [`record`] at [`Level::Error`].
pub fn error(code: &'static str, text: impl Into<String>) {
    record(Level::Error, code, text);
}

#[cfg(target_os = "android")]
fn log_info(code: &str, text: &str) {
    ndk_log(3, code, text, false);
}

#[cfg(target_os = "android")]
fn log_warn(code: &str, text: &str) {
    ndk_log(4, code, text, false);
}

#[cfg(target_os = "android")]
fn log_error(code: &str, text: &str) {
    ndk_log(6, code, text, false);
}

#[cfg(target_os = "android")]
fn ndk_log(priority: i32, code: &str, text: &str, _fatal: bool) {
    use std::ffi::CString;
    // `__android_log_write` from liblog is already linked by every Android
    // process; going through it avoids depending on the `android_logger`
    // crate just for this.
    unsafe extern "C" {
        fn __android_log_write(prio: i32, tag: *const std::os::raw::c_char, text: *const std::os::raw::c_char) -> i32;
    }
    let tag = CString::new(format!("Rumo/{code}")).unwrap_or_default();
    let msg = CString::new(text.replace('\0', " ")).unwrap_or_default();
    // SAFETY: both pointers come from `CString`, so they are NUL-terminated and
    // outlive the call.
    unsafe {
        __android_log_write(priority, tag.as_ptr(), msg.as_ptr());
    }
}

#[cfg(not(target_os = "android"))]
fn log_info(_code: &str, _text: &str) {}

#[cfg(not(target_os = "android"))]
fn log_warn(_code: &str, _text: &str) {}

#[cfg(not(target_os = "android"))]
fn log_error(_code: &str, _text: &str) {}

/// Record the adapter that was actually created, once.
pub fn set_adapter(name: &str, backend: &str) {
    let recorded = with_inner(|state| {
        if state.adapter_recorded {
            return false;
        }
        state.adapter_recorded = true;
        state.adapter = name.to_string();
        state.backend = backend.to_string();
        true
    });
    // Log only on the transition: a second device (the engine and the preview
    // worker each build one) must not append a duplicate entry.
    if recorded {
        info("adapter", format!("GPU device ready: {name} ({backend})"));
    }
}

/// Adapter name, or an empty string when no device exists.
pub fn adapter() -> String {
    with_inner(|state| state.adapter.clone())
}

/// Graphics backend, or an empty string.
pub fn backend() -> String {
    with_inner(|state| state.backend.clone())
}

/// Replace the one-line user-facing summary.
pub fn set_hint(text: impl Into<String>) {
    let text = text.into();
    with_inner(|state| state.hint = text);
}

/// Note that `kind` produced a shader pipeline the driver would not accept.
pub fn reject_effect(kind_id: &str, reason: impl Into<String>) {
    let reason = reason.into();
    let first = with_inner(|state| {
        if state.rejected_effects.iter().any(|k| k == kind_id) {
            return false;
        }
        state.rejected_effects.push(kind_id.to_string());
        true
    });
    if first {
        error(
            "effect_pipeline_rejected",
            format!("effect `{kind_id}` disabled: {reason}"),
        );
    }
}

/// Effect kinds that were rejected.
pub fn rejected_effects() -> Vec<String> {
    with_inner(|state| state.rejected_effects.clone())
}

/// Clear the ring and the summary, keeping the adapter and path state.
pub fn clear() {
    with_inner(|state| {
        state.entries.clear();
        state.hint.clear();
        state.last_fallback_code = "";
    });
}

/// Mark that the last completed frame came from the GPU.
pub fn note_gpu_path() {
    PATH_GPU.store(true, Ordering::Relaxed);
    set_hint("");
}

/// Mark that the last completed frame came from a CPU fallback.
///
/// Records `why` only when `code` differs from the previous fallback reason, so
/// a 60 fps preview does not append 60 identical entries per second.
pub fn note_cpu_path(code: &'static str, why: impl Into<String>) {
    PATH_GPU.store(false, Ordering::Relaxed);
    let why = why.into();
    let fresh = with_inner(|state| {
        if state.last_fallback_code == code {
            return false;
        }
        state.last_fallback_code = code;
        true
    });
    if fresh {
        warn(code, format!("CPU fallback: {why}"));
        if !why.is_empty() {
            set_hint(format!("Rendering on CPU: {why}"));
        }
    }
}

/// `true` when the last completed frame came from the GPU.
pub fn path_is_gpu() -> bool {
    PATH_GPU.load(Ordering::Relaxed)
}

/// Record whether the Surface engine last drew on the GPU.
pub fn note_engine_ok(ok: bool) {
    ENGINE_OK.store(ok, Ordering::Relaxed);
}

/// Whether the Surface engine is currently drawing on the GPU.
pub fn engine_ok() -> bool {
    ENGINE_OK.load(Ordering::Relaxed)
}

/// Record whether the offscreen preview last drew on the GPU.
pub fn note_preview_ok(ok: bool) {
    PREVIEW_OK.store(ok, Ordering::Relaxed);
}

/// Whether the offscreen preview is currently drawing on the GPU.
pub fn preview_ok() -> bool {
    PREVIEW_OK.load(Ordering::Relaxed)
}

/// Number of driver errors seen so far.
///
/// Lets a caller bracket an operation and detect whether it produced a driver
/// error, without needing to poll an error scope.
pub fn error_count() -> u64 {
    ERROR_COUNT.load(Ordering::Relaxed)
}

/// Text of the most recent driver error.
pub fn last_error() -> String {
    last_error_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Loop recording a driver error. Install as the device's uncaptured-error
/// handler so a validation failure is *reported* instead of aborting the
/// process and taking the whole GPU path down with it.
pub fn note_driver_error(text: impl Into<String>) {
    let text = text.into();
    ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
    *last_error_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = text.clone();
    error("driver_error", text);
}

/// Build the report shown by the UI.
pub fn report() -> Report {
    Report {
        path_gpu: path_is_gpu(),
        adapter: adapter(),
        backend: backend(),
        hint: with_inner(|state| state.hint.clone()),
        rejected_effects: rejected_effects(),
        entries: with_inner(|state| state.entries.iter().cloned().collect()),
    }
}

/// JSON form of [`report`] for the JNI boundary.
///
/// Keys are fixed: `path`, `adapter`, `backend`, `hint`, `rejectedEffects`,
/// `entries[{seq, level, code, text}]`.
pub fn json() -> String {
    let report = report();
    let entries: Vec<serde_json::Value> = report
        .entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "seq": e.seq,
                "level": e.level.as_str(),
                "code": e.code,
                "text": e.text,
            })
        })
        .collect();
    let value = serde_json::json!({
        "path": if report.path_gpu { "gpu" } else { "cpu" },
        "adapter": report.adapter,
        "backend": report.backend,
        "hint": report.hint,
        "rejectedEffects": report.rejected_effects,
        // The two sub-paths are reported separately: the editor runs both the
        // Surface engine and the offscreen preview, and "which one is on the
        // GPU" is the question a user with a capable device actually has.
        "engineOk": engine_ok(),
        "previewOk": preview_ok(),
        "entries": entries,
    });
    serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report is intentionally process-global (the UI reads one report for
    /// the whole process), so the tests must not run concurrently with each
    /// other or they would observe one another's entries.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Full reset, including state the user-facing [`clear`] deliberately keeps
    /// (the adapter name and the rejected-effect list stay visible in the UI
    /// after a Clear).
    fn reset_all() {
        clear();
        with_inner(|state| {
            state.adapter_recorded = false;
            state.adapter.clear();
            state.backend.clear();
            state.rejected_effects.clear();
            state.seq = 0;
        });
        PATH_GPU.store(false, Ordering::Relaxed);
    }

    #[test]
    fn entries_keep_order_and_carry_a_sequence() {
        let _guard = serial();
        reset_all();
        info("a", "first");
        warn("b", "second");
        error("c", "third");
        let report = report();
        assert_eq!(report.entries.len(), 3);
        assert_eq!(report.entries[0].text, "first");
        assert_eq!(report.entries[1].level, Level::Warn);
        assert_eq!(report.entries[2].level, Level::Error);
        assert!(report.entries[0].seq < report.entries[2].seq);
    }

    #[test]
    fn the_ring_is_bounded() {
        let _guard = serial();
        reset_all();
        for i in 0..MAX_ENTRIES + 50 {
            info("bulk", format!("entry {i}"));
        }
        let report = report();
        assert_eq!(report.entries.len(), MAX_ENTRIES);
        // The oldest entries were dropped, not the newest.
        let last = report.entries.last().expect("non-empty");
        assert_eq!(last.text, format!("entry {}", MAX_ENTRIES + 50 - 1));
        assert_eq!(last.seq as usize, MAX_ENTRIES + 50);
        assert!(
            report.entries.iter().all(|e| e.text != "entry 0"),
            "the oldest entry must have been evicted"
        );
    }

    #[test]
    fn a_repeated_fallback_is_recorded_once_per_reason() {
        let _guard = serial();
        reset_all();
        note_cpu_path("same_code", "why once");
        note_cpu_path("same_code", "why once");
        note_cpu_path("same_code", "why once");
        let repeats = report()
            .entries
            .iter()
            .filter(|e| e.code == "same_code")
            .count();
        assert_eq!(repeats, 1, "a per-frame fallback must not flood the ring");

        // A different reason is a new event.
        note_cpu_path("other_code", "different");
        assert_eq!(
            report().entries.iter().filter(|e| e.code == "other_code").count(),
            1
        );
        assert!(!path_is_gpu());
    }

    #[test]
    fn gpu_path_clears_the_cpu_hint() {
        let _guard = serial();
        reset_all();
        note_cpu_path("no_gpu", "adapter request failed");
        assert!(!report().hint.is_empty());
        note_gpu_path();
        assert!(path_is_gpu());
        assert!(report().hint.is_empty());
        assert_eq!(report().entries.len(), 1, "clearing the hint keeps history");
    }

    #[test]
    fn adapter_is_recorded_once() {
        let _guard = serial();
        reset_all();
        set_adapter("Adreno (TM) 730", "Vulkan");
        set_adapter("Something Else", "Gl");
        let report = report();
        assert_eq!(report.adapter, "Adreno (TM) 730");
        assert_eq!(report.backend, "Vulkan");
        assert_eq!(
            report.entries.iter().filter(|e| e.code == "adapter").count(),
            1
        );
    }

    #[test]
    fn rejecting_an_effect_is_idempotent_but_keeps_distinct_kinds() {
        let _guard = serial();
        reset_all();
        reject_effect("blur", "no pipeline");
        reject_effect("blur", "no pipeline");
        reject_effect("glow", "no pipeline");
        assert_eq!(rejected_effects(), vec!["blur", "glow"]);
        assert_eq!(
            report()
                .entries
                .iter()
                .filter(|e| e.code == "effect_pipeline_rejected")
                .count(),
            2
        );
    }

    #[test]
    fn driver_errors_are_counted_for_bracketing() {
        let _guard = serial();
        let before = error_count();
        note_driver_error("validation: binding mismatch");
        assert_eq!(error_count(), before + 1);
        assert!(last_error().contains("binding mismatch"));
    }

    #[test]
    fn clear_keeps_the_adapter_and_the_rejected_list() {
        let _guard = serial();
        reset_all();
        set_adapter("Adreno (TM) 730", "Vulkan");
        reject_effect("blur", "no pipeline");
        note_cpu_path("no_gpu", "no adapter");
        clear();
        let report = report();
        assert!(report.entries.is_empty(), "Clear empties the log");
        assert!(report.hint.is_empty(), "Clear drops the stale hint");
        assert_eq!(report.adapter, "Adreno (TM) 730", "adapter stays visible");
        assert_eq!(
            report.rejected_effects,
            vec!["blur"],
            "rejected effects stay visible: they are the actionable part"
        );
    }

    #[test]
    fn json_has_the_documented_shape_and_survives_hostile_text() {
        let _guard = serial();
        reset_all();
        set_adapter("Adreno \"quoted\"\\ \\ 730", "Vulkan");
        note_driver_error("bad \"quote\" and \\ backslash and \n newline");
        note_cpu_path("fallback", "reason with \"quotes\"");
        let json = json();
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["path"], "cpu");
        assert_eq!(v["backend"], "Vulkan");
        assert!(v["rejectedEffects"].is_array());
        let entries = v["entries"].as_array().expect("entries array");
        assert!(!entries.is_empty());
        for e in entries {
            let level = e["level"].as_str().expect("level string");
            assert!(
                matches!(level, "info" | "warn" | "error"),
                "unexpected level {level}"
            );
            assert!(e["seq"].is_u64());
            assert!(e["code"].is_string());
            assert!(e["text"].is_string());
        }
        // Escaping round-trips: the newline in the message survives as one.
        assert!(entry_text(&v, "driver_error").contains('\n'));
        assert!(v["adapter"].as_str().unwrap().contains("quoted"));
    }

    fn entry_text(v: &serde_json::Value, code: &str) -> String {
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["code"] == code)
            .map(|e| e["text"].as_str().unwrap_or_default().to_string())
            .unwrap_or_default()
    }
}
