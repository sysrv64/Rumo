// SPDX-License-Identifier: Apache-2.0

//! A [`FrameSource`] that composites `rumo_render` layers.
//!
//! [`LayerFrameSource::render_frame`] uses the deterministic CPU
//! tessellation renderer, so it works on host builds and inside unit tests.
//! [`LayerFrameSource::render_frame_gpu`] renders through a caller-owned
//! async [`GpuRenderer`]; callers must run it on a worker thread, never on
//! the JNI thread (the pipeline never calls `block_on` for them).

use crate::frames::FrameSource;
use rumo_render::renderer::{GpuRenderer, MeshData, RenderConfig, blend_over, render_frame_cpu};
use std::time::Duration;

/// One placed render layer: geometry, color and its clip-space transform.
#[derive(Clone)]
pub struct Layer {
    pub mesh: MeshData,
    pub color: [f32; 4],
    pub transform: [[f32; 4]; 4],
}

/// Static layers over a solid background. Time-varying content is the
/// caller's job: swap `layers` (or feed [`crate::RgbaFrames`]) per frame.
pub struct LayerFrameSource {
    pub width: u32,
    pub height: u32,
    pub background: [f32; 4],
    pub layers: Vec<Layer>,
}

impl LayerFrameSource {
    pub fn new(width: u32, height: u32, background: [f32; 4], layers: Vec<Layer>) -> Self {
        Self {
            width,
            height,
            background,
            layers,
        }
    }

    fn composite_cpu(&self) -> Vec<u32> {
        let mut frame = vec![to_rgba(self.background); self.width as usize * self.height as usize];
        let cfg = RenderConfig {
            width: self.width,
            height: self.height,
            clear_color: [0.0, 0.0, 0.0, 0.0],
        };
        for layer in &self.layers {
            let px = render_frame_cpu(&layer.mesh, &cfg, &layer.transform, layer.color);
            blend_over(&mut frame, &px);
        }
        frame
    }

    /// Render every layer into `out_rgba` through `gpu`.
    pub async fn render_frame_gpu(
        &self,
        gpu: &GpuRenderer,
        t_seconds: f64,
        out_rgba: &mut [u8],
    ) -> std::result::Result<(), String> {
        let _ = t_seconds;
        let triples: Vec<(MeshData, [f32; 4], [[f32; 4]; 4])> = self
            .layers
            .iter()
            .map(|l| (l.mesh.clone(), l.color, l.transform))
            .collect();
        let px = gpu
            .render_layers(
                self.width,
                self.height,
                self.background,
                &triples,
                Duration::from_secs(10),
            )
            .await?;
        write_pixels(out_rgba, &px);
        Ok(())
    }
}

impl FrameSource for LayerFrameSource {
    fn render_frame(&self, _t_seconds: f64, out_rgba: &mut [u8]) {
        write_pixels(out_rgba, &self.composite_cpu());
    }
}

fn to_rgba(c: [f32; 4]) -> u32 {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    ch(c[0]) | (ch(c[1]) << 8) | (ch(c[2]) << 16) | (ch(c[3]) << 24)
}

fn write_pixels(out: &mut [u8], px: &[u32]) {
    for (chunk, p) in out.chunks_exact_mut(4).zip(px.iter()) {
        chunk.copy_from_slice(&p.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> [[f32; 4]; 4] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn triangle() -> MeshData {
        MeshData {
            vertices: vec![[0.0, 0.5], [-0.5, -0.5], [0.5, -0.5]],
            indices: vec![0, 1, 2],
        }
    }

    #[test]
    fn empty_layers_fill_background() {
        let src = LayerFrameSource::new(8, 8, [0.0, 0.0, 1.0, 1.0], vec![]);
        let mut out = vec![0u8; 8 * 8 * 4];
        src.render_frame(0.0, &mut out);
        for px in out.chunks_exact(4) {
            assert_eq!(px, [0, 0, 255, 255], "background must be pure blue");
        }
    }

    #[test]
    fn opaque_layer_paints_center_over_background() {
        let src = LayerFrameSource::new(
            16,
            16,
            [1.0, 0.0, 0.0, 1.0],
            vec![Layer {
                mesh: triangle(),
                color: [0.0, 1.0, 0.0, 1.0],
                transform: identity(),
            }],
        );
        let mut out = vec![0u8; 16 * 16 * 4];
        src.render_frame(0.0, &mut out);
        let center = (8 * 16 + 8) * 4;
        assert_eq!(&out[center..center + 4], &[0, 255, 0, 255]);
    }
}
