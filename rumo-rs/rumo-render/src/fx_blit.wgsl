// SPDX-License-Identifier: Apache-2.0
//
// Composite stage: draws an effect chain's final target onto the frame, using
// the layer's blend mode as fixed-function pipeline state.
//
// Keeping this separate from the effect passes means the chain's last pass does
// not have to care which surface it will end up on — the chain always writes a
// pooled `Rgba8Unorm` target in the engine's working format, and this pass
// re-blends it into whatever the destination is (offscreen target or swapchain
// texture, either format). The vertex stage is `fx_fullscreen.wgsl`.

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var input_smp: sampler;

@fragment
fn fs_blit(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    // Straight-alpha texel, passed through unchanged: the blend state applies
    // the layer's compositing rule at the fixed-function stage.
    return textureSampleLevel(input_tex, input_smp, uv, 0.0);
}
