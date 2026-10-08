// SPDX-License-Identifier: Apache-2.0

//! CPU reference ("oracle") for the effect catalogue.
//!
//! [`cpu_apply`] mirrors the maths of [`super::wgsl`] pass for pass, so it can
//! serve two purposes: it is the export fallback for effects marked
//! [`rumo_core::effect::EffectCost::Cpu`], and it is the reference the
//! analytic tests in `super::tests` are written against.
//!
//! Semantics chosen where `docs/08` leaves them open (the WGSL modules in
//! `super::wgsl` implement exactly these rules):
//!
//! * **Sampling** is bilinear with clamped edges. The host should bind a
//!   `FilterMode::Linear`, `AddressMode::ClampToEdge` sampler so the GPU pass
//!   sees the same filter.
//! * **Alpha is straight, never premultiplied.** Every blur accumulates
//!   `rgb * a` and `a`, accumulates the geometric weights, and un-premultiplies
//!   at the end: `a_out = sum(w*a)/sum(w)`, `rgb_out = sum(w*a*rgb)/sum(w*a)`.
//!   A constant image (opaque or not) is therefore reproduced exactly and a
//!   blur never drags colour out of transparent pixels.
//! * **Blur kernels.** `radius` is a target-texel cutoff: taps at every integer
//!   offset in `-r..=r`. Gaussian mode uses `sigma = radius / 3`; Box mode
//!   rounds `radius` and weights every tap equally; Directional mode rotates
//!   the pass axis (horizontal for pass 1, vertical for pass 2) by `angle`;
//!   Mask mode additionally multiplies every tap weight by the tap's alpha.
//! * **Downscale.** A pass with exponent `d` renders at
//!   `(w >> d).max(1) x (h >> d).max(1)` and reads its input one *target* texel
//!   apart, so `radius` is measured in target pixels. When the last pass is
//!   smaller than the layer, [`cpu_apply`] resamples back up to the layer size
//!   (that upscale is what the host's blit does).
//! * Intermediates stay in `f32` here; the GPU chain quantises every pass to
//!   `Rgba8Unorm`, so the two can differ by one LSB per pass.

use rumo_core::effect::{default_params, EffectInstance, EffectKind};

/// One RGBA8 input layer.
#[derive(Debug, Clone, Copy)]
pub struct EffectFrame<'a> {
    /// Straight-alpha RGBA8 pixels, row-major.
    pub rgba: &'a [u8],
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Layer time in seconds (unused by the v0.1 catalogue, kept for animated
    /// parameters).
    pub time: f32,
}

/// Planar-free RGBA image used inside the reference implementation.
#[derive(Debug, Clone)]
struct Img {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Img {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![[0.0; 4]; w.saturating_mul(h)],
        }
    }

    /// Safe texel read: out-of-range indices read transparent black.
    fn at(&self, x: usize, y: usize) -> [f32; 4] {
        self.px
            .get(y.saturating_mul(self.w).saturating_add(x))
            .copied()
            .unwrap_or([0.0; 4])
    }

    fn put(&mut self, x: usize, y: usize, v: [f32; 4]) {
        let i = y.saturating_mul(self.w).saturating_add(x);
        if let Some(slot) = self.px.get_mut(i) {
            *slot = v;
        }
    }

    /// Bilinear sample with clamped edges; non-finite coordinates read (0, 0).
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        if self.w == 0 || self.h == 0 {
            return [0.0; 4];
        }
        let w = self.w as f32;
        let h = self.h as f32;
        let u = if u.is_finite() { u } else { 0.0 };
        let v = if v.is_finite() { v } else { 0.0 };
        let x = (u * w - 0.5).clamp(0.0, (w - 1.0).max(0.0));
        let y = (v * h - 0.5).clamp(0.0, (h - 1.0).max(0.0));
        let x0 = x.floor();
        let y0 = y.floor();
        let x1 = (x0 + 1.0).min((w - 1.0).max(0.0));
        let y1 = (y0 + 1.0).min((h - 1.0).max(0.0));
        let fx = x - x0;
        let fy = y - y0;
        let p00 = self.at(x0 as usize, y0 as usize);
        let p10 = self.at(x1 as usize, y0 as usize);
        let p01 = self.at(x0 as usize, y1 as usize);
        let p11 = self.at(x1 as usize, y1 as usize);
        let mut out = [0.0; 4];
        for c in 0..4 {
            let top = p00[c] + (p10[c] - p00[c]) * fx;
            let bottom = p01[c] + (p11[c] - p01[c]) * fx;
            out[c] = top + (bottom - top) * fy;
        }
        out
    }

    /// Bilinear read at a pixel index (pixel centres are at `x + 0.5`).
    fn pixel_at(&self, p: [f32; 2]) -> [f32; 4] {
        let sx = (self.w as f32 - 1.0).max(0.0);
        let sy = (self.h as f32 - 1.0).max(0.0);
        let qx = p[0].clamp(0.0, sx);
        let qy = p[1].clamp(0.0, sy);
        self.sample(
            (qx + 0.5) / self.w.max(1) as f32,
            (qy + 0.5) / self.h.max(1) as f32,
        )
    }
}

fn quant(v: f32) -> u8 {
    let s = v.clamp(0.0, 1.0) * 255.0 + 0.5;
    if s.is_finite() { s.floor() as u8 } else { 0 }
}

/// Target size for a pass with power-of-two shrink exponent `d`.
fn shrunk(w: usize, h: usize, d: u8) -> (usize, usize) {
    let d = d.min(31);
    ((w >> d).max(1), (h >> d).max(1))
}

/// Bilinear resample of `img` onto a `w x h` target.
fn resample(img: &Img, w: usize, h: usize) -> Img {
    let mut out = Img::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = img.sample((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
            out.put(x, y, v);
        }
    }
    out
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let d = hi - lo;
    if d.abs() < 1e-12 {
        return if x >= hi { 1.0 } else { 0.0 };
    }
    let t = ((x - lo) / d).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn luma709(c: [f32; 3]) -> f32 {
    c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722
}

// --- colour transfer + Oklab (mirrors `wgsl_oklab!`) ------------------------

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

fn cbrt_f32(x: f32) -> f32 {
    x.max(1e-12).powf(1.0 / 3.0)
}

fn rgb_to_oklab(c: [f32; 3]) -> [f32; 3] {
    let l = 0.4122214708 * c[0] + 0.5363325363 * c[1] + 0.0514459929 * c[2];
    let m = 0.2119034982 * c[0] + 0.6806995451 * c[1] + 0.1073969566 * c[2];
    let s = 0.0883024619 * c[0] + 0.2817188376 * c[1] + 0.6299787005 * c[2];
    let (l_, m_, s_) = (cbrt_f32(l), cbrt_f32(m), cbrt_f32(s));
    [
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    ]
}

fn oklab_to_rgb(lab: [f32; 3]) -> [f32; 3] {
    let l_ = lab[0] + 0.3963377774 * lab[1] + 0.2158037573 * lab[2];
    let m_ = lab[0] - 0.1055613458 * lab[1] - 0.0638541728 * lab[2];
    let s_ = lab[0] - 0.0894841775 * lab[1] - 1.2914855480 * lab[2];
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}

// --- per-effect reference implementations ----------------------------------

fn color_tune(s: [f32; 4], hue: f32, chroma: f32, lightness: f32, brightness: f32, contrast: f32, saturation: f32) -> [f32; 4] {
    let lin = [
        srgb_to_linear(s[0]),
        srgb_to_linear(s[1]),
        srgb_to_linear(s[2]),
    ];
    let lab = rgb_to_oklab(lin);
    let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    let h = lab[2].atan2(lab[1]);
    let c2 = (c * (1.0 + chroma)).max(0.0);
    let h2 = h + hue.to_radians();
    let l2 = (lab[0] + lightness).clamp(0.0, 1.0);
    let back = oklab_to_rgb([l2, c2 * h2.cos(), c2 * h2.sin()]);
    let mut rgb = [
        linear_to_srgb(back[0]),
        linear_to_srgb(back[1]),
        linear_to_srgb(back[2]),
    ];
    let g = luma709(rgb);
    for v in rgb.iter_mut() {
        *v = g + (*v - g) * (1.0 + saturation);
    }
    for v in rgb.iter_mut() {
        *v += brightness;
    }
    for v in rgb.iter_mut() {
        *v = (*v - 0.5) * (1.0 + contrast) + 0.5;
    }
    [
        rgb[0].clamp(0.0, 1.0),
        rgb[1].clamp(0.0, 1.0),
        rgb[2].clamp(0.0, 1.0),
        s[3],
    ]
}

fn threshold(s: [f32; 4], level: f32, softness: f32, mode: f32) -> [f32; 4] {
    let mut sig = luma709([s[0], s[1], s[2]]);
    if mode > 0.5 && mode < 1.5 {
        sig = s[3];
    } else if mode > 1.5 {
        sig = s[0].max(s[1]).max(s[2]) - s[0].min(s[1]).min(s[2]);
    }
    let soft = softness.max(0.0);
    let t = if soft > 1e-4 {
        smoothstep(level - soft * 0.5, level + soft * 0.5, sig)
    } else if sig >= level {
        1.0
    } else {
        0.0
    };
    [s[0] * t, s[1] * t, s[2] * t, s[3] * t]
}

fn cbcr_delta(c: [f32; 3]) -> [f32; 2] {
    let y = luma709(c);
    [c[2] - y, c[0] - y]
}

fn chroma_key(s: [f32; 4], key: [f32; 4], similarity: f32, softness: f32, spill: f32) -> [f32; 4] {
    let key = [key[0], key[1], key[2]];
    let a = cbcr_delta([s[0], s[1], s[2]]);
    let b = cbcr_delta(key);
    let d = ((a[0] - b[0]) * (a[0] - b[0]) + (a[1] - b[1]) * (a[1] - b[1])).sqrt();
    let sim = similarity.max(0.0);
    let soft = softness.max(1e-3);
    let alpha = smoothstep(sim, sim + soft, d);
    let yk = luma709(key);
    let kc0 = [key[0] - yk, key[1] - yk, key[2] - yk];
    let klen = (kc0[0] * kc0[0] + kc0[1] * kc0[1] + kc0[2] * kc0[2]).sqrt();
    let mut rgb = [s[0], s[1], s[2]];
    if klen > 1e-4 {
        let kc = [kc0[0] / klen, kc0[1] / klen, kc0[2] / klen];
        let y = luma709([s[0], s[1], s[2]]);
        let px = [s[0] - y, s[1] - y, s[2] - y];
        let proj = (px[0] * kc[0] + px[1] * kc[1] + px[2] * kc[2]).max(0.0);
        let f = spill * (1.0 - alpha) * proj;
        for c in 0..3 {
            rgb[c] = (rgb[c] - kc[c] * f).max(0.0);
        }
    }
    [rgb[0], rgb[1], rgb[2], alpha]
}

fn copy_background(s: [f32; 4], mode: f32, color: [f32; 4], tolerance: f32, feather: f32) -> [f32; 4] {
    let fill = if mode > 1.5 {
        [color[0], color[1], color[2]]
    } else if mode > 0.5 {
        [1.0, 1.0, 1.0]
    } else {
        [0.0, 0.0, 0.0]
    };
    let tol = tolerance.max(0.0);
    let fea = feather.max(1e-4);
    let m = smoothstep(tol, tol + fea, s[3]);
    let mut out = [0.0; 4];
    for c in 0..3 {
        let flat = s[c] * s[3] + fill[c] * (1.0 - s[3]);
        out[c] = fill[c] + (flat - fill[c]) * m;
    }
    out[3] = 1.0;
    out
}

fn hex_centre(p: [f32; 2], s: f32) -> [f32; 2] {
    let row_h = s * 0.8660254;
    let r0 = (p[1] / row_h).floor();
    let mut best = [0.0f32, 0.0];
    let mut best_d = 1e30f32;
    for dr in -1..=1 {
        let r = r0 + dr as f32;
        let off = 0.5 * s * (r - 2.0 * (r * 0.5).floor());
        let c0 = ((p[0] - off) / s).floor();
        for dc in 0..=1 {
            let cc = c0 + dc as f32;
            let centre = [(cc + 0.5) * s + off, (r + 0.5) * row_h];
            let dx = centre[0] - p[0];
            let dy = centre[1] - p[1];
            let d = dx * dx + dy * dy;
            if d < best_d {
                best_d = d;
                best = centre;
            }
        }
    }
    best
}

fn pixelate(layer: &Img, size_px: f32, shape: f32) -> Img {
    let (w, h) = (layer.w, layer.h);
    let s = if size_px.is_finite() {
        size_px.round().clamp(1.0, 128.0)
    } else {
        8.0
    };
    let n = s as i32;
    let mut out = Img::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let c = if shape > 0.5 {
                hex_centre(p, s)
            } else {
                [
                    ((p[0] / s).floor() + 0.5) * s,
                    ((p[1] / s).floor() + 0.5) * s,
                ]
            };
            let p0 = [
                ((c[0] - 0.5 * s) / s).floor() * s,
                ((c[1] - 0.5 * s) / s).floor() * s,
            ];
            let mut acc = [0.0f32; 4];
            let mut count = 0.0f32;
            for j in 0..n {
                for k in 0..n {
                    let smp = layer.pixel_at([p0[0] + k as f32, p0[1] + j as f32]);
                    for ch in 0..4 {
                        acc[ch] += smp[ch];
                    }
                    count += 1.0;
                }
            }
            let count = if count > 0.0 { count } else { 1.0 };
            out.put(
                x,
                y,
                [acc[0] / count, acc[1] / count, acc[2] / count, acc[3] / count],
            );
        }
    }
    out
}

/// v0.1 stub: a perspective rotation of the layer about its centre.
fn sphere360(layer: &Img, radius: f32, yaw: f32, pitch: f32, fov: f32) -> Img {
    let (w, h) = (layer.w, layer.h);
    let k = (fov.to_radians() * 0.5).tan() / (2.0 * radius).max(1e-3);
    let (cy, sy) = (yaw.to_radians().cos(), yaw.to_radians().sin());
    let (cp, sp) = (pitch.to_radians().cos(), pitch.to_radians().sin());
    let mut out = Img::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let fx = (x as f32 + 0.5) / w as f32 - 0.5;
            let fy = (y as f32 + 0.5) / h as f32 - 0.5;
            let f = [fx * 2.0, fy * 2.0];
            let d = [f[0] * k, f[1] * k, 1.0];
            let len = (d[0] * d[0] + d[1] * d[1] + 1.0).sqrt().max(1e-8);
            let d = [d[0] / len, d[1] / len, 1.0 / len];
            let v1 = [d[0] * cy - d[2] * sy, d[1], d[0] * sy + d[2] * cy];
            let v2 = [
                v1[0],
                v1[1] * cp + v1[2] * sp,
                -v1[1] * sp + v1[2] * cp,
            ];
            let su = 0.5 + (v2[0] / v2[2].max(1e-4)) / k * 0.5;
            let sv = 0.5 + (v2[1] / v2[2].max(1e-4)) / k * 0.5;
            out.put(x, y, layer.sample(su, sv));
        }
    }
    out
}

/// One separable colour-blur pass (mirrors `blur_axis` in `wgsl_colour_blur!`).
fn blur_pass(
    input: &Img,
    ow: usize,
    oh: usize,
    base: [f32; 2],
    mode: f32,
    radius: f32,
    angle_deg: f32,
) -> Img {
    let a = angle_deg.to_radians();
    let (ca, sa) = (a.cos(), a.sin());
    let axis = [
        base[0] * ca - base[1] * sa,
        base[0] * sa + base[1] * ca,
    ];
    let radius = if radius.is_finite() { radius.max(0.0) } else { 0.0 };
    let is_box = mode.is_finite() && mode > 0.5 && mode < 1.5;
    let is_mask = mode.is_finite() && mode > 2.5;
    let r = if is_box { radius.round() } else { radius };
    let ri = r.clamp(0.0, 512.0) as i32;
    let sigma = (radius / 3.0).max(1e-4);
    let texel = [1.0 / ow.max(1) as f32, 1.0 / oh.max(1) as f32];
    let mut out = Img::new(ow, oh);
    for y in 0..oh {
        for x in 0..ow {
            let uv = [
                (x as f32 + 0.5) / ow.max(1) as f32,
                (y as f32 + 0.5) / oh.max(1) as f32,
            ];
            let mut acc = [0.0f32; 3];
            let mut aa = 0.0f32;
            let mut wsum = 0.0f32;
            for i in -ri..=ri {
                let t = i as f32;
                let mut w = 1.0 / (2.0 * ri as f32 + 1.0);
                if !is_box {
                    let z = t / sigma;
                    w = (-0.5 * z * z).exp();
                }
                let s = input.sample(
                    uv[0] + axis[0] * t * texel[0],
                    uv[1] + axis[1] * t * texel[1],
                );
                if is_mask {
                    w *= s[3];
                }
                for c in 0..3 {
                    acc[c] += s[c] * (w * s[3]);
                }
                aa += w * s[3];
                wsum += w;
            }
            let d = aa.max(1e-5);
            let dw = wsum.max(1e-5);
            out.put(x, y, [acc[0] / d, acc[1] / d, acc[2] / d, aa / dw]);
        }
    }
    out
}

/// One separable shadow/glow-style blur pass over a single-channel mask.
fn mask_blur_pass(
    input: &Img,
    ow: usize,
    oh: usize,
    base: [f32; 2],
    from_layer: bool,
    radius: f32,
    spread: f32,
    dilation: bool,
) -> Img {
    let radius = if radius.is_finite() { radius.max(0.0) } else { 0.0 };
    let spread = if spread.is_finite() { spread.max(0.0) } else { 0.0 };
    let ri = radius.round().clamp(0.0, 512.0) as i32;
    let sigma = (radius / 3.0).max(1e-4);
    let texel = [1.0 / ow.max(1) as f32, 1.0 / oh.max(1) as f32];
    let read = |u: f32, v: f32| -> f32 {
        let s = input.sample(u, v);
        if from_layer { s[3] } else { s[0] }
    };
    let mut out = Img::new(ow, oh);
    for y in 0..oh {
        for x in 0..ow {
            let uv = [
                (x as f32 + 0.5) / ow.max(1) as f32,
                (y as f32 + 0.5) / oh.max(1) as f32,
            ];
            let mut acc = 0.0f32;
            let mut wsum = 0.0f32;
            for i in -ri..=ri {
                let t = i as f32;
                let z = t / sigma;
                let w = (-0.5 * z * z).exp();
                let mut m = read(uv[0] + base[0] * t * texel[0], uv[1] + base[1] * t * texel[1]);
                if dilation && spread > 0.5 {
                    let lo = read(
                        uv[0] + base[0] * (t - spread) * texel[0],
                        uv[1] + base[1] * (t - spread) * texel[1],
                    );
                    let hi = read(
                        uv[0] + base[0] * (t + spread) * texel[0],
                        uv[1] + base[1] * (t + spread) * texel[1],
                    );
                    m = m.max(lo).max(hi);
                }
                acc += m * w;
                wsum += w;
            }
            let m = acc / wsum.max(1e-5);
            out.put(x, y, [m, 0.0, 0.0, 1.0]);
        }
    }
    out
}

/// Straight-alpha premultiplied "shadow behind / above the layer" composite.
fn shadow_composite(
    layer: &Img,
    mask: &Img,
    offset: [f32; 2],
    color: [f32; 4],
    opacity: f32,
    inner: bool,
) -> Img {
    let (w, h) = (layer.w, layer.h);
    let texel = [1.0 / w.max(1) as f32, 1.0 / h.max(1) as f32];
    let (ox, oy) = (offset[0] * texel[0], offset[1] * texel[1]);
    let mut out = Img::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let u = (x as f32 + 0.5) / w.max(1) as f32;
            let v = (y as f32 + 0.5) / h.max(1) as f32;
            let src = layer.sample(u, v);
            let m = mask.sample(u - ox, v - oy)[0];
            let (cp, ap) = if inner {
                let sa = color[3] * opacity * (1.0 - m) * src[3];
                let mut cp = [0.0f32; 3];
                for c in 0..3 {
                    cp[c] = color[c] * sa + src[c] * src[3] * (1.0 - sa);
                }
                (cp, sa + src[3] * (1.0 - sa))
            } else {
                let sa = color[3] * opacity * m;
                let mut cp = [0.0f32; 3];
                for c in 0..3 {
                    cp[c] = src[c] * src[3] + color[c] * (sa * (1.0 - src[3]));
                }
                (cp, src[3] + sa * (1.0 - src[3]))
            };
            let d = ap.max(1e-5);
            out.put(x, y, [cp[0] / d, cp[1] / d, cp[2] / d, ap]);
        }
    }
    out
}

/// Applies `f` to every pixel of `layer` (a full-resolution single pass).
fn map_layer(layer: &Img, f: impl Fn([f32; 4]) -> [f32; 4]) -> Img {
    let mut out = Img::new(layer.w, layer.h);
    for y in 0..layer.h {
        for x in 0..layer.w {
            out.put(x, y, f(layer.at(x, y)));
        }
    }
    out
}

/// Run the whole effect chain over one RGBA8 frame.
///
/// A disabled instance returns its input byte for byte. An instance whose
/// `params` length does not match its layout's `slots` is treated as if it
/// carried the layout defaults (see [`EffectInstance::normalise`]). A
/// project-defined effect has no CPU oracle and is skipped here.
pub fn cpu_apply(inst: &EffectInstance, src: &EffectFrame<'_>) -> Vec<u8> {
    let w = src.width as usize;
    let h = src.height as usize;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    if !inst.enabled {
        return src.rgba.to_vec();
    }
    let mut fixed = inst.clone();
    fixed.normalise();
    // A project-defined effect has no CPU oracle: only its author's WGSL knows
    // what it does, so this reference path cannot mirror it. The layer passes
    // through unchanged rather than through a fabricated result.
    let Some(kind) = fixed.target.builtin() else {
        return src.rgba.to_vec();
    };
    let layout = fixed.layout.clone();
    let p: &[f32] = &fixed.params;
    let get = |key: &str| -> f32 {
        layout
            .slot_index(key)
            .and_then(|i| p.get(i))
            .copied()
            .unwrap_or(0.0)
    };
    let color = |key: &str| -> [f32; 4] {
        match layout.slot_index(key) {
            Some(i) => [
                p.get(i).copied().unwrap_or(0.0),
                p.get(i + 1).copied().unwrap_or(0.0),
                p.get(i + 2).copied().unwrap_or(0.0),
                p.get(i + 3).copied().unwrap_or(0.0),
            ],
            None => [0.0; 4],
        }
    };

    let layer = {
        let mut img = Img::new(w, h);
        for (i, b) in src.rgba.chunks_exact(4).enumerate() {
            if i >= img.px.len() {
                break;
            }
            img.px[i] = [
                b[0] as f32 / 255.0,
                b[1] as f32 / 255.0,
                b[2] as f32 / 255.0,
                b[3] as f32 / 255.0,
            ];
        }
        img
    };

    let out = match kind {
        EffectKind::Blur => {
            let mode = get("mode");
            let radius = get("radius");
            let angle = get("angle");
            let shrinks = layout.pass_shrinks(&fixed);
            let mut cur = layer.clone();
            for (i, entry) in layout.entries.iter().enumerate() {
                let d = shrinks.get(i).copied().unwrap_or(0);
                let (ow, oh) = shrunk(w, h, d);
                let base = if entry.ends_with("_h") {
                    [1.0, 0.0]
                } else {
                    [0.0, 1.0]
                };
                cur = blur_pass(&cur, ow, oh, base, mode, radius, angle);
            }
            if cur.w == w && cur.h == h {
                cur
            } else {
                resample(&cur, w, h)
            }
        }
        EffectKind::ColorTune => map_layer(&layer, |s| {
            color_tune(
                s,
                get("hue"),
                get("chroma"),
                get("lightness"),
                get("brightness"),
                get("contrast"),
                get("saturation"),
            )
        }),
        EffectKind::Threshold => {
            map_layer(&layer, |s| threshold(s, get("level"), get("softness"), get("mode")))
        }
        EffectKind::ChromaKey => map_layer(&layer, |s| {
            chroma_key(
                s,
                color("key"),
                get("similarity"),
                get("softness"),
                get("spill"),
            )
        }),
        EffectKind::CopyBackground => map_layer(&layer, |s| {
            copy_background(
                s,
                get("mode"),
                color("color"),
                get("tolerance"),
                get("feather"),
            )
        }),
        EffectKind::Pixelate => pixelate(&layer, get("size"), get("shape")),
        EffectKind::Sphere360 => sphere360(
            &layer,
            get("radius"),
            get("yaw"),
            get("pitch"),
            get("fov"),
        ),
        EffectKind::DropShadow | EffectKind::InnerShadow => {
            let inner = kind == EffectKind::InnerShadow;
            let blur = get("blur");
            let spread = get("spread");
            let h_pass = mask_blur_pass(&layer, w, h, [1.0, 0.0], true, blur, spread, true);
            let v_pass = mask_blur_pass(&h_pass, w, h, [0.0, 1.0], false, blur, spread, true);
            shadow_composite(
                &layer,
                &v_pass,
                [get("offset_x"), get("offset_y")],
                color("color"),
                get("opacity"),
                inner,
            )
        }
        EffectKind::Glow => {
            let thr = get("threshold");
            let radius = get("radius");
            let mask = map_layer(&layer, |s| {
                let m = smoothstep(thr, thr + 0.1, luma709([s[0], s[1], s[2]])) * s[3];
                [m, 0.0, 0.0, 1.0]
            });
            let h_pass = mask_blur_pass(&mask, w, h, [1.0, 0.0], false, radius, 0.0, false);
            let v_pass = mask_blur_pass(&h_pass, w, h, [0.0, 1.0], false, radius, 0.0, false);
            let tint = get("tint");
            let glow_color = color("color");
            let intensity = get("intensity");
            let mut out = Img::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let src = layer.at(x, y);
                    let m = v_pass.at(x, y)[0];
                    let ga = glow_color[3] * intensity * m;
                    let mut cp = [0.0f32; 3];
                    for c in 0..3 {
                        let tinted = 1.0 + (glow_color[c] - 1.0) * tint;
                        cp[c] = src[c] * src[3] + tinted * ga;
                    }
                    let ap = (src[3] + ga).min(1.0);
                    let d = ap.max(1e-5);
                    out.put(x, y, [cp[0] / d, cp[1] / d, cp[2] / d, ap]);
                }
            }
            out
        }
    };

    let mut bytes = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let s = out.at(x, y);
            for c in 0..4 {
                bytes.push(quant(s[c]));
            }
        }
    }
    bytes
}

/// Default parameter vector for a kind (re-exported for tests/hosts).
pub fn default_params_for(kind: EffectKind) -> Vec<f32> {
    default_params(kind)
}
