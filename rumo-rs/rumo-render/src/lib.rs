// SPDX-License-Identifier: Apache-2.0

pub mod composite;
pub mod diag;
pub mod effect;
pub mod engine;
pub mod fx;
pub mod gpu_worker;
pub mod jni;
pub mod m3shape;
pub mod nv12_pack;
pub mod params;
pub mod path;
pub mod renderer;
pub mod svg;
pub mod svg_legacy;
pub mod text;
pub mod texture;
mod tess;

pub use composite::{
    ATLAS_TEXTURE_ID, CpuLayerShape, CpuTexturedDraw, DrawWindow, FxCpuLayer, ShapeSpec,
    argb_to_f32, composite_preview, composite_preview_fx, composite_preview_gpu, f32_to_rgba_u32,
    image_mesh, image_quad, in_frame, ndc_matrix, preview_shape_triples, rgba8_to_rgba_u32,
    rgba_u32_to_argb, rgba_u32_to_rgba8, scale_text_mesh, shape_mesh_vertices, text_mesh, text_quad,
    window_allows, window_of,
};
pub use engine::{
    ENGINE_ERR_BAD_ARG, ENGINE_ERR_FRAME, ENGINE_ERR_NO_GPU, ENGINE_ERR_NO_SURFACE,
    ENGINE_ERR_TIMEOUT, ENGINE_OK, ENGINE_OP_TIMEOUT, Engine, EngineLayer, EngineScene,
};
pub use diag::{Level as DiagLevel, Report as DiagReport};
pub use gpu_worker::{
    FrameRequest, render as render_offscreen_gpu, render_nv12 as render_offscreen_gpu_nv12,
};
pub use nv12_pack::{Nv12Coeffs, nv12_len, nv12_word_count};
pub use effect::validate_custom;
pub use fx::{
    ALLOWED_BINDINGS, BlendMode, FX_FORMAT, FX_VERTEX_WGSL, FxFrame, FxRuntime, ModuleShape,
    POOL_BUDGET_BYTES, ParamsLayout, PooledTarget, TargetPool, shrunk, target_bytes,
    validate_module, validate_source,
};
pub use params::ShapeParams;
pub use path::shape_path;
pub use renderer::{
    GpuRenderer, LayerDraw, LayerShape, MeshData, RenderConfig, SceneDraw, TexturedQuad, blend_over,
    draw_textured_cpu, ordered_layer_draws, render_frame_cpu, render_frame_gpu, sample_nearest,
};
pub use tess::tessellate;
pub use text::{Atlas, AtlasRect, GlyphKey, GlyphQuad, TextEngine, TextLayout, TextStyle};
pub use texture::{
    GpuTextureCache, SceneTexture, TexVertex, TextureImage, TextureStamp, TexturedMesh,
    UploadOutcome,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShapeKind {
    Circle,
    Square,
    Slanted,
    Arch,
    Fan,
    Arrow,
    SemiCircle,
    Oval,
    Pill,
    Triangle,
    Diamond,
    ClamShell,
    Pentagon,
    Gem,
    Sunny,
    VerySunny,
    Cookie4Sided,
    Cookie6Sided,
    Cookie7Sided,
    Cookie9Sided,
    Cookie12Sided,
    Ghostish,
    Clover4Leaf,
    Clover8Leaf,
    Burst,
    SoftBurst,
    Boom,
    SoftBoom,
    Flower,
    Puffy,
    PuffyDiamond,
    PixelCircle,
    PixelTriangle,
    Bun,
    Heart,
    /// The frame itself, not a shape placed in it.
    ///
    /// Every other shape is tessellated in a 256 box and then scaled by
    /// 60% of the frame's height and moved to the layer's offset. This one is
    /// drawn to cover the whole target instead, because that is what a
    /// background is: `preview_shape_draws` special-cases it and ignores the
    /// placement fields. It exists as a shape at all so the background can be an
    /// ordinary layer — which is what gives it effects, opacity, keyframes and a
    /// row in the layer list without any of that being written twice.
    Frame,
}

pub fn all_shapes() -> [ShapeKind; 36] {
    use ShapeKind::*;
    [
        Circle,
        Square,
        Slanted,
        Arch,
        Fan,
        Arrow,
        SemiCircle,
        Oval,
        Pill,
        Triangle,
        Diamond,
        ClamShell,
        Pentagon,
        Gem,
        Sunny,
        VerySunny,
        Cookie4Sided,
        Cookie6Sided,
        Cookie7Sided,
        Cookie9Sided,
        Cookie12Sided,
        Ghostish,
        Clover4Leaf,
        Clover8Leaf,
        Burst,
        SoftBurst,
        Boom,
        SoftBoom,
        Flower,
        Puffy,
        PuffyDiamond,
        PixelCircle,
        PixelTriangle,
        Bun,
        Heart,
        // Appended, never inserted: the ordinal IS the identity that Kotlin
        // stores in a layer's name, so an insertion would renumber every shape
        // after it and repaint existing projects as different shapes.
        Frame,
    ]
}

pub fn is_pixel(kind: ShapeKind) -> bool {
    matches!(kind, ShapeKind::PixelCircle | ShapeKind::PixelTriangle)
}

pub fn morph_pair(a: ShapeKind, b: ShapeKind) -> Option<(ShapeKind, ShapeKind)> {
    use ShapeKind::*;
    match (a, b) {
        (Boom, SoftBoom) | (SoftBoom, Boom) => Some((a, b)),
        (Burst, SoftBurst) | (SoftBurst, Burst) => Some((a, b)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_shapes_count_is_36_and_unique() {
        let shapes = all_shapes();
        assert_eq!(shapes.len(), 36);
        let set: HashSet<ShapeKind> = shapes.into_iter().collect();
        assert_eq!(set.len(), 36);
    }

    #[test]
    fn pixel_flag_only_two() {
        let pixels: Vec<ShapeKind> = all_shapes().into_iter().filter(|s| is_pixel(*s)).collect();
        assert_eq!(pixels.len(), 2);
        assert!(pixels.contains(&ShapeKind::PixelCircle));
        assert!(pixels.contains(&ShapeKind::PixelTriangle));
    }

    #[test]
    fn morph_pairs_symmetric() {
        assert_eq!(
            morph_pair(ShapeKind::Boom, ShapeKind::SoftBoom),
            Some((ShapeKind::Boom, ShapeKind::SoftBoom))
        );
        assert_eq!(
            morph_pair(ShapeKind::SoftBoom, ShapeKind::Boom),
            Some((ShapeKind::SoftBoom, ShapeKind::Boom))
        );
        assert_eq!(
            morph_pair(ShapeKind::Burst, ShapeKind::SoftBurst),
            Some((ShapeKind::Burst, ShapeKind::SoftBurst))
        );
        assert_eq!(
            morph_pair(ShapeKind::SoftBurst, ShapeKind::Burst),
            Some((ShapeKind::SoftBurst, ShapeKind::Burst))
        );
        assert_eq!(morph_pair(ShapeKind::Circle, ShapeKind::Square), None);
        assert_eq!(morph_pair(ShapeKind::Boom, ShapeKind::Boom), None);
    }
}
