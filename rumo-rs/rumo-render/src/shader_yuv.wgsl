// SPDX-License-Identifier: Apache-2.0
//
// A decoded video frame as planes, converted to RGBA8 in the fragment shader.
//
// This module holds **no colour constants**. The YCbCr → RGB matrix and the
// limited/full range stretches arrive in the uniform, computed once on the CPU
// by `rumo_media::video::yuv::coeffs_f32` — the same function the CPU
// conversion uses. A shader that got a wrong matrix would show green or pink
// frames and still run clean, so the matrix is deliberately kept in exactly one
// place in the workspace and merely *passed* here.
//
// The sampling is deliberately identical to the CPU converter:
//   * `textureLoad` on an R8Uint plane reads the decoded byte itself, so the
//     value is exactly what `y[row * stride + x]` returned on the CPU. A
//     sampled-and-rescaled `texture_2d<f32>` would round-trip through
//     `value / 255 * 255` and could land a whole code value low, which shows up
//     as dark banding in gradients.
//   * There is no sampler binding at all, so nearest is the only sampling this
//     shader can do — chroma is read at `(x >> 1, y >> 1)`, exactly the CPU's
//     nearest 4:2:0 upsample. Bilinear chroma (and bilinear minification of the
//     luma) is a separate, deliberate change: docs/12 §12.3.
//   * `quantize` is the CPU's `floor(x + 0.5)` clamped to `0..=255`, so the two
//     paths round identically. A host test in `texture.rs` pins this arithmetic
//     against `Coeffs::rgb` over the whole luma domain.

struct YuvUniforms {
    transform: mat4x4<f32>,
    color: vec4<f32>,
    // x = y_scale, y = y_offset, z = c_center, w = format (0 I420, 1 NV12, 2 NV21).
    luma: vec4<f32>,
    // x = r_v, y = g_u, z = g_v, w = b_u.
    chroma: vec4<f32>,
    // x = crop width, y = crop height. The visible crop, before the decoder's
    // rotation — which is already folded into the quad's UVs (see
    // `rumo_media::video::yuv::corner_uvs`), so nothing here knows about it.
    size: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: YuvUniforms;
@group(0) @binding(1) var t_y: texture_2d<u32>;
@group(0) @binding(2) var t_c0: texture_2d<u32>;
@group(0) @binding(3) var t_c1: texture_2d<u32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_texture(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    var out: VsOut;
    out.pos = u.transform * vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    return out;
}

/// Clamp to `0..=255` and round half-up — the CPU `quantize`, line for line.
fn quantize(x: f32) -> f32 {
    return clamp(floor(x + 0.5), 0.0, 255.0);
}

@fragment
fn fs_yuv(in: VsOut) -> @location(0) vec4<f32> {
    // A decoded row carries stride padding, so the plane texture is wider than
    // the crop; the UVs are crop-relative and `floor` recovers the crop index.
    let px = i32(clamp(floor(in.uv.x * u.size.x), 0.0, u.size.x - 1.0));
    let py = i32(clamp(floor(in.uv.y * u.size.y), 0.0, u.size.y - 1.0));

    let luma = f32(textureLoad(t_y, vec2<i32>(px, py), 0).r);
    // Nearest 4:2:0 chroma, the same `>> 1` the CPU converter uses.
    let chroma_pos = vec2<i32>(px >> 1u, py >> 1u);
    var cb: f32;
    var cr: f32;
    if (u.luma.w < 0.5) {
        // I420: separate U and V planes.
        cb = f32(textureLoad(t_c0, chroma_pos, 0).r);
        cr = f32(textureLoad(t_c1, chroma_pos, 0).r);
    } else if (u.luma.w < 1.5) {
        // NV12: interleaved U then V, at the luma row stride, one byte per texel.
        cb = f32(textureLoad(t_c0, vec2<i32>(chroma_pos.x * 2, chroma_pos.y), 0).r);
        cr = f32(textureLoad(t_c0, vec2<i32>(chroma_pos.x * 2 + 1, chroma_pos.y), 0).r);
    } else {
        // NV21: interleaved V then U.
        cr = f32(textureLoad(t_c0, vec2<i32>(chroma_pos.x * 2, chroma_pos.y), 0).r);
        cb = f32(textureLoad(t_c0, vec2<i32>(chroma_pos.x * 2 + 1, chroma_pos.y), 0).r);
    }

    let y = u.luma.x * (luma - u.luma.y);
    let cb_off = cb - u.luma.z;
    let cr_off = cr - u.luma.z;
    let rgb = vec3<f32>(
        quantize(y + u.chroma.x * cr_off),
        quantize(y - u.chroma.y * cb_off - u.chroma.z * cr_off),
        quantize(y + u.chroma.w * cb_off),
    );
    // Divided back to 0..=1 for the RGBA8 target; the value is an exact integer,
    // so the target's own unorm rounding returns it unchanged.
    return vec4<f32>(rgb / 255.0, 1.0) * u.color;
}