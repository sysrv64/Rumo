// SPDX-License-Identifier: Apache-2.0
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ease::Ease;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub schema_tag: u32,
    pub name: String,
    pub layers: Vec<Layer>,
    /// Project-defined effects this project carries. Instances address one by id
    /// through [`crate::effect::EffectTarget::Custom`], and the definitions ride
    /// in the project document rather than in each instance, so one definition
    /// serves every chain that uses it. Tag ≤ 4 bytes decode as empty.
    #[serde(default)]
    pub custom: Vec<crate::effect::CustomEffect>,
    /// The frame this project is composed in. Tag ≤ 5 bytes decode as
    /// [`Canvas::default`], the 512 × 288 frame the editor always used.
    #[serde(default)]
    pub canvas: Canvas,
}

/// The frame a project is composed in: its design size and its background.
///
/// Both are project data rather than engine constants, so a vertical clip and a
/// different background survive a save instead of snapping back to the built-in
/// frame. The compositor may scale this to the surface; the size here is the
/// design frame, not a window size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Canvas {
    pub width: u16,
    pub height: u16,
    /// Background colour, `0xAARRGGBB`.
    pub background: u32,
}

impl Canvas {
    /// Smallest accepted side, in pixels. Anything smaller (a zero, a negative)
    /// is nonsense and the loader falls back to [`Canvas::default`].
    pub const MIN_SIDE: u16 = 16;
    /// Largest accepted side, in pixels.
    pub const MAX_SIDE: u16 = 8192;
}

impl Default for Canvas {
    fn default() -> Self {
        // 0xFF14_1824 is the background the editor has always drawn with
        // (`EditorState.PREVIEW_BG`).
        Self {
            width: 512,
            height: 288,
            background: 0xFF14_1824,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: Uuid,
    pub kind: LayerKind,
    pub name: String,
    pub keyframes: Vec<Keyframe>,
    /// Append-only visual props (v1 bytes decode as `None`).
    #[serde(default)]
    pub extra: Option<LayerExtra>,
    /// Row visibility toggle (v1 bytes decode as `true`).
    #[serde(default = "default_visible")]
    pub visible: bool,
    /// Per-layer duration in ms (v1 bytes decode as default).
    #[serde(default = "default_layer_duration_ms")]
    pub duration_ms: i64,
    /// Where the layer sits on the timeline, in ms from the start of the
    /// project. It lives on `Layer` rather than in `LayerExtra` because it is a
    /// position on the timeline, not a drawing property, and it must be readable
    /// for a layer whose `extra` is `None` altogether. Tag ≤ 7 bytes decode as
    /// 0 — every layer used to be pinned to the first frame.
    #[serde(default)]
    pub start_ms: i64,
    /// Media/audio content URI (v1 bytes decode as `None`).
    #[serde(default)]
    pub uri: Option<String>,
    /// Effect chain applied to this layer, in draw order (v1/v2 bytes decode
    /// as an empty chain).
    #[serde(default)]
    pub effects: Vec<crate::effect::EffectInstance>,
}

/// Per-layer fallback duration: mirrors the Kotlin editor default
/// (`EditorState.DEFAULT_MIN_DURATION_MS`).
pub const DEFAULT_LAYER_DURATION_MS: i64 = 5000;

fn default_visible() -> bool {
    true
}

fn default_layer_duration_ms() -> i64 {
    DEFAULT_LAYER_DURATION_MS
}

fn default_alpha() -> f32 {
    1.0
}

fn default_scale() -> f32 {
    1.0
}

fn default_text() -> Option<String> {
    None
}

/// Weight a text layer carries when the document does not say: the regular face.
fn default_text_weight() -> u16 {
    400
}

fn default_true() -> bool {
    true
}

fn default_transition_duration_ms() -> i64 {
    500
}

/// A cross-fade of this layer against the layer *under* it.
///
/// The engine has no graph executor: it composites the stack in paint order and
/// blends every layer by an alpha the editor passes in per frame. A transition is
/// therefore stored as data on the *incoming* layer — "over this window, fade me
/// in from the layer below" — and the editor expands it into the per-frame alphas
/// it already hands to the compositor. The dissolve stays a GPU blend of two real
/// layers; nothing is baked and no second renderer is invented.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// Offset from the start of the project, in ms.
    #[serde(default)]
    pub start_ms: i64,
    /// Ramp length in ms (the editor clamps it to at least 1).
    #[serde(default = "default_transition_duration_ms")]
    pub duration_ms: i64,
    /// `true` — dissolve against the layer below, so both ramps move;
    /// `false` — fade in from nothing and leave the layer below untouched.
    #[serde(default = "default_true")]
    pub with_previous: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The shape of the ramp. Defaults to [`Ease::SMOOTH`], which **is** the
    /// `3t² - 2t³` smoothstep this ramp used before curves existed — so a
    /// project that never chose a curve looks exactly as it did.
    #[serde(default = "default_transition_ease")]
    pub ease: Ease,
}

impl Default for Transition {
    fn default() -> Self {
        Self {
            start_ms: 0,
            duration_ms: default_transition_duration_ms(),
            with_previous: true,
            enabled: true,
            ease: default_transition_ease(),
        }
    }
}

/// The ramp every transition had before curves existed.
fn default_transition_ease() -> Ease {
    Ease::SMOOTH
}

impl Transition {
    /// Progress of the cross-fade at project time `t_ms`, in `0..=1`.
    ///
    /// The editor expands this into the per-frame alphas it hands the
    /// compositor, and it is the *same* formula on both sides — Kotlin's
    /// `TransitionUi.rampAt` must return the same number for the same inputs,
    /// because the two are one ramp, not two similar ones.
    ///
    /// A zero duration would divide by zero, so it reads as the shortest legal
    /// ramp rather than as a jump to the end.
    pub fn ramp_at(&self, t_ms: i64) -> f32 {
        let duration = self.duration_ms.max(1) as f32;
        let progress = ((t_ms - self.start_ms) as f32 / duration).clamp(0.0, 1.0);
        self.ease.at(progress)
    }
}

/// Per-layer props for [`Layer`]: the visual transform, the TEXT payload and the
/// optional cross-fade.
///
/// Postcard is positional, so appending a field here is *not* self-describing —
/// [`crate::codec`] names the shape by wire tag and keeps a legacy reader per
/// older tag. Tag ≤ 3 bytes decode with `transition = None`.
///
/// Not `Copy` since `text` landed: `Layer::extra` is read behind a shared
/// reference, so callers clone instead of copying.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerExtra {
    #[serde(default)]
    pub argb: Option<u32>,
    #[serde(default)]
    pub dx: f32,
    #[serde(default)]
    pub dy: f32,
    #[serde(default = "default_alpha")]
    pub alpha: f32,
    /// Uniform preview scale of MEDIA/SHAPE layers; `1.0` is the legacy size
    /// (60% of frame height). Tag ≤ 2 bytes decode as `1.0`.
    #[serde(default = "default_scale")]
    pub scale: f32,
    /// TEXT payload. Mirrored by `EditorState.toJson`/`loadFromJson`; `None`
    /// means "the editor falls back to [`Layer::name`]".
    #[serde(default = "default_text")]
    pub text: Option<String>,
    /// Weight asked of the font database for a TEXT layer (400 regular, 700
    /// bold). A family that has the face uses it; one that does not gets a
    /// thickened bitmap, so the control does something on every device. Tag ≤ 6
    /// bytes decode as 400.
    #[serde(default = "default_text_weight")]
    pub text_weight: u16,
    /// Outline thickness of a TEXT layer, in pixels; 0 draws no contour. The
    /// outline is drawn behind the glyphs, so it reads as a contour and not as
    /// a heavier letter. Tag ≤ 6 bytes decode as 0.
    #[serde(default)]
    pub stroke_px: f32,
    /// Outline colour as `0xAARRGGBB`; `None` is opaque black. Tag ≤ 6 bytes
    /// decode as `None`.
    #[serde(default)]
    pub stroke_argb: Option<u32>,
    /// Cross-fade against the layer below. Tag ≤ 3 bytes decode as `None`.
    #[serde(default)]
    pub transition: Option<Transition>,
    /// Animated horizontal offset. Empty means "not animated": the base
    /// [`LayerExtra::dx`] applies. Tag ≤ 7 bytes decode as empty.
    #[serde(default)]
    pub track_x: Vec<Keyframe>,
    /// Animated vertical offset; empty means the base [`LayerExtra::dy`].
    #[serde(default)]
    pub track_y: Vec<Keyframe>,
    /// Animated uniform size; empty means the base [`LayerExtra::scale`].
    #[serde(default)]
    pub track_scale: Vec<Keyframe>,
    /// Animated opacity; empty means the base [`LayerExtra::alpha`].
    #[serde(default)]
    pub track_alpha: Vec<Keyframe>,
}

impl Default for LayerExtra {
    fn default() -> Self {
        Self {
            argb: None,
            dx: 0.0,
            dy: 0.0,
            alpha: 1.0,
            scale: 1.0,
            text: None,
            text_weight: default_text_weight(),
            stroke_px: 0.0,
            stroke_argb: None,
            transition: None,
            track_x: Vec::new(),
            track_y: Vec::new(),
            track_scale: Vec::new(),
            track_alpha: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum LayerKind {
    #[default]
    Shape = 0,
    Text = 1,
    Media = 2,
    Audio = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub time_ms: i64,
    pub value_f32: f32,
    /// The shape of the motion **from this key to the next one**.
    ///
    /// Stored on the outgoing key rather than on the pair, which is what CSS's
    /// `animation-timing-function` and every keyframe editor do: a key owns how
    /// it leaves. The last key's curve is unused — there is no segment after it.
    ///
    /// Tag ≤ 8 bytes decode as [`Ease::Linear`], so a document written before
    /// curves existed keeps the exact arithmetic it was written with.
    #[serde(default)]
    pub ease: Ease,
}

impl Keyframe {
    /// A key with no curve of its own: the straight line to the next one.
    pub fn new(time_ms: i64, value_f32: f32) -> Self {
        Self {
            time_ms,
            value_f32,
            ease: Ease::Linear,
        }
    }

    /// A key that leaves along `ease`.
    pub fn eased(time_ms: i64, value_f32: f32, ease: Ease) -> Self {
        Self {
            time_ms,
            value_f32,
            ease,
        }
    }
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            schema_tag: crate::codec::CURRENT_TAG,
            name: name.into(),
            layers: Vec::new(),
            custom: Vec::new(),
            canvas: Canvas::default(),
        }
    }
}

/// Linearly interpolates `value_f32` over `time_ms`.
///
/// Empty input yields 0.0; `t_ms` before the first key clamps to the first
/// value, after the last key to the last value; exact hits return the key.
pub fn sample_value(keys: &[Keyframe], t_ms: i64) -> f32 {
    let Some(first) = keys.first() else {
        return 0.0;
    };
    if t_ms <= first.time_ms {
        return first.value_f32;
    }
    let last = keys[keys.len() - 1];
    if t_ms >= last.time_ms {
        return last.value_f32;
    }
    let i = keys.iter().position(|k| t_ms <= k.time_ms).unwrap_or(1);
    let a = keys[i - 1];
    let b = keys[i];
    let dt = (b.time_ms - a.time_ms) as f32;
    if !(dt > 0.0) {
        return b.value_f32;
    }
    let progress = ((t_ms - a.time_ms) as f32 / dt).clamp(0.0, 1.0);
    // The curve belongs to the *outgoing* key: `a` decides how it leaves. A
    // linear curve makes this the exact expression it was before curves
    // existed, which is what keeps every old document identical.
    a.value_f32 + (b.value_f32 - a.value_f32) * a.ease.at(progress)
}

/// Sample one animated property at `t_ms`, falling back to `base` when the
/// track is empty.
///
/// "Empty track" is the one case that must *not* reach [`sample_value`], which
/// answers 0.0 for it: a 0.0 alpha or a 0.0 scale does not mean "no animation",
/// it means the layer is invisible — the failure a user sees as "the layer
/// disappeared".
fn sample_or_base(keys: &[Keyframe], base: f32, t_ms: i64) -> f32 {
    if keys.is_empty() {
        base
    } else {
        sample_value(keys, t_ms)
    }
}

impl Layer {
    /// Rotation at `t_ms`, in degrees. `keyframes` stays the rotation track
    /// only; the property tracks live in [`LayerExtra`].
    pub fn sample_rotation(&self, t_ms: i64) -> f32 {
        sample_value(&self.keyframes, t_ms)
    }

    /// Horizontal offset at `t_ms`: the `track_x` track, else `extra.dx`.
    pub fn sample_x(&self, t_ms: i64) -> f32 {
        // A layer with no `extra` at all has no props, which is exactly the
        // `LayerExtra::default()` set — read it from there rather than repeating
        // the base values, so a default that changes cannot drift away from the
        // sampler.
        let defaults = LayerExtra::default();
        let extra = self.extra.as_ref().unwrap_or(&defaults);
        sample_or_base(&extra.track_x, extra.dx, t_ms)
    }

    /// Vertical offset at `t_ms`: the `track_y` track, else `extra.dy`.
    pub fn sample_y(&self, t_ms: i64) -> f32 {
        let defaults = LayerExtra::default();
        let extra = self.extra.as_ref().unwrap_or(&defaults);
        sample_or_base(&extra.track_y, extra.dy, t_ms)
    }

    /// Uniform size at `t_ms`: the `track_scale` track, else `extra.scale`.
    pub fn sample_scale(&self, t_ms: i64) -> f32 {
        let defaults = LayerExtra::default();
        let extra = self.extra.as_ref().unwrap_or(&defaults);
        sample_or_base(&extra.track_scale, extra.scale, t_ms)
    }

    /// Opacity at `t_ms`: the `track_alpha` track, else `extra.alpha`.
    pub fn sample_alpha(&self, t_ms: i64) -> f32 {
        let defaults = LayerExtra::default();
        let extra = self.extra.as_ref().unwrap_or(&defaults);
        sample_or_base(&extra.track_alpha, extra.alpha, t_ms)
    }

    /// Whether this layer belongs to the frame at `t_ms`:
    /// `t >= start_ms && t < start_ms + duration_ms`.
    ///
    /// A layer whose `duration_ms` did not survive clamping is therefore drawn
    /// *never*: `start + 0` leaves no instant that satisfies both bounds.
    pub fn in_frame(&self, t_ms: i64) -> bool {
        // Qualified: a bare path here could be read as the method itself.
        self::in_frame(t_ms, self.start_ms, self.duration_ms)
    }
}

/// `t >= start_ms && t < start_ms + duration_ms` — the in-frame predicate of
/// §11.2, shared by the document (`Layer::in_frame`) and the frame builder.
///
/// `checked_add` rather than `saturating_add`, and the difference is not
/// cosmetic. A window that cannot be expressed as two instants — the sentinel
/// "no time limit" being `i64::MIN` with an `i64::MAX` duration — *saturates* to
/// an end of `-1`, which would silently turn "always" into "never" and blank the
/// whole frame. A window whose end leaves the range of `i64` covers every instant
/// a caller can actually ask about, so that is what it answers.
pub fn in_frame(t_ms: i64, start_ms: i64, duration_ms: i64) -> bool {
    if t_ms < start_ms {
        return false;
    }
    match start_ms.checked_add(duration_ms) {
        Some(end) => t_ms < end,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_empty_zero() {
        assert_eq!(sample_value(&[], 500), 0.0);
    }

    #[test]
    fn sample_clamps_ends() {
        let keys = [
            Keyframe {
                time_ms: 100,
                value_f32: 1.0,
                ease: Ease::Linear,
            },
            Keyframe {
                time_ms: 900,
                value_f32: 9.0,
                ease: Ease::Linear,
            },
        ];
        assert_eq!(sample_value(&keys, 0), 1.0);
        assert_eq!(sample_value(&keys, 100), 1.0);
        assert_eq!(sample_value(&keys, 900), 9.0);
        assert_eq!(sample_value(&keys, 10_000), 9.0);
    }

    #[test]
    fn sample_midpoint_lerp() {
        let keys = [
            Keyframe {
                time_ms: 0,
                value_f32: 0.0,
                ease: Ease::Linear,
            },
            Keyframe {
                time_ms: 1000,
                value_f32: 10.0,
                ease: Ease::Linear,
            },
        ];
        assert_eq!(sample_value(&keys, 500), 5.0);
    }

    /// A layer whose four property tracks are all empty and whose bases are
    /// deliberately unlike 0.0/1.0, so "the sampler returned its base" cannot
    /// pass by accident.
    fn layer_with_bases() -> Layer {
        Layer {
            id: Uuid::new_v4(),
            kind: LayerKind::Shape,
            name: "tracked".to_string(),
            keyframes: Vec::new(),
            extra: Some(LayerExtra {
                dx: 12.5,
                dy: -7.0,
                alpha: 0.25,
                scale: 2.5,
                ..Default::default()
            }),
            visible: true,
            duration_ms: DEFAULT_LAYER_DURATION_MS,
            start_ms: 0,
            uri: None,
            effects: Vec::new(),
        }
    }

    fn keys(pairs: &[(i64, f32)]) -> Vec<Keyframe> {
        pairs
            .iter()
            .map(|&(time_ms, value_f32)| Keyframe { time_ms, value_f32 ,
                ease: Ease::Linear,
            })
            .collect()
    }

    #[test]
    fn empty_tracks_sample_the_base_not_zero() {
        let layer = layer_with_bases();
        for t in [0, 100, 5_000, 1_000_000] {
            assert_eq!(layer.sample_x(t), 12.5, "empty track_x at {t}");
            assert_eq!(layer.sample_y(t), -7.0, "empty track_y at {t}");
            assert_eq!(layer.sample_scale(t), 2.5, "empty track_scale at {t}");
            assert_eq!(layer.sample_alpha(t), 0.25, "empty track_alpha at {t}");
        }
    }

    #[test]
    fn a_bare_layer_samples_the_default_bases() {
        // No `extra` at all: the defaults must still come through, or every
        // un-propsed layer would render at alpha 0.
        let mut layer = layer_with_bases();
        layer.extra = None;
        assert_eq!(layer.sample_x(400), 0.0);
        assert_eq!(layer.sample_y(400), 0.0);
        assert_eq!(layer.sample_scale(400), 1.0);
        assert_eq!(layer.sample_alpha(400), 1.0);
    }

    #[test]
    fn one_key_holds_for_every_time() {
        let mut layer = layer_with_bases();
        layer.extra.as_mut().expect("extra").track_scale = keys(&[(300, 4.0)]);
        for t in [0, 299, 300, 301, 10_000] {
            assert_eq!(layer.sample_scale(t), 4.0, "one key at {t}");
        }
    }

    #[test]
    fn keys_bracket_the_sample_time() {
        // Before the first key the first value holds, between the keys the
        // value is linear in time, after the last one the last value holds.
        let mut layer = layer_with_bases();
        {
            let extra = layer.extra.as_mut().expect("extra");
            extra.track_x = keys(&[(100, 0.0), (1100, 100.0)]);
            extra.track_y = keys(&[(100, -10.0), (1100, 10.0)]);
            extra.track_alpha = keys(&[(0, 1.0), (1000, 0.0)]);
        }
        assert_eq!(layer.sample_x(0), 0.0, "before the first key");
        assert_eq!(layer.sample_x(99), 0.0);
        assert_eq!(layer.sample_x(600), 50.0, "halfway");
        assert_eq!(layer.sample_x(1100), 100.0, "on the last key");
        assert_eq!(layer.sample_x(9999), 100.0, "after the last key");
        assert_eq!(layer.sample_y(600), 0.0);
        assert_eq!(layer.sample_y(350), -5.0);
        assert_eq!(layer.sample_alpha(500), 0.5);
        assert_eq!(layer.sample_alpha(1000), 0.0);
        // The tracks that were not filled in still answer their base.
        assert_eq!(layer.sample_scale(500), 2.5);
    }

    #[test]
    fn a_key_exactly_on_the_sample_time_returns_it() {
        let mut layer = layer_with_bases();
        {
            let extra = layer.extra.as_mut().expect("extra");
            extra.track_x = keys(&[(0, 0.0), (500, 50.0), (1000, 100.0)]);
            extra.track_y = keys(&[(200, 7.0)]);
        }
        assert_eq!(layer.sample_x(0), 0.0);
        assert_eq!(layer.sample_x(500), 50.0);
        assert_eq!(layer.sample_x(1000), 100.0);
        assert_eq!(layer.sample_y(200), 7.0);
        assert_eq!(layer.sample_y(199), 7.0);
        assert_eq!(layer.sample_y(201), 7.0);
    }

    #[test]
    fn duplicate_and_unordered_keys_answer_a_finite_value() {
        // The document is not validated on the way in, so a track can hold two
        // keys at the same instant or keys out of order. `sample_value` is
        // shared with rotation and must not be changed to cope — the samplers
        // only have to stay total.
        let mut layer = layer_with_bases();
        {
            let extra = layer.extra.as_mut().expect("extra");
            extra.track_x = keys(&[(500, 1.0), (500, 2.0)]);
            extra.track_y = keys(&[(900, 3.0), (100, 1.0)]);
            extra.track_scale = keys(&[(i64::MAX, 1.0), (0, 0.0)]);
        }
        for t in [-1, 0, 250, 500, 750, i64::MAX] {
            assert!(layer.sample_x(t).is_finite(), "duplicate x at {t}");
            assert!(layer.sample_y(t).is_finite(), "unordered y at {t}");
            assert!(layer.sample_scale(t).is_finite(), "reversed scale at {t}");
        }
    }

    #[test]
    fn rotation_still_comes_from_keyframes() {
        let mut layer = layer_with_bases();
        layer.keyframes = keys(&[(0, 0.0), (1000, 90.0)]);
        // A property track is deliberately present and unrelated: rotation must
        // keep coming from `keyframes` alone.
        layer.extra.as_mut().expect("extra").track_scale = keys(&[(0, 5.0)]);
        assert_eq!(layer.sample_rotation(0), 0.0);
        assert_eq!(layer.sample_rotation(500), 45.0);
        assert_eq!(layer.sample_rotation(1000), 90.0);
        assert_eq!(layer.sample_rotation(5000), 90.0);
        // A layer with no rotation keys rotates by none, not by a property
        // track's value.
        layer.keyframes.clear();
        assert_eq!(layer.sample_rotation(500), 0.0);
    }

    #[test]
    fn in_frame_is_the_half_open_window() {
        let mut layer = layer_with_bases();
        layer.start_ms = 1000;
        layer.duration_ms = 2000;
        assert!(!layer.in_frame(999), "one ms before the start");
        assert!(layer.in_frame(1000), "the start instant belongs to it");
        assert!(layer.in_frame(2999));
        assert!(!layer.in_frame(3000), "the end instant belongs to the next");
        assert!(!layer.in_frame(1_000_000));
        // The default duration is still a duration: a layer is a window, not a
        // ray. Only an unbounded `duration_ms` makes it one.
        let mut open = layer_with_bases();
        open.start_ms = 0;
        open.duration_ms = DEFAULT_LAYER_DURATION_MS;
        assert!(open.in_frame(0));
        assert!(!open.in_frame(i64::MAX - 1), "a 5s layer is not endless");
        open.duration_ms = i64::MAX;
        assert!(open.in_frame(i64::MAX - 1), "an unbounded layer really is");
    }

    #[test]
    fn a_zero_duration_layer_is_never_in_frame() {
        // The editor should not produce this, but a layer with a zero length
        // must not cover the whole project instead.
        let mut layer = layer_with_bases();
        layer.start_ms = 500;
        layer.duration_ms = 0;
        assert!(!layer.in_frame(0));
        assert!(!layer.in_frame(500));
        assert!(!layer.in_frame(1_000_000));
        layer.duration_ms = -10;
        assert!(!layer.in_frame(500));
    }

    #[test]
    fn an_absurd_window_answers_instead_of_overflowing() {
        // `start + duration` is computed, so a pair that would overflow a `+`
        // must still answer — and answer with the truth rather than whatever a
        // wrapped bound happens to contain.
        // A window whose end leaves the `i64` range cannot be written down as two
        // instants, so it covers every instant a caller can ask about. This is
        // why the sum is `checked_add` and not a wrapping one: two absurdly
        // large document values must answer "inside", not "somewhere before the
        // end of time".
        assert!(in_frame(i64::MAX - 5, i64::MAX - 5, i64::MAX));
        assert!(in_frame(i64::MAX, i64::MAX - 5, i64::MAX));
        // A window at the very bottom: `MIN + 10` is representable, and it does
        // not contain -5 — the answer is "not in frame", not a wrapped `true`.
        assert!(!in_frame(1_000, 10, i64::MIN));
        assert!(!in_frame(-5, i64::MIN, 10));
        assert!(in_frame(i64::MIN, i64::MIN, 10), "the first instant is in");
        assert!(!in_frame(i64::MIN + 10, i64::MIN, 10), "the end instant is out");
        // "No time limit" is not expressible as a pair of instants at all —
        // `MIN` plus `MAX` is the representable value `-1`, i.e. an empty
        // window — so the frame builder models it as the *absence* of a window
        // rather than as a sentinel pair. See `rumo_render::window_of`.
    }
}
