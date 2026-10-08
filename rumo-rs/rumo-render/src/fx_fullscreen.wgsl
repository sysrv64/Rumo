// SPDX-License-Identifier: Apache-2.0
//
// Shared vertex stage for every effect pass.
//
// One oversized triangle covers the whole render target, so a pass needs no
// vertex or index buffer at all. `uv` is emitted with a top-left origin — uv
// (0,0) sits at NDC (-1, +1) — which means a fragment shader can feed `uv`
// straight into `textureSampleLevel(input_tex, input_smp, uv, 0.0)` and read the
// image the way a viewer sees it, with no flip anywhere in the chain.
//
// Effect modules deliberately declare no vertex stage of their own: keeping the
// one entry point here makes a `uv`-convention drift between the host and ten
// separate shader files impossible.

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    // vertex 0 -> (0,0), vertex 1 -> (2,0), vertex 2 -> (0,2)
    let x = f32((vertex_index << 1u) & 2u);
    let y = f32(vertex_index & 2u);

    var out: VsOut;
    // 0 -> -1, 2 -> 3: the triangle overhangs the clip box by design.
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}
