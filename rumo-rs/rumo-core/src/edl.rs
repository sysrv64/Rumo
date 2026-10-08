// SPDX-License-Identifier: Apache-2.0
//! One-pass editorial operations over a [`Project`].
//!
//! Why this exists: the editor's assistant used to apply a timeline change
//! through one narrow call per step. A hundred-cut edit was a hundred round
//! trips and a hundred undo steps, so the timeline the model was reasoning about
//! desynchronised from the one the engine was drawing. This module takes the
//! whole array of operations at once and replays it in Rust, so a batch is
//! atomic, ordered, and exactly one round trip — the JNI layer hands over project
//! JSON and ops JSON and gets project JSON back ([`apply_edl_json`]).
//!
//! The batch never fails as a whole for one bad operation. An op that cannot be
//! applied is recorded in [`EdlOutcome::skipped`] with a reason and the rest of
//! the array still runs; the input project is cloned and never mutated
//! ([`apply_edl`]). Nothing here panics: bad JSON becomes a `Result::Err`,
//! everything else becomes a skip.
//!
//! ## `ripple_delete` overlap rules
//!
//! A layer occupies the half-open window `[start_ms, start_ms + duration_ms)`;
//! the removed range is `[from, to)` with `to > from` (otherwise the op is
//! skipped). Then, per layer (`end = start_ms + duration_ms`):
//!
//! 1. `end <= from` — the layer ends at or before the range: untouched.
//! 2. `start_ms >= to` — the layer begins at or after the range: its window,
//!    its keys, its animated tracks and its transition all move left by
//!    `to - from`.
//! 3. `start_ms >= from && end <= to` — wholly inside the range: removed.
//! 4. Otherwise the layer overlaps a boundary, and the surviving pieces are
//!    closed up into the single window `[new_start, new_end)` with
//!    `new_start = start_ms` when `start_ms < from`, else `from`, and
//!    `new_end = end - (to - from)` when `end > to`, else `from`:
//!    - `start_ms < from && end <= to` (overlaps the range's start): trimmed to
//!      `[start_ms, from)`.
//!    - `start_ms >= from && end > to` (overlaps the range's end): the surviving
//!      tail `[to, end)` moves to `[from, end - (to - from))`.
//!    - `start_ms < from && end > to` (spans the whole range): the two pieces
//!      `[start_ms, from)` and `[to, end)` become one window, because the gap is
//!      closed and they abut.
//!
//! Keys follow the layer through the same removal: a key time `< from` stays, a
//! key time in `[from, to)` is dropped, and a key time `>= to` shifts left by the
//! removed length. A transition whose `start_ms` lands in the removed range is
//! dropped rather than left pointing at a moment the timeline no longer has.
//! Times past the project's current extent are allowed; they simply fall outside
//! every layer's window.
//!
//! ## Keyframes on `split`
//!
//! Keyframe times in this codebase are **absolute project time**, not offsets
//! from the layer's start: `rumo_core::model::sample_value` compares the frame
//! time straight against `Keyframe::time_ms`, `docs/11 §11.8` states "our keys
//! are in absolute milliseconds", and `project_json`'s own track test pins keys at
//! absolute ms (`sample_x(1500) == 0.0` for a layer at `startMs: 1500` with keys
//! at 1500/2500). A split therefore *partitions* each track at the cut — keys at
//! or before `atMs` stay on the first half, keys after it go to the second — and
//! the stored times do **not** shift. Because the second half's `start_ms` is
//! set to `atMs`, its keys read as offsets from its own zero already, but the
//! numbers stay absolute, which is what the engine samples with. Subtracting
//! `atMs` would put every one of the second half's keys before its own window and
//! freeze its animation on the last key's value.
//!
//! The segment that straddles the cut is not repaired with a synthetic key: the
//! brief asks for a partition, not a copy, so the first half holds its last key
//! and the second holds its first across the cut. The second half gets a fresh
//! `Uuid`; everything else (`kind`, `name`, `uri`, `effects`) is copied.
//!
//! A transition is dropped from the second half — left in place it would re-fade
//! a clip that is no longer arriving — and kept on the first. A transition whose
//! window lies entirely after the cut is a degenerate document (it would leave
//! the first half faded out); it is kept per this rule and is the caller's to
//! clean up.
//!
//! ## `duck_audio` is refused
//!
//! There is no gain, volume or level field anywhere in [`Layer`] or
//! [`LayerExtra`] — the engine has no audio gain stage at all. The editor's own
//! inspector says "no gain stage in engine" rather than offering a slider that
//! would do nothing (`InspectorPanel.kt`), and this module matches that honesty:
//! `duck_audio` is parsed and then always recorded in `skipped` with
//! [`DUCK_AUDIO_REASON`]. It is not faked by writing a field the renderer
//! ignores.

use serde::Deserialize;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::codec;
use crate::model::{Keyframe, Layer, LayerExtra, LayerKind, Project};
use crate::project_json::{project_from_json, project_to_json};

/// Why `duck_audio` never applies. Named so the reason has one source of truth
/// and can be asserted in a test.
pub const DUCK_AUDIO_REASON: &str = "duck_audio: the engine has no audio gain stage, so a level \
change is not representable in the project model; split the audio instead, or cut it";

/// One editing operation. Deserialised from JSON with the snake_case `op` names
/// below; unknown fields are ignored by the parse and an unknown `op` becomes
/// [`EdlOp::Unknown`] so a batch of many ops loses only the one it cannot read.
#[derive(Debug, Clone, PartialEq)]
pub enum EdlOp {
    /// `{op:"split", layer:"<id or name>", atMs:N}`.
    Split { layer: String, at_ms: i64 },
    /// `{op:"ripple_delete", fromMs:N, toMs:N}`.
    RippleDelete { from_ms: i64, to_ms: i64 },
    /// `{op:"insert_clip", kind, atMs, durationMs, ripple, …}`.
    InsertClip(InsertClipOp),
    /// `{op:"duck_audio", layer, fromMs, toMs, gain}`. Always refused; see the
    /// module docs and [`DUCK_AUDIO_REASON`].
    DuckAudio {
        layer: String,
        from_ms: i64,
        to_ms: i64,
        gain: f32,
    },
    /// `{op:"add_subtitles", cues:[…], style:{…}}`.
    AddSubtitles {
        cues: Vec<SubtitleCue>,
        style: SubtitleStyle,
    },
    /// An `op` this build does not know, or one whose body did not parse. Kept
    /// as a variant so one unreadable op is a skip and not a failed batch.
    Unknown { op: String, reason: String },
}

/// `insert_clip`'s payload. Only the fields the chosen `kind` actually uses are
/// written into the new layer's `extra`.
#[derive(Debug, Clone, PartialEq)]
pub struct InsertClipOp {
    pub kind: LayerKind,
    pub at_ms: i64,
    pub duration_ms: i64,
    /// `true` pushes every layer at or after `atMs` right by `durationMs`;
    /// `false` punches `[atMs, atMs + durationMs)` out of whatever it lands on.
    /// Absent on the wire means `false` (overwrite): the conservative choice,
    /// because it never silently moves unrelated layers.
    pub ripple: bool,
    pub name: Option<String>,
    pub uri: Option<String>,
    pub text: Option<String>,
    pub argb: Option<u32>,
    pub dx: Option<f32>,
    pub dy: Option<f32>,
    pub scale: Option<f32>,
    pub alpha: Option<f32>,
}

/// One subtitle cue. Times are absolute project ms, as everywhere else.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleCue {
    pub from_ms: i64,
    pub to_ms: i64,
    pub text: String,
}

/// Optional look applied to every layer `add_subtitles` creates.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SubtitleStyle {
    pub argb: Option<u32>,
    pub text_weight: Option<u16>,
    pub stroke_px: Option<f32>,
    pub stroke_argb: Option<u32>,
}

/// What a batch produced: the edited project plus one line per applied op and
/// one line per op (or cue) that could not be applied, each with its reason.
#[derive(Debug, Clone, PartialEq)]
pub struct EdlOutcome {
    pub project: Project,
    pub applied: Vec<String>,
    pub skipped: Vec<String>,
}

/// Apply `ops` to `project` in order, on a clone. See the module docs for the
/// per-op contract. Never panics and never fails the batch as a whole.
pub fn apply_edl(project: &Project, ops: &[EdlOp]) -> EdlOutcome {
    let mut work = project.clone();
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    for op in ops {
        if let Some(line) = apply_op(&mut work, op, &mut skipped) {
            applied.push(line);
        }
    }
    EdlOutcome {
        project: work,
        applied,
        skipped,
    }
}

/// The whole JNI round trip: project JSON in, ops JSON in, new project JSON out.
///
/// Bad project JSON or bad ops JSON (not valid JSON, or ops that is not an
/// array) is a `Result::Err`; a single op whose body does not parse is a skip,
/// not an error.
pub fn apply_edl_json(project_json: &str, ops_json: &str) -> Result<String, String> {
    let bytes = project_from_json(project_json)?;
    let project = codec::decode(&bytes).map_err(|e| format!("bad project bytes: {e}"))?;
    let ops = parse_ops_json(ops_json)?;
    let outcome = apply_edl(&project, &ops);
    let out_bytes = codec::encode(&outcome.project);
    project_to_json(&out_bytes)
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Apply one op. `Some(line)` means applied; `None` means it was recorded in
/// `skipped` (which may already hold per-cue reasons for `add_subtitles`).
fn apply_op(project: &mut Project, op: &EdlOp, skipped: &mut Vec<String>) -> Option<String> {
    match op {
        EdlOp::Split { layer, at_ms } => match find_layer_index(project, layer) {
            Ok(index) => match split_layer(project, index, *at_ms) {
                Ok(line) => Some(line),
                Err(reason) => {
                    skipped.push(reason);
                    None
                }
            },
            Err(reason) => {
                skipped.push(format!("split: {reason}"));
                None
            }
        },
        EdlOp::RippleDelete { from_ms, to_ms } => match ripple_delete(project, *from_ms, *to_ms) {
            Ok(line) => Some(line),
            Err(reason) => {
                skipped.push(reason);
                None
            }
        },
        EdlOp::InsertClip(clip) => match insert_clip(project, clip) {
            Ok(line) => Some(line),
            Err(reason) => {
                skipped.push(reason);
                None
            }
        },
        EdlOp::DuckAudio { .. } => {
            skipped.push(DUCK_AUDIO_REASON.to_string());
            None
        }
        EdlOp::AddSubtitles { cues, style } => apply_subtitles(project, cues, style, skipped),
        EdlOp::Unknown { reason, .. } => {
            skipped.push(reason.clone());
            None
        }
    }
}

/// Resolve a layer reference to an index, by id first and then by name.
///
/// An exact `Uuid` match wins. Otherwise the reference is a name: no match, or
/// two or more matches, is an error (the caller records it as a skip). A
/// reference that parses as a `Uuid` but matches no id still falls back to name,
/// so a caller can never be locked out by a name that looks like an id.
fn find_layer_index(project: &Project, key: &str) -> Result<usize, String> {
    if let Ok(id) = Uuid::parse_str(key) {
        if let Some(index) = project.layers.iter().position(|l| l.id == id) {
            return Ok(index);
        }
    }
    let matches: Vec<usize> = project
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| l.name == key)
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [] => Err(format!("no layer with id or name '{key}'")),
        [only] => Ok(*only),
        many => Err(format!(
            "layer name '{key}' is ambiguous: {} layers match",
            many.len()
        )),
    }
}

// ---------------------------------------------------------------------------
// split
// ---------------------------------------------------------------------------

/// Cut `project.layers[index]` at absolute time `at_ms` into two layers.
///
/// `at_ms` must be strictly inside the layer's window, or the op is a skip: the
/// cut would otherwise produce an empty half. Keyframes and the four animated
/// tracks are partitioned (see the module docs on the absolute time base), the
/// second half gets a fresh `Uuid`, and the transition is dropped from it.
fn split_layer(project: &mut Project, index: usize, at_ms: i64) -> Result<String, String> {
    let original = match project.layers.get(index) {
        Some(layer) => layer.clone(),
        None => return Err("split: the layer vanished before it could be cut".to_string()),
    };
    let start = original.start_ms;
    let end = start.saturating_add(original.duration_ms);
    if at_ms <= start || at_ms >= end {
        return Err(format!(
            "split: atMs {at_ms} is not strictly inside layer '{}' ({}..{})",
            original.name, start, end
        ));
    }

    let mut first = original.clone();
    first.duration_ms = at_ms - start;
    // Keys at or before the cut belong to the first half. Their times are
    // absolute project time and the first half's start is unchanged, so they
    // are copied verbatim.
    first.keyframes.retain(|k| k.time_ms <= at_ms);
    if let Some(extra) = first.extra.as_mut() {
        retain_tracks(extra, |t| t <= at_ms);
    }

    let mut second = original.clone();
    second.id = Uuid::new_v4();
    second.start_ms = at_ms;
    second.duration_ms = end - at_ms;
    // Keys after the cut move to the second half unchanged: the engine samples
    // key times against absolute project time, so the second half's keys must
    // stay absolute even though its new start is not zero (docs/11 §11.8).
    second.keyframes.retain(|k| k.time_ms > at_ms);
    if let Some(extra) = second.extra.as_mut() {
        retain_tracks(extra, |t| t > at_ms);
        // A transition on the second half would re-fade a clip that is no
        // longer arriving; the first half keeps it.
        extra.transition = None;
    }

    if let Some(slot) = project.layers.get_mut(index) {
        *slot = first;
    }
    project.layers.push(second);

    Ok(format!(
        "split: '{}' at {at_ms}ms -> [{start}..{at_ms}) + [{at_ms}..{end})",
        original.name
    ))
}

/// Keep only the keys whose absolute time satisfies `keep`, on a layer's four
/// animated property tracks.
fn retain_tracks(extra: &mut LayerExtra, keep: impl Fn(i64) -> bool) {
    extra.track_x.retain(|k| keep(k.time_ms));
    extra.track_y.retain(|k| keep(k.time_ms));
    extra.track_scale.retain(|k| keep(k.time_ms));
    extra.track_alpha.retain(|k| keep(k.time_ms));
}

// ---------------------------------------------------------------------------
// ripple_delete
// ---------------------------------------------------------------------------

/// Remove `[from, to)` from every layer and close the gap. Overlap rules are the
/// table in the module docs.
fn ripple_delete(project: &mut Project, from: i64, to: i64) -> Result<String, String> {
    if to <= from {
        return Err(format!(
            "ripple_delete: toMs ({to}) must be greater than fromMs ({from})"
        ));
    }
    let len = to - from;
    let mut out = Vec::with_capacity(project.layers.len());
    let mut removed = 0u32;
    let mut trimmed = 0u32;
    let mut shifted = 0u32;

    for layer in std::mem::take(&mut project.layers) {
        let start = layer.start_ms;
        // Saturating keeps an absurd document value from panicking; a duration
        // that cannot be represented as an instant simply reaches the end.
        let end = start.saturating_add(layer.duration_ms);
        if end <= from {
            out.push(layer);
            continue;
        }
        if start >= to {
            let mut moved = layer;
            shift_layer_time(&mut moved, -len);
            shifted += 1;
            out.push(moved);
            continue;
        }
        if start >= from && end <= to {
            removed += 1;
            continue;
        }
        // Overlaps a boundary: keep the pieces outside the range, closing the
        // gap so the head and tail abut.
        let new_start = if start < from { start } else { from };
        let new_end = if end > to { end - len } else { from };
        if new_end <= new_start {
            removed += 1;
            continue;
        }
        let mut kept = layer;
        retime_layer_ripple(&mut kept, from, to, len);
        kept.start_ms = new_start;
        kept.duration_ms = new_end - new_start;
        trimmed += 1;
        out.push(kept);
    }

    project.layers = out;
    Ok(format!(
        "ripple_delete: {from}..{to}ms removed {removed} layer(s), trimmed {trimmed}, shifted {shifted}"
    ))
}

/// Map one absolute time through a ripple removal: before the range unchanged,
/// inside the range gone, after the range moved left by `len`.
fn ripple_time(t: i64, from: i64, to: i64, len: i64) -> Option<i64> {
    if t < from {
        Some(t)
    } else if t < to {
        None
    } else {
        Some(t - len)
    }
}

/// Retime every key, track and transition of an overlapping layer through a
/// ripple removal.
fn retime_layer_ripple(layer: &mut Layer, from: i64, to: i64, len: i64) {
    layer.keyframes = layer
        .keyframes
        .iter()
        .filter_map(|k| ripple_time(k.time_ms, from, to, len).map(|t| Keyframe { time_ms: t, ..*k }))
        .collect();
    if let Some(extra) = layer.extra.as_mut() {
        extra.track_x = retime_track_ripple(&extra.track_x, from, to, len);
        extra.track_y = retime_track_ripple(&extra.track_y, from, to, len);
        extra.track_scale = retime_track_ripple(&extra.track_scale, from, to, len);
        extra.track_alpha = retime_track_ripple(&extra.track_alpha, from, to, len);
        if let Some(transition) = extra.transition.as_mut() {
            match ripple_time(transition.start_ms, from, to, len) {
                Some(t) => transition.start_ms = t,
                None => extra.transition = None,
            }
        }
    }
}

fn retime_track_ripple(keys: &[Keyframe], from: i64, to: i64, len: i64) -> Vec<Keyframe> {
    keys.iter()
        .filter_map(|k| ripple_time(k.time_ms, from, to, len).map(|t| Keyframe { time_ms: t, ..*k }))
        .collect()
}

/// Move a whole layer in time: its window, its keys, its tracks and its
/// transition. Used by `ripple_delete` (negative shift) and `insert_clip` with
/// `ripple: true` (positive shift).
fn shift_layer_time(layer: &mut Layer, by: i64) {
    layer.start_ms = layer.start_ms.saturating_add(by);
    for key in layer.keyframes.iter_mut() {
        key.time_ms = key.time_ms.saturating_add(by);
    }
    if let Some(extra) = layer.extra.as_mut() {
        for track in [
            &mut extra.track_x,
            &mut extra.track_y,
            &mut extra.track_scale,
            &mut extra.track_alpha,
        ] {
            for key in track.iter_mut() {
                key.time_ms = key.time_ms.saturating_add(by);
            }
        }
        if let Some(transition) = extra.transition.as_mut() {
            transition.start_ms = transition.start_ms.saturating_add(by);
        }
    }
}

// ---------------------------------------------------------------------------
// insert_clip
// ---------------------------------------------------------------------------

/// Create a layer for `clip` and place it at `atMs`.
///
/// `ripple: true` pushes every layer whose `start_ms >= atMs` right by
/// `durationMs` (a layer that starts earlier and merely spans `atMs` is not
/// pushed — the new clip overlays its tail). `ripple: false` punches the span
/// out of whatever it lands on, leaving a head and/or a tail; a layer that spans
/// the whole span is cut into two layers, the tail with a fresh `Uuid`.
///
/// A negative `atMs` is clamped to 0, like every other timeline position in the
/// document. Only the fields the `kind` uses are written into `extra`: TEXT gets
/// its payload and the visual props, SHAPE the visual props, MEDIA the geometry
/// props (its `uri` lives on the layer), AUDIO nothing (it is not drawn and the
/// engine has no gain stage).
fn insert_clip(project: &mut Project, clip: &InsertClipOp) -> Result<String, String> {
    if clip.duration_ms <= 0 {
        return Err(format!(
            "insert_clip: durationMs ({}) must be positive",
            clip.duration_ms
        ));
    }
    let at = clip.at_ms.max(0);
    let duration = clip.duration_ms;

    let extra = match clip.kind {
        LayerKind::Text => {
            let Some(text) = clip.text.clone().filter(|t| !t.trim().is_empty()) else {
                return Err("insert_clip: a text layer requires a non-empty 'text'".to_string());
            };
            Some(LayerExtra {
                text: Some(text),
                argb: clip.argb,
                dx: clip.dx.unwrap_or(0.0),
                dy: clip.dy.unwrap_or(0.0),
                scale: clip.scale.unwrap_or(1.0),
                alpha: clip.alpha.unwrap_or(1.0),
                ..Default::default()
            })
        }
        LayerKind::Shape => {
            if clip.argb.is_none()
                && clip.dx.is_none()
                && clip.dy.is_none()
                && clip.scale.is_none()
                && clip.alpha.is_none()
            {
                None
            } else {
                Some(LayerExtra {
                    argb: clip.argb,
                    dx: clip.dx.unwrap_or(0.0),
                    dy: clip.dy.unwrap_or(0.0),
                    scale: clip.scale.unwrap_or(1.0),
                    alpha: clip.alpha.unwrap_or(1.0),
                    ..Default::default()
                })
            }
        }
        LayerKind::Media => {
            if clip.dx.is_none() && clip.dy.is_none() && clip.scale.is_none() && clip.alpha.is_none()
            {
                None
            } else {
                Some(LayerExtra {
                    dx: clip.dx.unwrap_or(0.0),
                    dy: clip.dy.unwrap_or(0.0),
                    scale: clip.scale.unwrap_or(1.0),
                    alpha: clip.alpha.unwrap_or(1.0),
                    ..Default::default()
                })
            }
        }
        LayerKind::Audio => None,
    };

    let name = clip
        .name
        .clone()
        .unwrap_or_else(|| default_name(clip.kind).to_string());

    if clip.ripple {
        for layer in project.layers.iter_mut() {
            if layer.start_ms >= at {
                shift_layer_time(layer, duration);
            }
        }
    } else {
        punch_layers(project, at, at.saturating_add(duration));
    }

    project.layers.push(Layer {
        id: Uuid::new_v4(),
        kind: clip.kind,
        name: name.clone(),
        keyframes: Vec::new(),
        extra,
        visible: true,
        duration_ms: duration,
        start_ms: at,
        uri: clip.uri.clone().filter(|u| !u.is_empty()),
        effects: Vec::new(),
    });

    Ok(format!(
        "insert_clip: {name} at {at}ms for {duration}ms ({})",
        if clip.ripple { "ripple" } else { "overwrite" }
    ))
}

fn default_name(kind: LayerKind) -> &'static str {
    match kind {
        LayerKind::Shape => "Shape",
        LayerKind::Text => "Text",
        LayerKind::Media => "Media",
        LayerKind::Audio => "Audio",
    }
}

/// Remove the half-open span `[at, to)` from every layer **without** closing the
/// gap: a covered layer loses the middle, which may split it into a head and a
/// tail (the tail gets a fresh `Uuid` and only the keys at or after `to`).
fn punch_layers(project: &mut Project, at: i64, to: i64) {
    let mut out = Vec::with_capacity(project.layers.len() + 1);
    for layer in std::mem::take(&mut project.layers) {
        let start = layer.start_ms;
        let end = start.saturating_add(layer.duration_ms);
        if end <= at || start >= to {
            out.push(layer);
            continue;
        }
        let keep_head = start < at;
        let keep_tail = end > to;
        match (keep_head, keep_tail) {
            (false, false) => {}
            (true, false) => {
                let mut head = layer;
                head.duration_ms = at - head.start_ms;
                retain_layer_before(&mut head, at);
                out.push(head);
            }
            (false, true) => {
                let mut tail = layer;
                tail.start_ms = to;
                tail.duration_ms = end - to;
                retain_layer_from(&mut tail, to);
                out.push(tail);
            }
            (true, true) => {
                let mut head = layer.clone();
                head.duration_ms = at - head.start_ms;
                retain_layer_before(&mut head, at);
                let mut tail = layer;
                tail.id = Uuid::new_v4();
                tail.start_ms = to;
                tail.duration_ms = end - to;
                retain_layer_from(&mut tail, to);
                out.push(head);
                out.push(tail);
            }
        }
    }
    project.layers = out;
}

/// Keep only the part of a layer at or before `at`: keys before `at`, tracks
/// before `at`, and a transition only if it starts before `at`.
fn retain_layer_before(layer: &mut Layer, at: i64) {
    layer.keyframes.retain(|k| k.time_ms < at);
    if let Some(extra) = layer.extra.as_mut() {
        retain_tracks(extra, |t| t < at);
        if extra
            .transition
            .as_ref()
            .is_some_and(|t| t.start_ms >= at)
        {
            extra.transition = None;
        }
    }
}

/// Keep only the part of a layer at or after `at`: keys from `at`, tracks from
/// `at`, and a transition only if it starts at or after `at`.
fn retain_layer_from(layer: &mut Layer, at: i64) {
    layer.keyframes.retain(|k| k.time_ms >= at);
    if let Some(extra) = layer.extra.as_mut() {
        retain_tracks(extra, |t| t >= at);
        if extra.transition.as_ref().is_some_and(|t| t.start_ms < at) {
            extra.transition = None;
        }
    }
}

// ---------------------------------------------------------------------------
// add_subtitles
// ---------------------------------------------------------------------------

/// Create one TEXT layer per valid cue, in cue order. A cue with empty text or
/// `toMs <= fromMs` is recorded in `skipped` and the rest still run; overlapping
/// cues are fine, they are separate layers.
fn apply_subtitles(
    project: &mut Project,
    cues: &[SubtitleCue],
    style: &SubtitleStyle,
    skipped: &mut Vec<String>,
) -> Option<String> {
    if cues.is_empty() {
        skipped.push("add_subtitles: no cues".to_string());
        return None;
    }
    let mut added = 0u32;
    for (index, cue) in cues.iter().enumerate() {
        if cue.text.trim().is_empty() {
            skipped.push(format!("add_subtitles: cue {} has empty text", index + 1));
            continue;
        }
        if cue.to_ms <= cue.from_ms {
            skipped.push(format!(
                "add_subtitles: cue {} has toMs ({}) <= fromMs ({})",
                index + 1,
                cue.to_ms,
                cue.from_ms
            ));
            continue;
        }
        let mut extra = LayerExtra {
            text: Some(cue.text.clone()),
            ..Default::default()
        };
        if let Some(argb) = style.argb {
            extra.argb = Some(argb);
        }
        if let Some(weight) = style.text_weight {
            // A weight outside the range real faces live in would make the
            // shaper's query miss every face; clamp where it enters the document.
            extra.text_weight = weight.clamp(100, 900);
        }
        if let Some(stroke) = style.stroke_px {
            extra.stroke_px = if stroke.is_finite() { stroke.max(0.0) } else { 0.0 };
        }
        if let Some(argb) = style.stroke_argb {
            extra.stroke_argb = Some(argb);
        }
        project.layers.push(Layer {
            id: Uuid::new_v4(),
            kind: LayerKind::Text,
            name: cue.text.clone(),
            keyframes: Vec::new(),
            extra: Some(extra),
            visible: true,
            duration_ms: cue.to_ms - cue.from_ms,
            start_ms: cue.from_ms.max(0),
            uri: None,
            effects: Vec::new(),
        });
        added += 1;
    }
    if added == 0 {
        None
    } else {
        Some(format!("add_subtitles: added {added} text layer(s)"))
    }
}

// ---------------------------------------------------------------------------
// Wire parsing
// ---------------------------------------------------------------------------

/// Parse the ops array. The array itself must be valid JSON and an array (else
/// `Err`); each element is parsed on its own, so one unreadable op becomes
/// [`EdlOp::Unknown`] and the rest of the batch survives.
fn parse_ops_json(ops_json: &str) -> Result<Vec<EdlOp>, String> {
    let value: Value = serde_json::from_str(ops_json).map_err(|e| format!("bad ops json: {e}"))?;
    let array = value
        .as_array()
        .ok_or_else(|| "ops json must be an array".to_string())?;
    let mut ops = Vec::with_capacity(array.len());
    for element in array {
        match serde_json::from_value::<EdlOp>(element.clone()) {
            Ok(op) => ops.push(op),
            Err(e) => {
                let op = element.get("op").and_then(|v| v.as_str()).unwrap_or("");
                let reason = if op.is_empty() {
                    format!("an op without a name could not be read: {e}")
                } else {
                    format!("{op}: {e}")
                };
                ops.push(EdlOp::Unknown {
                    op: op.to_string(),
                    reason,
                });
            }
        }
    }
    Ok(ops)
}

impl<'de> Deserialize<'de> for EdlOp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Going through `Value` is what lets one bad op be a skip: a typed
        // `Deserialize` on the enum would fail the whole array instead.
        let value = Value::deserialize(deserializer)?;
        EdlOp::from_json(&value).map_err(serde::de::Error::custom)
    }
}

impl EdlOp {
    fn from_json(value: &Value) -> Result<Self, String> {
        let obj = value
            .as_object()
            .ok_or_else(|| "op is not a JSON object".to_string())?;
        let op = obj
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "'op' is missing or not a string".to_string())?;
        match op {
            "split" => Ok(EdlOp::Split {
                layer: req_str(obj, "layer")?,
                at_ms: req_i64(obj, "atMs")?,
            }),
            "ripple_delete" => Ok(EdlOp::RippleDelete {
                from_ms: req_i64(obj, "fromMs")?,
                to_ms: req_i64(obj, "toMs")?,
            }),
            "insert_clip" => Ok(EdlOp::InsertClip(insert_clip_from_json(obj)?)),
            "duck_audio" => Ok(EdlOp::DuckAudio {
                layer: req_str(obj, "layer")?,
                from_ms: req_i64(obj, "fromMs")?,
                to_ms: req_i64(obj, "toMs")?,
                gain: req_f32(obj, "gain")?,
            }),
            "add_subtitles" => {
                let raw_cues = obj
                    .get("cues")
                    .ok_or_else(|| "'cues' is missing".to_string())?
                    .as_array()
                    .ok_or_else(|| "'cues' is not an array".to_string())?;
                let mut cues = Vec::with_capacity(raw_cues.len());
                for (index, raw) in raw_cues.iter().enumerate() {
                    cues.push(cue_from_json(raw, index)?);
                }
                Ok(EdlOp::AddSubtitles {
                    cues,
                    style: subtitle_style_from_json(obj.get("style"))?,
                })
            }
            other => Ok(EdlOp::Unknown {
                op: other.to_string(),
                reason: format!("unknown op '{other}'"),
            }),
        }
    }
}

fn insert_clip_from_json(obj: &Map<String, Value>) -> Result<InsertClipOp, String> {
    let raw_kind = req_str(obj, "kind")?;
    let kind = parse_layer_kind(&raw_kind)
        .ok_or_else(|| format!("unknown kind '{raw_kind}' (expected text|shape|media|audio)"))?;
    Ok(InsertClipOp {
        kind,
        at_ms: req_i64(obj, "atMs")?,
        duration_ms: req_i64(obj, "durationMs")?,
        ripple: opt_bool(obj, "ripple")?.unwrap_or(false),
        name: opt_str(obj, "name"),
        uri: opt_str(obj, "uri"),
        text: opt_str(obj, "text"),
        argb: opt_u32(obj, "argb")?,
        dx: opt_f32(obj, "dx")?,
        dy: opt_f32(obj, "dy")?,
        scale: opt_f32(obj, "scale")?,
        alpha: opt_f32(obj, "alpha")?,
    })
}

/// A cue is read leniently: a missing number becomes 0 and a missing string
/// becomes `""`, so an incomplete cue is reported per-cue at apply time rather
/// than taking the whole op down. Only an element that is not an object at all
/// is a parse error for the op.
fn cue_from_json(value: &Value, index: usize) -> Result<SubtitleCue, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| format!("cue {} is not an object", index + 1))?;
    Ok(SubtitleCue {
        from_ms: obj.get("fromMs").and_then(as_i64).unwrap_or(0),
        to_ms: obj.get("toMs").and_then(as_i64).unwrap_or(0),
        text: obj
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

fn subtitle_style_from_json(value: Option<&Value>) -> Result<SubtitleStyle, String> {
    let Some(value) = value else {
        return Ok(SubtitleStyle::default());
    };
    if value.is_null() {
        return Ok(SubtitleStyle::default());
    }
    let obj = value
        .as_object()
        .ok_or_else(|| "'style' is not an object".to_string())?;
    Ok(SubtitleStyle {
        argb: opt_u32(obj, "argb")?,
        text_weight: opt_u16(obj, "textWeight")?,
        stroke_px: opt_f32(obj, "strokePx")?,
        stroke_argb: opt_u32(obj, "strokeArgb")?,
    })
}

fn parse_layer_kind(raw: &str) -> Option<LayerKind> {
    match raw.to_ascii_lowercase().as_str() {
        "shape" | "0" => Some(LayerKind::Shape),
        "text" | "1" => Some(LayerKind::Text),
        "media" | "2" => Some(LayerKind::Media),
        "audio" | "3" => Some(LayerKind::Audio),
        _ => None,
    }
}

// --- small typed readers ---------------------------------------------------

fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
}

fn as_f32(value: &Value) -> Option<f32> {
    value.as_f64().filter(|f| f.is_finite()).map(|f| f as f32)
}

fn as_u32(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .or_else(|| value.as_i64().map(|n| n.max(0) as u64))
        .and_then(|n| u32::try_from(n).ok())
}

fn as_u16(value: &Value) -> Option<u16> {
    value
        .as_u64()
        .or_else(|| value.as_i64().map(|n| n.max(0) as u64))
        .and_then(|n| u16::try_from(n).ok())
}

fn req_i64(obj: &Map<String, Value>, key: &str) -> Result<i64, String> {
    obj.get(key)
        .and_then(as_i64)
        .ok_or_else(|| format!("'{key}' is missing or not a number"))
}

fn req_f32(obj: &Map<String, Value>, key: &str) -> Result<f32, String> {
    obj.get(key)
        .and_then(as_f32)
        .ok_or_else(|| format!("'{key}' is missing or not a number"))
}

fn req_str(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("'{key}' is missing or not a string"))
}

/// An optional field: absent or `null` is `None`, present-but-wrong-type is a
/// `Err` so a wrong type cannot be silently swallowed.
fn opt_f32(obj: &Map<String, Value>, key: &str) -> Result<Option<f32>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => as_f32(v)
            .map(Some)
            .ok_or_else(|| format!("'{key}' is not a number")),
    }
}

fn opt_u32(obj: &Map<String, Value>, key: &str) -> Result<Option<u32>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => as_u32(v)
            .map(Some)
            .ok_or_else(|| format!("'{key}' is not a 32-bit unsigned number")),
    }
}

fn opt_u16(obj: &Map<String, Value>, key: &str) -> Result<Option<u16>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => as_u16(v)
            .map(Some)
            .ok_or_else(|| format!("'{key}' is not a 16-bit unsigned number")),
    }
}

fn opt_str(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn opt_bool(obj: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("'{key}' is not a boolean")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Transition;

    fn layer(name: &str, kind: LayerKind, start_ms: i64, duration_ms: i64) -> Layer {
        Layer {
            id: Uuid::new_v4(),
            kind,
            name: name.to_string(),
            keyframes: Vec::new(),
            extra: None,
            visible: true,
            duration_ms,
            start_ms,
            uri: None,
            effects: Vec::new(),
        }
    }

    fn project_of(layers: Vec<Layer>) -> Project {
        let mut project = Project::new("test");
        project.layers = layers;
        project
    }

    fn key(t: i64, v: f32) -> Keyframe {
        Keyframe::new(t, v)
    }

    fn by_name<'a>(project: &'a Project, name: &str) -> Vec<&'a Layer> {
        project.layers.iter().filter(|l| l.name == name).collect()
    }

    fn clip(kind: LayerKind, at: i64, duration: i64, ripple: bool) -> InsertClipOp {
        InsertClipOp {
            kind,
            at_ms: at,
            duration_ms: duration,
            ripple,
            name: None,
            uri: None,
            text: None,
            argb: None,
            dx: None,
            dy: None,
            scale: None,
            alpha: None,
        }
    }

    // ---- split ------------------------------------------------------------

    #[test]
    fn split_in_the_middle_keeps_the_span() {
        let project = project_of(vec![layer("A", LayerKind::Shape, 0, 1000)]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::Split {
                layer: "A".to_string(),
                at_ms: 400,
            }],
        );
        assert_eq!(outcome.applied.len(), 1, "{}", outcome.applied[0]);
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);

        let mut halves = by_name(&outcome.project, "A");
        assert_eq!(halves.len(), 2);
        halves.sort_by_key(|l| l.start_ms);
        assert_eq!((halves[0].start_ms, halves[0].duration_ms), (0, 400));
        assert_eq!((halves[1].start_ms, halves[1].duration_ms), (400, 600));
        assert_eq!(
            halves[0].duration_ms + halves[1].duration_ms,
            1000,
            "the total span is unchanged"
        );
        assert_ne!(halves[0].id, halves[1].id, "the second half is a new layer");
        assert_eq!(halves[0].kind, halves[1].kind);
        // The input project is untouched.
        assert_eq!(project.layers.len(), 1);
    }

    #[test]
    fn split_offsets_both_halves_from_a_non_zero_start() {
        let project = project_of(vec![layer("A", LayerKind::Media, 1000, 4000)]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::Split {
                layer: "A".to_string(),
                at_ms: 3000,
            }],
        );
        let mut halves = by_name(&outcome.project, "A");
        halves.sort_by_key(|l| l.start_ms);
        assert_eq!((halves[0].start_ms, halves[0].duration_ms), (1000, 2000));
        assert_eq!((halves[1].start_ms, halves[1].duration_ms), (3000, 2000));
    }

    #[test]
    fn split_partitions_and_rebases_keyframes() {
        // Key times are absolute project time (docs/11 §11.8): the engine
        // samples `Keyframe::time_ms` against the frame clock directly, so the
        // stored numbers must not be shifted. The partition puts keys at or
        // before the cut on the first half and keys after it on the second,
        // and because the second half's `start_ms` becomes the cut, reading
        // those keys relative to `start_ms` is the rebase onto its own zero.
        let mut original = layer("Anim", LayerKind::Shape, 1000, 4000);
        original.keyframes = vec![
            key(1000, 0.0),
            key(2000, 10.0),
            key(3000, 20.0),
            key(4000, 30.0),
            key(5000, 40.0),
        ];
        let mut extra = LayerExtra::default();
        extra.track_x = vec![key(1000, 0.0), key(5000, 100.0)];
        original.extra = Some(extra);

        let project = project_of(vec![original]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::Split {
                layer: "Anim".to_string(),
                at_ms: 3000,
            }],
        );
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
        let mut halves = by_name(&outcome.project, "Anim");
        halves.sort_by_key(|l| l.start_ms);
        let (first, second) = (halves[0], halves[1]);

        assert_eq!(
            first
                .keyframes
                .iter()
                .map(|k| k.time_ms)
                .collect::<Vec<_>>(),
            vec![1000, 2000, 3000],
            "keys at or before the cut stay on the first half"
        );
        assert_eq!(
            second
                .keyframes
                .iter()
                .map(|k| k.time_ms)
                .collect::<Vec<_>>(),
            vec![4000, 5000],
            "keys after the cut go to the second half"
        );
        // The second half's keys rebased onto its own zero, then back to
        // absolute: exactly the times the engine will sample.
        assert_eq!(second.start_ms, 3000);
        let rebased: Vec<i64> = second
            .keyframes
            .iter()
            .map(|k| k.time_ms - second.start_ms)
            .collect();
        assert_eq!(rebased, vec![1000, 2000]);
        // The value at any absolute instant is what the unsplit layer gave:
        // 2500 is 15.0 on the first half, 4500 is 35.0 on the second.
        assert_eq!(first.sample_rotation(2500), 15.0);
        assert_eq!(second.sample_rotation(4500), 35.0);
        // The animated tracks are partitioned by the same cut.
        assert_eq!(
            first
                .extra
                .as_ref()
                .expect("first extra")
                .track_x
                .iter()
                .map(|k| k.time_ms)
                .collect::<Vec<_>>(),
            vec![1000]
        );
        assert_eq!(
            second
                .extra
                .as_ref()
                .expect("second extra")
                .track_x
                .iter()
                .map(|k| k.time_ms)
                .collect::<Vec<_>>(),
            vec![5000]
        );
    }

    #[test]
    fn split_drops_the_transition_on_the_second_half() {
        let mut original = layer("A", LayerKind::Shape, 0, 2000);
        original.extra = Some(LayerExtra {
            transition: Some(Transition {
                start_ms: 100,
                duration_ms: 500,
                ..Default::default()
            }),
            ..Default::default()
        });
        let project = project_of(vec![original]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::Split {
                layer: "A".to_string(),
                at_ms: 1000,
            }],
        );
        let mut halves = by_name(&outcome.project, "A");
        halves.sort_by_key(|l| l.start_ms);
        assert!(
            halves[0].extra.as_ref().unwrap().transition.is_some(),
            "the first half keeps the fade"
        );
        assert!(
            halves[1].extra.as_ref().unwrap().transition.is_none(),
            "the second half must not re-fade"
        );
    }

    #[test]
    fn split_outside_the_window_is_a_skip() {
        for at in [-1, 0, 1000, 5000] {
            let project = project_of(vec![layer("A", LayerKind::Shape, 0, 1000)]);
            let outcome = apply_edl(
                &project,
                &[EdlOp::Split {
                    layer: "A".to_string(),
                    at_ms: at,
                }],
            );
            assert!(outcome.applied.is_empty(), "at {at}");
            assert_eq!(outcome.skipped.len(), 1, "at {at}");
            assert_eq!(outcome.project.layers.len(), 1, "at {at}");
        }
    }

    // ---- ripple_delete ----------------------------------------------------

    /// `[1000..6000)` split across every overlap case, with a layer wholly
    /// inside and one wholly before.
    fn ripple_project() -> Project {
        project_of(vec![
            layer("before", LayerKind::Shape, 0, 1000),          // [0, 1000)
            layer("straddle_start", LayerKind::Shape, 1000, 1500), // [1000, 2500)
            layer("spanning", LayerKind::Shape, 1000, 5000),      // [1000, 6000)
            layer("inside", LayerKind::Shape, 2500, 500),         // [2500, 3000)
            layer("straddle_end", LayerKind::Shape, 3000, 2000),  // [3000, 5000)
            layer("after", LayerKind::Shape, 6000, 2000),         // [6000, 8000)
        ])
    }

    fn ripple_delete_once(project: &Project) -> EdlOutcome {
        apply_edl(
            project,
            &[EdlOp::RippleDelete {
                from_ms: 2000,
                to_ms: 4000,
            }],
        )
    }

    #[test]
    fn ripple_delete_leaves_a_layer_before_the_range_untouched() {
        let outcome = ripple_delete_once(&ripple_project());
        let before = &by_name(&outcome.project, "before")[0];
        assert_eq!((before.start_ms, before.duration_ms), (0, 1000));
    }

    #[test]
    fn ripple_delete_trims_a_layer_straddling_the_start() {
        let outcome = ripple_delete_once(&ripple_project());
        let layer = &by_name(&outcome.project, "straddle_start")[0];
        assert_eq!((layer.start_ms, layer.duration_ms), (1000, 1000));
    }

    #[test]
    fn ripple_delete_trims_a_layer_straddling_the_end() {
        let outcome = ripple_delete_once(&ripple_project());
        let layer = &by_name(&outcome.project, "straddle_end")[0];
        assert_eq!((layer.start_ms, layer.duration_ms), (2000, 1000));
    }

    #[test]
    fn ripple_delete_closes_a_layer_spanning_the_range() {
        let outcome = ripple_delete_once(&ripple_project());
        let layer = &by_name(&outcome.project, "spanning")[0];
        assert_eq!((layer.start_ms, layer.duration_ms), (1000, 3000));
    }

    #[test]
    fn ripple_delete_removes_a_layer_inside_the_range() {
        let outcome = ripple_delete_once(&ripple_project());
        assert!(by_name(&outcome.project, "inside").is_empty());
        assert_eq!(outcome.project.layers.len(), 5);
    }

    #[test]
    fn ripple_delete_shifts_a_layer_after_the_range_by_the_removed_length() {
        let outcome = ripple_delete_once(&ripple_project());
        let layer = &by_name(&outcome.project, "after")[0];
        // [6000, 8000) minus the 2000ms gap closes to [4000, 6000).
        assert_eq!((layer.start_ms, layer.duration_ms), (4000, 2000));
    }

    #[test]
    fn ripple_delete_moves_keyframes_with_the_layer() {
        let mut original = layer("Anim", LayerKind::Shape, 0, 5000);
        original.keyframes = vec![key(500, 1.0), key(2500, 2.0), key(3000, 3.0), key(4500, 4.0)];
        let project = project_of(vec![original]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::RippleDelete {
                from_ms: 2000,
                to_ms: 4000,
            }],
        );
        let kept = &by_name(&outcome.project, "Anim")[0];
        // 500 stays, 2500 and 3000 are in the removed range and go, 4500 shifts
        // left by the 2000ms removed.
        assert_eq!(
            kept.keyframes.iter().map(|k| k.time_ms).collect::<Vec<_>>(),
            vec![500, 2500]
        );
        assert_eq!((kept.start_ms, kept.duration_ms), (0, 3000));
    }

    #[test]
    fn ripple_delete_inverted_range_is_a_skip() {
        let project = project_of(vec![layer("A", LayerKind::Shape, 0, 1000)]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::RippleDelete {
                from_ms: 500,
                to_ms: 500,
            }],
        );
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.project.layers.len(), 1);
    }

    // ---- insert_clip ------------------------------------------------------

    #[test]
    fn insert_clip_with_ripple_pushes_later_layers_right() {
        let project = project_of(vec![
            layer("A", LayerKind::Shape, 0, 1000),
            layer("B", LayerKind::Shape, 2000, 1000),
        ]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::InsertClip(clip(
                LayerKind::Shape,
                1000,
                500,
                true,
            ))],
        );
        assert_eq!(outcome.project.layers.len(), 3);
        assert_eq!(by_name(&outcome.project, "A")[0].start_ms, 0, "A is before");
        assert_eq!(
            by_name(&outcome.project, "B")[0].start_ms,
            2500,
            "B is pushed by the clip's duration"
        );
        let inserted = outcome
            .project
            .layers
            .iter()
            .find(|l| l.kind == LayerKind::Shape && l.start_ms == 1000 && l.duration_ms == 500)
            .expect("the new clip");
        assert_eq!(inserted.name, "Shape");
    }

    #[test]
    fn insert_clip_without_ripple_punches_the_span_out() {
        let project = project_of(vec![
            layer("span", LayerKind::Shape, 0, 2000),
            layer("tail", LayerKind::Shape, 1000, 2000),
        ]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::InsertClip(clip(LayerKind::Shape, 1000, 500, false))],
        );
        // "span" [0, 2000) is cut into [0, 1000) + [1500, 2000); "tail"
        // [1000, 3000) loses its head and keeps [1500, 3000).
        let spans = by_name(&outcome.project, "span");
        assert_eq!(spans.len(), 2, "a covered middle splits the layer");
        let mut spans = spans;
        spans.sort_by_key(|l| l.start_ms);
        assert_eq!((spans[0].start_ms, spans[0].duration_ms), (0, 1000));
        assert_eq!((spans[1].start_ms, spans[1].duration_ms), (1500, 500));
        let tail = &by_name(&outcome.project, "tail")[0];
        assert_eq!((tail.start_ms, tail.duration_ms), (1500, 1500));
    }

    #[test]
    fn insert_clip_text_needs_a_payload() {
        let project = project_of(vec![]);
        let mut op = clip(LayerKind::Text, 0, 1000, false);
        op.text = None;
        let outcome = apply_edl(&project, &[EdlOp::InsertClip(op)]);
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert!(outcome.skipped[0].contains("requires"), "{}", outcome.skipped[0]);
        assert!(outcome.project.layers.is_empty());
    }

    #[test]
    fn insert_clip_text_payload_lands_in_extra_text() {
        let project = project_of(vec![]);
        let mut op = clip(LayerKind::Text, 500, 1500, false);
        op.text = Some("hello".to_string());
        let outcome = apply_edl(&project, &[EdlOp::InsertClip(op)]);
        let text = &outcome.project.layers[0];
        assert_eq!(text.kind, LayerKind::Text);
        assert_eq!(text.start_ms, 500);
        assert_eq!(text.duration_ms, 1500);
        assert_eq!(
            text.extra.as_ref().unwrap().text.as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn insert_clip_negative_duration_is_a_skip() {
        let project = project_of(vec![]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::InsertClip(clip(LayerKind::Shape, 0, 0, false))],
        );
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
    }

    // ---- duck_audio -------------------------------------------------------

    #[test]
    fn duck_audio_is_refused_and_changes_nothing() {
        let project = project_of(vec![layer("A", LayerKind::Audio, 0, 1000)]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::DuckAudio {
                layer: "A".to_string(),
                from_ms: 0,
                to_ms: 500,
                gain: 0.25,
            }],
        );
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert!(
            outcome.skipped[0].contains("no audio gain stage"),
            "{}",
            outcome.skipped[0]
        );
        assert_eq!(outcome.skipped[0], DUCK_AUDIO_REASON);
        assert_eq!(outcome.project, project, "the project is untouched");
    }

    // ---- add_subtitles ----------------------------------------------------

    #[test]
    fn add_subtitles_creates_one_text_layer_per_cue() {
        let project = project_of(vec![]);
        let cues = vec![
            SubtitleCue {
                from_ms: 0,
                to_ms: 1000,
                text: "one".to_string(),
            },
            SubtitleCue {
                from_ms: 500,
                to_ms: 1500,
                text: "two".to_string(),
            },
        ];
        let style = SubtitleStyle {
            argb: Some(0xFFFFFFFF),
            text_weight: Some(700),
            stroke_px: Some(3.0),
            stroke_argb: Some(0xFF000000),
        };
        let outcome = apply_edl(
            &project,
            &[EdlOp::AddSubtitles {
                cues: cues.clone(),
                style: style.clone(),
            }],
        );
        assert_eq!(outcome.applied.len(), 1);
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
        assert_eq!(outcome.project.layers.len(), 2, "overlaps are allowed");
        for (created, cue) in outcome.project.layers.iter().zip(cues.iter()) {
            assert_eq!(created.kind, LayerKind::Text);
            assert_eq!(created.start_ms, cue.from_ms);
            assert_eq!(created.duration_ms, cue.to_ms - cue.from_ms);
            let extra = created.extra.as_ref().expect("extra");
            assert_eq!(extra.text.as_deref(), Some(cue.text.as_str()));
            assert_eq!(extra.argb, Some(0xFFFFFFFF));
            assert_eq!(extra.text_weight, 700);
            assert_eq!(extra.stroke_px, 3.0);
        }
    }

    #[test]
    fn add_subtitles_skips_empty_text_and_bad_times_per_cue() {
        let project = project_of(vec![]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::AddSubtitles {
                cues: vec![
                    SubtitleCue {
                        from_ms: 0,
                        to_ms: 1000,
                        text: "  ".to_string(),
                    },
                    SubtitleCue {
                        from_ms: 900,
                        to_ms: 100,
                        text: "backwards".to_string(),
                    },
                    SubtitleCue {
                        from_ms: 2000,
                        to_ms: 2500,
                        text: "good".to_string(),
                    },
                ],
                style: SubtitleStyle::default(),
            }],
        );
        assert_eq!(outcome.project.layers.len(), 1, "only the good cue lands");
        assert_eq!(outcome.project.layers[0].start_ms, 2000);
        assert_eq!(outcome.applied.len(), 1);
        assert_eq!(outcome.skipped.len(), 2, "{:?}", outcome.skipped);
    }

    // ---- bad input, lookup, JSON round trip -------------------------------

    #[test]
    fn lookup_reports_missing_and_ambiguous_layers() {
        let project = project_of(vec![
            layer("Dup", LayerKind::Shape, 0, 1000),
            layer("Dup", LayerKind::Shape, 1000, 1000),
        ]);
        let missing = apply_edl(
            &project,
            &[EdlOp::Split {
                layer: "ghost".to_string(),
                at_ms: 100,
            }],
        );
        assert_eq!(missing.skipped.len(), 1);
        assert!(missing.skipped[0].contains("ghost"));

        let ambiguous = apply_edl(
            &project,
            &[EdlOp::Split {
                layer: "Dup".to_string(),
                at_ms: 500,
            }],
        );
        assert_eq!(ambiguous.skipped.len(), 1);
        assert!(
            ambiguous.skipped[0].contains("ambiguous") && ambiguous.skipped[0].contains('2'),
            "{}",
            ambiguous.skipped[0]
        );
    }

    #[test]
    fn lookup_by_uuid_string_works() {
        let mut original = layer("A", LayerKind::Shape, 0, 1000);
        let id = original.id;
        original.name = "renamed".to_string();
        let project = project_of(vec![original]);
        let outcome = apply_edl(
            &project,
            &[EdlOp::Split {
                layer: id.to_string(),
                at_ms: 500,
            }],
        );
        assert_eq!(outcome.applied.len(), 1, "{:?}", outcome.skipped);
        assert_eq!(outcome.project.layers.len(), 2);
    }

    #[test]
    fn unknown_and_malformed_ops_are_skips_not_panics() {
        // An unknown op name survives parsing as `Unknown` and is reported.
        let parsed = parse_ops_json(r#"[{"op":"frobnicate","x":1}]"#).expect("parse");
        assert_eq!(parsed.len(), 1);
        let project = project_of(vec![layer("A", LayerKind::Shape, 0, 1000)]);
        let outcome = apply_edl(&project, &parsed);
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert!(outcome.skipped[0].contains("frobnicate"));

        // A known op with a wrong-typed field becomes an `Unknown` skip for
        // that op alone.
        let parsed = parse_ops_json(r#"[{"op":"split","layer":"A","atMs":"soon"}]"#).expect("parse");
        let outcome = apply_edl(&project, &parsed);
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert!(outcome.skipped[0].contains("split"), "{}", outcome.skipped[0]);
    }

    #[test]
    fn bad_json_is_an_error_not_a_panic() {
        const PROJECT: &str = r#"{"name":"p","layers":[
            {"id":"11111111-1111-4111-8111-111111111111","kind":"SHAPE",
             "name":"A","durationMs":1000,"startMs":0}]}"#;
        assert!(apply_edl_json(PROJECT, "{not json").is_err());
        assert!(apply_edl_json(PROJECT, r#"{"op":"split"}"#).is_err(), "not an array");
        assert!(apply_edl_json("{not json", "[]").is_err());
        // A whole op that is structurally wrong is still just a skip.
        let out = apply_edl_json(PROJECT, r#"[42,{"op":"nope"}]"#).expect("the batch runs");
        let bytes = project_from_json(&out).expect("reparse");
        let decoded = codec::decode(&bytes).expect("decode");
        assert_eq!(decoded.layers.len(), 1, "the project is unchanged");
    }

    #[test]
    fn apply_edl_json_round_trip() {
        const PROJECT: &str = r#"{"name":"p","layers":[
            {"id":"11111111-1111-4111-8111-111111111111","kind":"SHAPE",
             "name":"A","durationMs":1000,"startMs":0}]}"#;
        let ops = r#"[
            {"op":"split","layer":"A","atMs":500},
            {"op":"insert_clip","kind":"text","atMs":1500,"durationMs":400,
             "ripple":true,"text":"hi"},
            {"op":"duck_audio","layer":"A","fromMs":0,"toMs":100,"gain":0.5}
        ]"#;
        let out = apply_edl_json(PROJECT, ops).expect("apply");
        let bytes = project_from_json(&out).expect("the output parses back");
        let project = codec::decode(&bytes).expect("decode");
        assert_eq!(project.layers.len(), 3, "split(2) + text");
        let texts: Vec<&Layer> = project
            .layers
            .iter()
            .filter(|l| l.kind == LayerKind::Text)
            .collect();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].duration_ms, 400);
    }
}
