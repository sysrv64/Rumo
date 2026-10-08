// SPDX-License-Identifier: Apache-2.0

//! One self-contained WGSL module per [`EffectKind`].
//!
//! Every module is composed from the same host-owned prelude (see
//! [`wgsl_source`]) and contains **no vertex stage** and exactly the
//! `@fragment` entry points named by `EffectKind::spec().passes[*].entry`.
//!
//! Conventions that apply to every module (see `docs/08`, 8.1/8.3):
//!
//! * Colour maths happens in **display space** (gamma-encoded 0..1), because
//!   the compositor works on `Rgba8Unorm` targets. `ColorTune` linearises
//!   internally for the Oklab step only and converts straight back.
//! * Layer textures are **straight alpha** (not premultiplied). Every blur in
//!   this file averages `rgb * a` and `a` and divides at the end, so a blur
//!   never bleeds colour out of transparent pixels and never produces a halo.
//! * Pass 0 reads `input_tex == origin_tex` (the untouched layer). Later passes
//!   read the previous pass's output in `input_tex` while `origin_tex` still
//!   holds the layer. `params` (group 1) never changes across the chain.
//! * Sampling always uses `textureSampleLevel(..., 0.0)`: explicit LOD keeps
//!   the effects legal without derivatives and usable inside control flow.
//!   The CPU oracle in [`super::cpu`] assumes a linear/clamped sampler.
//! * `radius`-like pixel sizes are expressed in **this pass's target texels**
//!   (`Frame::size`), so a downscaled pass blurs a wider area of the layer.

use rumo_core::effect::EffectKind;

/// The 32-byte frame block plus the four group-0 bindings.
macro_rules! wgsl_frame {
    () => {
        r#"
// ---------------------------------------------------------------------------
// Host-owned binding contract: no @vertex entry point, five bindings exactly.
// ---------------------------------------------------------------------------
struct Frame {
    size: vec2<f32>,
    time: f32,
    pass_index: f32,
    texel_input: vec2<f32>,
    texel_origin: vec2<f32>,
};

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var input_smp: sampler;
@group(0) @binding(2) var<uniform> frame: Frame;
@group(0) @binding(3) var origin_tex: texture_2d<f32>;
"#
    };
}

/// Clamped sampling helpers and the Rec.709 luma used by the catalogue.
macro_rules! wgsl_sample {
    () => {
        r#"
fn sample_at(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(input_tex, input_smp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);
}

fn origin_at(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(origin_tex, input_smp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);
}

fn luma709(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}
"#
    };
}

/// Full-colour separable blur used by `Blur` (all four modes).
macro_rules! wgsl_colour_blur {
    () => {
        r#"
// Separable blur of a straight-alpha colour image.
//
// `base` is the pass axis in unit space: (1,0) for the horizontal pass and
// (0,1) for the vertical one; `params.angle` rotates it, so the pair stays
// orthogonal. Weights are normalised, taps are one target texel apart, and the
// premultiplied accumulator is un-premultiplied at the end (no halos).
fn blur_axis(uv: vec2<f32>, base: vec2<f32>) -> vec4<f32> {
    let a = radians(params.angle);
    let ca = cos(a);
    let sa = sin(a);
    let axis = vec2<f32>(base.x * ca - base.y * sa, base.x * sa + base.y * ca);
    let radius = max(params.radius, 0.0);
    let is_box = params.mode > 0.5 && params.mode < 1.5;
    let is_mask = params.mode > 2.5;
    var r = radius;
    if (is_box) {
        r = round(r);
    }
    let ri = i32(clamp(r, 0.0, 512.0));
    let sigma = max(radius / 3.0, 1e-4);
    var acc = vec3<f32>(0.0);
    var aa = 0.0;
    var wsum = 0.0;
    for (var i = -ri; i <= ri; i = i + 1) {
        let t = f32(i);
        var w = 1.0 / (2.0 * f32(ri) + 1.0);
        if (!is_box) {
            let z = t / sigma;
            w = exp(-0.5 * z * z);
        }
        let s = sample_at(uv + axis * (t * frame.texel_input));
        if (is_mask) {
            w = w * s.a;
        }
        acc = acc + s.rgb * (w * s.a);
        aa = aa + w * s.a;
        wsum = wsum + w;
    }
    return vec4<f32>(acc / max(aa, 1e-5), aa / max(wsum, 1e-5));
}
"#
    };
}

/// Gaussian blur of a single-channel mask carried in `.r` (Glow's blur passes).
macro_rules! wgsl_mask_blur {
    () => {
        r#"
// Separable Gaussian blur of the mask in `input_tex.r`; the result stays in
// `.r` so the next pass can keep blurring it.
fn glow_blur(uv: vec2<f32>, base: vec2<f32>) -> vec4<f32> {
    let radius = max(params.radius, 0.0);
    let ri = i32(clamp(round(radius), 0.0, 512.0));
    let sigma = max(radius / 3.0, 1e-4);
    var acc = 0.0;
    var wsum = 0.0;
    for (var i = -ri; i <= ri; i = i + 1) {
        let t = f32(i);
        let z = t / sigma;
        let w = exp(-0.5 * z * z);
        acc = acc + sample_at(uv + base * (t * frame.texel_input)).r * w;
        wsum = wsum + w;
    }
    return vec4<f32>(acc / max(wsum, 1e-5), 0.0, 0.0, 1.0);
}
"#
    };
}

/// sRGB transfer functions and Oklab for `ColorTune`.
macro_rules! wgsl_oklab {
    () => {
        r#"
fn srgb_to_linear(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn linear_to_srgb(c: f32) -> f32 {
    if (c <= 0.0031308) {
        return c * 12.92;
    }
    return 1.055 * pow(max(c, 0.0), 1.0 / 2.4) - 0.055;
}

// cbrt via pow: WGSL has no cube root and `pow` of a negative base is
// undefined, so the base is floored at a tiny positive epsilon.
fn cbrt_f32(x: f32) -> f32 {
    return pow(max(x, 1e-12), 1.0 / 3.0);
}

// Oklab (Bjorn Ottosson), operating on linear sRGB.
fn rgb_to_oklab(c: vec3<f32>) -> vec3<f32> {
    let l = 0.4122214708 * c.r + 0.5363325363 * c.g + 0.0514459929 * c.b;
    let m = 0.2119034982 * c.r + 0.6806995451 * c.g + 0.1073969566 * c.b;
    let s = 0.0883024619 * c.r + 0.2817188376 * c.g + 0.6299787005 * c.b;
    let l_ = cbrt_f32(l);
    let m_ = cbrt_f32(m);
    let s_ = cbrt_f32(s);
    return vec3<f32>(
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    );
}

fn oklab_to_rgb(lab: vec3<f32>) -> vec3<f32> {
    let l_ = lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z;
    let m_ = lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z;
    let s_ = lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    return vec3<f32>(
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    );
}
"#
    };
}

/// `Blur`: two passes, `params.downscale` drives the shrink of both.
const BLUR_WGSL: &str = concat!(
    wgsl_frame!(),
    r#"
struct Params {
    mode: f32,
    radius: f32,
    angle: f32,
    downscale: f32,
};
@group(1) @binding(0) var<uniform> params: Params;
"#,
    wgsl_sample!(),
    wgsl_colour_blur!(),
    r#"
@fragment
fn fs_blur_h(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return blur_axis(uv, vec2<f32>(1.0, 0.0));
}

@fragment
fn fs_blur_v(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return blur_axis(uv, vec2<f32>(0.0, 1.0));
}
"#
);

/// `ColorTune`: hue/chroma/lightness in Oklch, then luma/saturation/brightness/
/// contrast in display space. All five operations are identities at 0.
const COLOR_TUNE_WGSL: &str = concat!(
    wgsl_frame!(),
    r#"
struct Params {
    hue: f32,
    chroma: f32,
    lightness: f32,
    brightness: f32,
    contrast: f32,
    saturation: f32,
};
@group(1) @binding(0) var<uniform> params: Params;
"#,
    wgsl_sample!(),
    wgsl_oklab!(),
    r#"
@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let s = sample_at(uv);
    let lin = vec3<f32>(srgb_to_linear(s.r), srgb_to_linear(s.g), srgb_to_linear(s.b));
    var lab = rgb_to_oklab(lin);
    let c = sqrt(lab.y * lab.y + lab.z * lab.z);
    let h = atan2(lab.z, lab.y);
    let c2 = max(c * (1.0 + params.chroma), 0.0);
    let h2 = h + radians(params.hue);
    let l2 = clamp(lab.x + params.lightness, 0.0, 1.0);
    lab = vec3<f32>(l2, c2 * cos(h2), c2 * sin(h2));
    let back = oklab_to_rgb(lab);
    var rgb = vec3<f32>(
        linear_to_srgb(back.x),
        linear_to_srgb(back.y),
        linear_to_srgb(back.z),
    );
    let g = luma709(rgb);
    rgb = vec3<f32>(g) + (rgb - vec3<f32>(g)) * (1.0 + params.saturation);
    rgb = rgb + vec3<f32>(params.brightness);
    rgb = (rgb - vec3<f32>(0.5)) * (1.0 + params.contrast) + vec3<f32>(0.5);
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), s.a);
}
"#
);

/// `Threshold`: a matte selected by luma, alpha or chroma magnitude.
const THRESHOLD_WGSL: &str = concat!(
    wgsl_frame!(),
    r#"
struct Params {
    level: f32,
    softness: f32,
    mode: f32,
};
@group(1) @binding(0) var<uniform> params: Params;
"#,
    wgsl_sample!(),
    r#"
@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let s = sample_at(uv);
    var sig = luma709(s.rgb);
    if (params.mode > 0.5 && params.mode < 1.5) {
        sig = s.a;
    } else if (params.mode > 1.5) {
        sig = max(s.r, max(s.g, s.b)) - min(s.r, min(s.g, s.b));
    }
    let level = params.level;
    let soft = max(params.softness, 0.0);
    var t = 0.0;
    if (soft > 1e-4) {
        t = smoothstep(level - soft * 0.5, level + soft * 0.5, sig);
    } else {
        t = select(0.0, 1.0, sig >= level);
    }
    return vec4<f32>(s.rgb * t, s.a * t);
}
"#
);

/// `ChromaKey`: CbCr distance to the key, plus chroma-keyed spill suppression.
const CHROMA_KEY_WGSL: &str = concat!(
    wgsl_frame!(),
    r#"
struct Params {
    key_r: f32,
    key_g: f32,
    key_b: f32,
    key_a: f32,
    similarity: f32,
    softness: f32,
    spill: f32,
};
@group(1) @binding(0) var<uniform> params: Params;
"#,
    wgsl_sample!(),
    r#"
// Chroma-only difference: the luma axis is removed so a bright green and a
// dark green key alike.
fn cbcr_delta(c: vec3<f32>) -> vec2<f32> {
    let y = luma709(c);
    return vec2<f32>(c.b - y, c.r - y);
}

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let s = sample_at(uv);
    let key = vec3<f32>(params.key_r, params.key_g, params.key_b);
    let d = distance(cbcr_delta(s.rgb), cbcr_delta(key));
    let sim = max(params.similarity, 0.0);
    let soft = max(params.softness, 1e-3);
    let a = smoothstep(sim, sim + soft, d);
    let kc0 = key - vec3<f32>(luma709(key));
    let klen = length(kc0);
    var rgb = s.rgb;
    if (klen > 1e-4) {
        let kc = kc0 / klen;
        let px = s.rgb - vec3<f32>(luma709(s.rgb));
        let proj = max(dot(px, kc), 0.0);
        rgb = s.rgb - kc * (proj * (params.spill * (1.0 - a)));
    }
    return vec4<f32>(max(rgb, vec3<f32>(0.0)), a);
}
"#
);

/// `CopyBackground`: flattens the layer onto black, white or a chosen colour.
const COPY_BACKGROUND_WGSL: &str = concat!(
    wgsl_frame!(),
    r#"
struct Params {
    mode: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
    tolerance: f32,
    feather: f32,
};
@group(1) @binding(0) var<uniform> params: Params;
"#,
    wgsl_sample!(),
    r#"
@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let s = sample_at(uv);
    var fill = vec3<f32>(0.0);
    if (params.mode > 1.5) {
        fill = vec3<f32>(params.color_r, params.color_g, params.color_b);
    } else if (params.mode > 0.5) {
        fill = vec3<f32>(1.0);
    }
    let tol = max(params.tolerance, 0.0);
    let feather = max(params.feather, 1e-4);
    let m = smoothstep(tol, tol + feather, s.a);
    let flat = s.rgb * s.a + fill * (1.0 - s.a);
    return vec4<f32>(mix(fill, flat, m), 1.0);
}
"#
);

/// `Pixelate`: exact block average over an aligned square or hex lattice.
const PIXELATE_WGSL: &str = concat!(
    wgsl_frame!(),
    r#"
struct Params {
    size: f32,
    shape: f32,
};
@group(1) @binding(0) var<uniform> params: Params;
"#,
    wgsl_sample!(),
    r#"
// Nearest centre of a brick-offset triangular lattice (s is the hex width).
fn hex_centre(p: vec2<f32>, s: f32) -> vec2<f32> {
    let row_h = s * 0.8660254;
    let r0 = floor(p.y / row_h);
    var best = vec2<f32>(0.0, 0.0);
    var best_d = 1e30;
    for (var dr = -1; dr <= 1; dr = dr + 1) {
        let r = r0 + f32(dr);
        let off = 0.5 * s * (r - 2.0 * floor(r * 0.5));
        let c0 = floor((p.x - off) / s);
        for (var dc = 0; dc <= 1; dc = dc + 1) {
            let cc = c0 + f32(dc);
            let centre = vec2<f32>((cc + 0.5) * s + off, (r + 0.5) * row_h);
            let d = dot(centre - p, centre - p);
            if (d < best_d) {
                best_d = d;
                best = centre;
            }
        }
    }
    return best;
}

// Nearest-neighbour read at input pixel (x, y), clamped to the image edge.
fn pixel_at(p: vec2<f32>) -> vec4<f32> {
    let q = clamp(p, vec2<f32>(0.0), frame.size - vec2<f32>(1.0));
    return sample_at((q + vec2<f32>(0.5)) / frame.size);
}

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let s = clamp(round(params.size), 1.0, 128.0);
    var c = (floor(uv * frame.size / s) + 0.5) * s;
    if (params.shape > 0.5) {
        c = hex_centre(uv * frame.size, s);
    }
    let p0 = floor((c - 0.5 * s) / s) * s;
    let n = i32(s);
    var acc = vec4<f32>(0.0);
    var count = 0.0;
    for (var j = 0; j < n; j = j + 1) {
        for (var k = 0; k < n; k = k + 1) {
            acc = acc + pixel_at(p0 + vec2<f32>(f32(k), f32(j)));
            count = count + 1.0;
        }
    }
    return acc / max(count, 1.0);
}
"#
);

/// `Sphere360` (v0.1 stub): a perspective rotation of the layer about its
/// centre. `radius` is a magnification about the centre (1.0 at 0.5).
const SPHERE_WGSL: &str = concat!(
    wgsl_frame!(),
    r#"
struct Params {
    radius: f32,
    yaw: f32,
    pitch: f32,
    fov: f32,
};
@group(1) @binding(0) var<uniform> params: Params;
"#,
    wgsl_sample!(),
    r#"
@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let k = tan(radians(params.fov) * 0.5) / max(2.0 * params.radius, 1e-3);
    let f = (uv - vec2<f32>(0.5)) * 2.0;
    let d = normalize(vec3<f32>(f.x * k, f.y * k, 1.0));
    let cy = cos(radians(params.yaw));
    let sy = sin(radians(params.yaw));
    let cp = cos(radians(params.pitch));
    let sp = sin(radians(params.pitch));
    let v1 = vec3<f32>(d.x * cy - d.z * sy, d.y, d.x * sy + d.z * cy);
    let v2 = vec3<f32>(v1.x, v1.y * cp + v1.z * sp, -v1.y * sp + v1.z * cp);
    let suv = vec2<f32>(0.5) + (v2.xy / max(v2.z, 1e-4)) / k * 0.5;
    return sample_at(suv);
}
"#
);

/// Shared half of `DropShadow`/`InnerShadow` (`params.blur` always Gaussian).
macro_rules! shadow_blur_passes {
    () => {
        r#"
// The mask source: pass 0 consumes the layer (input == origin, so its alpha is
// the matte); the following blur pass consumes the previous pass's `.r` mask.
fn mask_at(uv: vec2<f32>) -> f32 {
    let s = sample_at(uv);
    if (frame.pass_index < 0.5) {
        return s.a;
    }
    return s.r;
}

// Separable Gaussian blur of the layer's alpha, with `params.spread` applied as
// a 3-tap dilation along the pass axis before the weights are accumulated.
fn shadow_blur(uv: vec2<f32>, base: vec2<f32>) -> vec4<f32> {
    let spread = max(params.spread, 0.0);
    let radius = max(params.blur, 0.0);
    let ri = i32(clamp(round(radius), 0.0, 512.0));
    let sigma = max(radius / 3.0, 1e-4);
    var acc = 0.0;
    var wsum = 0.0;
    for (var i = -ri; i <= ri; i = i + 1) {
        let t = f32(i);
        let z = t / sigma;
        let w = exp(-0.5 * z * z);
        var m = mask_at(uv + base * (t * frame.texel_input));
        if (spread > 0.5) {
            let lo = mask_at(uv + base * ((t - spread) * frame.texel_input));
            let hi = mask_at(uv + base * ((t + spread) * frame.texel_input));
            m = max(m, max(lo, hi));
        }
        acc = acc + m * w;
        wsum = wsum + w;
    }
    return vec4<f32>(acc / max(wsum, 1e-5), 0.0, 0.0, 1.0);
}

@fragment
fn fs_blur_h(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return shadow_blur(uv, vec2<f32>(1.0, 0.0));
}

@fragment
fn fs_blur_v(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return shadow_blur(uv, vec2<f32>(0.0, 1.0));
}
"#
    };
}

macro_rules! shadow_params {
    () => {
        r#"
struct Params {
    offset_x: f32,
    offset_y: f32,
    blur: f32,
    spread: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
    opacity: f32,
};
@group(1) @binding(0) var<uniform> params: Params;
"#
    };
}

/// `DropShadow`: blurred masked alpha, offset, composited *behind* the layer.
const DROP_SHADOW_WGSL: &str = concat!(
    wgsl_frame!(),
    shadow_params!(),
    wgsl_sample!(),
    shadow_blur_passes!(),
    r#"
@fragment
fn fs_composite(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let src = origin_at(uv);
    let off = vec2<f32>(params.offset_x, params.offset_y) * frame.texel_input;
    let m = sample_at(uv - off).r;
    let sa = params.color_a * params.opacity * m;
    let sh = vec3<f32>(params.color_r, params.color_g, params.color_b);
    let cp = src.rgb * src.a + sh * (sa * (1.0 - src.a));
    let ap = src.a + sa * (1.0 - src.a);
    return vec4<f32>(cp / max(ap, 1e-5), ap);
}
"#
);

/// `InnerShadow`: the same blurred alpha inverted and clipped to the layer.
const INNER_SHADOW_WGSL: &str = concat!(
    wgsl_frame!(),
    shadow_params!(),
    wgsl_sample!(),
    shadow_blur_passes!(),
    r#"
@fragment
fn fs_composite(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let src = origin_at(uv);
    let off = vec2<f32>(params.offset_x, params.offset_y) * frame.texel_input;
    let m = sample_at(uv - off).r;
    let sa = params.color_a * params.opacity * (1.0 - m) * src.a;
    let sh = vec3<f32>(params.color_r, params.color_g, params.color_b);
    let cp = sh * sa + src.rgb * src.a * (1.0 - sa);
    let ap = sa + src.a * (1.0 - sa);
    return vec4<f32>(cp / max(ap, 1e-5), ap);
}
"#
);

/// `Glow`: threshold the layer into a mask, blur it twice, add it back.
const GLOW_WGSL: &str = concat!(
    wgsl_frame!(),
    r#"
struct Params {
    threshold: f32,
    radius: f32,
    intensity: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
    tint: f32,
};
@group(1) @binding(0) var<uniform> params: Params;
"#,
    wgsl_sample!(),
    wgsl_mask_blur!(),
    r#"
// Pass 0: bright, opaque parts of the layer become a soft mask in `.r`.
@fragment
fn fs_threshold(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let s = sample_at(uv);
    let m = smoothstep(params.threshold, params.threshold + 0.1, luma709(s.rgb)) * s.a;
    return vec4<f32>(m, 0.0, 0.0, 1.0);
}

@fragment
fn fs_blur_h(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return glow_blur(uv, vec2<f32>(1.0, 0.0));
}

@fragment
fn fs_blur_v(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return glow_blur(uv, vec2<f32>(0.0, 1.0));
}

// Pass 3: additive premultiplied glow over the untouched layer.
@fragment
fn fs_composite(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let src = origin_at(uv);
    let m = sample_at(uv).r;
    let ga = params.color_a * params.intensity * m;
    let tintc = vec3<f32>(params.color_r, params.color_g, params.color_b);
    let glow = mix(vec3<f32>(1.0), tintc, params.tint) * ga;
    let cp = src.rgb * src.a + glow;
    let ap = min(src.a + ga, 1.0);
    return vec4<f32>(cp / max(ap, 1e-5), ap);
}
"#
);

/// The complete WGSL module for one effect kind.
///
/// The returned source declares the shared `Frame` block, the four group-0
/// bindings, a `Params` struct whose fields are exactly
/// `kind.spec().field_names()` in order, and one `@fragment` entry point per
/// `kind.spec().passes[*].entry`. It never declares a `@vertex` function: the
/// host owns the full-screen triangle.
pub fn wgsl_source(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Blur => BLUR_WGSL,
        EffectKind::ColorTune => COLOR_TUNE_WGSL,
        EffectKind::Threshold => THRESHOLD_WGSL,
        EffectKind::ChromaKey => CHROMA_KEY_WGSL,
        EffectKind::CopyBackground => COPY_BACKGROUND_WGSL,
        EffectKind::Pixelate => PIXELATE_WGSL,
        EffectKind::Sphere360 => SPHERE_WGSL,
        EffectKind::DropShadow => DROP_SHADOW_WGSL,
        EffectKind::InnerShadow => INNER_SHADOW_WGSL,
        EffectKind::Glow => GLOW_WGSL,
    }
}

/// A `@group(g) @binding(b) var...` line, with its optional attributes kept in
/// order. Only [`AUTHORISED_BINDINGS`] may appear in a module.
pub const AUTHORISED_BINDINGS: [&str; 5] = [
    "@group(0) @binding(0)",
    "@group(0) @binding(1)",
    "@group(0) @binding(2)",
    "@group(0) @binding(3)",
    "@group(1) @binding(0)",
];

/// Members of `struct Frame`, in declaration order (8 `f32` slots).
pub const FRAME_FIELDS: [&str; 5] = ["size", "time", "pass_index", "texel_input", "texel_origin"];

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip `//` comments so struct bodies can be parsed field by field.
    fn strip_comments(src: &str) -> String {
        src.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Field names declared inside `struct <name> { ... }`, in order.
    fn struct_fields(src: &str, name: &str) -> Vec<String> {
        let needle = format!("struct {name} {{");
        let start = match src.find(&needle) {
            Some(i) => i + needle.len(),
            None => return Vec::new(),
        };
        let end = match src[start..].find('}') {
            Some(i) => start + i,
            None => return Vec::new(),
        };
        src[start..end]
            .split(',')
            .filter_map(|f| f.split(':').next())
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty())
            .collect()
    }

    /// `true` when `fn <entry>(` is declared with `@fragment` on the same line.
    fn is_fragment_entry(src: &str, entry: &str) -> bool {
        let needle = format!("fn {entry}(");
        let Some(at) = src.find(&needle) else {
            return false;
        };
        // `@fragment` sits on the line *above* the signature, so look back to
        // the end of the previous item and inspect the attribute block.
        let head = &src[..at];
        let cut = head.rfind('}').map(|i| i + 1).unwrap_or(0);
        head[cut..].contains("@fragment")
    }

    fn count(hay: &str, needle: &str) -> usize {
        hay.matches(needle).count()
    }

    #[test]
    fn every_module_agrees_with_its_spec() {
        for kind in EffectKind::ALL {
            let spec = kind.spec();
            let raw = wgsl_source(*kind);
            assert!(!raw.is_empty(), "{}: empty WGSL module", kind.id());
            let src = strip_comments(raw);

            // 1. `struct Params` field names, in order, equal the spec's.
            let expected = spec.field_names();
            let actual = struct_fields(&src, "Params");
            assert_eq!(
                actual.len(),
                expected.len(),
                "{}: struct Params has {} fields, spec wants {} ({expected:?})",
                kind.id(),
                actual.len(),
                expected.len()
            );
            for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
                assert_eq!(
                    a, e,
                    "{}: struct Params field #{i} is `{a}`, spec wants `{e}` (full order: {actual:?})",
                    kind.id()
                );
            }

            // 2. Every pass entry exists exactly once as a `@fragment` function.
            for pass in spec.passes {
                let n = count(&src, &format!("fn {}(", pass.entry));
                assert_eq!(
                    n,
                    1,
                    "{}: pass entry `{}` appears {n} times in the module",
                    kind.id(),
                    pass.entry
                );
                assert!(
                    is_fragment_entry(&src, pass.entry),
                    "{}: pass entry `{}` is not declared as a @fragment function",
                    kind.id(),
                    pass.entry
                );
            }

            // 3. No vertex stage, and no binding outside the allow-list.
            assert!(
                !src.contains("@vertex"),
                "{}: effect modules must not declare a vertex entry point",
                kind.id()
            );
            assert_eq!(
                count(&src, "@binding("),
                AUTHORISED_BINDINGS.len(),
                "{}: module must declare exactly {} bindings",
                kind.id(),
                AUTHORISED_BINDINGS.len()
            );
            for b in AUTHORISED_BINDINGS {
                assert_eq!(
                    count(&src, b),
                    1,
                    "{}: binding `{b}` must appear exactly once",
                    kind.id()
                );
            }
            // Derivative-free: implicit-LOD sampling is not allowed here.
            assert!(
                !src.contains("textureSample("),
                "{}: use textureSampleLevel, never textureSample",
                kind.id()
            );

            // 4. `struct Frame` is the documented 32-byte block, in order.
            let frame = struct_fields(&src, "Frame");
            assert_eq!(
                frame,
                FRAME_FIELDS.to_vec(),
                "{}: struct Frame members drifted from the host layout",
                kind.id()
            );
            let slots: usize = frame.iter().map(|f| if f == "size" || f.starts_with("texel") { 2 } else { 1 }).sum();
            assert_eq!(slots, 8, "{}: Frame must occupy 8 f32 slots", kind.id());
        }
    }
}
