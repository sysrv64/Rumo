// SPDX-License-Identifier: Apache-2.0
//
// The composited scene, packed into an NV12 (YUV 4:2:0 semi-planar) frame on the
// GPU, for the MP4 export.
//
// # Why a compute pass and not a fragment pass into a texture
//
// The obvious route is a fragment shader writing an `R8Unorm` luma texture and
// an `Rg8Unorm` chroma texture, then `copy_texture_to_buffer` for each plane.
// That route cannot produce NV12: a texture copy needs `bytes_per_row` to be a
// multiple of `COPY_BYTES_PER_ROW_ALIGNMENT` (256), and an NV12 frame has no row
// stride at all — its chroma plane starts at exactly `width * height`. A 1080p
// luma row is 1920 bytes, so every row would arrive padded and a CPU pass would
// have to strip it again: the readback this wave removes, put back by hand.
//
// A storage buffer has no row stride, so this shader writes the frame's bytes in
// place, in order, and the host maps exactly `nv12_len` bytes.
//
// # No colour constants
//
// Like `shader_yuv.wgsl`, this module contains **no** matrix: the BT.601
// coefficients and the chroma bias arrive in the uniform, filled from
// `rumo_export::nv12` — the module that also holds the CPU reference this shader
// is compared against (docs/12 §12.3). A wrong matrix here shows as a green or
// pink export and runs perfectly clean, so there is deliberately nowhere to put
// one.
//
// # Sampling
//
// `textureLoad` on the scene target, no sampler binding at all: the byte the
// engine rendered is the byte this reads, with no filter and no `value / 255 *
// 255` round trip that could land a code value low (the same reasoning as
// `shader_yuv.wgsl`). The target is straight RGBA8 with R in the low byte, which
// is what `.rgb` returns.

struct Nv12Uniforms {
    // BT.601 full-range luma coefficients.
    luma: vec4<f32>,
    // Chroma U coefficients, over the 2x2 block average.
    chroma_u: vec4<f32>,
    // Chroma V coefficients, over the 2x2 block average.
    chroma_v: vec4<f32>,
    // x = chroma bias (128), y = sample scale (255).
    range: vec4<f32>,
    // x = width, y = height. Both even: NV12 4:2:0 needs it, and the exporter
    // rejects the config otherwise.
    size: vec4<u32>,
};

@group(0) @binding(0) var<uniform> u: Nv12Uniforms;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<u32>;

/// The source pixel at `(x, y)` as its exact byte values, 0..=255.
///
/// `round` undoes the target's unorm encoding exactly: the engine stored an
/// integer byte, so `byte / 255 * 255` rounds back to that byte rather than
/// landing a fraction below it.
fn source_rgb(x: u32, y: u32) -> vec3<f32> {
    let texel = textureLoad(src, vec2<i32>(i32(x), i32(y)), 0);
    return round(texel.rgb * u.range.y);
}

/// Clamp to `0..=255` and round half away from zero — `f32::round`, the rule
/// the CPU reference's `clamp_u8` uses.
fn clamp_byte(v: f32) -> u32 {
    return u32(clamp(round(v), 0.0, u.range.y));
}

/// Luma of one pixel. The CPU reference keeps this expression in f32 on purpose
/// (`rumo_export::nv12`), so it is transcribed here rather than reformulated.
fn luma_byte(rgb: vec3<f32>) -> u32 {
    return clamp_byte(dot(u.luma.xyz, rgb));
}

/// Chroma U of one 2x2 block, over the block's four pixels.
fn chroma_u_byte(a: vec3<f32>, b: vec3<f32>, c: vec3<f32>, d: vec3<f32>) -> u32 {
    let avg = (a + b + c + d) * 0.25;
    return clamp_byte(dot(u.chroma_u.xyz, avg) + u.range.x);
}

/// Chroma V of one 2x2 block.
fn chroma_v_byte(a: vec3<f32>, b: vec3<f32>, c: vec3<f32>, d: vec3<f32>) -> u32 {
    let avg = (a + b + c + d) * 0.25;
    return clamp_byte(dot(u.chroma_v.xyz, avg) + u.range.x);
}

/// One byte of the NV12 frame, by its index in the frame.
///
/// The first `width * height` bytes are the luma plane, row-major; the rest is
/// the chroma plane, interleaved U then V, one pair per 2x2 block.
fn frame_byte(index: u32) -> u32 {
    let w = u.size.x;
    let y_len = w * u.size.y;
    if (index < y_len) {
        return luma_byte(source_rgb(index % w, index / w));
    }
    let chroma = index - y_len;
    let cw = w / 2u;
    // Both chroma components of a block need the same four pixels, so the pair
    // is addressed by its block and its position within the pair.
    let block = chroma / 2u;
    let cx = block % cw;
    let cy = block / cw;
    let x = cx * 2u;
    let y = cy * 2u;
    let p0 = source_rgb(x, y);
    let p1 = source_rgb(x + 1u, y);
    let p2 = source_rgb(x, y + 1u);
    let p3 = source_rgb(x + 1u, y + 1u);
    if (chroma % 2u == 0u) {
        return chroma_u_byte(p0, p1, p2, p3);
    }
    return chroma_v_byte(p0, p1, p2, p3);
}

/// One invocation per output word: four frame bytes, little-endian, which is the
/// order the host maps them in.
///
/// `width * height` is a multiple of four (both dimensions are even), so a word
/// never straddles the luma/chroma boundary; the last word of a frame may carry
/// padding, which is why the host hands the encoder exactly `nv12_len` bytes
/// rather than the whole buffer.
@compute @workgroup_size(64)
fn cs_nv12(@builtin(global_invocation_id) gid: vec3<u32>) {
    let word = gid.x;
    if (word >= arrayLength(&out)) {
        return;
    }
    let w = u.size.x;
    let y_len = w * u.size.y;
    let total = y_len + y_len / 2u;
    var packed: array<u32, 4>;
    for (var k = 0u; k < 4u; k = k + 1u) {
        let index = word * 4u + k;
        if (index < total) {
            packed[k] = frame_byte(index);
        } else {
            packed[k] = 0u;
        }
    }
    out[word] = packed[0] | (packed[1] << 8u) | (packed[2] << 16u) | (packed[3] << 24u);
}
