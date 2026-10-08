// SPDX-License-Identifier: Apache-2.0

//! Analytic tests for the CPU reference (`docs/08`, 8.7).
//!
//! There is no GPU adapter in the test environment, so nothing here can
//! compare GPU output against [`cpu_apply`]. Instead every test asserts a
//! property that the WGSL must also satisfy, and the structural validator in
//! `wgsl.rs` keeps the two sides in sync.

use super::cpu::{EffectFrame, cpu_apply};
use rumo_core::effect::{EffectInstance, EffectKind};

fn frame(rgba: &[u8], width: u32, height: u32) -> EffectFrame<'_> {
    EffectFrame {
        rgba,
        width,
        height,
        time: 0.0,
    }
}

/// Opaque constant image.
fn constant(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        v.extend_from_slice(&rgba);
    }
    v
}

/// Linear ramps in x and y: the blur of a linear ramp keeps its mean exactly,
/// which makes the energy test a real (not vacuous) invariant.
fn gradient(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            v.push((30 + 3 * x).min(255) as u8);
            v.push((30 + 3 * y).min(255) as u8);
            v.push(128);
            v.push(255);
        }
    }
    v
}

/// Deterministic pseudo-random opaque image.
fn pattern(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    let mut s: u32 = 0x1234_5678;
    for _ in 0..w * h {
        for c in 0..4 {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            v.push(if c == 3 { 255 } else { (s >> 17) as u8 });
        }
    }
    v
}

fn set(kind: EffectKind, key: &str, value: f32) -> EffectInstance {
    let mut inst = EffectInstance::new(kind);
    assert!(inst.set(key, &[value]), "{} has no key {key}", kind.id());
    inst
}

#[test]
fn colour_tune_defaults_are_identity() {
    let (w, h) = (24u32, 17u32);
    let img = pattern(w, h);
    let inst = EffectInstance::new(EffectKind::ColorTune);
    assert!(inst.is_default());
    let out = cpu_apply(&inst, &frame(&img, w, h));
    assert_eq!(out.len(), img.len());
    for (i, (a, b)) in img.iter().zip(out.iter()).enumerate() {
        assert!(
            (*a as i32 - *b as i32).abs() <= 1,
            "ColorTune defaults must be identity, byte {i}: {a} -> {b}"
        );
    }
}

#[test]
fn blur_of_a_constant_image_is_the_same_constant() {
    let (w, h) = (32u32, 32u32);
    for rgba in [[120u8, 200, 40, 255], [77, 9, 250, 128]] {
        let img = constant(w, h, rgba);
        for mode in [0.0f32, 1.0] {
            for radius in [0.0f32, 1.0, 4.0, 12.5, 40.0] {
                for downscale in [0.0f32, 3.0] {
                    let mut inst = EffectInstance::new(EffectKind::Blur);
                    assert!(inst.set("mode", &[mode]));
                    assert!(inst.set("radius", &[radius]));
                    assert!(inst.set("downscale", &[downscale]));
                    let out = cpu_apply(&inst, &frame(&img, w, h));
                    assert_eq!(out.len(), img.len());
                    for i in 0..out.len() {
                        assert!(
                            (out[i] as i32 - img[i] as i32).abs() <= 2,
                            "blur mode {mode} radius {radius} downscale {downscale} \
                             rgba {rgba:?}: byte {i} {} -> {}",
                            img[i],
                            out[i]
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn blur_preserves_rgb_energy() {
    let (w, h) = (64u32, 64u32);
    let img = gradient(w, h);
    let energy = |buf: &[u8]| -> f64 {
        buf.chunks_exact(4)
            .map(|p| p[0] as f64 + p[1] as f64 + p[2] as f64)
            .sum()
    };
    let before = energy(&img);
    for radius in [8.0f32, 32.0] {
        let inst = set(EffectKind::Blur, "radius", radius);
        let out = cpu_apply(&inst, &frame(&img, w, h));
        let after = energy(&out);
        let rel = (after - before).abs() / before;
        assert!(
            rel < 0.02,
            "blur radius {radius} changed total RGB energy by {:.4} ({before} -> {after})",
            rel
        );
    }
}

#[test]
fn chroma_key_cuts_the_key_colour_and_spares_far_colours() {
    let inst = EffectInstance::new(EffectKind::ChromaKey);
    let (w, h) = (8u32, 8u32);

    let keyed = constant(w, h, [0, 255, 0, 255]);
    let out = cpu_apply(&inst, &frame(&keyed, w, h));
    for (i, p) in out.chunks_exact(4).enumerate() {
        assert_eq!(p[3], 0, "pure key colour must be cut, pixel {i} kept {}", p[3]);
    }

    let far = constant(w, h, [200, 30, 240, 255]);
    let out = cpu_apply(&inst, &frame(&far, w, h));
    for (i, (a, b)) in far.chunks_exact(4).zip(out.chunks_exact(4)).enumerate() {
        assert_eq!(b[3], 255, "far colour lost alpha at pixel {i}");
        for c in 0..3 {
            assert!(
                (a[c] as i32 - b[c] as i32).abs() <= 1,
                "far colour changed at pixel {i} channel {c}: {} -> {}",
                a[c],
                b[c]
            );
        }
    }
}

#[test]
fn pixelate_matches_independent_block_averages() {
    let (w, h) = (16u32, 12u32);
    let size = 4u32;
    assert_eq!(w % size, 0);
    assert_eq!(h % size, 0);
    let img = pattern(w, h);
    let inst = set(EffectKind::Pixelate, "size", size as f32);
    let out = cpu_apply(&inst, &frame(&img, w, h));
    assert_eq!(out.len(), img.len());
    let n = (size * size) as f64;
    for by in 0..h / size {
        for bx in 0..w / size {
            for c in 0..4usize {
                let mut sum = 0u32;
                for y in by * size..(by + 1) * size {
                    for x in bx * size..(bx + 1) * size {
                        sum += img[((y * w + x) * 4) as usize + c] as u32;
                    }
                }
                let exact = sum as f64 / n;
                let got = out[((by * size * w + bx * size) * 4) as usize + c] as f64;
                assert!(
                    (got - exact).abs() <= 0.5 + 1e-6,
                    "block ({bx},{by}) channel {c}: output {got} is not the block mean {exact}"
                );
                let frac = exact - exact.floor();
                if (frac - 0.5).abs() > 1e-6 {
                    assert_eq!(
                        got as i32, exact.round() as i32,
                        "block ({bx},{by}) channel {c} mean {exact} must round exactly"
                    );
                }
            }
        }
    }
}

#[test]
fn threshold_is_monotone_in_level() {
    let (w, h) = (16u32, 16u32);
    let img = pattern(w, h);
    let render = |level: f32| cpu_apply(&set(EffectKind::Threshold, "level", level), &frame(&img, w, h));
    let mut prev = render(0.0);
    for level in [0.1f32, 0.25, 0.5, 0.75, 0.9, 1.0] {
        let cur = render(level);
        for i in 0..cur.len() {
            assert!(
                cur[i] as i32 <= prev[i] as i32 + 1,
                "raising level to {level} turned byte {i} back on: {} -> {}",
                prev[i],
                cur[i]
            );
        }
        prev = cur;
    }
}

#[test]
fn cpu_apply_never_panics_and_keeps_length() {
    for kind in EffectKind::ALL {
        for (w, h) in [(1u32, 1u32), (3, 5), (64, 64)] {
            let img = pattern(w, h);
            let inst = EffectInstance::new(*kind);
            let out = cpu_apply(&inst, &frame(&img, w, h));
            assert_eq!(
                out.len(),
                (w * h * 4) as usize,
                "{} at {w}x{h} returned {} bytes",
                kind.id(),
                out.len()
            );
        }
    }
}

#[test]
fn cpu_apply_survives_extreme_parameters() {
    let (w, h) = (3u32, 5u32);
    let img = pattern(w, h);
    for kind in EffectKind::ALL {
        let mut inst = EffectInstance::new(*kind);
        for v in inst.params.iter_mut() {
            *v = 1.0e9;
        }
        inst.normalise();
        let out = cpu_apply(&inst, &frame(&img, w, h));
        assert_eq!(out.len(), 60, "{} with maxed params", kind.id());
    }
    // Non-finite values must degrade to defaults, not poison the frame.
    for kind in EffectKind::ALL {
        let mut inst = EffectInstance::new(*kind);
        for v in inst.params.iter_mut() {
            *v = f32::NAN;
        }
        inst.normalise();
        let out = cpu_apply(&inst, &frame(&img, w, h));
        assert_eq!(out.len(), 60, "{} with NaN params", kind.id());
    }
}

#[test]
fn disabled_instance_returns_input_bytes() {
    let (w, h) = (5u32, 3u32);
    let img = pattern(w, h);
    let mut inst = set(EffectKind::Blur, "radius", 50.0);
    inst.enabled = false;
    assert_eq!(cpu_apply(&inst, &frame(&img, w, h)), img);
}

#[test]
fn wrong_length_params_behave_as_defaults() {
    let (w, h) = (9u32, 7u32);
    let img = pattern(w, h);
    for kind in EffectKind::ALL {
        let expected = cpu_apply(&EffectInstance::new(*kind), &frame(&img, w, h));
        for bad in [vec![], vec![1.0f32], vec![0.5f32; 32]] {
            let mut inst = EffectInstance::new(*kind);
            inst.params = bad.clone();
            let out = cpu_apply(&inst, &frame(&img, w, h));
            assert_eq!(
                out,
                expected,
                "{} with {} params must fall back to the spec defaults",
                kind.id(),
                bad.len()
            );
        }
    }
}
