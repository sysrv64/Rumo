// SPDX-License-Identifier: Apache-2.0

//! Resolution / canvas model: the one place that decides what "a frame" is —
//! pixel size, frame rate, and the derived encoder bitrate.
//!
//! Implemented from `docs/08-rust-layer-deepening.md`, 8.5. Design notes:
//!
//! * **Sides are even.** NV12 chroma planes are half-resolution, and every
//!   H.264/HEVC profile in the export path requires macroblock-aligned luma
//!   dimensions; instead of discovering that at encode time, [`CanvasSpec::new`]
//!   refuses odd sides up front ([`CanvasError::Odd`]).
//! * **No max dimension in the constructor.** A canvas is a *user intent*; how
//!   large the GPU can actually allocate is an adapter property (see the wgpu
//!   `max_texture_dimension_2d` note in 8.5). [`CanvasSpec::fits_limits`] and
//!   [`CanvasSpec::clamped_to_limits`] express the device side, so a project
//!   saved on a desktop and opened on a downlevel device validates instead of
//!   failing to load.
//! * **`fps` accepts anything in `1..=240`**, not only [`FPS_CHOICES`]: the
//!   choice list is a UI convenience, and a project imported from a 23.976 fps
//!   source may legitimately carry a non-listed integer rate.
//! * **Nothing here panics on degenerate input.** The struct fields are public,
//!   so a value can exist without passing through [`CanvasSpec::new`]; every
//!   derived computation guards against zeros and non-finite floats.

/// Inclusive frame-rate bounds accepted by [`CanvasSpec::new`].
const MIN_FPS: u32 = 1;
/// Inclusive frame-rate bounds accepted by [`CanvasSpec::new`].
const MAX_FPS: u32 = 240;

/// Lower clamp of [`CanvasSpec::bitrate_bps`], in bits per second.
const MIN_BITRATE_BPS: u32 = 1;
/// Upper clamp of [`CanvasSpec::bitrate_bps`], in bits per second (120 Mbit/s).
const MAX_BITRATE_BPS: u32 = 120_000_000;

/// Bitrate heuristic coefficient, in bits per pixel per frame at 1 pixel.
///
/// The rate is `fps * BITRATE_COEFF * pixels^BITRATE_EXPONENT`; the exponent
/// below 1.0 is what makes it "bits per pixel *and* frame": the effective
/// bits-per-pixel-per-frame is `BITRATE_COEFF * pixels^(-0.15)`, which
/// *decreases* as frames grow, because a larger frame decorrelates better per
/// pixel. Concretely the heuristic lands at ≈ 0.245 bpp/frame at 480p, 0.20 at
/// 720p, 0.17 at 1080p, 0.156 at 4K and 0.127 at 8K, giving ≈ 3 Mbit/s for
/// 854×480@30, ≈ 6 for 720p@30, ≈ 12 for 1080p@30 and ≈ 39 for 4K@30 — i.e.
/// the usual "good quality" ladder for H.264.
const BITRATE_COEFF: f64 = 1.7;

/// Resolution-compression exponent of the [`BITRATE_COEFF`] heuristic.
///
/// Strictly between 0 and 1 so that `pixels * bpp` is **strictly increasing** in
/// the pixel count: that is what makes [`CanvasSpec::bitrate_bps`] monotone (see
/// the `bitrate_is_monotone_in_pixels_at_fixed_fps` test).
const BITRATE_EXPONENT: f64 = 0.85;

/// A concrete render/export canvas.
///
/// Constructed through [`CanvasSpec::new`], which enforces the invariants the
/// downstream NV12 encoder relies on (non-zero, even-sided, sane `fps`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanvasSpec {
    /// Width in pixels. Even and non-zero for any spec built by [`CanvasSpec::new`].
    pub width: u32,
    /// Height in pixels. Even and non-zero for any spec built by [`CanvasSpec::new`].
    pub height: u32,
    /// Frames per second, in `1..=240`.
    pub fps: u32,
}

/// One named entry of the export-resolution menu ([`PRESETS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionPreset {
    /// Stable machine id (`"1080p"`, `"4k_portrait"`). Never shown to users.
    pub id: &'static str,
    /// Human-readable label (`"1080p (FHD)"`).
    pub label: &'static str,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Why a [`CanvasSpec`] was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasError {
    /// One or both sides are `0`.
    Zero,
    /// A side exceeds the device limit carried in `max`.
    TooLarge {
        /// Largest accepted side, in pixels (e.g. `max_texture_dimension_2d`).
        max: u32,
    },
    /// A side is odd; the NV12/H.264 path needs even sides.
    Odd,
    /// `fps` is outside `1..=240`.
    BadFps,
}

impl std::fmt::Display for CanvasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zero => write!(f, "canvas dimensions must be non-zero"),
            Self::TooLarge { max } => {
                write!(f, "canvas dimension exceeds the device limit of {max} px")
            }
            Self::Odd => write!(
                f,
                "canvas sides must be even (NV12 chroma / H.264 macroblock requirement)"
            ),
            Self::BadFps => write!(f, "fps must be in {MIN_FPS}..={MAX_FPS}"),
        }
    }
}

impl std::error::Error for CanvasError {}

/// Export resolution presets: landscape **and** portrait for every tier from
/// 480p to 4K.
///
/// Portrait entries are literal transposes of the landscape ones (not a
/// separate size ladder), so a phone-shot project keeps its native framing.
/// Callers should render the label and keep `id` as the persisted value.
pub const PRESETS: &[ResolutionPreset] = &[
    ResolutionPreset {
        id: "480p",
        label: "480p (SD)",
        width: 854,
        height: 480,
    },
    ResolutionPreset {
        id: "480p_portrait",
        label: "480p (SD, portrait)",
        width: 480,
        height: 854,
    },
    ResolutionPreset {
        id: "720p",
        label: "720p (HD)",
        width: 1280,
        height: 720,
    },
    ResolutionPreset {
        id: "720p_portrait",
        label: "720p (HD, portrait)",
        width: 720,
        height: 1280,
    },
    ResolutionPreset {
        id: "1080p",
        label: "1080p (FHD)",
        width: 1920,
        height: 1080,
    },
    ResolutionPreset {
        id: "1080p_portrait",
        label: "1080p (FHD, portrait)",
        width: 1080,
        height: 1920,
    },
    ResolutionPreset {
        id: "1440p",
        label: "1440p (QHD)",
        width: 2560,
        height: 1440,
    },
    ResolutionPreset {
        id: "1440p_portrait",
        label: "1440p (QHD, portrait)",
        width: 1440,
        height: 2560,
    },
    ResolutionPreset {
        id: "4k",
        label: "4K (UHD)",
        width: 3840,
        height: 2160,
    },
    ResolutionPreset {
        id: "4k_portrait",
        label: "4K (UHD, portrait)",
        width: 2160,
        height: 3840,
    },
];

/// Aspect-ratio shortcuts for the crop/letterbox UI, as `(label, w, h)`.
pub const ASPECTS: &[(&str, u32, u32)] = &[
    ("16:9", 16, 9),
    ("9:16", 9, 16),
    ("1:1", 1, 1),
    ("4:5", 4, 5),
    ("21:9", 21, 9),
];

/// Frame rates offered by the UI, ascending.
///
/// The cinema/broadcast pair (24/25), the online default (30) and the smooth
/// high-refresh pair (50/60). Not exhaustive: [`CanvasSpec::new`] accepts any
/// rate in `1..=240`.
pub const FPS_CHOICES: &[u32] = &[24, 25, 30, 50, 60];

impl CanvasSpec {
    /// Validate a canvas.
    ///
    /// Rejects zero sides ([`CanvasError::Zero`]), odd sides
    /// ([`CanvasError::Odd`]) and `fps` outside `1..=240`
    /// ([`CanvasError::BadFps`]), in that order. It deliberately does **not**
    /// check a maximum dimension: that is a device property, expressed by
    /// [`CanvasSpec::fits_limits`] / [`CanvasSpec::clamped_to_limits`]
    /// ([`CanvasError::TooLarge`]).
    ///
    /// ```
    /// # use rumo_core::canvas::CanvasSpec;
    /// assert!(CanvasSpec::new(1920, 1080, 30).is_ok());
    /// assert!(CanvasSpec::new(1921, 1080, 30).is_err());
    /// ```
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, CanvasError> {
        if width == 0 || height == 0 {
            return Err(CanvasError::Zero);
        }
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(CanvasError::Odd);
        }
        if !(MIN_FPS..=MAX_FPS).contains(&fps) {
            return Err(CanvasError::BadFps);
        }
        Ok(Self {
            width,
            height,
            fps,
        })
    }

    /// Scale both sides by `factor`, rounding each to the nearest multiple of
    /// `align`, and keep `fps`.
    ///
    /// * Rounding is to the *nearest* even multiple; the result is never `0`
    ///   (an undersized result is raised to the alignment itself, so
    ///   `factor <= 0.0` yields the minimum canvas for the alignment).
    /// * `align` is treated as a lower bound on the alignment and rounded up to
    ///   an even value (`2 -> 2`, `4 -> 4`, `3 -> 6`). Odd multiples can never
    ///   be produced, because an odd side would fail [`CanvasSpec::new`]: the
    ///   result therefore always stays valid, for any `align`.
    /// * `align == 0` is treated as `1` (then evened to 2). A non-finite factor
    ///   (`NaN`, `±inf`) is treated as `1.0`; a negative one as `0.0`.
    /// * A result larger than `u32::MAX` is clamped to the largest valid
    ///   multiple of the alignment instead of wrapping.
    ///
    /// Scaling is *not* an idempotent operation for sizes that are not already
    /// aligned (it quantises); it is exact for `factor == 1.0` on even sides
    /// with `align == 2`.
    pub fn scaled(self, factor: f32, align: u32) -> Self {
        let step = even_step(align);
        let factor = if factor.is_finite() {
            f64::from(factor).max(0.0)
        } else {
            1.0
        };
        Self {
            width: round_to_step(f64::from(self.width) * factor, step),
            height: round_to_step(f64::from(self.height) * factor, step),
            fps: self.fps,
        }
    }

    /// Target encoder bitrate in bits per second.
    ///
    /// `fps * BITRATE_COEFF * pixels^BITRATE_EXPONENT`, rounded to the nearest
    /// bit and clamped to `1..=120_000_000` (120 Mbit/s). The exponent below
    /// `1.0` makes the effective bits-per-pixel-per-frame fall as the frame
    /// grows (≈ 0.245 at 480p, ≈ 0.17 at 1080p, ≈ 0.156 at 4K), so the result is
    /// **strictly increasing** in the pixel count at a fixed `fps` and
    /// proportional to `fps` at a fixed size, until the ceiling binds (e.g.
    /// 8K60 saturates at 120 Mbit/s).
    pub fn bitrate_bps(self) -> u32 {
        let raw = f64::from(self.fps) * BITRATE_COEFF * (self.pixels() as f64).powf(BITRATE_EXPONENT);
        let clamped = raw.clamp(f64::from(MIN_BITRATE_BPS), f64::from(MAX_BITRATE_BPS));
        clamped.round() as u32
    }

    /// Total pixel count of one frame, as `u64` (4K is ~8.3 M, so `u32` would
    /// still fit, but the wide type keeps `width * height` total for any `u32`).
    pub fn pixels(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Aspect ratio as a reduced fraction, e.g. `1920x1080 -> "16:9"`.
    ///
    /// The exact reduced fraction is returned, with no snapping to the nearest
    /// [`ASPECTS`] entry: `1000x333` labels as `"1000:333"` and `1366x768` as
    /// `"683:384"`. Snapping would silently misdescribe an imported project and
    /// would make the label useless for debugging odd sources. A degenerate
    /// spec with a zero side (only constructible via the public fields) is
    /// rendered verbatim as `"w:h"` rather than dividing by zero.
    pub fn aspect_label(self) -> String {
        let g = gcd(self.width, self.height);
        if g == 0 {
            return format!("{}:{}", self.width, self.height);
        }
        format!("{}:{}", self.width / g, self.height / g)
    }

    /// Whether both sides fit within a square-like device limit of `max_dim`
    /// pixels on the longest supported side (`max_texture_dimension_2d`).
    ///
    /// A `max_dim` of `0` never fits anything, since valid specs are non-zero.
    pub fn fits_limits(self, max_dim: u32) -> bool {
        self.width <= max_dim && self.height <= max_dim
    }

    /// Validate `self` and check it against a device limit in one step.
    ///
    /// Returns the spec unchanged when it fits, else [`CanvasError::TooLarge`]
    /// (or the [`CanvasSpec::new`] error, when the spec is not valid at all).
    /// Despite the name it does **not** rescale: silently shrinking a 4K project
    /// to a 2K device would corrupt the user's intent, so callers must decide
    /// whether to downscale deliberately with [`CanvasSpec::scaled`]. This is
    /// the constructor that makes [`CanvasError::TooLarge`] reachable.
    pub fn clamped_to_limits(self, max_dim: u32) -> Result<Self, CanvasError> {
        let spec = Self::new(self.width, self.height, self.fps)?;
        if !spec.fits_limits(max_dim) {
            return Err(CanvasError::TooLarge { max: max_dim });
        }
        Ok(spec)
    }
}

/// Per-axis "cover" factors for fitting `src` into `target`.
///
/// `scale_x = target.0 / src.0` and `scale_y = target.1 / src.1`: multiply the
/// source *width* by `scale_x` (resp. *height* by `scale_y`) and that axis
/// exactly covers the target box.
///
/// * The aspect-preserving **"contain"** scale is `min(scale_x, scale_y)`;
///   the other axis keeps spare room, and *that* is where the bars go. Because
///   `scale_x / scale_y == target_aspect / src_aspect`, `scale_x > scale_y`
///   means the target box is wider than the source (pillarbox, bars on the
///   left/right) and `scale_x < scale_y` means it is taller (letterbox, bars
///   top/bottom).
/// * Upscaling is allowed and therefore factors above `1.0` are normal: they
///   only report that the box is bigger than the source, and `min(..) < 1.0` is
///   the "this touch will have to downscale" signal.
/// * A zero on either side returns `(0.0, 0.0)` instead of `inf`/`NaN`.
///
/// ```
/// # use rumo_core::canvas::aspect_fit;
/// let (sx, sy) = aspect_fit((640, 480), (1920, 1080)); // 4:3 into 16:9
/// assert!(sx > sy); // pillarbox: the height binds, bars sit left/right
/// assert_eq!(sx.min(sy), 2.25);
/// ```
pub fn aspect_fit(src: (u32, u32), target: (u32, u32)) -> (f32, f32) {
    if src.0 == 0 || src.1 == 0 || target.0 == 0 || target.1 == 0 {
        return (0.0, 0.0);
    }
    (
        target.0 as f32 / src.0 as f32,
        target.1 as f32 / src.1 as f32,
    )
}

/// Largest common divisor of two pixel counts; `0` when both are `0`, and the
/// non-zero side when exactly one is `0` (so [`CanvasSpec::aspect_label`] can
/// guard without a special case per side).
fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Effective alignment for [`CanvasSpec::scaled`]: at least 1, and always even
/// so the result can never violate the even-side invariant.
fn even_step(align: u32) -> u64 {
    let a = align.max(1);
    let even = if a.is_multiple_of(2) {
        a
    } else {
        // `checked_mul` keeps this panic-free for absurd alignments; the
        // fallback (`a & !1`) is still even and non-zero.
        a.checked_mul(2).unwrap_or(a & !1)
    };
    u64::from(even)
}

/// Round `value` to the nearest multiple of `step`, clamped into
/// `[step, largest multiple of step that fits in u32]`.
fn round_to_step(value: f64, step: u64) -> u32 {
    let step_f = step as f64;
    let max_value = ((u64::from(u32::MAX)) / step) * step;
    let rounded = if value.is_finite() {
        (value / step_f).round() * step_f
    } else {
        max_value as f64
    };
    rounded.clamp(step_f, max_value as f64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_valid_and_within_8k() {
        for p in PRESETS {
            let spec = CanvasSpec::new(p.width, p.height, 30)
                .unwrap_or_else(|e| panic!("preset {} rejected: {e}", p.id));
            assert!(spec.fits_limits(8192), "{} exceeds 8K", p.id);
            assert_eq!(spec.pixels(), u64::from(p.width) * u64::from(p.height));
        }
    }

    #[test]
    fn preset_ids_and_labels_are_unique_and_non_empty() {
        let mut ids = std::collections::HashSet::new();
        let mut labels = std::collections::HashSet::new();
        for p in PRESETS {
            assert!(!p.id.is_empty(), "empty id");
            assert!(!p.label.is_empty(), "{}: empty label", p.id);
            assert!(ids.insert(p.id), "duplicate preset id {}", p.id);
            assert!(labels.insert(p.label), "duplicate preset label {}", p.label);
        }
        assert_eq!(ids.len(), PRESETS.len());
        assert_eq!(labels.len(), PRESETS.len());
    }

    #[test]
    fn presets_are_even_sided_and_non_zero() {
        for p in PRESETS {
            assert!(p.width > 0 && p.height > 0, "{} is zero-sized", p.id);
            assert_eq!(p.width % 2, 0, "{} has odd width", p.id);
            assert_eq!(p.height % 2, 0, "{} has odd height", p.id);
        }
    }

    #[test]
    fn presets_cover_each_tier_in_both_orientations() {
        for tier in ["480p", "720p", "1080p", "1440p", "4k"] {
            let landscape = PRESETS
                .iter()
                .find(|p| p.id == tier)
                .unwrap_or_else(|| panic!("missing {tier}"));
            let portrait_id = format!("{tier}_portrait");
            let portrait = PRESETS
                .iter()
                .find(|p| p.id == portrait_id)
                .unwrap_or_else(|| panic!("missing {portrait_id}"));
            assert!(
                landscape.width > landscape.height,
                "{tier} is not landscape"
            );
            assert!(
                portrait.height > portrait.width,
                "{portrait_id} is not portrait"
            );
            assert_eq!(portrait.width, landscape.height, "{portrait_id} transpose");
            assert_eq!(portrait.height, landscape.width, "{portrait_id} transpose");
        }
    }

    #[test]
    fn new_rejects_zero_odd_and_bad_fps() {
        assert_eq!(CanvasSpec::new(0, 1080, 30), Err(CanvasError::Zero));
        assert_eq!(CanvasSpec::new(1920, 0, 30), Err(CanvasError::Zero));
        assert_eq!(CanvasSpec::new(0, 0, 0), Err(CanvasError::Zero));
        assert_eq!(CanvasSpec::new(1921, 1080, 30), Err(CanvasError::Odd));
        assert_eq!(CanvasSpec::new(1920, 1081, 30), Err(CanvasError::Odd));
        assert_eq!(CanvasSpec::new(1920, 1080, 0), Err(CanvasError::BadFps));
        assert_eq!(CanvasSpec::new(1920, 1080, 241), Err(CanvasError::BadFps));
        assert_eq!(CanvasSpec::new(1920, 1080, u32::MAX), Err(CanvasError::BadFps));
    }

    #[test]
    fn new_accepts_the_fps_range_and_size_limits_are_separate() {
        for fps in [1u32, 23, 60, 120, 240] {
            assert!(CanvasSpec::new(2, 2, fps).is_ok(), "fps {fps} rejected");
        }
        for fps in FPS_CHOICES {
            assert!(CanvasSpec::new(1920, 1080, *fps).is_ok());
        }
        // `new` must not enforce a max dimension ...
        let big = CanvasSpec::new(16_384, 16_384, 30).expect("no max dim in new");
        // ... that is `fits_limits` / `clamped_to_limits`.
        assert!(!big.fits_limits(8192));
        assert!(big.fits_limits(16_384));
        assert!(!big.fits_limits(0));
    }

    #[test]
    fn scaled_keeps_sides_even_and_never_zero() {
        for factor in [0.0f32, 0.01, 0.25, 0.5, 1.0, 2.0, 4.0] {
            for p in PRESETS {
                let s = CanvasSpec::new(p.width, p.height, 25).unwrap().scaled(factor, 2);
                assert_eq!(s.width % 2, 0, "{}@{factor}: odd width {}", p.id, s.width);
                assert_eq!(s.height % 2, 0, "{}@{factor}: odd height {}", p.id, s.height);
                assert!(s.width > 0 && s.height > 0, "{}@{factor} collapsed", p.id);
                assert_eq!(s.fps, 25, "fps must survive scaling");
                assert!(CanvasSpec::new(s.width, s.height, s.fps).is_ok());
            }
        }
    }

    #[test]
    fn scaled_is_idempotent_at_one() {
        for p in PRESETS {
            let spec = CanvasSpec::new(p.width, p.height, 30).unwrap();
            assert_eq!(spec.scaled(1.0, 2), spec, "{} changed at factor 1.0", p.id);
            // Align 4 is exact only for sides that are already multiples of 4;
            // 854 (480p) is not, so it is checked separately below.
            if p.width % 4 == 0 && p.height % 4 == 0 {
                assert_eq!(spec.scaled(1.0, 4), spec, "{} changed at align 4", p.id);
            }
        }
        // Any even side is a fixed point of `align 2`, aligned or not.
        let not_4_aligned = CanvasSpec::new(854, 480, 30).unwrap();
        assert_eq!(not_4_aligned.scaled(1.0, 2), not_4_aligned);
        // 854 -> nearest multiple of 4 is 856.
        assert_eq!(not_4_aligned.scaled(1.0, 4).width, 856);
    }

    #[test]
    fn scaled_rounds_to_the_nearest_aligned_multiple() {
        let spec = CanvasSpec::new(1920, 1080, 30).unwrap();
        assert_eq!(spec.scaled(0.5, 2).width, 960);
        assert_eq!(spec.scaled(0.5, 2).height, 540);
        assert_eq!(spec.scaled(0.5, 4).width, 960);
        // 540 -> nearest multiple of 4 is 540 (135 * 4).
        assert_eq!(spec.scaled(0.5, 4).height, 540);
        // 1080 * 0.3 = 324 -> nearest multiple of 4 is 324.
        assert_eq!(spec.scaled(0.3, 4).height, 324);
        // 1080 * 0.333 = 359.64 -> 360 for align 2.
        assert_eq!(spec.scaled(0.333, 2).height, 360);
        // Odd alignment is evened up: 3 -> 6.
        assert_eq!(spec.scaled(0.1, 3).width % 6, 0);
    }

    #[test]
    fn scaled_guards_bad_factors_and_alignments() {
        let spec = CanvasSpec::new(1920, 1080, 30).unwrap();
        // Zero / negative / NaN factors collapse to the alignment floor.
        assert_eq!(spec.scaled(0.0, 2).width, 2);
        assert_eq!(spec.scaled(-4.0, 2).height, 2);
        assert_eq!(spec.scaled(f32::NAN, 2), spec);
        assert_eq!(spec.scaled(f32::INFINITY, 2), spec);
        // align 0 behaves as align 1, evened up to 2.
        assert_eq!(spec.scaled(0.5, 0).width, 960);
        // Absurd growth clamps to the largest aligned u32 instead of wrapping.
        let huge = spec.scaled(f32::MAX, 2);
        assert_eq!(huge.width % 2, 0);
        assert!(huge.width >= 1920);
        assert!(CanvasSpec::new(huge.width, huge.height, huge.fps).is_ok());
    }

    #[test]
    fn bitrate_is_within_clamps() {
        for p in PRESETS {
            for fps in [1u32, 24, 30, 60, 239] {
                let bps = CanvasSpec::new(p.width, p.height, fps).unwrap().bitrate_bps();
                assert!(
                    (1..=120_000_000).contains(&bps),
                    "{}@{fps} -> {bps} out of clamp",
                    p.id
                );
            }
        }
        assert!((1..=120_000_000).contains(&CanvasSpec::new(2, 2, 1).unwrap().bitrate_bps()));
    }

    #[test]
    fn bitrate_is_monotone_in_pixels_at_fixed_fps() {
        let mut specs: Vec<CanvasSpec> = Vec::new();
        for w in (2..=200u32).step_by(2) {
            for h in (2..=200u32).step_by(2) {
                specs.push(CanvasSpec::new(w, h, 30).unwrap());
            }
        }
        specs.sort_by_key(|s| s.pixels());
        for pair in specs.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(
                a.bitrate_bps() <= b.bitrate_bps(),
                "not monotone: {}x{} ({}) -> {}x{} ({})",
                a.width,
                a.height,
                a.bitrate_bps(),
                b.width,
                b.height,
                b.bitrate_bps()
            );
        }
    }

    #[test]
    fn bitrate_grows_with_fps_at_fixed_size() {
        let size = CanvasSpec::new(1280, 720, 24).unwrap();
        let mut prev = 0;
        for fps in FPS_CHOICES {
            let bps = CanvasSpec {
                fps: *fps,
                ..size
            }
            .bitrate_bps();
            assert!(bps > prev, "fps {fps} did not raise the bitrate");
            assert!(bps < 120_000_000, "720p should not hit the ceiling");
            prev = bps;
        }
    }

    #[test]
    fn bitrate_clamps_at_the_ceiling_for_large_fast_frames() {
        let uhd240 = CanvasSpec::new(3840, 2160, 240).unwrap();
        assert_eq!(uhd240.bitrate_bps(), 120_000_000);
        let uhd60 = CanvasSpec::new(3840, 2160, 60).unwrap();
        assert!(uhd60.bitrate_bps() < 120_000_000);
    }

    #[test]
    fn pixels_is_the_product() {
        assert_eq!(CanvasSpec::new(1920, 1080, 30).unwrap().pixels(), 2_073_600);
        assert_eq!(CanvasSpec::new(3840, 2160, 30).unwrap().pixels(), 8_294_400);
        // Fields are public: the u64 widening keeps an extreme product exact.
        let extreme = CanvasSpec {
            width: u32::MAX,
            height: u32::MAX,
            fps: 30,
        };
        assert_eq!(extreme.pixels(), u64::from(u32::MAX) * u64::from(u32::MAX));
    }

    #[test]
    fn aspect_label_reduces_the_fraction() {
        let label = |w, h| {
            CanvasSpec::new(w, h, 30)
                .unwrap_or_else(|e| panic!("{w}x{h}: {e}"))
                .aspect_label()
        };
        assert_eq!(label(1920, 1080), "16:9");
        assert_eq!(label(1080, 1920), "9:16");
        assert_eq!(label(1000, 1000), "1:1");
        assert_eq!(label(1280, 720), "16:9");
        assert_eq!(label(854, 480), "427:240");
        assert_eq!(label(2, 2), "1:1");
        // A real preset stays exact rather than collapsing to a round ratio.
        assert_eq!(label(2560, 1440), "16:9");
        // Non-integer ratios keep the exact reduced fraction, never a preset.
        // 1000x333 is odd-sided, so it can only exist via the public fields.
        let odd_ratio = CanvasSpec {
            width: 1000,
            height: 333,
            fps: 30,
        };
        assert_eq!(odd_ratio.aspect_label(), "1000:333");
        // Degenerate public-field spec must not divide by zero.
        let zero = CanvasSpec {
            width: 0,
            height: 1080,
            fps: 30,
        };
        assert_eq!(zero.aspect_label(), "0:1");
    }

    #[test]
    fn aspect_fit_letterboxes_a_4_3_source_into_a_16_9_box() {
        let (sx, sy) = aspect_fit((640, 480), (1920, 1080));
        assert!((sx - 3.0).abs() < 1e-6, "sx = {sx}");
        assert!((sy - 2.25).abs() < 1e-6, "sy = {sy}");
        // target aspect (1.778) > source aspect (1.333): the width over-fills,
        // so the height binds and the bars sit left/right (pillarbox).
        assert!(sx > sy, "expected pillarbox relationship, got {sx} vs {sy}");
        let contain = sx.min(sy);
        assert_eq!(contain, 2.25);
        assert!(640.0 * contain <= 1920.0 + 1e-3);
        assert!((480.0 * contain - 1080.0).abs() < 1e-3);
    }

    #[test]
    fn aspect_fit_flips_for_a_tall_target_and_is_identity_on_match() {
        let (sx, sy) = aspect_fit((1920, 1080), (1080, 1920));
        assert!(sy > sx, "portrait target should bind on width");
        assert_eq!(aspect_fit((1920, 1080), (1920, 1080)), (1.0, 1.0));
        // Zero dimensions must not produce inf/NaN.
        assert_eq!(aspect_fit((0, 1080), (1920, 1080)), (0.0, 0.0));
        assert_eq!(aspect_fit((1920, 1080), (0, 0)), (0.0, 0.0));
    }

    #[test]
    fn aspects_and_fps_choices_are_sane() {
        assert!(!ASPECTS.is_empty());
        for (label, w, h) in ASPECTS {
            assert!(!label.is_empty());
            assert!(*w > 0 && *h > 0, "{label} has a zero side");
        }
        assert!(!FPS_CHOICES.is_empty());
        for pair in FPS_CHOICES.windows(2) {
            assert!(pair[0] < pair[1], "FPS_CHOICES is not ascending: {pair:?}");
        }
        // Every offered rate must be constructible.
        for fps in FPS_CHOICES {
            assert!(CanvasSpec::new(1920, 1080, *fps).is_ok());
        }
    }

    #[test]
    fn clamped_to_limits_reports_too_large_and_validates() {
        let ok = CanvasSpec::new(1920, 1080, 30).unwrap();
        assert_eq!(ok.clamped_to_limits(8192), Ok(ok));
        // The limit is per side, so 1920x1080 fits a 2048 device frame.
        assert_eq!(ok.clamped_to_limits(2048), Ok(ok));
        assert_eq!(ok.clamped_to_limits(1920), Ok(ok));
        assert_eq!(
            ok.clamped_to_limits(1080),
            Err(CanvasError::TooLarge { max: 1080 })
        );
        // The 8.5 wgpu pitfall: `downlevel_defaults()` caps 2D textures at 2048.
        let uhd = CanvasSpec::new(3840, 2160, 30).unwrap();
        assert_eq!(
            uhd.clamped_to_limits(2048),
            Err(CanvasError::TooLarge { max: 2048 })
        );
        assert!(uhd.clamped_to_limits(8192).is_ok());
        // Invalid public-field specs surface their own error, not TooLarge.
        let odd = CanvasSpec {
            width: 1921,
            height: 1080,
            fps: 30,
        };
        assert_eq!(odd.clamped_to_limits(8192), Err(CanvasError::Odd));
    }

    #[test]
    fn canvas_error_display_and_source() {
        let cases = [
            CanvasError::Zero,
            CanvasError::TooLarge { max: 2048 },
            CanvasError::Odd,
            CanvasError::BadFps,
        ];
        for e in cases {
            let text = e.to_string();
            assert!(!text.is_empty(), "{e:?} has an empty message");
            // `Error::source` is None: no variant wraps another error.
            assert!(std::error::Error::source(&e).is_none());
        }
        assert!(CanvasError::TooLarge { max: 2048 }
            .to_string()
            .contains("2048"));
        assert!(CanvasError::BadFps.to_string().contains("240"));
    }
}
