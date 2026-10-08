// SPDX-License-Identifier: Apache-2.0

//! Effect execution: the pure, GPU-free half of the effect pipeline
//! (`docs/08-rust-layer-deepening.md`, 8.3).
//!
//! Two pieces live here:
//!
//! * [`wgsl_source`] returns one complete WGSL module per
//!   [`rumo_core::effect::EffectKind`]: the `@fragment` entry points named by
//!   `EffectSpec::passes`, the shared `Frame` block and a `Params` block whose
//!   fields are exactly `EffectSpec::field_names()` in order.
//! * [`cpu::cpu_apply`] is the CPU reference ("oracle") that repeats the same
//!   maths pixel for pixel. It is the export fallback for
//!   [`rumo_core::effect::EffectCost::Cpu`] effects and the reference the tests
//!   in this module assert against.
//!
//! Pipeline creation and pass execution live in `renderer.rs`; nothing here
//! touches `wgpu`. A test in this module parses every module and fails if the
//! WGSL and `rumo_core::effect` ever drift apart.
//!
//! # Binding contract (owned by the host)
//!
//! ```wgsl
//! @group(0) @binding(0) var input_tex:  texture_2d<f32>;  // this pass's input
//! @group(0) @binding(1) var input_smp:  sampler;
//! @group(0) @binding(2) var<uniform> frame: Frame;
//! @group(0) @binding(3) var origin_tex: texture_2d<f32>;  // the unmodified layer
//! @group(1) @binding(0) var<uniform> params: Params;
//!
//! struct Frame {
//!     size: vec2<f32>,         // intermediate target size in px
//!     time: f32,               // seconds
//!     pass: f32,               // pass index
//!     texel_input: vec2<f32>,  // 1.0 / size
//!     texel_origin: vec2<f32>, // 1.0 / origin size
//! }   // exactly 8 f32 slots = 32 bytes
//! ```
//!
//! Every entry point has the signature
//! `fn NAME(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32>`, with `uv`
//! measured from the top-left pixel corner. The host owns the vertex stage: no
//! module declares a `@vertex` function.
//!
//! `params` is `EffectSpec::slots()` `f32` values in spec order (a colour
//! `key` becomes `key_r, key_g, key_b, key_a`), padded by the host to a
//! multiple of 16 bytes with no extra members in the WGSL struct.
//!
//! # Conventions the shaders and the oracle share
//!
//! * **Pass 0** reads the layer in both `input_tex` and `origin_tex`; later
//!   passes read the previous pass's output in `input_tex` while `origin_tex`
//!   keeps the layer.
//! * Colour maths is in **display space** (gamma-encoded 0..1) — see `docs/08`,
//!   8.1. `ColorTune` linearises only for its Oklab step and converts back.
//! * Alpha is **straight**, never premultiplied. Blurs accumulate `rgb * a`
//!   and `a` and un-premultiply at the end, so they blur colour and alpha
//!   together without halos from transparent pixels.
//! * Sampling uses `textureSampleLevel(..., 0.0)` with clamped edges. The host
//!   should bind a linear, `ClampToEdge` sampler to match [`cpu::cpu_apply`].
//! * A pass with shrink exponent `d` renders at `(w >> d).max(1)` by
//!   `(h >> d).max(1)` and `radius`-like distances are measured in *target*
//!   texels (`Frame::texel_input`), so a downscaled pass blurs more of the
//!   layer for the same number.
//!
//! # What is not verified here
//!
//! There is no GPU adapter in the test environment, so nothing compiles the
//! WGSL and nothing compares GPU output with [`cpu::cpu_apply`]. The
//! `every_module_agrees_with_its_spec` test is a textual/structural validator
//! only; see `docs/08`, 8.7.

pub mod cpu;
mod wgsl;

pub use wgsl::{AUTHORISED_BINDINGS, FRAME_FIELDS, wgsl_source};

use rumo_core::effect::CustomEffect;

use crate::fx::{ParamsLayout, validate_source};

/// Validate one project-defined effect and return its `Params` layout.
///
/// The structural check runs first (no WGSL front end needed), then the module
/// is parsed and validated exactly like a built-in's. Finally the host's
/// packing contract is enforced: the module's `Params` fields must equal
/// `effect.field_names()` in order, the struct must cover the values the host
/// packs (its span must be at least `effect.slots() * 4` bytes), and every
/// declared pass entry must be a fragment entry point in the module. Every
/// message names the effect and the offending field, so it is safe to show to
/// the author.
///
/// The span bound is the *declared* size, deliberately not the padded block
/// length: the host binds a block padded up to a multiple of four `f32`, and a
/// struct smaller than the buffer bound to it is legal. Every built-in module
/// does exactly that (`Pixelate` declares two fields for a four-`f32` block), so
/// demanding a padded struct here would refuse a module written in the same
/// style as the ones that already ship.
///
/// This proves the module compiles and matches the declared `Params`. It cannot
/// prove the effect *looks* right, nor that any CPU reference mirrors it —
/// there is no CPU oracle for a project-defined effect by construction.
pub fn validate_custom(effect: &CustomEffect) -> Result<ParamsLayout, String> {
    if let Some(reason) = effect.shape_error() {
        return Err(reason);
    }
    let shape = validate_source(&effect.id, &effect.source)?;
    let expected = effect.field_names();
    if shape.params.names != expected {
        return Err(format!(
            "effect `{}`: `Params` fields {:?} do not match the declared parameters {:?} (they must match in order)",
            effect.id, shape.params.names, expected
        ));
    }
    let needed = effect.slots() as u32 * 4;
    if shape.params.span < needed {
        return Err(format!(
            "effect `{}`: `Params` spans {} bytes but the declared parameters need {needed}",
            effect.id, shape.params.span
        ));
    }
    for pass in &effect.passes {
        if !shape.fragments.iter().any(|f| f == &pass.entry) {
            return Err(format!(
                "effect `{}`: pass entry `{}` is not a fragment entry point in the module",
                effect.id, pass.entry
            ));
        }
    }
    Ok(shape.params)
}

#[cfg(test)]
mod tests;
