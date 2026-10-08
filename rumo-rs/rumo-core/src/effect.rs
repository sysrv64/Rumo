// SPDX-License-Identifier: Apache-2.0

//! Layer effects: a declarative, animatable effect model shared by the GPU
//! compositor (`rumo-render`), the project file (`postcard`) and the Kotlin UI
//! (`serde_json` catalogue, see [`catalogue_json`]).
//!
//! Design notes (rationale in `docs/08-rust-layer-deepening.md`, 8.2):
//!
//! * **Every parameter value is a plain `f32`.** That gives one keyframe track
//!   implementation for any parameter (`docs/04`, 4.4) and removes every
//!   uniform-alignment pitfall: a `vec3<f32>` in a uniform block occupies 16
//!   bytes, a colour parameter here occupies exactly four `f32` slots.
//! * **The GPU `Params` block is `EffectSpec::slots()` floats in spec order**,
//!   zero-padded to a multiple of four. A test in `rumo-render` asserts that the
//!   `struct Params` field names of each WGSL module match
//!   [`EffectSpec::field_names`] in order, so the two can never drift.
//! * Effects run in display space on 8-bit targets; see 8.1 for why.

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Advisory cost classification.
///
/// Every built-in effect has both a GPU pass and a CPU reference, so the
/// compositor runs all of them; this field records which ones are expensive
/// enough that a host may prefer to apply them only on export (the interactive
/// preview is the latency-sensitive path). It is metadata, not a switch: see
/// `FxRuntime::chain_is_effective`, which deliberately does not consult it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectCost {
    /// Cheap enough for interactive use on the GPU path.
    Gpu,
    /// Expensive; a host may defer it to export only.
    Cpu,
}

/// Colour space an effect's arithmetic assumes.
///
/// The engine composites in 8-bit display space (`docs/08`, 8.1), so every
/// built-in effect declares [`EffectSpace::Display`]. The field exists so a
/// future `Rgba16Float` pipeline can opt individual effects into linear light
/// without touching the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectSpace {
    /// Gamma-encoded 0..1 values, as stored in the RGBA8 working targets.
    Display,
    /// Scene-linear light. No built-in effect uses this yet.
    Linear,
}

/// How a parameter is validated and presented in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    /// Continuous scalar in `[min, max]`.
    Float,
    /// Scalar in degrees; wrapped into `[-180, 180)`.
    Angle,
    /// Integer-valued scalar in `[min, max]`.
    Int,
    /// `0` or `1`.
    Bool,
    /// Index into [`ParamSpec::choices`].
    Choice,
    /// RGBA colour in display space; occupies four slots.
    Color,
    /// `vec2<f32>`; occupies two slots.
    Vec2,
    /// `vec3<f32>`; occupies three slots.
    Vec3,
    /// `vec4<f32>`; occupies four slots.
    Vec4,
    /// `mat3x3<f32>`; occupies nine slots, as three padded columns of three.
    Mat3,
    /// `mat4x4<f32>`; occupies sixteen slots.
    Mat4,
}

/// How one `Params` member is written into the uniform block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberShape {
    /// A single `f32`.
    Scalar,
    /// `vec2<f32>` / `vec3<f32>` / `vec4<f32>`; the value occupies this many
    /// contiguous `f32` at the member's offset.
    Vector(u8),
    /// `mat3x3<f32>`: three columns, each a `vec3` padded to 16 bytes in the
    /// uniform address space, so a column's `f32` start every fourth value.
    Matrix3,
    /// `mat4x4<f32>`: four columns of four, contiguous.
    Matrix4,
}

/// One member of the `Params` struct that a parameter contributes.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamMember {
    /// Member name in the struct.
    pub name: String,
    /// `f32` values this member carries.
    pub components: u8,
    pub shape: MemberShape,
}

/// The member shape a parameter of `kind` contributes and the number of `f32`
/// values it carries. [`ParamKind::Color`] is the exception: it expands to four
/// separately named scalars, so it has no single shape and returns `None`.
fn member_shape(kind: ParamKind) -> Option<(MemberShape, u8)> {
    Some(match kind {
        ParamKind::Float
        | ParamKind::Angle
        | ParamKind::Int
        | ParamKind::Bool
        | ParamKind::Choice => (MemberShape::Scalar, 1),
        ParamKind::Color => return None,
        ParamKind::Vec2 => (MemberShape::Vector(2), 2),
        ParamKind::Vec3 => (MemberShape::Vector(3), 3),
        ParamKind::Vec4 => (MemberShape::Vector(4), 4),
        ParamKind::Mat3 => (MemberShape::Matrix3, 9),
        ParamKind::Mat4 => (MemberShape::Matrix4, 16),
    })
}

/// The `Params` members a parameter named `key` contributes.
///
/// A colour is the one kind that does not map one-to-one: its four channels
/// become the members `key_r`, `key_g`, `key_b`, `key_a`, which is why the
/// uniform alone cannot tell a colour from four scalars.
fn members_of(key: &str, kind: ParamKind) -> Vec<ParamMember> {
    match member_shape(kind) {
        Some((shape, components)) => vec![ParamMember {
            name: key.to_string(),
            components,
            shape,
        }],
        None => ["_r", "_g", "_b", "_a"]
            .iter()
            .map(|suffix| ParamMember {
                name: format!("{key}{suffix}"),
                components: 1,
                shape: MemberShape::Scalar,
            })
            .collect(),
    }
}

/// Declarative description of one effect parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamSpec {
    /// Base name. For scalars this is also the WGSL `Params` field name; a
    /// colour named `key` expands to the fields `key_r`, `key_g`, `key_b`,
    /// `key_a`.
    pub key: &'static str,
    /// Human-readable label for the UI.
    pub label: &'static str,
    /// Validation and presentation kind.
    pub kind: ParamKind,
    /// Lowest accepted value (for [`ParamKind::Color`]: the per-channel floor).
    pub min: f32,
    /// Highest accepted value.
    pub max: f32,
    /// Default value. For [`ParamKind::Color`] all four channels are used,
    /// otherwise only `[0]`.
    pub default: [f32; 4],
    /// Labels for [`ParamKind::Choice`]; empty otherwise.
    pub choices: &'static [&'static str],
    /// Unit suffix for the UI (`""`, `"px"`, `"°"`, `"%s"`).
    pub unit: &'static str,
}

impl ParamSpec {
    /// Number of `f32` slots this parameter occupies in the uniform block.
    pub const fn slots(&self) -> usize {
        match self.kind {
            ParamKind::Color => 4,
            ParamKind::Vec2 => 2,
            ParamKind::Vec3 => 3,
            ParamKind::Vec4 => 4,
            ParamKind::Mat3 => 9,
            ParamKind::Mat4 => 16,
            _ => 1,
        }
    }

    /// The `Params` members this parameter contributes, in declaration order.
    pub fn members(&self) -> Vec<ParamMember> {
        members_of(self.key, self.kind)
    }

    /// Names of the WGSL `Params` fields this parameter contributes, in order.
    pub fn field_names(&self) -> Vec<String> {
        self.members().into_iter().map(|m| m.name).collect()
    }

    /// Default values in slot order.
    pub fn default_slots(&self) -> [f32; 4] {
        if matches!(self.kind, ParamKind::Color) {
            self.default
        } else {
            [self.default[0], 0.0, 0.0, 0.0]
        }
    }

    /// Clamp/normalise the `slots()` values of this parameter in place.
    pub fn clamp(&self, values: &mut [f32]) {
        clamp_slots(
            self.kind,
            self.min,
            self.max,
            self.default.first().copied().unwrap_or(0.0),
            self.choices.len(),
            self.slots(),
            values,
        );
    }
}

/// Wrap degrees into `[-180, 180)`.
pub fn wrap_angle(deg: f32) -> f32 {
    if !deg.is_finite() {
        return 0.0;
    }
    let mut d = (deg + 180.0) % 360.0;
    if d < 0.0 {
        d += 360.0;
    }
    d - 180.0
}

/// Clamp/normalise one parameter's `slots` values in place.
///
/// The same policy serves a static [`ParamSpec`] and an owned [`ParamRow`]:
/// scalars, colours and the vector/matrix kinds share the `min..max` clamp
/// component by component, `Angle` wraps, `Int`/`Choice` snap, `Bool` becomes
/// `0` or `1`, and a non-finite value falls back to the parameter's first
/// default. A colour's alpha additionally obeys the `0..1` unit interval.
fn clamp_slots(
    kind: ParamKind,
    min: f32,
    max: f32,
    default0: f32,
    choices_len: usize,
    slots: usize,
    values: &mut [f32],
) {
    let n = slots.min(values.len());
    for v in values.iter_mut().take(n) {
        if !v.is_finite() {
            *v = default0;
            continue;
        }
        *v = match kind {
            ParamKind::Float
            | ParamKind::Color
            | ParamKind::Vec2
            | ParamKind::Vec3
            | ParamKind::Vec4
            | ParamKind::Mat3
            | ParamKind::Mat4 => v.clamp(min, max),
            ParamKind::Angle => wrap_angle(*v),
            ParamKind::Int => v.round().clamp(min, max),
            ParamKind::Bool => {
                if *v >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            ParamKind::Choice => {
                let last = choices_len.saturating_sub(1) as f32;
                v.round().clamp(0.0, last)
            }
        };
    }
    // A colour's alpha obeys the same 0..1 unit interval as its channels.
    if matches!(kind, ParamKind::Color) && values.len() >= 4 {
        values[3] = values[3].clamp(0.0, 1.0);
    }
}

const fn float(
    key: &'static str,
    label: &'static str,
    min: f32,
    max: f32,
    default: f32,
) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Float,
        min,
        max,
        default: [default, 0.0, 0.0, 0.0],
        choices: &[],
        unit: "",
    }
}

const fn float_unit(
    key: &'static str,
    label: &'static str,
    min: f32,
    max: f32,
    default: f32,
    unit: &'static str,
) -> ParamSpec {
    let mut p = float(key, label, min, max, default);
    p.unit = unit;
    p
}

const fn angle(key: &'static str, label: &'static str, default: f32) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Angle,
        min: -180.0,
        max: 180.0,
        default: [default, 0.0, 0.0, 0.0],
        choices: &[],
        unit: "°",
    }
}

const fn int(
    key: &'static str,
    label: &'static str,
    min: f32,
    max: f32,
    default: f32,
) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Int,
        min,
        max,
        default: [default, 0.0, 0.0, 0.0],
        choices: &[],
        unit: "",
    }
}

const fn choice(
    key: &'static str,
    label: &'static str,
    choices: &'static [&'static str],
    default: f32,
) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Choice,
        min: 0.0,
        max: (choices.len() as f32) - 1.0,
        default: [default, 0.0, 0.0, 0.0],
        choices,
        unit: "",
    }
}

const fn color(key: &'static str, label: &'static str, rgba: [f32; 4]) -> ParamSpec {
    ParamSpec {
        key,
        label,
        kind: ParamKind::Color,
        min: 0.0,
        max: 1.0,
        default: rgba,
        choices: &[],
        unit: "",
    }
}

/// How a pass picks the resolution of its render target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShrinkRule {
    /// Always divide by `2^fixed`.
    Fixed(u8),
    /// Divide by `2^value`, where `value` is the named [`ParamKind::Int`]
    /// parameter clamped to `0..=6`.
    FromParam(&'static str),
}

impl ShrinkRule {
    /// Resolve the divisor exponent for one effect instance.
    pub fn resolve(self, inst: &EffectInstance) -> u8 {
        match self {
            ShrinkRule::Fixed(n) => n.min(MAX_SHRINK),
            ShrinkRule::FromParam(key) => inst
                .get(key)
                .and_then(|v| v.first())
                .map(|v| (*v).round().clamp(0.0, MAX_SHRINK as f32) as u8)
                .unwrap_or(0),
        }
    }
}

/// Largest supported power-of-two downscale in a pass (`/64`).
pub const MAX_SHRINK: u8 = 6;

/// One render pass of an effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassSpec {
    /// `@fragment` entry point name in the effect's WGSL module.
    pub entry: &'static str,
    /// Target resolution rule for this pass.
    pub shrink: ShrinkRule,
}

const fn pass(entry: &'static str, shrink: ShrinkRule) -> PassSpec {
    PassSpec { entry, shrink }
}

const FULL: ShrinkRule = ShrinkRule::Fixed(0);

/// The effect catalogue: which effects exist and how they are parameterised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectKind {
    Blur,
    ColorTune,
    Threshold,
    ChromaKey,
    CopyBackground,
    Pixelate,
    Sphere360,
    DropShadow,
    InnerShadow,
    Glow,
}

const BLUR_MODES: &[&str] = &["Gaussian", "Box", "Directional", "Mask"];
const THRESHOLD_MODES: &[&str] = &["Luma", "Alpha", "Chroma"];
const BACKGROUND_MODES: &[&str] = &["Black", "White", "Color"];
const PIXELATE_SHAPES: &[&str] = &["Square", "Hex"];

const BLUR_PARAMS: &[ParamSpec] = &[
    choice("mode", "Mode", BLUR_MODES, 0.0),
    float_unit("radius", "Radius", 0.0, 200.0, 12.0, "px"),
    angle("angle", "Angle", 0.0),
    int("downscale", "Downscale", 0.0, 6.0, 0.0),
];

const COLOR_TUNE_PARAMS: &[ParamSpec] = &[
    angle("hue", "Hue", 0.0),
    float("chroma", "Chroma", -1.0, 1.0, 0.0),
    float("lightness", "Lightness", -1.0, 1.0, 0.0),
    float("brightness", "Brightness", -1.0, 1.0, 0.0),
    float("contrast", "Contrast", -1.0, 1.0, 0.0),
    float("saturation", "Saturation", -1.0, 1.0, 0.0),
];

const THRESHOLD_PARAMS: &[ParamSpec] = &[
    float("level", "Level", 0.0, 1.0, 0.5),
    float("softness", "Softness", 0.0, 1.0, 0.05),
    choice("mode", "Mode", THRESHOLD_MODES, 0.0),
];

const CHROMA_KEY_PARAMS: &[ParamSpec] = &[
    color("key", "Key Colour", [0.0, 1.0, 0.0, 1.0]),
    float("similarity", "Similarity", 0.0, 1.0, 0.2),
    float("softness", "Softness", 0.0, 1.0, 0.1),
    float("spill", "Spill Suppression", 0.0, 1.0, 0.5),
];

const COPY_BACKGROUND_PARAMS: &[ParamSpec] = &[
    choice("mode", "Mode", BACKGROUND_MODES, 0.0),
    color("color", "Colour", [1.0, 1.0, 1.0, 1.0]),
    float("tolerance", "Tolerance", 0.0, 1.0, 0.15),
    float("feather", "Feather", 0.0, 1.0, 0.05),
];

const PIXELATE_PARAMS: &[ParamSpec] = &[
    float_unit("size", "Block Size", 1.0, 128.0, 8.0, "px"),
    choice("shape", "Shape", PIXELATE_SHAPES, 0.0),
];

const SPHERE_PARAMS: &[ParamSpec] = &[
    float("radius", "Radius", 0.0, 1.0, 0.5),
    angle("yaw", "Yaw", 0.0),
    angle("pitch", "Pitch", 0.0),
    float_unit("fov", "Field of View", 10.0, 170.0, 75.0, "°"),
];

const SHADOW_PARAMS: &[ParamSpec] = &[
    float_unit("offset_x", "Offset X", -512.0, 512.0, 12.0, "px"),
    float_unit("offset_y", "Offset Y", -512.0, 512.0, 12.0, "px"),
    float_unit("blur", "Blur", 0.0, 200.0, 16.0, "px"),
    float_unit("spread", "Spread", 0.0, 64.0, 0.0, "px"),
    color("color", "Colour", [0.0, 0.0, 0.0, 1.0]),
    float("opacity", "Opacity", 0.0, 1.0, 0.6),
];

const GLOW_PARAMS: &[ParamSpec] = &[
    float("threshold", "Threshold", 0.0, 1.0, 0.6),
    float_unit("radius", "Radius", 0.0, 200.0, 24.0, "px"),
    float("intensity", "Intensity", 0.0, 4.0, 1.0),
    color("color", "Tint", [1.0, 1.0, 1.0, 1.0]),
    float("tint", "Tint Amount", 0.0, 1.0, 0.0),
];

const BLUR_PASSES: &[PassSpec] = &[
    pass("fs_blur_h", ShrinkRule::FromParam("downscale")),
    pass("fs_blur_v", ShrinkRule::FromParam("downscale")),
];

const SINGLE_PASS: &[PassSpec] = &[pass("fs_main", FULL)];

const SHADOW_PASSES: &[PassSpec] = &[
    pass("fs_blur_h", FULL),
    pass("fs_blur_v", FULL),
    pass("fs_composite", FULL),
];

const GLOW_PASSES: &[PassSpec] = &[
    pass("fs_threshold", FULL),
    pass("fs_blur_h", FULL),
    pass("fs_blur_v", FULL),
    pass("fs_composite", FULL),
];

const BLUR_SPEC: EffectSpec = EffectSpec {
    kind: EffectKind::Blur,
    label: "Blur",
    cost: EffectCost::Gpu,
    space: EffectSpace::Display,
    params: BLUR_PARAMS,
    passes: BLUR_PASSES,
};

const COLOR_TUNE_SPEC: EffectSpec = EffectSpec {
    kind: EffectKind::ColorTune,
    label: "Colour Tune",
    cost: EffectCost::Gpu,
    space: EffectSpace::Display,
    params: COLOR_TUNE_PARAMS,
    passes: SINGLE_PASS,
};

const THRESHOLD_SPEC: EffectSpec = EffectSpec {
    kind: EffectKind::Threshold,
    label: "Threshold",
    cost: EffectCost::Gpu,
    space: EffectSpace::Display,
    params: THRESHOLD_PARAMS,
    passes: SINGLE_PASS,
};

const CHROMA_KEY_SPEC: EffectSpec = EffectSpec {
    kind: EffectKind::ChromaKey,
    label: "Chroma Key",
    cost: EffectCost::Gpu,
    space: EffectSpace::Display,
    params: CHROMA_KEY_PARAMS,
    passes: SINGLE_PASS,
};

const COPY_BACKGROUND_SPEC: EffectSpec = EffectSpec {
    kind: EffectKind::CopyBackground,
    label: "Copy Background",
    cost: EffectCost::Cpu,
    space: EffectSpace::Display,
    params: COPY_BACKGROUND_PARAMS,
    passes: SINGLE_PASS,
};

const PIXELATE_SPEC: EffectSpec = EffectSpec {
    kind: EffectKind::Pixelate,
    label: "Pixelate",
    cost: EffectCost::Gpu,
    space: EffectSpace::Display,
    params: PIXELATE_PARAMS,
    passes: SINGLE_PASS,
};

const SPHERE_SPEC: EffectSpec = EffectSpec {
    kind: EffectKind::Sphere360,
    label: "Reorient Sphere",
    cost: EffectCost::Gpu,
    space: EffectSpace::Display,
    params: SPHERE_PARAMS,
    passes: SINGLE_PASS,
};

const DROP_SHADOW_SPEC: EffectSpec = EffectSpec {
    kind: EffectKind::DropShadow,
    label: "Drop Shadow",
    cost: EffectCost::Gpu,
    space: EffectSpace::Display,
    params: SHADOW_PARAMS,
    passes: SHADOW_PASSES,
};

const INNER_SHADOW_SPEC: EffectSpec = EffectSpec {
    kind: EffectKind::InnerShadow,
    label: "Inner Shadow",
    cost: EffectCost::Gpu,
    space: EffectSpace::Display,
    params: SHADOW_PARAMS,
    passes: SHADOW_PASSES,
};

const GLOW_SPEC: EffectSpec = EffectSpec {
    kind: EffectKind::Glow,
    label: "Glow",
    cost: EffectCost::Gpu,
    space: EffectSpace::Display,
    params: GLOW_PARAMS,
    passes: GLOW_PASSES,
};

/// Full description of one effect kind.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectSpec {
    /// Which effect this describes.
    pub kind: EffectKind,
    /// UI label.
    pub label: &'static str,
    /// Where it may run.
    pub cost: EffectCost,
    /// Assumed colour space.
    pub space: EffectSpace,
    /// Parameters, in WGSL `Params` field order.
    pub params: &'static [ParamSpec],
    /// Render passes, in execution order. The last pass produces the layer.
    pub passes: &'static [PassSpec],
}

impl EffectSpec {
    /// Total `f32` slots across all parameters.
    pub fn slots(&self) -> usize {
        self.params.iter().map(|p| p.slots()).sum()
    }

    /// Uniform block length in `f32`, declared as the slots padded to a
    /// multiple of four (16 bytes). A declared estimate: the renderer packs
    /// against the span `naga` reports for the module, not against this.
    pub fn block_len(&self) -> usize {
        self.slots().div_ceil(4) * 4
    }

    /// WGSL `Params` field names, in declaration order.
    pub fn field_names(&self) -> Vec<String> {
        self.params.iter().flat_map(|p| p.field_names()).collect()
    }

    /// Slot offset of a parameter, or `None` if the key is unknown.
    pub fn slot_index(&self, key: &str) -> Option<usize> {
        let mut offset = 0;
        for p in self.params {
            if p.key == key {
                return Some(offset);
            }
            offset += p.slots();
        }
        None
    }

    /// The parameter whose `key` matches, if any.
    pub fn param(&self, key: &str) -> Option<&'static ParamSpec> {
        self.params.iter().find(|p| p.key == key)
    }

    /// Resolve every pass's downscale exponent for one instance.
    pub fn pass_shrinks(&self, inst: &EffectInstance) -> Vec<u8> {
        self.passes.iter().map(|p| p.shrink.resolve(inst)).collect()
    }
}

/// One parameter of an [`EffectLayout`], owned so layouts outlive static specs.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamRow {
    /// Base name. For scalars this is also the WGSL `Params` field name; a
    /// colour named `key` expands to the fields `key_r`, `key_g`, `key_b`,
    /// `key_a`.
    pub key: String,
    /// Human-readable label for the UI.
    pub label: String,
    /// Validation and presentation kind.
    pub kind: ParamKind,
    /// Lowest accepted value (for [`ParamKind::Color`]: the per-channel floor).
    pub min: f32,
    /// Highest accepted value.
    pub max: f32,
    /// Values in slot order. Built-in rows carry the spec's four-element array;
    /// a row owns as many values as its kind needs, so a matrix has sixteen.
    pub default: Vec<f32>,
    /// Unit suffix for the UI (`""`, `"px"`, `"°"`, `"%s"`).
    pub unit: String,
    /// Labels for [`ParamKind::Choice`]; empty otherwise.
    pub choices: Vec<String>,
    /// Offset of this row inside the flat `params` vector.
    pub offset: usize,
}

impl ParamRow {
    /// Own a static [`ParamSpec`], recording its offset in the flat vector.
    pub fn from_spec(p: &ParamSpec, offset: usize) -> ParamRow {
        ParamRow {
            key: p.key.to_string(),
            label: p.label.to_string(),
            kind: p.kind,
            min: p.min,
            max: p.max,
            default: p.default.to_vec(),
            unit: p.unit.to_string(),
            choices: p.choices.iter().map(|c| (*c).to_string()).collect(),
            offset,
        }
    }

    /// Number of `f32` slots this parameter occupies in the uniform block.
    pub const fn slots(&self) -> usize {
        match self.kind {
            ParamKind::Color => 4,
            ParamKind::Vec2 => 2,
            ParamKind::Vec3 => 3,
            ParamKind::Vec4 => 4,
            ParamKind::Mat3 => 9,
            ParamKind::Mat4 => 16,
            _ => 1,
        }
    }

    /// The `Params` members this parameter contributes, in declaration order.
    pub fn members(&self) -> Vec<ParamMember> {
        members_of(&self.key, self.kind)
    }

    /// Names of the WGSL `Params` fields this parameter contributes, in order.
    pub fn field_names(&self) -> Vec<String> {
        self.members().into_iter().map(|m| m.name).collect()
    }

    /// Default values in slot order, zero-filled to [`ParamRow::slots`] when the
    /// stored vector is shorter.
    pub fn default_slots(&self) -> Vec<f32> {
        let n = self.slots();
        let mut values = vec![0.0; n];
        let take = self.default.len().min(n);
        values[..take].copy_from_slice(&self.default[..take]);
        values
    }

    /// Clamp/normalise the `slots()` values of this parameter in place.
    pub fn clamp(&self, values: &mut [f32]) {
        clamp_slots(
            self.kind,
            self.min,
            self.max,
            self.default.first().copied().unwrap_or(0.0),
            self.choices.len(),
            self.slots(),
            values,
        );
    }
}

/// Owned description of one effect, so an instance resolves with no registry.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectLayout {
    /// Stable id used in JSON, JNI and Kotlin.
    pub id: String,
    /// UI label.
    pub label: String,
    /// Built-in kind, when this layout describes one.
    pub builtin: Option<EffectKind>,
    pub cost: EffectCost,
    pub space: EffectSpace,
    /// Parameter rows, in slot order.
    pub params: Vec<ParamRow>,
    /// Fragment entry points, in execution order.
    pub entries: Vec<String>,
    /// Downscale rule per pass, same order as `entries`.
    pub shrinks: Vec<ShrinkRule>,
    /// Total `f32` slots.
    pub slots: usize,
    /// Uniform block length in `f32`, declared as the slots padded to a
    /// multiple of four (16 bytes). This is an estimate for the UI and for
    /// sizing: the authoritative span is what `naga` reports when the module is
    /// validated, and the renderer packs against that, not against this.
    pub block_len: usize,
    /// WGSL `Params` field names, in declaration order.
    pub fields: Vec<String>,
    /// WGSL module source. `None` for built-ins: their modules live in the
    /// renderer.
    pub source: Option<String>,
}

impl EffectLayout {
    /// Own the description of a built-in kind, from its static spec.
    ///
    /// `source` stays `None`: a built-in module is produced by the renderer's
    /// `wgsl_source`, not carried here.
    pub fn of_kind(kind: EffectKind) -> EffectLayout {
        let spec = kind.spec();
        let mut offset = 0usize;
        let params = spec
            .params
            .iter()
            .map(|p| {
                let row = ParamRow::from_spec(p, offset);
                offset += row.slots();
                row
            })
            .collect();
        EffectLayout {
            id: kind.id().to_string(),
            label: spec.label.to_string(),
            builtin: Some(kind),
            cost: spec.cost,
            space: spec.space,
            params,
            entries: spec.passes.iter().map(|p| p.entry.to_string()).collect(),
            shrinks: spec.passes.iter().map(|p| p.shrink).collect(),
            slots: spec.slots(),
            block_len: spec.block_len(),
            fields: spec.field_names(),
            source: None,
        }
    }

    /// WGSL `Params` field names, in declaration order.
    pub fn field_names(&self) -> Vec<String> {
        self.fields.clone()
    }

    /// Slot offset of a parameter, or `None` if the key is unknown.
    pub fn slot_index(&self, key: &str) -> Option<usize> {
        self.params.iter().find(|p| p.key == key).map(|p| p.offset)
    }

    /// The parameter whose `key` matches, if any.
    pub fn param(&self, key: &str) -> Option<&ParamRow> {
        self.params.iter().find(|p| p.key == key)
    }

    /// Default value vector in slot order.
    pub fn default_params(&self) -> Vec<f32> {
        let mut values = Vec::with_capacity(self.slots);
        for row in &self.params {
            let defaults = row.default_slots();
            values.extend_from_slice(&defaults[..row.slots()]);
        }
        values
    }

    /// Resolve every pass's downscale exponent for one instance.
    pub fn pass_shrinks(&self, inst: &EffectInstance) -> Vec<u8> {
        self.shrinks.iter().map(|s| s.resolve(inst)).collect()
    }
}

/// A project-defined effect: WGSL module plus declared parameters and passes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomEffect {
    pub id: String,
    pub label: String,
    pub space: EffectSpace,
    pub passes: Vec<CustomPass>,
    pub params: Vec<CustomParam>,
    pub source: String,
}

/// One pass of a [`CustomEffect`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomPass {
    /// `@fragment` entry point name in `source`.
    pub entry: String,
    /// Downscale exponent, `0..=MAX_SHRINK`.
    pub shrink: u8,
}

/// One parameter of a [`CustomEffect`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomParam {
    pub key: String,
    pub label: String,
    pub kind: ParamKind,
    pub min: f32,
    pub max: f32,
    /// Declared defaults in slot order. May be absent or shorter than
    /// [`CustomParam::slots`] (the remainder reads as zero), and may carry the
    /// legacy four-element array: existing files and the Kotlin layer both send
    /// one, and a `vec3` or a matrix simply sends more.
    #[serde(default)]
    pub default: Vec<f32>,
    pub unit: String,
    pub choices: Vec<String>,
}

impl CustomParam {
    /// Number of `f32` slots this parameter occupies.
    pub const fn slots(&self) -> usize {
        match self.kind {
            ParamKind::Color => 4,
            ParamKind::Vec2 => 2,
            ParamKind::Vec3 => 3,
            ParamKind::Vec4 => 4,
            ParamKind::Mat3 => 9,
            ParamKind::Mat4 => 16,
            _ => 1,
        }
    }

    /// The `Params` members this parameter contributes, in declaration order.
    pub fn members(&self) -> Vec<ParamMember> {
        members_of(&self.key, self.kind)
    }

    /// Default values in slot order, zero-filled to [`CustomParam::slots`] when
    /// the wire vector is shorter.
    pub fn default_slots(&self) -> Vec<f32> {
        let n = self.slots();
        let mut values = vec![0.0; n];
        let take = self.default.len().min(n);
        values[..take].copy_from_slice(&self.default[..take]);
        values
    }

    /// Own this declaration as an [`EffectLayout`] row at `offset`.
    fn row(&self, offset: usize) -> ParamRow {
        ParamRow {
            key: self.key.clone(),
            label: self.label.clone(),
            kind: self.kind,
            min: self.min,
            max: self.max,
            default: self.default_slots(),
            unit: self.unit.clone(),
            choices: self.choices.clone(),
            offset,
        }
    }
}

impl CustomEffect {
    /// Parameter rows in slot order, with running offsets.
    fn rows(&self) -> Vec<ParamRow> {
        let mut offset = 0usize;
        self.params
            .iter()
            .map(|p| {
                let row = p.row(offset);
                offset += row.slots();
                row
            })
            .collect()
    }

    /// Total `f32` slots across all declared parameters.
    pub fn slots(&self) -> usize {
        self.params.iter().map(CustomParam::slots).sum()
    }

    /// Uniform block length in `f32`, declared as the slots padded to a
    /// multiple of four (16 bytes). This is an estimate: the authoritative span
    /// is what `naga` reports at validation time, and that is what the renderer
    /// packs against.
    pub fn block_len(&self) -> usize {
        self.slots().div_ceil(4) * 4
    }

    /// WGSL `Params` field names, in declaration order.
    pub fn field_names(&self) -> Vec<String> {
        self.rows().iter().flat_map(ParamRow::field_names).collect()
    }

    /// Default value vector in slot order.
    pub fn default_params(&self) -> Vec<f32> {
        let mut values = Vec::with_capacity(self.slots());
        for row in self.rows() {
            let defaults = row.default_slots();
            values.extend_from_slice(&defaults[..row.slots()]);
        }
        values
    }

    /// Downscale exponent per pass, clamped into `0..=MAX_SHRINK`.
    pub fn pass_shrinks(&self) -> Vec<u8> {
        self.passes
            .iter()
            .map(|p| p.shrink.min(MAX_SHRINK))
            .collect()
    }

    /// Structural check that needs no WGSL front end, because `naga` is only a
    /// dependency of the renderer. Returns the first problem, phrased for a
    /// user, or `None` when the shape is sound.
    pub fn shape_error(&self) -> Option<String> {
        if self.id.is_empty() {
            return Some("effect id is empty".to_string());
        }
        if self.id.len() > 64 {
            return Some(format!(
                "effect id `{}` is longer than 64 characters",
                self.id
            ));
        }
        if !self
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Some(format!(
                "effect id `{}` must contain only [a-z0-9_]",
                self.id
            ));
        }
        if EffectKind::from_id(&self.id).is_some() {
            return Some(format!(
                "effect id `{}` collides with a built-in effect",
                self.id
            ));
        }
        if self.label.trim().is_empty() {
            return Some("effect label is empty".to_string());
        }
        if self.source.trim().is_empty() {
            return Some("effect source is empty".to_string());
        }
        if self.passes.is_empty() {
            return Some("effect declares no passes".to_string());
        }
        let slots = self.slots();
        if slots == 0 || slots > 128 {
            return Some(format!(
                "effect declares {slots} parameter slots; the limit is 1..=128"
            ));
        }
        let mut entries = HashSet::new();
        for pass in &self.passes {
            if pass.entry.is_empty() {
                return Some("pass entry is empty".to_string());
            }
            if !is_identifier(&pass.entry) {
                return Some(format!(
                    "pass entry `{}` is not a WGSL identifier",
                    pass.entry
                ));
            }
            if !entries.insert(pass.entry.as_str()) {
                return Some(format!(
                    "pass entry `{}` is declared more than once",
                    pass.entry
                ));
            }
            if pass.shrink > MAX_SHRINK {
                return Some(format!(
                    "pass `{}` shrink {} exceeds MAX_SHRINK ({MAX_SHRINK})",
                    pass.entry, pass.shrink
                ));
            }
        }
        let mut keys = HashSet::new();
        for p in &self.params {
            if p.key.is_empty() {
                return Some("parameter key is empty".to_string());
            }
            if !keys.insert(p.key.as_str()) {
                return Some(format!(
                    "parameter key `{}` is declared more than once",
                    p.key
                ));
            }
            let n = p.slots();
            // Trailing slots beyond the kind's own are padding: a legacy
            // four-element default on a scalar, or whatever a hand-written file
            // sent. They may be present, but only if they are zero — a non-zero
            // one would silently become part of an adjacent member's block.
            if p.default.len() > n && p.default[n..].iter().any(|v| *v != 0.0) {
                return Some(format!("parameter `{}` sets unused default slots", p.key));
            }
            if p.min > p.max {
                return Some(format!(
                    "parameter `{}` has min {} greater than max {}",
                    p.key, p.min, p.max
                ));
            }
            if matches!(p.kind, ParamKind::Choice) && p.choices.is_empty() {
                return Some(format!("parameter `{}` is a Choice with no choices", p.key));
            }
        }
        None
    }

    /// Build the owned layout for this effect, after the structural check.
    pub fn layout(&self) -> Result<EffectLayout, String> {
        if let Some(reason) = self.shape_error() {
            return Err(reason);
        }
        let params = self.rows();
        Ok(EffectLayout {
            id: self.id.clone(),
            label: self.label.clone(),
            builtin: None,
            cost: EffectCost::Gpu,
            space: self.space,
            slots: self.slots(),
            block_len: self.block_len(),
            fields: params.iter().flat_map(ParamRow::field_names).collect(),
            entries: self.passes.iter().map(|p| p.entry.clone()).collect(),
            shrinks: self
                .passes
                .iter()
                .map(|p| ShrinkRule::Fixed(p.shrink.min(MAX_SHRINK)))
                .collect(),
            params,
            source: Some(self.source.clone()),
        })
    }
}

/// `true` for a WGSL identifier: a letter or `_`, then letters, digits or `_`.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl EffectKind {
    /// Every kind, in catalogue order.
    pub const ALL: &'static [EffectKind] = &[
        EffectKind::Blur,
        EffectKind::ColorTune,
        EffectKind::Threshold,
        EffectKind::ChromaKey,
        EffectKind::CopyBackground,
        EffectKind::Pixelate,
        EffectKind::Sphere360,
        EffectKind::DropShadow,
        EffectKind::InnerShadow,
        EffectKind::Glow,
    ];

    /// The spec for this kind.
    pub const fn spec(self) -> &'static EffectSpec {
        match self {
            EffectKind::Blur => &BLUR_SPEC,
            EffectKind::ColorTune => &COLOR_TUNE_SPEC,
            EffectKind::Threshold => &THRESHOLD_SPEC,
            EffectKind::ChromaKey => &CHROMA_KEY_SPEC,
            EffectKind::CopyBackground => &COPY_BACKGROUND_SPEC,
            EffectKind::Pixelate => &PIXELATE_SPEC,
            EffectKind::Sphere360 => &SPHERE_SPEC,
            EffectKind::DropShadow => &DROP_SHADOW_SPEC,
            EffectKind::InnerShadow => &INNER_SHADOW_SPEC,
            EffectKind::Glow => &GLOW_SPEC,
        }
    }

    /// Stable identifier used in JSON, JNI and Kotlin.
    pub const fn id(self) -> &'static str {
        match self {
            EffectKind::Blur => "blur",
            EffectKind::ColorTune => "color_tune",
            EffectKind::Threshold => "threshold",
            EffectKind::ChromaKey => "chroma_key",
            EffectKind::CopyBackground => "copy_background",
            EffectKind::Pixelate => "pixelate",
            EffectKind::Sphere360 => "sphere360",
            EffectKind::DropShadow => "drop_shadow",
            EffectKind::InnerShadow => "inner_shadow",
            EffectKind::Glow => "glow",
        }
    }

    /// Inverse of [`EffectKind::id`].
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.id() == id)
    }
}

/// Which effect an instance runs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectTarget {
    /// A built-in from the catalogue.
    Builtin(EffectKind),
    /// The id of a project-defined effect.
    Custom(String),
}

impl EffectTarget {
    /// A built-in when the id names one, otherwise a project-defined id.
    pub fn from_id(id: &str) -> EffectTarget {
        match EffectKind::from_id(id) {
            Some(kind) => EffectTarget::Builtin(kind),
            None => EffectTarget::Custom(id.to_string()),
        }
    }

    /// The stable id: a built-in's catalogue id, or the custom id.
    pub fn id(&self) -> &str {
        match self {
            EffectTarget::Builtin(kind) => kind.id(),
            EffectTarget::Custom(id) => id,
        }
    }

    /// The built-in kind, when this names one.
    pub fn builtin(&self) -> Option<EffectKind> {
        match self {
            EffectTarget::Builtin(kind) => Some(*kind),
            EffectTarget::Custom(_) => None,
        }
    }
}

/// One effect applied to one layer, with animatable values.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectInstance {
    /// Stable identity, so keyframe tracks can address this instance.
    pub id: Uuid,
    /// Built-in kind, or the id of a project-defined effect.
    pub target: EffectTarget,
    /// Disabled effects are kept in the model but skipped when drawing.
    pub enabled: bool,
    /// Values in layout slot order; exactly `layout.slots` entries.
    pub params: Vec<f32>,
    /// Resolved description. Owned so a custom instance needs no registry.
    pub layout: Arc<EffectLayout>,
}

/// Today's serialised shape. Kept private and separate: the binary codec is a
/// legacy read path, so the live struct is not what lays the bytes out.
///
/// `custom` carries the id of a project-defined effect; when it is `Some` it
/// wins and `kind` is only a placeholder, because postcard is positional and
/// the slot has to hold something. The definition itself is not written here —
/// it rides in the project document, so one definition serves every chain that
/// uses it.
#[derive(Serialize, Deserialize)]
struct EffectInstanceCodec {
    id: Uuid,
    kind: EffectKind,
    enabled: bool,
    params: Vec<f32>,
    custom: Option<String>,
}

impl Serialize for EffectInstance {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let (kind, custom) = match &self.target {
            EffectTarget::Builtin(kind) => (*kind, None),
            // The kind is ignored on read; `Blur` is just a well-formed filler.
            EffectTarget::Custom(id) => (EffectKind::Blur, Some(id.clone())),
        };
        EffectInstanceCodec {
            id: self.id,
            kind,
            enabled: self.enabled,
            params: self.params.clone(),
            custom,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EffectInstance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let codec = EffectInstanceCodec::deserialize(deserializer)?;
        let (target, layout) = match codec.custom {
            Some(id) => {
                let layout = unresolved_custom_layout(&id, codec.params.len());
                (EffectTarget::Custom(id), Arc::new(layout))
            }
            None => (
                EffectTarget::Builtin(codec.kind),
                Arc::new(EffectLayout::of_kind(codec.kind)),
            ),
        };
        let mut inst = EffectInstance {
            id: codec.id,
            target,
            enabled: codec.enabled,
            params: codec.params,
            layout,
        };
        inst.normalise();
        Ok(inst)
    }
}

/// Stand-in layout for a decoded project-defined instance.
///
/// The codec carries only the id and the values; the definition lives in the
/// project document and is not in scope here. Every decoded row is therefore
/// treated as an untyped slot: the length is preserved, so [`EffectInstance`]'s
/// params survive untouched, but key-addressed access has to wait for the
/// definition to be resolved (the renderer registers it separately). Making the
/// slot count match the values is what stops `normalise` from discarding them.
fn unresolved_custom_layout(id: &str, slots: usize) -> EffectLayout {
    EffectLayout {
        id: id.to_string(),
        label: id.to_string(),
        builtin: None,
        cost: EffectCost::Gpu,
        space: EffectSpace::Display,
        params: Vec::new(),
        entries: Vec::new(),
        shrinks: Vec::new(),
        slots,
        block_len: slots.div_ceil(4) * 4,
        fields: Vec::new(),
        source: None,
    }
}

impl EffectInstance {
    /// A new instance of a built-in kind, with the spec's default values.
    pub fn new(kind: EffectKind) -> Self {
        let layout = Arc::new(EffectLayout::of_kind(kind));
        let params = layout.default_params();
        Self {
            id: Uuid::new_v4(),
            target: EffectTarget::Builtin(kind),
            enabled: true,
            params,
            layout,
        }
    }

    /// A new instance of a project-defined effect, with its declared defaults.
    pub fn custom(effect: &CustomEffect) -> Result<EffectInstance, String> {
        let layout = Arc::new(effect.layout()?);
        let params = layout.default_params();
        Ok(Self {
            id: Uuid::new_v4(),
            target: EffectTarget::Custom(effect.id.clone()),
            enabled: true,
            params,
            layout,
        })
    }

    /// Slot offset of `key` within [`EffectInstance::params`].
    pub fn slot_index(&self, key: &str) -> Option<usize> {
        self.layout.slot_index(key)
    }

    /// The value slice for `key`, or `None` if the key is unknown or `params`
    /// is the wrong length.
    pub fn get(&self, key: &str) -> Option<&[f32]> {
        let row = self.layout.param(key)?;
        let at = row.offset;
        self.params.get(at..at + row.slots())
    }

    /// Overwrite `key` with `values`, clamped to the layout. Returns `false`
    /// for an unknown key or a wrong-length slice.
    pub fn set(&mut self, key: &str, values: &[f32]) -> bool {
        let Some(row) = self.layout.param(key) else {
            return false;
        };
        if values.len() != row.slots() {
            return false;
        }
        let at = row.offset;
        let Some(dst) = self.params.get_mut(at..at + row.slots()) else {
            return false;
        };
        dst.copy_from_slice(values);
        row.clamp(dst);
        true
    }

    /// Repair the invariant `params.len() == layout.slots` and clamp every
    /// value. Call after deserialising untrusted input.
    pub fn normalise(&mut self) {
        if self.params.len() != self.layout.slots {
            self.params = self.layout.default_params();
            return;
        }
        for row in &self.layout.params {
            let n = row.slots();
            row.clamp(&mut self.params[row.offset..row.offset + n]);
        }
    }

    /// `true` when every value still equals its layout default.
    pub fn is_default(&self) -> bool {
        if self.params.len() != self.layout.slots {
            return false;
        }
        self.layout.params.iter().all(|row| {
            let n = row.slots();
            let defaults = row.default_slots();
            self.params[row.offset..row.offset + n] == defaults[..n]
        })
    }

    /// Every key/value pair, for the UI.
    pub fn named(&self) -> Vec<(String, Vec<f32>)> {
        self.layout
            .params
            .iter()
            .map(|row| {
                let n = row.slots();
                (
                    row.key.clone(),
                    self.params[row.offset..row.offset + n].to_vec(),
                )
            })
            .collect()
    }
}

/// Default value vector for a kind.
pub fn default_params(kind: EffectKind) -> Vec<f32> {
    let spec = kind.spec();
    let mut values = Vec::with_capacity(spec.slots());
    for p in spec.params {
        let defaults = p.default_slots();
        values.extend_from_slice(&defaults[..p.slots()]);
    }
    values
}

/// Per-group effect chains, as delivered by the Kotlin layer.
///
/// The editor's wire format flattens a scene into groups (solid shapes, then
/// textured draws), so a chain is addressed by group and index rather than by
/// layer identity. Input is sparse-friendly: a group may be shorter than the
/// draw list, or absent entirely, in which case the remaining draws simply have
/// no effects.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffectChains {
    /// Chains for the solid-shape draws, by draw index.
    pub shapes: Vec<Vec<EffectInstance>>,
    /// Chains for the textured draws, by draw index.
    pub textures: Vec<Vec<EffectInstance>>,
    /// Project-defined effects this document declares. Instances reference
    /// them by id through [`EffectTarget::Custom`].
    pub custom: Vec<CustomEffect>,
}

impl EffectChains {
    /// `true` when no draw carries an effect.
    pub fn is_empty(&self) -> bool {
        self.shapes
            .iter()
            .chain(self.textures.iter())
            .all(Vec::is_empty)
    }

    /// Chain for solid-shape draw `index`, or an empty slice.
    pub fn shape_chain(&self, index: usize) -> &[EffectInstance] {
        self.shapes.get(index).map_or(&[], Vec::as_slice)
    }

    /// Chain for textured draw `index`, or an empty slice.
    pub fn texture_chain(&self, index: usize) -> &[EffectInstance] {
        self.textures.get(index).map_or(&[], Vec::as_slice)
    }

    /// The project-defined effects this document declares.
    pub fn customs(&self) -> &[CustomEffect] {
        &self.custom
    }
}

#[derive(serde::Deserialize)]
struct ChainWire {
    #[serde(default)]
    kind: String,
    #[serde(default = "wire_enabled")]
    enabled: bool,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    params: Vec<f32>,
}

fn wire_enabled() -> bool {
    true
}

fn instance_from_wire(wire: ChainWire, customs: &[CustomEffect]) -> Option<EffectInstance> {
    let mut inst = match EffectKind::from_id(&wire.kind) {
        Some(kind) => EffectInstance::new(kind),
        None => {
            // Not a built-in: a project-defined effect referenced by id. An
            // unknown id is dropped, exactly as an unknown built-in kind is.
            let effect = customs.iter().find(|c| c.id == wire.kind)?;
            EffectInstance::custom(effect).ok()?
        }
    };
    if wire.params.len() == inst.params.len() {
        inst.params.copy_from_slice(&wire.params);
    }
    inst.enabled = wire.enabled;
    inst.normalise();
    if let Some(id) = wire.id.as_deref().and_then(|s| Uuid::parse_str(s).ok()) {
        inst.id = id;
    }
    Some(inst)
}

/// Parse `{"shapes":[[{…}]],"textures":[[{…}]],"custom":[{…}]}` into
/// [`EffectChains`].
///
/// An empty or blank string yields the default (no effects). Entries with an
/// unknown `kind` are dropped, and a `params` vector of the wrong arity falls
/// back to that kind's defaults, so a chain written by a newer build still
/// renders instead of failing the frame. A custom that fails the structural
/// check or repeats an id is dropped the same way, first declaration winning.
pub fn parse_chains_json(json: &str) -> Result<EffectChains, String> {
    if json.trim().is_empty() {
        return Ok(EffectChains::default());
    }
    #[derive(serde::Deserialize)]
    struct ChainsWire {
        #[serde(default)]
        shapes: Vec<Vec<ChainWire>>,
        #[serde(default)]
        textures: Vec<Vec<ChainWire>>,
        #[serde(default)]
        custom: Vec<CustomEffect>,
    }
    let wire: ChainsWire =
        serde_json::from_str(json).map_err(|e| format!("bad effect chains json: {e}"))?;
    let mut seen = HashSet::new();
    let custom: Vec<CustomEffect> = wire
        .custom
        .into_iter()
        .filter(|c| c.shape_error().is_none() && seen.insert(c.id.clone()))
        .collect();
    let convert = |groups: Vec<Vec<ChainWire>>| {
        groups
            .into_iter()
            .map(|group| {
                group
                    .into_iter()
                    .filter_map(|w| instance_from_wire(w, &custom))
                    .collect()
            })
            .collect()
    };
    let shapes = convert(wire.shapes);
    let textures = convert(wire.textures);
    Ok(EffectChains {
        shapes,
        textures,
        custom,
    })
}

/// JSON catalogue of every effect and parameter, for the Kotlin UI.
///
/// Shape: `{"effects":[{"id":"blur","label":"Blur","cost":"gpu",
/// "space":"display","slots":4,"passes":2,"params":[{"key":"radius",...}]}]}`
pub fn catalogue_json() -> String {
    catalogue_json_with(&[])
}

/// The built-in catalogue plus one entry per project-defined effect, in the
/// same shape with an extra `"custom": true` flag.
///
/// Custom entries report `cost` as `"gpu"`: a project-defined effect has no CPU
/// oracle, so the GPU path is the only one it can take.
pub fn catalogue_json_with(customs: &[CustomEffect]) -> String {
    use serde_json::{Value, json};
    let mut effects: Vec<Value> = EffectKind::ALL
        .iter()
        .map(|kind| {
            let spec = kind.spec();
            let mut offset = 0usize;
            let params: Vec<Value> = spec
                .params
                .iter()
                .map(|p| {
                    let row = ParamRow::from_spec(p, offset);
                    offset += row.slots();
                    param_json(&row)
                })
                .collect();
            json!({
                "id": kind.id(),
                "label": spec.label,
                "cost": match spec.cost {
                    EffectCost::Gpu => "gpu",
                    EffectCost::Cpu => "cpu",
                },
                "space": match spec.space {
                    EffectSpace::Display => "display",
                    EffectSpace::Linear => "linear",
                },
                "slots": spec.slots(),
                "block_len": spec.block_len(),
                "passes": spec.passes.len(),
                "params": params,
            })
        })
        .collect();
    for effect in customs {
        // `slot` is emitted from the same walk that owns the ordering, so the
        // UI never has to re-derive it. Getting that arithmetic wrong silently
        // writes one parameter's value into another's slot.
        let params: Vec<Value> = effect.rows().iter().map(param_json).collect();
        effects.push(json!({
            "id": effect.id,
            "label": effect.label,
            "cost": "gpu",
            "space": match effect.space {
                EffectSpace::Display => "display",
                EffectSpace::Linear => "linear",
            },
            "slots": effect.slots(),
            "block_len": effect.block_len(),
            "passes": effect.passes.len(),
            "params": params,
            "custom": true,
        }));
    }
    serde_json::to_string(&json!({ "effects": effects }))
        .unwrap_or_else(|_| "{\"effects\":[]}".into())
}

/// One catalogue parameter entry, from an owned row. The `slot` offset comes
/// from [`ParamRow::offset`], which is the same number the decoder uses.
fn param_json(row: &ParamRow) -> serde_json::Value {
    use serde_json::json;
    json!({
        "key": row.key,
        "label": row.label,
        "kind": match row.kind {
            ParamKind::Float => "float",
            ParamKind::Angle => "angle",
            ParamKind::Int => "int",
            ParamKind::Bool => "bool",
            ParamKind::Choice => "choice",
            ParamKind::Color => "color",
            ParamKind::Vec2 => "vec2",
            ParamKind::Vec3 => "vec3",
            ParamKind::Vec4 => "vec4",
            ParamKind::Mat3 => "mat3",
            ParamKind::Mat4 => "mat4",
        },
        "min": row.min,
        "max": row.max,
        "default": row.default,
        "choices": row.choices,
        "unit": row.unit,
        "slots": row.slots(),
        "slot": row.offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_json_parses_groups_and_drops_unknown_kinds() {
        let json = r#"{
            "shapes": [
                [{"kind": "blur", "enabled": true, "params": [1.0, 24.0, 0.0, 2.0]}],
                [],
                [{"kind": "ghost", "params": []}]
            ],
            "textures": [[{"kind": "glow", "enabled": false}]]
        }"#;
        let chains = parse_chains_json(json).expect("valid chains");
        assert_eq!(chains.shapes.len(), 3);
        assert_eq!(chains.shape_chain(0).len(), 1);
        assert_eq!(
            chains.shape_chain(0)[0].target,
            EffectTarget::Builtin(EffectKind::Blur)
        );
        assert_eq!(chains.shape_chain(0)[0].get("radius"), Some(&[24.0f32][..]));
        assert!(chains.shape_chain(1).is_empty());
        assert!(chains.shape_chain(2).is_empty(), "unknown kind dropped");
        assert_eq!(chains.texture_chain(0).len(), 1);
        assert!(!chains.texture_chain(0)[0].enabled);
        assert!(chains.texture_chain(9).is_empty(), "out of range is empty");
        assert!(!chains.is_empty());
    }

    #[test]
    fn chains_json_is_lenient_about_absent_groups_and_bad_params() {
        assert!(parse_chains_json("").expect("blank").is_empty());
        assert!(parse_chains_json("   ").expect("spaces").is_empty());
        assert!(parse_chains_json("{}").expect("empty object").is_empty());
        assert!(
            parse_chains_json(r#"{"shapes":[]}"#)
                .expect("only shapes")
                .is_empty()
        );

        // Wrong arity falls back to the spec defaults rather than failing.
        let chains = parse_chains_json(r#"{"textures":[[{"kind":"glow","params":[1.0]}]]}"#)
            .expect("bad arity degrades");
        let chain = chains.texture_chain(0);
        assert_eq!(chain.len(), 1);
        assert!(
            chain[0].is_default(),
            "wrong arity must not be half-applied"
        );

        assert!(parse_chains_json("not json").is_err());
    }

    #[test]
    fn chains_json_preserves_a_stable_id() {
        let id = "11111111-1111-4111-8111-111111111111";
        let chains = parse_chains_json(&format!(
            r#"{{"shapes":[[{{"id":"{id}","kind":"pixelate","params":[4.0,0.0]}}]]}}"#
        ))
        .expect("valid");
        assert_eq!(chains.shape_chain(0)[0].id.to_string(), id);
    }

    #[test]
    fn every_kind_has_a_spec_and_unique_id() {
        let mut ids = std::collections::HashSet::new();
        for kind in EffectKind::ALL {
            let spec = kind.spec();
            assert_eq!(spec.kind, *kind, "spec/kind mismatch for {}", kind.id());
            assert!(ids.insert(kind.id()), "duplicate id {}", kind.id());
            assert_eq!(EffectKind::from_id(kind.id()), Some(*kind));
            assert!(!spec.passes.is_empty(), "{} has no passes", kind.id());
            assert!(!spec.params.is_empty(), "{} has no params", kind.id());
        }
        assert_eq!(ids.len(), EffectKind::ALL.len());
        assert_eq!(EffectKind::from_id("nope"), None);
    }

    #[test]
    fn param_keys_are_unique_within_an_effect() {
        for kind in EffectKind::ALL {
            let spec = kind.spec();
            let mut seen = std::collections::HashSet::new();
            for p in spec.params {
                assert!(
                    seen.insert(p.key),
                    "{}: duplicate param {}",
                    kind.id(),
                    p.key
                );
                assert!(p.min <= p.max, "{}:{} bad range", kind.id(), p.key);
                if matches!(p.kind, ParamKind::Choice) {
                    assert!(!p.choices.is_empty());
                    assert!((0.0..p.choices.len() as f32).contains(&p.default[0]));
                }
                let d = p.default_slots();
                for value in d.iter().take(p.slots()) {
                    assert!(
                        (p.min..=p.max).contains(value) || matches!(p.kind, ParamKind::Angle),
                        "{}:{} default out of range",
                        kind.id(),
                        p.key
                    );
                }
            }
        }
    }

    #[test]
    fn field_names_match_slot_count_and_are_unique() {
        for kind in EffectKind::ALL {
            let spec = kind.spec();
            let names = spec.field_names();
            assert_eq!(names.len(), spec.slots(), "{}", kind.id());
            let unique: std::collections::HashSet<_> = names.iter().collect();
            assert_eq!(unique.len(), names.len(), "{}", kind.id());
            assert_eq!(spec.block_len() % 4, 0);
            assert!(spec.block_len() >= spec.slots());
        }
    }

    #[test]
    fn instance_starts_default_and_normalised() {
        for kind in EffectKind::ALL {
            let inst = EffectInstance::new(*kind);
            assert_eq!(inst.params.len(), kind.spec().slots(), "{}", kind.id());
            assert!(inst.is_default(), "{}", kind.id());
            assert_eq!(inst.layout.block_len, kind.spec().block_len());
            let sum: f32 = inst.params.iter().sum();
            let named: f32 = inst.named().iter().flat_map(|(_, v)| v.iter()).sum();
            assert_eq!(sum, named, "named() must cover every slot");
        }
    }

    #[test]
    fn set_clamps_and_rejects_bad_input() {
        let mut inst = EffectInstance::new(EffectKind::Blur);
        assert!(inst.set("radius", &[999.0]));
        assert_eq!(inst.get("radius"), Some(&[200.0f32][..]));
        assert!(inst.set("radius", &[-5.0]));
        assert_eq!(inst.get("radius"), Some(&[0.0f32][..]));
        assert!(!inst.set("radius", &[1.0, 2.0]), "wrong arity");
        assert!(!inst.set("nope", &[1.0]), "unknown key");
        assert!(!inst.is_default());
    }

    #[test]
    fn angle_wraps_into_half_open_range() {
        assert_eq!(wrap_angle(0.0), 0.0);
        assert_eq!(wrap_angle(180.0), -180.0);
        assert_eq!(wrap_angle(-180.0), -180.0);
        assert_eq!(wrap_angle(190.0), -170.0);
        assert_eq!(wrap_angle(-190.0), 170.0);
        assert_eq!(wrap_angle(540.0), -180.0);
        assert_eq!(wrap_angle(f32::NAN), 0.0);

        let mut inst = EffectInstance::new(EffectKind::ColorTune);
        assert!(inst.set("hue", &[725.0]));
        assert_eq!(inst.get("hue"), Some(&[5.0f32][..]));
    }

    #[test]
    fn choice_and_bool_and_int_snap() {
        let mut blur = EffectInstance::new(EffectKind::Blur);
        assert!(blur.set("mode", &[2.4]));
        assert_eq!(blur.get("mode"), Some(&[2.0f32][..]));
        assert!(blur.set("mode", &[99.0]));
        assert_eq!(
            blur.get("mode"),
            Some(&[3.0f32][..]),
            "clamped to last choice"
        );
        assert!(blur.set("downscale", &[3.6]));
        assert_eq!(blur.get("downscale"), Some(&[4.0f32][..]));
    }

    #[test]
    fn colour_uses_four_slots_and_clamps_alpha() {
        let mut inst = EffectInstance::new(EffectKind::ChromaKey);
        let at = inst.slot_index("key").expect("key param");
        assert_eq!(inst.get("key").map(<[f32]>::len), Some(4));
        assert!(inst.set("key", &[0.25, 2.0, -1.0, 3.0]));
        assert_eq!(
            &inst.params[at..at + 4],
            &[0.25, 1.0, 0.0, 1.0],
            "channels clamped to 0..1"
        );
        let names = EffectKind::ChromaKey.spec().field_names();
        assert!(names.contains(&"key_r".to_string()));
        assert!(names.contains(&"key_a".to_string()));
    }

    #[test]
    fn non_finite_values_fall_back_to_default() {
        let mut inst = EffectInstance::new(EffectKind::Pixelate);
        inst.params[0] = f32::NAN;
        inst.normalise();
        assert_eq!(inst.get("size"), Some(&[8.0f32][..]));
    }

    #[test]
    fn normalise_repairs_wrong_length() {
        let mut inst = EffectInstance::new(EffectKind::Glow);
        inst.params.truncate(1);
        inst.normalise();
        assert_eq!(inst.params.len(), EffectKind::Glow.spec().slots());
        assert!(inst.is_default());
    }

    #[test]
    fn blur_downscale_drives_pass_shrinks() {
        let mut inst = EffectInstance::new(EffectKind::Blur);
        let spec = EffectKind::Blur.spec();
        assert_eq!(spec.passes.len(), 2);
        assert_eq!(spec.pass_shrinks(&inst), vec![0, 0]);
        assert!(inst.set("downscale", &[3.0]));
        assert_eq!(spec.pass_shrinks(&inst), vec![3, 3]);
        assert!(inst.set("downscale", &[99.0]));
        assert_eq!(spec.pass_shrinks(&inst), vec![MAX_SHRINK, MAX_SHRINK]);
    }

    #[test]
    fn multi_pass_effects_end_with_a_full_resolution_composite() {
        for kind in [
            EffectKind::Glow,
            EffectKind::DropShadow,
            EffectKind::InnerShadow,
        ] {
            let spec = kind.spec();
            assert!(
                spec.passes.len() >= 3,
                "{} should have blur passes plus a composite",
                kind.id()
            );
            assert_eq!(
                spec.passes.last().map(|p| p.shrink),
                Some(FULL),
                "{} composite must run at full resolution",
                kind.id()
            );
            assert_eq!(spec.passes.last().map(|p| p.entry), Some("fs_composite"));
        }
    }

    #[test]
    fn cpu_effects_are_marked_and_have_a_gpu_pass_too() {
        let cpu: Vec<_> = EffectKind::ALL
            .iter()
            .filter(|k| k.spec().cost == EffectCost::Cpu)
            .collect();
        assert_eq!(cpu, vec![&EffectKind::CopyBackground]);
        for kind in cpu {
            assert_eq!(kind.spec().passes.len(), 1);
        }
    }

    #[test]
    fn postcard_roundtrip_preserves_instances() {
        let original: Vec<EffectInstance> = EffectKind::ALL
            .iter()
            .map(|k| EffectInstance::new(*k))
            .collect();
        let bytes = postcard::to_allocvec(&original).expect("encode");
        let decoded: Vec<EffectInstance> = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(original, decoded);
    }

    #[test]
    fn catalogue_json_lists_every_effect_and_param() {
        let json: serde_json::Value =
            serde_json::from_str(&catalogue_json()).expect("catalogue is valid JSON");
        let effects = json["effects"].as_array().expect("effects array");
        assert_eq!(effects.len(), EffectKind::ALL.len());
        for (entry, kind) in effects.iter().zip(EffectKind::ALL) {
            assert_eq!(entry["id"], kind.id());
            assert_eq!(
                entry["slots"].as_u64().unwrap() as usize,
                kind.spec().slots()
            );
            assert_eq!(
                entry["params"].as_array().unwrap().len(),
                kind.spec().params.len()
            );
        }
    }

    #[test]
    fn catalogue_json_exposes_a_contiguous_slot_offset_per_param() {
        let json: serde_json::Value =
            serde_json::from_str(&catalogue_json()).expect("catalogue is valid JSON");
        for (entry, kind) in json["effects"]
            .as_array()
            .unwrap()
            .iter()
            .zip(EffectKind::ALL)
        {
            let spec = kind.spec();
            let mut expected = 0usize;
            for (param_json, param) in entry["params"].as_array().unwrap().iter().zip(spec.params) {
                assert_eq!(
                    param_json["slot"].as_u64().unwrap() as usize,
                    expected,
                    "{}.{}: slot offset must be the running sum of slots",
                    kind.id(),
                    param.key
                );
                assert_eq!(
                    param_json["slots"].as_u64().unwrap() as usize,
                    param.slots()
                );
                // The published offset must agree with the in-crate lookup the
                // decoder uses, or a UI and `parse_chains_json` would disagree.
                assert_eq!(spec.slot_index(param.key), Some(expected));
                expected += param.slots();
            }
            assert_eq!(
                expected,
                spec.slots(),
                "{}: slots must tile the block",
                kind.id()
            );
            assert!(expected <= spec.block_len(), "{}", kind.id());
        }
    }

    #[test]
    fn unknown_effect_id_in_json_is_ignored_not_fatal() {
        assert_eq!(EffectKind::from_id("ghost"), None);
        assert_eq!(EffectKind::from_id(""), None);
    }
    /// A minimal structurally-sound project-defined effect.
    fn custom_effect(id: &str) -> CustomEffect {
        CustomEffect {
            id: id.to_string(),
            label: "Test Effect".to_string(),
            space: EffectSpace::Display,
            passes: vec![CustomPass {
                entry: "fs_main".to_string(),
                shrink: 0,
            }],
            params: vec![CustomParam {
                key: "amount".to_string(),
                label: "Amount".to_string(),
                kind: ParamKind::Float,
                min: 0.0,
                max: 1.0,
                default: vec![0.5, 0.0, 0.0, 0.0],
                unit: String::new(),
                choices: Vec::new(),
            }],
            source: "// wgsl under test; rumo-core never parses it".to_string(),
        }
    }

    #[test]
    fn effect_layout_of_kind_matches_the_static_spec() {
        // Regression guard for the refactor: the owned layout must describe a
        // built-in exactly as its static spec does, or the GPU packer and the
        // shader's `Params` block drift apart silently.
        for kind in EffectKind::ALL {
            let spec = kind.spec();
            let layout = EffectLayout::of_kind(*kind);
            assert_eq!(layout.id, kind.id());
            assert_eq!(layout.label, spec.label);
            assert_eq!(layout.builtin, Some(*kind));
            assert_eq!(layout.cost, spec.cost);
            assert_eq!(layout.space, spec.space);
            assert_eq!(layout.slots, spec.slots(), "{}", kind.id());
            assert_eq!(layout.block_len, spec.block_len(), "{}", kind.id());
            assert_eq!(layout.fields, spec.field_names(), "{}", kind.id());
            assert_eq!(
                layout.entries,
                spec.passes
                    .iter()
                    .map(|p| p.entry.to_string())
                    .collect::<Vec<_>>(),
                "{}",
                kind.id()
            );
            assert_eq!(
                layout.shrinks,
                spec.passes.iter().map(|p| p.shrink).collect::<Vec<_>>(),
                "{}",
                kind.id()
            );
            assert_eq!(layout.source, None, "{}", kind.id());
            assert_eq!(
                layout.default_params(),
                default_params(*kind),
                "{}",
                kind.id()
            );
            for p in spec.params {
                let row = layout.param(p.key).expect("spec row must exist");
                assert_eq!(row.offset, spec.slot_index(p.key).unwrap());
                assert_eq!(row.slots(), p.slots());
                assert_eq!(row.field_names(), p.field_names());
                assert_eq!(
                    row.default_slots(),
                    p.default_slots()[..p.slots()].to_vec(),
                    "{}:{}",
                    kind.id(),
                    p.key
                );
            }
            let inst = EffectInstance::new(*kind);
            assert_eq!(layout.pass_shrinks(&inst), spec.pass_shrinks(&inst));
        }
    }

    #[test]
    fn custom_effect_round_trips_through_chains_json() {
        let json = r#"{
            "custom": [{
                "id": "vignette_x",
                "label": "Vignette",
                "space": "display",
                "passes": [{"entry": "fs_main", "shrink": 1}],
                "params": [{
                    "key": "amount",
                    "label": "Amount",
                    "kind": "float",
                    "min": 0.0,
                    "max": 1.0,
                    "default": [0.25, 0.0, 0.0, 0.0],
                    "unit": "",
                    "choices": []
                }],
                "source": "@fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }"
            }],
            "shapes": [[{"kind": "vignette_x", "params": [0.75]}]]
        }"#;
        let chains = parse_chains_json(json).expect("valid chains");
        assert_eq!(chains.customs().len(), 1);
        assert_eq!(chains.customs()[0].id, "vignette_x");
        assert_eq!(chains.customs()[0].passes.len(), 1);
        assert_eq!(chains.customs()[0].passes[0].entry, "fs_main");
        assert_eq!(chains.customs()[0].passes[0].shrink, 1);
        let inst = &chains.shape_chain(0)[0];
        assert_eq!(inst.target, EffectTarget::Custom("vignette_x".to_string()));
        assert_eq!(inst.layout.slots, 1);
        assert_eq!(inst.get("amount"), Some(&[0.75f32][..]));
    }

    #[test]
    fn unknown_custom_id_is_dropped() {
        let chains =
            parse_chains_json(r#"{"shapes":[[{"kind":"ghost_custom"}]]}"#).expect("valid chains");
        assert!(chains.shape_chain(0).is_empty());
        assert!(chains.customs().is_empty());
    }

    #[test]
    fn invalid_and_duplicate_customs_are_dropped_first_wins() {
        let json = r#"{
            "custom": [
                {"id": "Bad-Id", "label": "x", "space": "display", "passes": [],
                 "params": [], "source": "//"},
                {"id": "dup", "label": "ok", "space": "display",
                 "passes": [{"entry": "fs_main", "shrink": 0}],
                 "params": [{"key": "a", "label": "A", "kind": "float", "min": 0.0,
                             "max": 1.0, "default": [0.0,0.0,0.0,0.0], "unit": "", "choices": []}],
                 "source": "//"},
                {"id": "dup", "label": "second", "space": "display",
                 "passes": [{"entry": "fs_main", "shrink": 0}],
                 "params": [{"key": "a", "label": "A", "kind": "float", "min": 0.0,
                             "max": 1.0, "default": [0.0,0.0,0.0,0.0], "unit": "", "choices": []}],
                 "source": "//"}
            ]
        }"#;
        let chains = parse_chains_json(json).expect("valid chains");
        assert_eq!(
            chains.customs().len(),
            1,
            "invalid dropped, duplicate deduped"
        );
        assert_eq!(
            chains.customs()[0].label,
            "ok",
            "first valid declaration wins"
        );
    }

    #[test]
    fn shape_error_rejects_representative_mistakes() {
        let e = custom_effect("ok");
        assert!(e.shape_error().is_none());

        let mut bad = custom_effect("ok");
        bad.id = "Blur".to_string();
        assert!(bad.shape_error().unwrap().contains("[a-z0-9_]"));
        bad.id = "blur".to_string();
        assert!(bad.shape_error().unwrap().contains("built-in"));
        bad.id = String::new();
        assert!(bad.shape_error().unwrap().contains("empty"));

        let mut bad = custom_effect("ok");
        bad.passes.clear();
        assert!(bad.shape_error().unwrap().contains("no passes"));

        let mut bad = custom_effect("ok");
        bad.passes[0].shrink = MAX_SHRINK + 1;
        assert!(bad.shape_error().unwrap().contains("shrink"));
        bad.passes[0].shrink = 0;
        bad.passes[0].entry = "not an identifier".to_string();
        assert!(bad.shape_error().unwrap().contains("identifier"));

        let mut bad = custom_effect("ok");
        bad.params.clear();
        assert!(bad.shape_error().unwrap().contains("slots"));

        let mut bad = custom_effect("ok");
        bad.params[0].kind = ParamKind::Choice;
        assert!(bad.shape_error().unwrap().contains("Choice"));

        let mut bad = custom_effect("ok");
        bad.params[0].default[1] = 1.0;
        assert!(bad.shape_error().unwrap().contains("unused"));
    }

    /// A bare declaration of one parameter, for the shape tests below.
    fn param(key: &str, kind: ParamKind) -> CustomParam {
        CustomParam {
            key: key.to_string(),
            label: key.to_string(),
            kind,
            min: 0.0,
            max: 1.0,
            default: Vec::new(),
            unit: String::new(),
            choices: Vec::new(),
        }
    }

    #[test]
    fn every_param_kind_reports_the_members_it_writes() {
        use MemberShape::{Matrix3, Matrix4, Scalar, Vector};
        /// One kind and the members it is expected to contribute.
        type Case = (ParamKind, Vec<(&'static str, MemberShape, u8)>);
        let cases: Vec<Case> = vec![
            (ParamKind::Float, vec![("amount", Scalar, 1)]),
            (ParamKind::Angle, vec![("amount", Scalar, 1)]),
            (ParamKind::Int, vec![("amount", Scalar, 1)]),
            (ParamKind::Bool, vec![("amount", Scalar, 1)]),
            (ParamKind::Choice, vec![("amount", Scalar, 1)]),
            (
                ParamKind::Color,
                vec![
                    ("amount_r", Scalar, 1),
                    ("amount_g", Scalar, 1),
                    ("amount_b", Scalar, 1),
                    ("amount_a", Scalar, 1),
                ],
            ),
            (ParamKind::Vec2, vec![("amount", Vector(2), 2)]),
            (ParamKind::Vec3, vec![("amount", Vector(3), 3)]),
            (ParamKind::Vec4, vec![("amount", Vector(4), 4)]),
            (ParamKind::Mat3, vec![("amount", Matrix3, 9)]),
            (ParamKind::Mat4, vec![("amount", Matrix4, 16)]),
        ];
        for (kind, expected) in cases {
            let p = param("amount", kind);
            let members: Vec<(String, MemberShape, u8)> = p
                .members()
                .into_iter()
                .map(|m| (m.name, m.shape, m.components))
                .collect();
            let expected: Vec<(String, MemberShape, u8)> = expected
                .into_iter()
                .map(|(name, shape, components)| (name.to_string(), shape, components))
                .collect();
            assert_eq!(members, expected, "{kind:?}");
            // A row's members are the same, and they tile exactly its slots.
            let row = p.row(0);
            assert_eq!(row.members(), p.members(), "{kind:?}");
            let components: usize = row.members().iter().map(|m| m.components as usize).sum();
            assert_eq!(components, row.slots(), "{kind:?}");
            assert_eq!(row.field_names().len(), row.members().len(), "{kind:?}");
        }
    }

    #[test]
    fn a_matrix_param_is_one_field_not_one_per_component() {
        let mat4 = param("warp", ParamKind::Mat4).row(0);
        assert_eq!(mat4.field_names(), vec!["warp".to_string()]);
        assert_eq!(mat4.slots(), 16);
        let mat3 = param("spin", ParamKind::Mat3).row(0);
        assert_eq!(mat3.field_names(), vec!["spin".to_string()]);
        assert_eq!(mat3.slots(), 9);
    }

    #[test]
    fn shape_error_accepts_one_hundred_and_twenty_eight_slots_and_rejects_more() {
        let mut effect = custom_effect("ok");
        // Eight matrices of sixteen slots each reach exactly the limit.
        effect.params = (0..8)
            .map(|i| param(&format!("m{i}"), ParamKind::Mat4))
            .collect();
        assert_eq!(effect.slots(), 128);
        assert!(effect.shape_error().is_none(), "{:?}", effect.shape_error());
        effect.params.push(param("extra", ParamKind::Float));
        let err = effect.shape_error().expect("129 slots must be refused");
        assert!(err.contains("1..=128"), "{err}");
    }

    #[test]
    fn a_vector_default_with_a_non_zero_unused_component_is_rejected() {
        let mut effect = custom_effect("ok");
        effect.params[0].kind = ParamKind::Vec3;
        // The fourth value has no slot; a colour-style four-array therefore has
        // to end in zero, exactly as a scalar's did.
        effect.params[0].default = vec![1.0, 2.0, 3.0, 4.0];
        let err = effect.shape_error().expect("must be refused");
        assert!(err.contains("unused"), "{err}");
        effect.params[0].default = vec![1.0, 2.0, 3.0, 0.0];
        assert!(effect.shape_error().is_none(), "{:?}", effect.shape_error());
    }

    #[test]
    fn custom_effect_with_vector_and_matrix_params_round_trips_through_chains_json() {
        let json = r#"{
            "custom": [{
                "id": "warp_x",
                "label": "Warp",
                "space": "display",
                "passes": [{"entry": "fs_main", "shrink": 0}],
                "params": [
                    {"key": "tint", "label": "Tint", "kind": "vec4",
                     "min": 0.0, "max": 1.0, "default": [1.0, 0.5, 0.25, 1.0],
                     "unit": "", "choices": []},
                    {"key": "warp", "label": "Warp", "kind": "mat4",
                     "min": -100.0, "max": 100.0,
                     "default": [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0,
                                 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
                     "unit": "", "choices": []}
                ],
                "source": "//"
            }],
            "shapes": [[{"kind": "warp_x", "params": [
                0.1, 0.2, 0.3, 0.4,
                2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0,
                0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0
            ]}]]
        }"#;
        let chains = parse_chains_json(json).expect("valid chains");
        assert_eq!(chains.customs().len(), 1);
        let effect = &chains.customs()[0];
        assert_eq!(effect.slots(), 20);
        assert_eq!(effect.field_names(), vec!["tint", "warp"]);
        assert_eq!(
            effect.params[0].default,
            vec![1.0, 0.5, 0.25, 1.0],
            "a four-element default still parses"
        );
        assert_eq!(
            effect.params[1].default,
            vec![
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0
            ],
            "a matrix keeps all sixteen defaults"
        );
        let inst = &chains.shape_chain(0)[0];
        assert_eq!(inst.target, EffectTarget::Custom("warp_x".to_string()));
        assert_eq!(inst.get("tint"), Some(&[0.1f32, 0.2, 0.3, 0.4][..]));
        assert_eq!(
            inst.get("warp"),
            Some(
                &[
                    2.0f32, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0,
                    2.0
                ][..]
            )
        );
    }

    #[test]
    fn postcard_roundtrips_a_custom_instance_by_id() {
        let effect = custom_effect("my_effect");
        let mut inst = EffectInstance::custom(&effect).expect("valid custom");
        assert!(inst.set("amount", &[0.25]));

        // The codec writes the id, not the definition: the project document
        // owns that, so one definition serves every chain that names it.
        let bytes = postcard::to_allocvec(&inst).expect("a custom instance now has a codec form");
        let back: EffectInstance = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(back.id, inst.id);
        assert_eq!(back.target, EffectTarget::Custom("my_effect".to_string()));
        assert_eq!(back.enabled, inst.enabled);
        assert_eq!(
            back.params,
            vec![0.25],
            "values ride with the instance; only the layout needs the definition"
        );
    }

    #[test]
    fn catalogue_json_with_appends_customs_and_keeps_offsets() {
        let base = catalogue_json();
        assert_eq!(
            catalogue_json_with(&[]),
            base,
            "no customs is the built-in list"
        );
        let effect = custom_effect("vignette_x");
        let json: serde_json::Value =
            serde_json::from_str(&catalogue_json_with(&[effect])).expect("valid JSON");
        let effects = json["effects"].as_array().unwrap();
        assert_eq!(effects.len(), EffectKind::ALL.len() + 1);
        let last = effects.last().unwrap();
        assert_eq!(last["id"], "vignette_x");
        assert_eq!(last["custom"], true);
        assert_eq!(last["slots"].as_u64().unwrap(), 1);
        assert_eq!(last["params"][0]["slot"].as_u64().unwrap(), 0);
    }
}
