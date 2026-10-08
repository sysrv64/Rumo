// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use bytemuck::{Pod, Zeroable};
use rumo_core::effect::{CustomEffect, EffectInstance};
use wgpu::util::DeviceExt;

use crate::fx::BlendMode;
use crate::texture::{
    GpuTextureCache, TexVertex, TextureImage, TextureStamp, TexturedMesh, UploadOutcome,
    YuvTexture, YuvUniforms,
};

pub const SHADER_WGSL: &str = include_str!("shader.wgsl");
pub const SHADER_TEXTURE_WGSL: &str = include_str!("shader_texture.wgsl");
/// Decodes a frame's planes and converts YCbCr to RGBA8 in the fragment shader.
/// Holds no colour constants: the matrix arrives in the uniform from
/// `rumo_media::video::yuv::coeffs_f32`, the same numbers the CPU converter uses.
pub const SHADER_YUV_WGSL: &str = include_str!("shader_yuv.wgsl");

#[derive(Debug, Clone, Copy)]
pub struct RenderConfig {
    pub width: u32,
    pub height: u32,
    pub clear_color: [f32; 4],
}

#[derive(Debug, Clone)]
pub struct MeshData {
    pub vertices: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

/// One textured draw: a texture id from [`GpuRenderer::set_texture`] (an RGBA8
/// image) or [`GpuRenderer::set_yuv_texture`] (a decoded frame's planes), the
/// quad geometry, a tint/alpha color and a clip-space transform.
///
/// Which of the two the id holds decides the shader at draw time; nothing else
/// about the draw differs, so adding the plane path adds no entry to any draw
/// list and the effect chains keep the indices they had (docs/08 §8.13).
#[derive(Debug, Clone)]
pub struct TexturedQuad {
    pub mesh: TexturedMesh,
    pub color: [f32; 4],
    pub transform: [[f32; 4]; 4],
    pub texture_id: u64,
}

/// One textured quad's GPU-side resources, as
/// [`GpuRenderer::prepare_texture_quad`] assembles them. Kept as one value so
/// the three draw paths (surface, offscreen, effect-chain pass) share the
/// assembly and cannot drift apart on which payload a texture id means.
struct PreparedTexture {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
}

/// The geometry of one layer, borrowed from the caller's scene.
///
/// Exists so the effect-aware render paths can treat a solid mesh and a
/// textured quad uniformly: both can be drawn into their own render target and
/// then pushed through an effect chain.
#[derive(Debug, Clone, Copy)]
pub enum LayerShape<'a> {
    /// Solid-colour tessellated geometry.
    Mesh {
        /// Triangle geometry in pixel coordinates.
        mesh: &'a MeshData,
        /// Linear RGBA tint.
        color: [f32; 4],
        /// Pixel-space → clip-space transform.
        transform: [[f32; 4]; 4],
    },
    /// A textured draw referencing an uploaded RGBA8 texture.
    Textured(&'a TexturedQuad),
}

/// One layer to draw, with its effect chain and blend mode.
///
/// This is the effect-aware counterpart of the `(MeshData, color, transform)`
/// triples and bare [`TexturedQuad`] slices the older entry points take; those
/// remain for callers with no effects.
#[derive(Debug, Clone)]
pub struct LayerDraw<'a> {
    /// What to draw.
    pub shape: LayerShape<'a>,
    /// Effects applied to this layer only, in order. An empty chain keeps the
    /// direct draw path.
    pub effects: &'a [EffectInstance],
    /// How the result combines with the layers beneath it.
    pub blend: BlendMode,
    /// Timeline time in seconds, handed to effect shaders.
    pub time: f32,
}

impl<'a> LayerDraw<'a> {
    /// A layer with no effects, drawn source-over.
    pub fn plain(shape: LayerShape<'a>) -> Self {
        Self {
            shape,
            effects: &[],
            blend: BlendMode::Normal,
            time: 0.0,
        }
    }
    /// A mesh layer with no effects.
    pub fn mesh(mesh: &'a MeshData, color: [f32; 4], transform: [[f32; 4]; 4]) -> Self {
        Self::plain(LayerShape::Mesh {
            mesh,
            color,
            transform,
        })
    }

    /// A textured layer with no effects.
    pub fn textured(quad: &'a TexturedQuad) -> Self {
        Self::plain(LayerShape::Textured(quad))
    }
}

/// One draw of an extended scene, named by the group it belongs to and its
/// index inside that group.
///
/// An extended scene arrives as three separately-typed groups — meshes
/// (shapes/SVG), atlas text and images — but the layer list interleaves them,
/// so the group a draw belongs to says nothing about *when* it is drawn.
/// `SceneDraw` is one element of the scene's global draw order: it points at a
/// draw in the mesh list or in the textured list, and the render paths walk
/// this order instead of walking the groups. A mesh draw and a textured draw
/// are drawn by different pipelines, so the tag is also what tells the loop
/// which pipeline to bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneDraw {
    /// Index into the mesh/shape draw list.
    Mesh(usize),
    /// Index into the textured draw list (text entries first, images after).
    Textured(usize),
}

/// Build the effect-aware draw list of an extended scene in the scene's global
/// draw order.
///
/// `order` names each draw by group and index, so the returned list may
/// interleave meshes and textured quads. Chains stay addressed by the *group*
/// index of a draw (`shapes[i]` / `textures[i]`), which is why an interleaved
/// order cannot shift a chain onto somebody else's layer.
///
/// An empty `order` means the caller predates cross-group ordering: the group
/// order is used instead, every mesh draw followed by every textured draw —
/// exactly what this function replaced.
pub fn ordered_layer_draws<'a>(
    shapes: &'a [(MeshData, [f32; 4], [[f32; 4]; 4])],
    textured: &'a [TexturedQuad],
    order: &[SceneDraw],
    chains: &'a rumo_core::effect::EffectChains,
    time: f32,
) -> Vec<LayerDraw<'a>> {
    let mut draws = Vec::with_capacity(shapes.len() + textured.len());
    if order.is_empty() {
        for (index, (mesh, color, transform)) in shapes.iter().enumerate() {
            draws.push(LayerDraw {
                shape: LayerShape::Mesh {
                    mesh,
                    color: *color,
                    transform: *transform,
                },
                effects: chains.shape_chain(index),
                blend: BlendMode::Normal,
                time,
            });
        }
        for (index, quad) in textured.iter().enumerate() {
            draws.push(LayerDraw {
                shape: LayerShape::Textured(quad),
                effects: chains.texture_chain(index),
                blend: BlendMode::Normal,
                time,
            });
        }
        return draws;
    }
    for draw in order {
        match *draw {
            SceneDraw::Mesh(index) => {
                // `get`, not `[]`: a caller bug must cost an ordering mistake
                // at worst, never a panic and never a dropped frame.
                let Some((mesh, color, transform)) = shapes.get(index) else {
                    continue;
                };
                draws.push(LayerDraw {
                    shape: LayerShape::Mesh {
                        mesh,
                        color: *color,
                        transform: *transform,
                    },
                    effects: chains.shape_chain(index),
                    blend: BlendMode::Normal,
                    time,
                });
            }
            SceneDraw::Textured(index) => {
                let Some(quad) = textured.get(index) else {
                    continue;
                };
                draws.push(LayerDraw {
                    shape: LayerShape::Textured(quad),
                    effects: chains.texture_chain(index),
                    blend: BlendMode::Normal,
                    time,
                });
            }
        }
    }
    draws
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    transform: [[f32; 4]; 4],
    color: [f32; 4],
}

fn transform_point(m: &[[f32; 4]; 4], p: [f32; 2]) -> [f32; 3] {
    let x = m[0][0] * p[0] + m[1][0] * p[1] + m[3][0];
    let y = m[0][1] * p[0] + m[1][1] * p[1] + m[3][1];
    let w = m[0][3] * p[0] + m[1][3] * p[1] + m[3][3];
    [x, y, w]
}

fn to_rgba(c: [f32; 4]) -> u32 {
    let r = (c[0].clamp(0.0, 1.0) * 255.0).round() as u32;
    let g = (c[1].clamp(0.0, 1.0) * 255.0).round() as u32;
    let b = (c[2].clamp(0.0, 1.0) * 255.0).round() as u32;
    let a = (c[3].clamp(0.0, 1.0) * 255.0).round() as u32;
    r | (g << 8) | (b << 16) | (a << 24)
}

pub fn render_frame_cpu(
    mesh: &MeshData,
    config: &RenderConfig,
    transform: &[[f32; 4]; 4],
    color: [f32; 4],
) -> Vec<u32> {
    let mut pixels = vec![to_rgba(config.clear_color); (config.width * config.height) as usize];

    let _half_w = config.width as f32 / 2.0;
    let _half_h = config.height as f32 / 2.0;

    let ndc: Vec<[f32; 3]> = mesh
        .vertices
        .iter()
        .map(|p| {
            let [x, y, w] = transform_point(transform, *p);
            if w.abs() < f32::EPSILON {
                [x, y, w]
            } else {
                [x / w, y / w, w]
            }
        })
        .collect();

    for tri in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        if i0 >= ndc.len() || i1 >= ndc.len() || i2 >= ndc.len() {
            continue;
        }
        let (p0, p1, p2) = (ndc[i0], ndc[i1], ndc[i2]);

        let clip = |p: [f32; 3]| -> bool {
            p[2].abs() < f32::EPSILON || p[0].abs() > p[2] || p[1].abs() > p[2]
        };
        if clip(p0) || clip(p1) || clip(p2) {
            continue;
        }

        let s0 = [
            (p0[0] * 0.5 + 0.5) * config.width as f32,
            (0.5 - p0[1] * 0.5) * config.height as f32,
        ];
        let s1 = [
            (p1[0] * 0.5 + 0.5) * config.width as f32,
            (0.5 - p1[1] * 0.5) * config.height as f32,
        ];
        let s2 = [
            (p2[0] * 0.5 + 0.5) * config.width as f32,
            (0.5 - p2[1] * 0.5) * config.height as f32,
        ];

        let min_x = s0[0].min(s1[0]).min(s2[0]).max(0.0) as i64;
        let max_x = s0[0].max(s1[0]).max(s2[0]).min(config.width as f32 - 1.0) as i64;
        let min_y = s0[1].min(s1[1]).min(s2[1]).max(0.0) as i64;
        let max_y = s0[1].max(s1[1]).max(s2[1]).min(config.height as f32 - 1.0) as i64;

        let area = (s1[0] - s0[0]) * (s2[1] - s0[1]) - (s2[0] - s0[0]) * (s1[1] - s0[1]);
        if area.abs() < f32::EPSILON {
            continue;
        }

        let inv_area = 1.0 / area;
        let fill = to_rgba(color);

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let w0 = ((s1[0] - px) * (s2[1] - py) - (s2[0] - px) * (s1[1] - py)) * inv_area;
                let w1 = ((s2[0] - px) * (s0[1] - py) - (s0[0] - px) * (s2[1] - py)) * inv_area;
                let w2 = 1.0 - w0 - w1;
                if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                    pixels[(y as u32 * config.width + x as u32) as usize] = fill;
                }
            }
        }
    }

    pixels
}

/// Composite one straight-alpha src-over pixel (`u32` LE RGBA) onto `dst`.
fn blend_pixel(dst: &mut u32, src: u32) {
    let sa = ((src >> 24) & 0xFF) as f32 / 255.0;
    if sa <= 0.0 {
        return;
    }
    if sa >= 1.0 {
        *dst = src;
        return;
    }
    let d = *dst;
    let da = ((d >> 24) & 0xFF) as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    let ch = |sv: u32, dv: u32| {
        (((sv as f32) * sa + (dv as f32) * da * (1.0 - sa)) / out_a).round() as u32
    };
    let r = ch(src & 0xFF, d & 0xFF);
    let g = ch((src >> 8) & 0xFF, (d >> 8) & 0xFF);
    let b = ch((src >> 16) & 0xFF, (d >> 16) & 0xFF);
    let a = (out_a * 255.0).round() as u32;
    *dst = r | (g << 8) | (b << 16) | (a << 24);
}

/// Composite `src` over `dst` in place (straight-alpha src-over, both
/// `u32` LE RGBA). Lengths must match; extra source pixels are ignored.
pub fn blend_over(dst: &mut [u32], src: &[u32]) {
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        blend_pixel(d, *s);
    }
}

/// Straight-alpha RGBA8 texel of `image` at normalized `uv` (nearest
/// neighbour, clamped to the edge), as linear f32. Returns transparent
/// black for an empty image.
pub fn sample_nearest(image: &TextureImage, uv: [f32; 2]) -> [f32; 4] {
    if image.width == 0 || image.height == 0 {
        return [0.0; 4];
    }
    let w = image.width as f32;
    let h = image.height as f32;
    let x = ((uv[0].clamp(0.0, 1.0) * w).floor() as u32).min(image.width - 1);
    let y = ((uv[1].clamp(0.0, 1.0) * h).floor() as u32).min(image.height - 1);
    let off = ((y * image.width + x) * 4) as usize;
    let px = &image.rgba[off..off + 4];
    [
        px[0] as f32 / 255.0,
        px[1] as f32 / 255.0,
        px[2] as f32 / 255.0,
        px[3] as f32 / 255.0,
    ]
}

/// Draw a pixel-space textured `mesh` with `image` tinted by `tint` over
/// `frame` (straight-alpha src-over, nearest sampling).
///
/// Mirrors the GPU texture shader (`texel * color`, `ALPHA_BLENDING`): the
/// source pixel is `(texel.rgb * tint.rgb, texel.a * tint.a)`. Empty meshes,
/// empty images and length-mismatched frames draw nothing.
pub fn draw_textured_cpu(
    frame: &mut [u32],
    width: u32,
    height: u32,
    image: &TextureImage,
    mesh: &TexturedMesh,
    tint: [f32; 4],
) {
    if width == 0 || height == 0 {
        return;
    }
    if frame.len() != width as usize * height as usize {
        return;
    }
    if mesh.is_empty() || image.width == 0 || image.height == 0 {
        return;
    }
    let tint_a = tint[3].clamp(0.0, 1.0);
    if tint_a <= 0.0 {
        return;
    }
    for tri in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        if i0 >= mesh.vertices.len() || i1 >= mesh.vertices.len() || i2 >= mesh.vertices.len() {
            continue;
        }
        let (v0, v1, v2) = (mesh.vertices[i0], mesh.vertices[i1], mesh.vertices[i2]);
        let (p0, p1, p2) = (v0.position, v1.position, v2.position);

        let min_x = p0[0].min(p1[0]).min(p2[0]).max(0.0) as i64;
        let max_x = p0[0].max(p1[0]).max(p2[0]).min(width as f32 - 1.0) as i64;
        let min_y = p0[1].min(p1[1]).min(p2[1]).max(0.0) as i64;
        let max_y = p0[1].max(p1[1]).max(p2[1]).min(height as f32 - 1.0) as i64;
        if max_x < min_x || max_y < min_y {
            continue;
        }

        let area = (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p2[0] - p0[0]) * (p1[1] - p0[1]);
        if area.abs() < f32::EPSILON {
            continue;
        }
        let inv_area = 1.0 / area;

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let w0 = ((p1[0] - px) * (p2[1] - py) - (p2[0] - px) * (p1[1] - py)) * inv_area;
                let w1 = ((p2[0] - px) * (p0[1] - py) - (p0[0] - px) * (p2[1] - py)) * inv_area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let uv = [
                    w0 * v0.uv[0] + w1 * v1.uv[0] + w2 * v2.uv[0],
                    w0 * v0.uv[1] + w1 * v1.uv[1] + w2 * v2.uv[1],
                ];
                let t = sample_nearest(image, uv);
                let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
                let src = ch(t[0] * tint[0])
                    | (ch(t[1] * tint[1]) << 8)
                    | (ch(t[2] * tint[2]) << 16)
                    | (ch(t[3] * tint_a) << 24);
                let idx = (y as u32 * width + x as u32) as usize;
                blend_pixel(&mut frame[idx], src);
            }
        }
    }
}

/// Persistent GPU context: adapter + device + compiled pipelines, built
/// once and reused for every preview frame. Hardware adapter first,
/// software fallback second; both can fail (no driver), hence `Result`.
///
/// The creating [`wgpu::Instance`] and [`wgpu::Adapter`] are retained so a
/// presentation [`wgpu::Surface`] can be created from the *same* instance
/// (Vulkan surfaces are instance-scoped) and queried for capabilities.
/// Format-specific mesh pipelines for surface presentation are cached in
/// `surface_pipes` ([`GpuRenderer::mesh_pipeline_for`]).
pub struct GpuRenderer {
    // Only read by `create_android_surface`; host builds keep it for
    // construction symmetry.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    tex_pipeline: wgpu::RenderPipeline,
    tex_bind_group_layout: wgpu::BindGroupLayout,
    /// Decoded-frame pipeline: same geometry as the texture pipeline, but the
    /// fragment stage converts the frame's planes instead of sampling an image.
    yuv_pipeline: wgpu::RenderPipeline,
    yuv_bind_group_layout: wgpu::BindGroupLayout,
    yuv_shader: wgpu::ShaderModule,
    sampler: wgpu::Sampler,
    textures: Mutex<GpuTextureCache>,
    /// Per-`(swapchain format, blend mode)` mesh pipelines.
    surface_pipes: Mutex<HashMap<(wgpu::TextureFormat, BlendMode), wgpu::RenderPipeline>>,
    /// Per-`(swapchain format, blend mode)` textured pipelines.
    surface_tex_pipes: Mutex<HashMap<(wgpu::TextureFormat, BlendMode), wgpu::RenderPipeline>>,
    /// Per-`(swapchain format, blend mode)` decoded-frame pipelines.
    surface_yuv_pipes: Mutex<HashMap<(wgpu::TextureFormat, BlendMode), wgpu::RenderPipeline>>,
    /// Effect pipelines, WGSL modules and the pooled effect render targets.
    fx: Mutex<crate::fx::FxRuntime>,
    /// The NV12 pack pass, compiled on first export frame. `None` until then:
    /// the preview never pays for a shader only the encoder needs.
    nv12: Mutex<Option<crate::nv12_pack::Nv12Pack>>,
}

async fn pick_adapter(instance: &wgpu::Instance, fallback: bool) -> Result<wgpu::Adapter, String> {
    instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: fallback,
            compatible_surface: None,
            apply_limit_buckets: false,
        })
        .await
        .map_err(|e| format!("adapter request failed (fallback={fallback}): {e:?}"))
}

/// Human-readable backend name for the diagnostics report.
fn backend_name(backend: wgpu::Backend) -> &'static str {
    match backend {
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Gl => "OpenGL",
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Dx12 => "Direct3D 12",
        _ => "unknown",
    }
}

fn uniform_buffer(device: &wgpu::Device, uniforms: &Uniforms) -> wgpu::Buffer {    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("uniforms"),
        contents: bytemuck::cast_slice(std::slice::from_ref(uniforms)),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

/// The YUV uniform entry: readable by both stages, because the vertex stage
/// transforms the quad with it.
fn yuv_uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// One plane of a decoded frame.
///
/// `Uint`: the shader reads the decoded byte itself, so a code value cannot come
/// back a step low from a `value / 255 * 255` round trip. It also means the
/// plane is not filterable — which is the second half of why this bind group
/// carries no sampler at all.
fn yuv_plane_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Uint,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// Solid-color mesh pipeline for `format`: the same shader/layout the
/// offscreen path uses, only the color target differs. Shared by
/// [`GpuRenderer::new`] (Rgba8Unorm) and surface presentation
/// ([`GpuRenderer::mesh_pipeline_for`], swapchain format).
fn mesh_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    blend: BlendMode,
    label: &str,
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline layout"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 8,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                }],
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // Straight-alpha src-over so stacked layer passes match
                // the CPU compositor over an opaque background.
                blend: Some(blend.state()),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Textured-quad pipeline for `format`: the same shader/layout the offscreen
/// textured path uses, only the color target differs. Shared by
/// [`GpuRenderer::new`] (Rgba8Unorm) and surface presentation
/// ([`GpuRenderer::tex_pipeline_for`], swapchain format).
///
/// `fragment_entry` is the only difference between the two textured shaders,
/// so the whole pipeline is built once rather than written twice: `fs_texture`
/// samples an RGBA8 image, `fs_yuv` converts a decoded frame's planes.
fn tex_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    blend: BlendMode,
    label: &str,
    fragment_entry: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_texture"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<TexVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 8,
                        shader_location: 1,
                    },
                ],
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend.state()),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

impl GpuRenderer {
    pub async fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::default();
        let adapter = match pick_adapter(&instance, false).await {
            Ok(a) => a,
            Err(hw_err) => {
                // Recording the software-fallback reason matters: a device that
                // only offers the fallback adapter renders on the CPU anyway,
                // and the user needs to know that is what happened.
                crate::diag::warn(
                    "adapter_fallback",
                    format!("no hardware adapter ({hw_err}); trying the fallback adapter"),
                );
                pick_adapter(&instance, true)
                    .await
                    .map_err(|sw_err| format!("{hw_err}; {sw_err}"))?
            }
        };
        let info = adapter.get_info();
        crate::diag::set_adapter(&info.name, backend_name(info.backend));

        // `downlevel_defaults()` caps `max_texture_dimension_2d` at 2048, which
        // silently makes 1440p/4K offscreen rendering (and any effect target at
        // those sizes) impossible. Keep the conservative downlevel baseline for
        // everything else, but take the texture-dimension limits from the
        // adapter so 3840x2160 canvases can actually be allocated.
        let required_limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("rumo-render device"),
                required_features: wgpu::Features::empty(),
                required_limits,
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| format!("device request failed: {e:?}"))?;

        // wgpu's default reaction to an uncaptured validation error is to panic.
        // On a device that rejects one of our shaders or a surface format that
        // would kill the worker thread that owns the GPU, and the app would sit
        // on the CPU path forever with no explanation. Recording the error
        // instead keeps the GPU path alive and makes the reason visible in the
        // diagnostics window.
        device.on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| {
            crate::diag::note_driver_error(format!("{error}"));
        }));

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_WGSL.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let pipeline = mesh_pipeline(
            &device,
            &bind_group_layout,
            &shader,
            wgpu::TextureFormat::Rgba8Unorm,
            BlendMode::Normal,
            "pipeline",
        );

        let tex_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("texture shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_TEXTURE_WGSL.into()),
        });

        let tex_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("texture bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let tex_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("texture pipeline layout"),
            bind_group_layouts: &[Some(&tex_bind_group_layout)],
            immediate_size: 0,
        });

        let tex_pipeline_res = tex_pipeline(
            &device,
            &tex_pipeline_layout,
            &tex_shader,
            wgpu::TextureFormat::Rgba8Unorm,
            BlendMode::Normal,
            "texture pipeline",
            "fs_texture",
        );

        // A decoded frame's planes: one uniform, three integer plane textures,
        // and no sampler at all — `textureLoad` is the only sampling this shader
        // can do, which is what keeps its nearest 4:2:0 chroma identical to the
        // CPU converter's.
        let yuv_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuv shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_YUV_WGSL.into()),
        });

        let yuv_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("yuv bind group layout"),
                entries: &[
                    yuv_uniform_entry(0),
                    yuv_plane_entry(1),
                    yuv_plane_entry(2),
                    yuv_plane_entry(3),
                ],
            });

        let yuv_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuv pipeline layout"),
            bind_group_layouts: &[Some(&yuv_bind_group_layout)],
            immediate_size: 0,
        });

        let yuv_pipeline = tex_pipeline(
            &device,
            &yuv_pipeline_layout,
            &yuv_shader,
            wgpu::TextureFormat::Rgba8Unorm,
            BlendMode::Normal,
            "yuv pipeline",
            "fs_yuv",
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("texture sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Effect pipelines are built here, once, so a malformed module is
        // discovered at startup and simply skipped at draw time instead of
        // panicking mid-frame.
        let fx = crate::fx::FxRuntime::new(&device);

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            pipeline,
            bind_group_layout,
            // The local is renamed because `let tex_pipeline = tex_pipeline(..)`
            // would shadow the builder for the rest of the function; the field
            // keeps its name, so the mapping is spelled out.
            tex_pipeline: tex_pipeline_res,
            tex_bind_group_layout,
            yuv_pipeline,
            yuv_bind_group_layout,
            yuv_shader,
            sampler,
            textures: Mutex::new(GpuTextureCache::new()),
            surface_pipes: Mutex::new(HashMap::new()),
            surface_tex_pipes: Mutex::new(HashMap::new()),
            surface_yuv_pipes: Mutex::new(HashMap::new()),
            fx: Mutex::new(fx),
            nv12: Mutex::new(None),
        })
    }

    /// Upload `image` under a fresh renderer-local id (RGBA8, straight
    /// alpha). Returns that id.
    pub fn upload_texture(&self, image: &TextureImage) -> Result<u64, String> {
        let mut cache = self
            .textures
            .lock()
            .map_err(|e| format!("texture cache poisoned: {e}"))?;
        cache.insert(&self.device, &self.queue, image)
    }

    /// Upload `image` under an explicit `id`, so the renderer's cache matches
    /// the JNI texture registry's handle, and report what that cost.
    ///
    /// `stamp` is the identity of `image`'s pixels (see [`TextureStamp`]):
    /// when the slot already holds that exact content nothing is transferred
    /// and [`UploadOutcome::Resident`] comes back. Still images therefore cost
    /// one upload, while a per-frame source — video, or the glyph atlas after
    /// an edit — hands over a new stamp and is uploaded again.
    pub fn set_texture(
        &self,
        id: u64,
        stamp: TextureStamp,
        image: &TextureImage,
    ) -> Result<UploadOutcome, String> {
        let mut cache = self
            .textures
            .lock()
            .map_err(|e| format!("texture cache poisoned: {e}"))?;
        cache.upsert(&self.device, &self.queue, id, stamp, image)
    }

    /// Build everything one textured quad needs to be drawn: its vertex and
    /// index buffers, its bind group, and the pipeline that goes with the
    /// payload `quad.texture_id` turned out to hold.
    ///
    /// The texture id is the only thing that decides the path — an id holding a
    /// decoded frame's planes gets the YCbCr→RGBA8 fragment shader, an id
    /// holding an image gets the ordinary sampler — so the draw list, the
    /// per-layer effect chains and every index into them stay exactly as they
    /// were (docs/08 §8.13).
    ///
    /// `Ok(None)` means the draw has to be skipped: an empty mesh, or a texture
    /// id the cache does not hold.
    fn prepare_texture_quad(
        &self,
        cache: &mut GpuTextureCache,
        quad: &TexturedQuad,
        format: wgpu::TextureFormat,
        blend: BlendMode,
        vertex_label: &'static str,
    ) -> Result<Option<PreparedTexture>, String> {
        if quad.mesh.is_empty() {
            return Ok(None);
        }
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(vertex_label),
                contents: bytemuck::cast_slice(&quad.mesh.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("textured indices"),
                contents: bytemuck::cast_slice(&quad.mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        // A decoded frame's planes are staged by whoever decoded it — a JNI
        // thread has no device — and claimed here, on the thread that owns the
        // device, the first time a draw references its id. The stamp is the id,
        // exactly as on the RGBA8 path: a fresh id per frame re-uploads, an id
        // that comes back after eviction is uploaded again, and nothing about
        // video needed a special case to get here.
        if cache.yuv_bind(quad.texture_id).is_none() {
            if let Some(frame) = crate::jni::yuv_frame(quad.texture_id) {
                cache.upsert_yuv(
                    &self.device,
                    &self.queue,
                    quad.texture_id,
                    TextureStamp::for_content(quad.texture_id),
                    &frame,
                )?;
            }
        }
        let (pipeline, bind_group) = if let Some(bind) = cache.yuv_bind(quad.texture_id) {
            let uniforms = YuvUniforms::for_draw(bind.slot, quad.transform, quad.color);
            let uniform_buffer =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("yuv uniforms"),
                        contents: bytemuck::cast_slice(std::slice::from_ref(&uniforms)),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuv bind group"),
                layout: &self.yuv_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&bind.views.planes[0]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&bind.views.planes[1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&bind.views.planes[2]),
                    },
                ],
            });
            (self.yuv_pipeline_for(format, blend), bind_group)
        } else if let Some(texture_view) = cache.view(quad.texture_id) {
            let uniforms = Uniforms {
                transform: quad.transform,
                color: quad.color,
            };
            let uniform_buffer = uniform_buffer(&self.device, &uniforms);
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("texture bind group"),
                layout: &self.tex_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            (self.tex_pipeline_for(format, blend), bind_group)
        } else {
            return Ok(None);
        };
        Ok(Some(PreparedTexture {
            pipeline,
            bind_group,
            vertex_buffer,
            index_buffer,
        }))
    }

    /// Drop the GPU texture for `id`; `true` when one was present.
    pub fn remove_texture(&self, id: u64) -> bool {
        let mut cache = self
            .textures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.remove(id)
    }

    /// Number of GPU textures currently cached.
    pub fn texture_count(&self) -> usize {
        let cache = self
            .textures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.len()
    }

    /// Capabilities of `surface` on this renderer's adapter.
    pub fn surface_capabilities(
        &self,
        surface: &wgpu::Surface<'_>,
    ) -> wgpu::SurfaceCapabilities {
        surface.get_capabilities(&self.adapter)
    }

    /// Configure `surface` for presentation with `config`.
    pub fn configure_surface(
        &self,
        surface: &wgpu::Surface<'_>,
        config: &wgpu::SurfaceConfiguration,
    ) {
        surface.configure(&self.device, config);
    }

    /// Mesh pipeline for a swapchain `format`/`blend`, cached per pair:
    /// swapchain textures (typically `Bgra8UnormSrgb`) need their own color
    /// target, the offscreen `Rgba8Unorm` pipeline cannot render into them.
    fn mesh_pipeline_for(
        &self,
        format: wgpu::TextureFormat,
        blend: BlendMode,
    ) -> wgpu::RenderPipeline {
        let key = (format, blend);
        if let Some(pipe) = self
            .surface_pipes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return pipe.clone();
        }
        let shader = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_WGSL.into()),
        });
        let pipe = mesh_pipeline(
            &self.device,
            &self.bind_group_layout,
            &shader,
            format,
            blend,
            "surface pipeline",
        );
        self.surface_pipes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, pipe.clone());
        pipe
    }

    /// Create a presentation surface for an Android `ANativeWindow`.
    ///
    /// # Safety
    ///
    /// `window` must be a valid `ANativeWindow` pointer that stays alive
    /// until after the returned surface is dropped; the caller (the render
    /// engine) holds the owning `NativeWindow` next to the surface.
    /// The surface is created from this renderer's own instance, so the
    /// adapter/device pair can present to it.
    #[cfg(target_os = "android")]
    pub unsafe fn create_android_surface(
        &self,
        window: *mut std::ffi::c_void,
    ) -> Result<wgpu::Surface<'static>, String> {
        use raw_window_handle::{AndroidDisplayHandle, AndroidNdkWindowHandle};
        let ptr = std::ptr::NonNull::new(window).ok_or("null ANativeWindow")?;
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(AndroidDisplayHandle::new().into()),
            raw_window_handle: AndroidNdkWindowHandle::new(ptr).into(),
        };
        // SAFETY: the caller guarantees `window` is a live ANativeWindow
        // kept alive past the surface; the raw handles are plain pointers.
        let surface = unsafe { self.instance.create_surface_unsafe(target) }
            .map_err(|e| format!("create surface failed: {e:?}"))?;
        // The lifetime only tracks the Rust borrow of a window object; raw
        // handles hold no borrow, and the engine pins the NativeWindow next
        // to the surface with matching drop order (surface first).
        Ok(unsafe { std::mem::transmute::<wgpu::Surface<'_>, wgpu::Surface<'static>>(surface) })
    }

    /// Textured-quad pipeline for a swapchain `format`/`blend`, cached per
    /// pair. Same need as [`GpuRenderer::mesh_pipeline_for`]: the offscreen
    /// `Rgba8Unorm` texture pipeline cannot render into swapchain textures.
    fn tex_pipeline_for(
        &self,
        format: wgpu::TextureFormat,
        blend: BlendMode,
    ) -> wgpu::RenderPipeline {
        let key = (format, blend);
        // The offscreen target format was built in `new`; reuse it rather than
        // compiling the same pipeline a second time under the cache's key.
        if key == (wgpu::TextureFormat::Rgba8Unorm, BlendMode::Normal) {
            return self.tex_pipeline.clone();
        }
        if let Some(pipe) = self
            .surface_tex_pipes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return pipe.clone();
        }
        let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("surface texture pipeline layout"),
            bind_group_layouts: &[Some(&self.tex_bind_group_layout)],
            immediate_size: 0,
        });
        let shader = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("surface texture shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_TEXTURE_WGSL.into()),
        });
        let pipe = tex_pipeline(
            &self.device,
            &layout,
            &shader,
            format,
            blend,
            "surface texture pipeline",
            "fs_texture",
        );
        self.surface_tex_pipes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, pipe.clone());
        pipe
    }

    /// Decoded-frame pipeline for a swapchain `format`/`blend`, cached per pair,
    /// exactly as [`Self::tex_pipeline_for`] caches the textured one.
    fn yuv_pipeline_for(
        &self,
        format: wgpu::TextureFormat,
        blend: BlendMode,
    ) -> wgpu::RenderPipeline {
        let key = (format, blend);
        if key == (wgpu::TextureFormat::Rgba8Unorm, BlendMode::Normal) {
            return self.yuv_pipeline.clone();
        }
        if let Some(pipe) = self
            .surface_yuv_pipes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return pipe.clone();
        }
        let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("surface yuv pipeline layout"),
            bind_group_layouts: &[Some(&self.yuv_bind_group_layout)],
            immediate_size: 0,
        });
        let pipe = tex_pipeline(
            &self.device,
            &layout,
            &self.yuv_shader,
            format,
            blend,
            "surface yuv pipeline",
            "fs_yuv",
        );
        self.surface_yuv_pipes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, pipe.clone());
        pipe
    }

    /// Upload a decoded frame's planes under an explicit `id`, so the renderer's
    /// cache matches the JNI texture registry's handle, and report what that
    /// cost.
    ///
    /// The residency rule is [`GpuRenderer::set_texture`]'s, unchanged: a fresh
    /// [`TextureStamp::for_content`] stamp per frame — which is exactly what a
    /// video frame has always handed over — means every new frame is uploaded,
    /// and dropping the slot drops the plane textures together with its
    /// residency record. The difference is only in *what* crosses the bus: the
    /// decoder's planes, once, instead of a freshly converted RGBA8 image.
    pub fn set_yuv_texture(
        &self,
        id: u64,
        stamp: TextureStamp,
        texture: &YuvTexture,
    ) -> Result<UploadOutcome, String> {
        let mut cache = self
            .textures
            .lock()
            .map_err(|e| format!("texture cache poisoned: {e}"))?;
        cache.upsert_yuv(&self.device, &self.queue, id, stamp, texture)
    }

    /// Render mesh `layers` over `bg` into the current swapchain texture of
    /// an already-configured `surface` and present it. Synchronous: no
    /// readback, so no timeout is needed (the engine bounds the whole call
    /// from the JNI side instead).
    ///
    /// Size and pipeline format come from the surface's own configuration;
    /// `Err` on any acquire/configure mismatch — the caller reconfigures or
    /// falls back, never panics.
    pub fn render_to_surface(
        &self,
        surface: &wgpu::Surface<'_>,
        bg: [f32; 4],
        layers: &[(MeshData, [f32; 4], [[f32; 4]; 4])],
    ) -> Result<(), String> {
        self.render_to_surface_ex(surface, bg, layers, &[])
    }

    /// Render mesh `layers` followed by textured `quads` (atlas glyphs,
    /// photos) into the current swapchain texture and present it. The first
    /// draw clears with `bg`, the rest load; an empty scene still fills with
    /// `bg`. Quads whose texture id is unknown (or whose mesh is empty) are
    /// skipped. Same acquire/configure error contract as
    /// [`GpuRenderer::render_to_surface`].
    pub fn render_to_surface_ex(
        &self,
        surface: &wgpu::Surface<'_>,
        bg: [f32; 4],
        layers: &[(MeshData, [f32; 4], [[f32; 4]; 4])],
        textured: &[TexturedQuad],
    ) -> Result<(), String> {
        let config = surface
            .get_configuration()
            .ok_or("surface is not configured")?;
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout => return Err("surface acquire timed out".into()),
            wgpu::CurrentSurfaceTexture::Occluded => return Err("surface occluded".into()),
            wgpu::CurrentSurfaceTexture::Outdated => return Err("surface outdated".into()),
            wgpu::CurrentSurfaceTexture::Lost => return Err("surface lost".into()),
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("surface validation error".into());
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let pipeline = self.mesh_pipeline_for(config.format, BlendMode::Normal);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("surface encoder"),
            });
        let mut first_pass = true;
        for (mesh, color, transform) in layers {
            let uniforms = Uniforms {
                transform: *transform,
                color: *color,
            };
            let uniform_buffer = uniform_buffer(&self.device, &uniforms);
            let vertex_data: Vec<f32> =
                mesh.vertices.iter().flat_map(|v| [v[0], v[1]]).collect();
            let vertex_buffer =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("surface vertices"),
                        contents: bytemuck::cast_slice(&vertex_data),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
            let index_buffer =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("surface indices"),
                        contents: bytemuck::cast_slice(&mesh.indices),
                        usage: wgpu::BufferUsages::INDEX,
                    });
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("surface bind group"),
                layout: &self.bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                }],
            });
            let load = if first_pass {
                clear_op(bg)
            } else {
                wgpu::LoadOp::Load
            };
            {
                let mut pass = begin_pass(&mut encoder, &view, load);
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.indices.len() as u32, 0, 0..1);
            }
            first_pass = false;
        }
        {
            let mut cache = self
                .textures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for quad in textured {
                let Some(prepared) = self.prepare_texture_quad(
                    &mut cache,
                    quad,
                    config.format,
                    BlendMode::Normal,
                    "surface textured vertices",
                )?
                else {
                    continue;
                };
                let load = if first_pass {
                    clear_op(bg)
                } else {
                    wgpu::LoadOp::Load
                };
                {
                    let mut pass = begin_pass(&mut encoder, &view, load);
                    pass.set_pipeline(&prepared.pipeline);
                    pass.set_bind_group(0, &prepared.bind_group, &[]);
                    pass.set_vertex_buffer(0, prepared.vertex_buffer.slice(..));
                    pass.set_index_buffer(
                        prepared.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    pass.draw_indexed(0..quad.mesh.indices.len() as u32, 0, 0..1);
                }
                first_pass = false;
            }
        }
        if first_pass {
            let _pass = begin_pass(&mut encoder, &view, clear_op(bg));
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        Ok(())
    }

    /// Render `layers` — each a (mesh, color, transform) triple — into one
    /// frame: the first layer clears with `bg`, the rest load. One submit.
    /// `timeout` bounds the readback wait; expiry is an `Err`, never a hang.
    pub async fn render_layers(
        &self,
        width: u32,
        height: u32,
        bg: [f32; 4],
        layers: &[(MeshData, [f32; 4], [[f32; 4]; 4])],
        timeout: std::time::Duration,
    ) -> Result<Vec<u32>, String> {
        self.render_scene(width, height, bg, layers, &[], timeout)
            .await
    }

    /// Render `textured` quads (images/atlas glyphs) alone. Shorthand for
    /// [`Self::render_scene`] with no mesh layers.
    pub async fn render_textured(
        &self,
        width: u32,
        height: u32,
        bg: [f32; 4],
        textured: &[TexturedQuad],
        timeout: std::time::Duration,
    ) -> Result<Vec<u32>, String> {
        self.render_scene(width, height, bg, &[], textured, timeout)
            .await
    }

    /// Render mesh layers followed by textured quads into one frame in a
    /// single submit. The first draw clears with `bg`; an empty scene still
    /// fills with `bg`. Quads whose texture id is unknown are skipped.
    /// `timeout` bounds the readback wait; expiry is an `Err`, never a hang.
    pub async fn render_scene(
        &self,
        width: u32,
        height: u32,
        bg: [f32; 4],
        meshes: &[(MeshData, [f32; 4], [[f32; 4]; 4])],
        textured: &[TexturedQuad],
        timeout: std::time::Duration,
    ) -> Result<Vec<u32>, String> {
        let device = &self.device;
        let target = self.acquire_offscreen(width, height)?;
        let target_view = target.view();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("encoder"),
        });

        let mut first_pass = true;
        for (mesh, color, transform) in meshes {
            let uniforms = Uniforms {
                transform: *transform,
                color: *color,
            };
            let uniform_buffer = uniform_buffer(device, &uniforms);
            let vertex_data: Vec<f32> = mesh.vertices.iter().flat_map(|v| [v[0], v[1]]).collect();
            let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vertices"),
                contents: bytemuck::cast_slice(&vertex_data),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("indices"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bind group"),
                layout: &self.bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                }],
            });
            let load = if first_pass {
                clear_op(bg)
            } else {
                wgpu::LoadOp::Load
            };
            {
                let mut pass = begin_pass(&mut encoder, target_view, load);
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.indices.len() as u32, 0, 0..1);
            }
            first_pass = false;
        }

        {
            let mut cache = self
                .textures
                .lock()
                .map_err(|e| format!("texture cache poisoned: {e}"))?;
            for quad in textured {
                let Some(prepared) = self.prepare_texture_quad(
                    &mut cache,
                    quad,
                    wgpu::TextureFormat::Rgba8Unorm,
                    BlendMode::Normal,
                    "textured vertices",
                )?
                else {
                    continue;
                };
                let load = if first_pass {
                    clear_op(bg)
                } else {
                    wgpu::LoadOp::Load
                };
                {
                    let mut pass = begin_pass(&mut encoder, target_view, load);
                    pass.set_pipeline(&prepared.pipeline);
                    pass.set_bind_group(0, &prepared.bind_group, &[]);
                    pass.set_vertex_buffer(0, prepared.vertex_buffer.slice(..));
                    pass.set_index_buffer(
                        prepared.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    pass.draw_indexed(0..quad.mesh.indices.len() as u32, 0, 0..1);
                }
                first_pass = false;
            }
        }

        if first_pass {
            // Nothing drawn: a clear-only pass yields the background frame.
            let _pass = begin_pass(&mut encoder, target_view, clear_op(bg));
        }

        let pixels = self
            .submit_and_read_back(encoder, target.texture(), width, height, timeout)
            .await;
        // The readback's map resolved above, so the copy out of the target has
        // completed and it is idle again.
        self.release_offscreen(target);
        pixels
    }

    /// Copy `target` into a row-aligned staging buffer, submit `encoder`, and
    /// map the buffer back as tightly packed LE-RGBA `u32`s.
    ///
    /// `COPY_BYTES_PER_ROW_ALIGNMENT` forces row padding, which
    /// [`Self::readback_pixels`] strips again; skipping it shears the image.
    async fn submit_and_read_back(
        &self,
        encoder: wgpu::CommandEncoder,
        target: &wgpu::Texture,
        width: u32,
        height: u32,
        timeout: Duration,
    ) -> Result<Vec<u32>, String> {
        let bytes_per_row = width * 4;
        let padded = (bytes_per_row + wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
            & !(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1);
        let buffer_size = (padded * height) as wgpu::BufferAddress;

        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = encoder;
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit(Some(encoder.finish()));
        self.readback_pixels(readback, width, height, padded, timeout)
            .await
    }

    /// Reconcile the fx runtime with this frame's project-defined effects.
    ///
    /// Best effort: a poisoned lock is not worth failing a frame over — the
    /// built-in path still works — so it is simply skipped.
    fn sync_customs(&self, custom: &[CustomEffect]) {
        if let Ok(mut fx) = self.fx.lock() {
            fx.sync_customs(&self.device, custom);
        }
    }

    /// `Some((meshes, textured))` when every layer is effect-free **and** drawn
    /// source-over, i.e. the direct draw path is sufficient and the
    /// effect-aware path would reproduce it exactly.
    ///
    /// This is the regression guard for the effect scheduler: an ordinary scene
    /// keeps running on the code path that shipped before effects existed, so
    /// the scheduler can only add behaviour.
    fn plain_layers(
        &self,
        layers: &[LayerDraw<'_>],
    ) -> Option<(Vec<(MeshData, [f32; 4], [[f32; 4]; 4])>, Vec<TexturedQuad>)> {
        if layers.iter().any(|layer| layer.blend != BlendMode::Normal) {
            return None;
        }
        // The direct path draws every mesh before every textured quad, so an
        // interleaved caller order would change the z-order. Fall through to
        // the effect path, which preserves the caller's order exactly.
        let mut seen_textured = false;
        for layer in layers {
            match layer.shape {
                LayerShape::Mesh { .. } if seen_textured => return None,
                LayerShape::Textured(_) => seen_textured = true,
                LayerShape::Mesh { .. } => {}
            }
        }
        {
            let fx = self.fx.lock().ok()?;
            if layers
                .iter()
                .any(|layer| fx.chain_is_effective(layer.effects))
            {
                return None;
            }
        }
        let mut meshes = Vec::with_capacity(layers.len());
        let mut textured = Vec::new();
        for layer in layers {
            match layer.shape {
                LayerShape::Mesh {
                    mesh,
                    color,
                    transform,
                } => meshes.push((mesh.clone(), color, transform)),
                LayerShape::Textured(quad) => textured.push(quad.clone()),
            }
        }
        Some((meshes, textured))
    }

    /// Record one layer into `view` with `blend`.    ///
    /// A pass is always recorded, even when the layer turns out to be
    /// undrawable (unknown texture id, empty mesh), so that a
    /// [`wgpu::LoadOp::Clear`] handed in for the frame's first layer is still
    /// applied instead of leaving garbage in the target.
    fn record_layer_into(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        shape: &LayerShape<'_>,
        blend: BlendMode,
        load: wgpu::LoadOp<wgpu::Color>,
    ) -> Result<(), String> {
        match shape {
            LayerShape::Mesh {
                mesh,
                color,
                transform,
            } => {
                if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                    let _clear_only = begin_pass(encoder, view, load);
                    return Ok(());
                }
                let uniforms = Uniforms {
                    transform: *transform,
                    color: *color,
                };
                let uniform_buffer = uniform_buffer(&self.device, &uniforms);
                let vertex_data: Vec<f32> =
                    mesh.vertices.iter().flat_map(|v| [v[0], v[1]]).collect();
                let vertex_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("fxs vertices"),
                            contents: bytemuck::cast_slice(&vertex_data),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("fxs indices"),
                            contents: bytemuck::cast_slice(&mesh.indices),
                            usage: wgpu::BufferUsages::INDEX,
                        });
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fxs bind group"),
                    layout: &self.bind_group_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    }],
                });
                let pipeline = self.mesh_pipeline_for(format, blend);
                let mut pass = begin_pass(encoder, view, load);
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.indices.len() as u32, 0, 0..1);
                Ok(())
            }
            LayerShape::Textured(quad) => {
                let mut cache = self
                    .textures
                    .lock()
                    .map_err(|e| format!("texture cache poisoned: {e}"))?;
                // A decoded frame's planes and an RGBA8 image differ only in the
                // payload the texture id names, so this is the same draw as
                // before: same draw index, same effect chain position.
                let prepared = self.prepare_texture_quad(
                    &mut cache,
                    quad,
                    format,
                    blend,
                    "fxs textured vertices",
                )?;
                let Some(prepared) = prepared else {
                    drop(cache);
                    let _clear_only = begin_pass(encoder, view, load);
                    return Ok(());
                };
                drop(cache);
                let mut pass = begin_pass(encoder, view, load);
                pass.set_pipeline(&prepared.pipeline);
                pass.set_bind_group(0, &prepared.bind_group, &[]);
                pass.set_vertex_buffer(0, prepared.vertex_buffer.slice(..));
                pass.set_index_buffer(prepared.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..quad.mesh.indices.len() as u32, 0, 0..1);
                Ok(())
            }
        }
    }

    /// Effect-aware offscreen render: mesh **and** textured layers, each with
    /// its own effect chain and blend mode, composited bottom-up.
    ///
    /// A layer whose chain is empty (or entirely unusable) takes the same
    /// direct draw path as [`Self::render_scene`], so this is a strict superset
    /// of that behaviour. `custom` is this frame's project-defined effects:
    /// they are synchronised into the runtime once, before the layer loop, so a
    /// chain referencing one has a pipeline bank by the time it draws. Returns
    /// `width * height` LE-RGBA pixels: R in the low byte, matching
    /// [`Self::render_scene`] and the export pipeline.
    pub async fn render_scene_ex(
        &self,
        width: u32,
        height: u32,
        bg: [f32; 4],
        layers: &[LayerDraw<'_>],
        custom: &[CustomEffect],
        timeout: Duration,
    ) -> Result<Vec<u32>, String> {
        self.sync_customs(custom);
        if let Some((meshes, textured)) = self.plain_layers(layers) {
            return self
                .render_scene(width, height, bg, &meshes, &textured, timeout)
                .await;
        }
        let target = self.acquire_offscreen(width, height)?;
        let target_view = target.view();
        let encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("fx encoder"),
        });
        let encoder =
            self.record_scene_ex_layers(encoder, target_view, width, height, bg, layers)?;

        let pixels = self
            .submit_and_read_back(encoder, target.texture(), width, height, timeout)
            .await;
        // The readback's map resolved above, so the copy out of the target has
        // completed and it is idle again.
        self.release_offscreen(target);
        pixels
    }

    /// Record the effect-aware layer loop into `target_view` and hand the encoder
    /// back, ready for whatever comes next.
    ///
    /// Shared by the RGBA readback path and the NV12 pack path so the draw order
    /// and the per-layer effect chains cannot drift between them: the chains are
    /// positional (§8.13), and two copies of this loop would be two places for a
    /// layer to inherit another's effects.
    fn record_scene_ex_layers(
        &self,
        mut encoder: wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        width: u32,
        height: u32,
        bg: [f32; 4],
        layers: &[LayerDraw<'_>],
    ) -> Result<wgpu::CommandEncoder, String> {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut first_pass = true;
        for layer in layers {
            let load = if first_pass {
                clear_op(bg)
            } else {
                wgpu::LoadOp::Load
            };
            let mut fx = self
                .fx
                .lock()
                .map_err(|e| format!("fx runtime poisoned: {e}"))?;
            if fx.chain_is_effective(layer.effects) {
                fx.record_effected_layer(
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    (width, height),
                    target_view,
                    format,
                    load,
                    layer.time,
                    layer.effects,
                    layer.blend,
                    |encoder, origin_view| {
                        self.record_layer_into(
                            encoder,
                            origin_view,
                            format,
                            &layer.shape,
                            BlendMode::Normal,
                            wgpu::LoadOp::Load,
                        )
                    },
                )?;
            } else {
                drop(fx);
                self.record_layer_into(
                    &mut encoder,
                    target_view,
                    format,
                    &layer.shape,
                    layer.blend,
                    load,
                )?;
            }
            first_pass = false;
        }

        if first_pass {
            let _clear_only = begin_pass(&mut encoder, target_view, clear_op(bg));
        }
        Ok(encoder)
    }

    /// Check out this frame's offscreen target from the effect runtime's pool.
    ///
    /// The pool already reuses targets across effect passes, and the frame's own
    /// target is the same kind of object: RGBA8, renderable, bindable and
    /// copyable. Reuse is safe by the pool's own contract — a target handed back
    /// after frame N is only re-encoded in frame N+1, and every path here waits
    /// for its map before releasing, so frame N's copy out has completed. What it
    /// saves is an 8 MiB texture allocation, and whatever the driver does to hand
    /// out memory for it, on every frame of the preview fallback and of the
    /// export.
    fn acquire_offscreen(&self, width: u32, height: u32) -> Result<crate::fx::PooledTarget, String> {
        let mut fx = self
            .fx
            .lock()
            .map_err(|e| format!("fx runtime poisoned: {e}"))?;
        Ok(fx.pool().acquire(&self.device, (width, height)))
    }

    /// Hand this frame's target back, once nothing is reading it any more.
    ///
    /// A poisoned lock drops the target instead of panicking: the pool is an
    /// optimisation, and losing one target to a poisoned mutex is cheaper than
    /// taking the process down over it.
    fn release_offscreen(&self, target: crate::fx::PooledTarget) {
        if let Ok(mut fx) = self.fx.lock() {
            fx.pool().release(target);
        }
    }

    /// Effect-aware offscreen render, packed into an NV12 frame **on the GPU**.
    ///
    /// The scene is rendered exactly as [`Self::render_scene_ex`] renders it —
    /// same layer loop, same effect chains — but the frame is never read back as
    /// RGBA: a compute pass reads the target as a texture and writes the NV12
    /// bytes into a storage buffer, and one map hands the encoder a frame that
    /// already has the layout it wants (docs/12 §12.3).
    ///
    /// `coeffs` carries the matrix; see [`crate::nv12_pack::Nv12Coeffs`].
    ///
    /// The `plain_layers` shortcut of [`Self::render_scene_ex`] is deliberately
    /// not taken here: it builds its own target and reads it back, and by its own
    /// contract the effect-aware loop reproduces it exactly. One loop, one place
    /// where a chain can be attached to the wrong layer.
    pub async fn render_scene_ex_nv12(
        &self,
        width: u32,
        height: u32,
        bg: [f32; 4],
        layers: &[LayerDraw<'_>],
        custom: &[CustomEffect],
        coeffs: &crate::nv12_pack::Nv12Coeffs,
        timeout: Duration,
    ) -> Result<Vec<u8>, String> {
        self.sync_customs(custom);
        // The pool's targets carry `TEXTURE_BINDING` on top of the readback
        // path's usages, which is exactly what the pack pass needs: it reads the
        // frame it was just rendered into.
        let target = self.acquire_offscreen(width, height)?;
        let target_view = target.view();
        let encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("nv12 encoder"),
        });
        let encoder =
            self.record_scene_ex_layers(encoder, target_view, width, height, bg, layers)?;

        // Nothing below this line can be exercised by any host in this project:
        // there is no GPU adapter in the build container (docs/12 §12.6). wgpu
        // reports a validation error by *panicking* on the calling thread, and on
        // the export worker that would abort the process instead of taking the
        // fallback the whole design rests on. So the entire pack — the lazy
        // pipeline, the bind group, the dispatch, the copy and the map — is
        // caught here and turned into the `Err` the caller already knows how to
        // handle. The panic message is carried through, because a silent
        // fallback would hide the reason the device is on the CPU path.
        let packed = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.pack_nv12(encoder, target_view, width, height, coeffs, timeout)
        })) {
            Ok(result) => result,
            Err(payload) => {
                let why = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "unknown panic".to_string());
                Err(format!("nv12 pack pass rejected by the driver: {why}"))
            }
        };
        // Released on both outcomes: the pack's map resolved above, or it never
        // got as far as touching the target. Either way nothing is still reading
        // it, and a target leaked here would cost the pool a whole frame's worth
        // of memory on every failed export frame.
        self.release_offscreen(target);
        packed
    }

    /// The pack pass itself: lazily compile it, record it, submit and map.
    fn pack_nv12(
        &self,
        encoder: wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        width: u32,
        height: u32,
        coeffs: &crate::nv12_pack::Nv12Coeffs,
        timeout: Duration,
    ) -> Result<Vec<u8>, String> {
        // The guard is held across the map on purpose: the pass has exactly one
        // caller, the export worker, so there is no second waiter to starve, and
        // releasing it would mean cloning pipeline handles per frame. A poisoned
        // lock is recovered rather than propagated — the payload is a compiled
        // pass, and refusing it would leave the device on the CPU path for good.
        let mut slot = self
            .nv12
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none() {
            *slot = Some(crate::nv12_pack::Nv12Pack::new(&self.device));
        }
        let pack = slot.as_mut().ok_or("nv12 pack pass vanished")?;
        pack.pack(
            &self.device,
            &self.queue,
            encoder,
            target_view,
            width,
            height,
            coeffs,
            timeout,
        )
    }

    /// Effect-aware surface render: same layer semantics as
    /// [`Self::render_scene_ex`], presented to an already-configured surface
    /// instead of read back. `custom` carries this frame's project-defined
    /// effects, synchronised once before the layer loop. Synchronous — the
    /// engine bounds it from the JNI side, and there is no mapping to wait on.
    pub fn render_to_surface_draws(
        &self,
        surface: &wgpu::Surface<'_>,
        bg: [f32; 4],
        layers: &[LayerDraw<'_>],
        custom: &[CustomEffect],
    ) -> Result<(), String> {
        self.sync_customs(custom);
        if let Some((meshes, textured)) = self.plain_layers(layers) {
            return self.render_to_surface_ex(surface, bg, &meshes, &textured);
        }
        let config = surface
            .get_configuration()
            .ok_or("surface is not configured")?;
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout => return Err("surface acquire timed out".into()),
            wgpu::CurrentSurfaceTexture::Occluded => return Err("surface occluded".into()),
            wgpu::CurrentSurfaceTexture::Outdated => return Err("surface outdated".into()),
            wgpu::CurrentSurfaceTexture::Lost => return Err("surface lost".into()),
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("surface validation error".into());
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = (config.width, config.height);
        let format = config.format;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fx surface encoder"),
            });

        let mut first_pass = true;
        for layer in layers {
            let load = if first_pass {
                clear_op(bg)
            } else {
                wgpu::LoadOp::Load
            };
            let mut fx = self
                .fx
                .lock()
                .map_err(|e| format!("fx runtime poisoned: {e}"))?;
            if fx.chain_is_effective(layer.effects) {
                fx.record_effected_layer(
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    size,
                    &view,
                    format,
                    load,
                    layer.time,
                    layer.effects,
                    layer.blend,
                    |encoder, origin_view| {
                        self.record_layer_into(
                            encoder,
                            origin_view,
                            wgpu::TextureFormat::Rgba8Unorm,
                            &layer.shape,
                            BlendMode::Normal,
                            wgpu::LoadOp::Load,
                        )
                    },
                )?;
            } else {
                drop(fx);
                self.record_layer_into(&mut encoder, &view, format, &layer.shape, layer.blend, load)?;
            }
            first_pass = false;
        }

        if first_pass {
            let _clear_only = begin_pass(&mut encoder, &view, clear_op(bg));
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        Ok(())
    }

    /// Map `readback`, unpad into tightly packed LE-RGBA `u32`s and unmap.
    async fn readback_pixels(
        &self,
        readback: wgpu::Buffer,
        width: u32,
        height: u32,
        padded: u32,
        timeout: Duration,
    ) -> Result<Vec<u32>, String> {
        let bytes_per_row = width * 4;
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(timeout),
            })
            .map_err(|e| format!("gpu poll failed: {e:?}"))?;
        rx.recv_timeout(timeout + Duration::from_secs(5))
            .map_err(|e| format!("gpu map timed out: {e:?}"))?
            .map_err(|e| format!("gpu buffer map failed: {e:?}"))?;

        let data = slice
            .get_mapped_range()
            .map_err(|e| format!("mapped range failed: {e:?}"))?;
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for row in 0..height {
            let start = row as usize * padded as usize;
            let row_bytes = &data[start..start + bytes_per_row as usize];
            for chunk in row_bytes.chunks_exact(4) {
                pixels.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
        drop(data);
        readback.unmap();
        Ok(pixels)
    }
}

/// Preferred swapchain format from `formats` offered by the surface
/// capabilities.
///
/// **Non-sRGB on purpose.** Everything in the engine is straight RGBA8 with no
/// colour management anywhere: textures are uploaded as `Rgba8Unorm`, the
/// offscreen target is `Rgba8Unorm`, and the CPU preview copies a photo byte for
/// byte when its alpha is 1. Presenting into an `-Srgb` surface breaks that
/// contract in exactly one place — the final write — and does it silently:
/// wgpu reads the fragment value as linear and encodes it to sRGB on store, so a
/// photo's encoded midtone 0.5 leaves as ~0.73. Every picture comes out washed
/// out and no layer's own colour setting explains it, because the damage is
/// applied to all of them at once, after they were correct.
///
/// Matching the swapchain to the offscreen target keeps one convention across
/// preview, export and the CPU path, which is the only way "what you see is the
/// file" holds. `None` when nothing is offered. Pure function — unit-testable
/// with mocked capability lists, no GPU needed.
pub fn pick_surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    use wgpu::TextureFormat::*;
    [Bgra8Unorm, Rgba8Unorm]
        .into_iter()
        .find(|want| formats.contains(want))
        .or_else(|| formats.first().copied())
}

/// Preferred present mode from `modes`: `Fifo` (the only universally
/// supported vsync mode) when offered, else the driver's first choice.
/// Pure function — unit-testable with mocked capability lists.
pub fn pick_present_mode(modes: &[wgpu::PresentMode]) -> Option<wgpu::PresentMode> {
    modes
        .iter()
        .find(|m| **m == wgpu::PresentMode::Fifo)
        .copied()
        .or_else(|| modes.first().copied())
}

/// Swapchain configuration for a `width`×`height` surface in `format`:
/// `RENDER_ATTACHMENT` usage, `Fifo` present, alpha `Auto`, 2 frames in
/// flight. `None` on zero size (`Surface::configure` would panic).
/// Pure constructor — unit-testable without a GPU.
pub fn make_surface_config(
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> Option<wgpu::SurfaceConfiguration> {
    if width == 0 || height == 0 {
        return None;
    }
    Some(wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width,
        height,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: vec![],
    })
}

fn clear_op(bg: [f32; 4]) -> wgpu::LoadOp<wgpu::Color> {
    wgpu::LoadOp::Clear(wgpu::Color {
        r: bg[0] as f64,
        g: bg[1] as f64,
        b: bg[2] as f64,
        a: bg[3] as f64,
    })
}

fn begin_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("render pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

pub async fn render_frame_gpu(
    mesh: &MeshData,
    config: &RenderConfig,
    transform: &[[f32; 4]; 4],
    color: [f32; 4],
    timeout: std::time::Duration,
) -> Result<Vec<u32>, String> {
    let gpu = GpuRenderer::new().await?;
    gpu.render_layers(
        config.width,
        config.height,
        config.clear_color,
        &[(mesh.clone(), color, *transform)],
        timeout,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opaque_photo_reaches_the_frame_byte_for_byte() {
        // The colour promise, stated as a measurement rather than a comment: a
        // staged photo drawn at alpha 1 must come out of the compositor exactly
        // as it went in, every channel, every pixel. Anything that re-encodes,
        // premultiplies and forgets to undo it, or rounds through linear light
        // shows up here as a changed byte.
        //
        // This is the CPU path. The GPU path has its own colour trap — presenting
        // into an sRGB swapchain re-encodes the final write, which is why
        // `pick_surface_format` avoids `-Srgb`.
        let (w, h) = (16u32, 16u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for i in 0..(w * h) as usize {
            // A deliberately awkward spread: near-black, midtones at every
            // level, near-white and saturated channels, because a gamma error or
            // a channel swap hides in the middle of the range.
            let r = (i % 256) as u8;
            let g = ((i * 7) % 256) as u8;
            let b = ((i * 29) % 256) as u8;
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
        let image = TextureImage::new(w, h, rgba).expect("image");
        let mut mesh = TexturedMesh::new();
        mesh.push_quad(0.0, 0.0, w as f32, h as f32, [0.0, 0.0, 1.0, 1.0]);
        let mut frame = vec![0u32; (w * h) as usize];
        draw_textured_cpu(&mut frame, w, h, &image, &mesh, [1.0, 1.0, 1.0, 1.0]);
        for (i, px) in frame.iter().enumerate() {
            let src = &image.rgba[i * 4..i * 4 + 4];
            assert_eq!(
                px.to_le_bytes()[..3],
                src[..3],
                "pixel {i} changed colour: {:?} -> {:?}",
                src,
                px.to_le_bytes()
            );
            assert_eq!(px.to_le_bytes()[3], 255, "alpha must survive opaque");
        }
    }

    fn test_config() -> RenderConfig {
        RenderConfig {
            width: 64,
            height: 64,
            clear_color: [0.0, 0.0, 0.0, 1.0],
        }
    }

    fn identity() -> [[f32; 4]; 4] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn center_triangle() -> MeshData {
        MeshData {
            vertices: vec![[0.0, 0.5], [-0.5, -0.5], [0.5, -0.5]],
            indices: vec![0, 1, 2],
        }
    }

    #[test]
    fn cpu_render_triangle_coverage() {
        let config = test_config();
        let mesh = center_triangle();
        let pixels = render_frame_cpu(&mesh, &config, &identity(), [1.0, 0.0, 0.0, 1.0]);

        let center = pixels[(32 * 64 + 32) as usize];
        assert_eq!(center, 0xFF0000FF, "center must be red");

        let corner = pixels[0];
        assert_eq!(corner, 0xFF000000, "corner must be clear color");
    }

    #[test]
    fn cpu_textured_blit_maps_texels_with_tint() {
        use crate::texture::TexturedMesh;
        let img = TextureImage::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).expect("image");
        let mut mesh = TexturedMesh::new();
        mesh.push_quad(0.0, 0.0, 2.0, 1.0, [0.0, 0.0, 1.0, 1.0]);
        let mut frame = vec![0xFF000000u32; 2];
        draw_textured_cpu(&mut frame, 2, 1, &img, &mesh, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(frame[0], 0xFF0000FF, "left pixel must be red (LE-RGBA)");
        assert_eq!(frame[1], 0xFFFF0000, "right pixel must be blue (LE-RGBA)");
    }

    #[test]
    fn cpu_textured_blit_skips_degenerate_input() {
        use crate::texture::TexturedMesh;
        let img = TextureImage::new(1, 1, vec![255, 255, 255, 255]).expect("image");
        let empty = TexturedMesh::new();
        let mut frame = vec![0xFF112233u32; 4];
        draw_textured_cpu(&mut frame, 2, 2, &img, &empty, [1.0; 4]);
        assert!(frame.iter().all(|&p| p == 0xFF112233u32));
        // Tinted fully transparent draws nothing either.
        let mut mesh = TexturedMesh::new();
        mesh.push_quad(0.0, 0.0, 2.0, 2.0, [0.0, 0.0, 1.0, 1.0]);
        draw_textured_cpu(&mut frame, 2, 2, &img, &mesh, [1.0, 1.0, 1.0, 0.0]);
        assert!(frame.iter().all(|&p| p == 0xFF112233u32));
        // Length-mismatched frame is left alone, never panics.
        let mut short = vec![0u32; 3];
        draw_textured_cpu(&mut short, 2, 2, &img, &mesh, [1.0; 4]);
        assert_eq!(short, vec![0u32; 3]);
    }

    #[test]
    fn sample_nearest_clamps_to_edge() {
        let img = TextureImage::new(1, 1, vec![10, 20, 30, 40]).expect("image");
        let t = sample_nearest(&img, [-5.0, 99.0]);
        assert!((t[0] - 10.0 / 255.0).abs() < 1e-6);
        assert!((t[3] - 40.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn cpu_render_clear_color() {
        let config = RenderConfig {
            clear_color: [0.25, 0.5, 0.75, 1.0],
            ..test_config()
        };
        let mesh = MeshData {
            vertices: vec![],
            indices: vec![],
        };
        let pixels = render_frame_cpu(&mesh, &config, &identity(), [0.25, 0.5, 0.75, 1.0]);

        let expected = to_rgba([0.25, 0.5, 0.75, 1.0]);
        assert!(pixels.iter().all(|&p| p == expected));
    }

    /// Full-frame NDC transform for a `w`×`h` pixel-space quad.
    fn ndc_quad(w: f32, h: f32) -> [[f32; 4]; 4] {
        [
            [2.0 / w, 0.0, 0.0, 0.0],
            [0.0, -2.0 / h, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0, 1.0],
        ]
    }

    #[test]
    #[ignore = "requires a working wgpu adapter (Vulkan/GL) at runtime"]
    fn cpu_gpu_parity() {
        let config = test_config();
        let mesh = center_triangle();
        let color = [0.2, 0.4, 0.6, 1.0];

        let cpu = render_frame_cpu(&mesh, &config, &identity(), color);
        let gpu = pollster::block_on(render_frame_gpu(
            &mesh,
            &config,
            &identity(),
            color,
            std::time::Duration::from_secs(30),
        ))
        .expect("gpu render failed");

        let cpu_center = cpu[(32 * 64 + 32) as usize];
        let gpu_center = gpu[(32 * 64 + 32) as usize];

        let channel = |v: u32, shift: u32| ((v >> shift) & 0xFF) as i32;
        for shift in [0u32, 8, 16, 24] {
            let diff = (channel(cpu_center, shift) - channel(gpu_center, shift)).abs();
            assert!(diff <= 1, "channel {shift} differs by {diff}");
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter (Vulkan/GL) at runtime"]
    fn textured_quad_uploads_and_draws() {
        let gpu = pollster::block_on(GpuRenderer::new()).expect("gpu");
        let img = TextureImage::new(2, 2, vec![255, 0, 0, 255].repeat(4)).expect("image");
        let id = gpu.upload_texture(&img).expect("upload");
        assert_eq!(gpu.texture_count(), 1);

        let mut mesh = TexturedMesh::new();
        mesh.push_quad(0.0, 0.0, 64.0, 64.0, [0.0, 0.0, 1.0, 1.0]);
        let quads = vec![TexturedQuad {
            mesh,
            color: [1.0, 1.0, 1.0, 1.0],
            transform: ndc_quad(64.0, 64.0),
            texture_id: id,
        }];
        let px = pollster::block_on(gpu.render_textured(
            64,
            64,
            [0.0, 0.0, 0.0, 1.0],
            &quads,
            std::time::Duration::from_secs(30),
        ))
        .expect("render");
        // Red texture, low byte is R in LE-RGBA.
        assert_eq!(px[32 * 64 + 32] & 0xFF, 255);

        assert!(gpu.remove_texture(id));
        assert_eq!(gpu.texture_count(), 0);
    }

    /// The YUV module has to be valid WGSL *before* a device ever sees it: a
    /// shader that only fails on the driver surfaces as a blank video layer, and
    /// naga is the same front end wgpu hands the source to.
    #[test]
    fn the_yuv_shader_parses_and_validates() {
        let module = naga::front::wgsl::parse_str(SHADER_YUV_WGSL).unwrap_or_else(|e| {
            panic!(
                "shader_yuv.wgsl does not parse: {}",
                e.emit_to_string(SHADER_YUV_WGSL)
            )
        });
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("shader_yuv.wgsl does not validate: {e:?}"));
        // Both entry points the pipeline names, so a rename cannot turn into a
        // pipeline that silently draws nothing.
        let entry_points: Vec<&str> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(entry_points.contains(&"vs_texture"), "{entry_points:?}");
        assert!(entry_points.contains(&"fs_yuv"), "{entry_points:?}");
    }

    /// The shader is pinned to the arithmetic the CPU converter is pinned to, by
    /// *shape*: these are the expressions the equivalence tests in
    /// `rumo_media::video::yuv` were transcribed from, and there is no adapter in
    /// CI to compare rendered pixels against.
    ///
    /// A wrong matrix makes video green or pink and nothing crashes, so the
    /// failure has to be a test rather than a screenshot somebody notices.
    #[test]
    fn the_yuv_shader_keeps_the_pinned_arithmetic() {
        for expected in [
            // The CPU's `quantize`: round half up, then clamp.
            "clamp(floor(x + 0.5), 0.0, 255.0)",
            // `Coeffs::rgb`'s three combinations, in the same order.
            "u.luma.x * (luma - u.luma.y)",
            "quantize(y + u.chroma.x * cr_off)",
            "quantize(y - u.chroma.y * cb_off - u.chroma.z * cr_off)",
            "quantize(y + u.chroma.w * cb_off)",
            // Nearest 4:2:0 chroma, and integer plane reads.
            "let chroma_pos = vec2<i32>(px >> 1u, py >> 1u);",
            "var t_y: texture_2d<u32>",
            // A UV is turned back into a crop texel index, which is what makes
            // the fetch identical to `y[row * stride + x]`.
            "floor(in.uv.x * u.size.x)",
        ] {
            assert!(
                SHADER_YUV_WGSL.contains(expected),
                "shader_yuv.wgsl no longer contains {expected:?}; the host tests that \
                 pin the GPU colour are transcribed from it and would now be pinning \
                 something the shader does not do"
            );
        }
        // No sampler type and no filtered sampling: nearest is structural, so
        // bilinear chroma cannot arrive by accident (it is a separate change,
        // docs/12 §12.3). The bind group layout in `GpuRenderer::new` is the
        // other half of that promise — one uniform, three integer textures.
        for forbidden in ["textureSample", "sampler_2d", "sampler_comparison"] {
            assert!(
                !SHADER_YUV_WGSL.contains(forbidden),
                "shader_yuv.wgsl uses {forbidden}; a filtered read would replace the \
                 CPU converter's nearest chroma with bilinear upsampling, which is a \
                 separate change (docs/12 §12.3)"
            );
        }
        for constant in [
            "0.299",
            "0.2126",
            "0.2627",
            "1.402",
            "1.164",
            "255.0 / 219.0",
        ] {
            assert!(
                !SHADER_YUV_WGSL.contains(constant),
                "shader_yuv.wgsl hard-codes {constant}; the matrix must come from \
                 the uniform so it cannot drift from `coeffs_f32`"
            );
        }
    }

    #[test]
    #[ignore = "needs-adapter: the residency check runs against a real wgpu device"]
    fn a_changed_atlas_page_reaches_the_gpu_and_an_unchanged_one_does_not() {
        // The device half of the residency rule: `TextureStamp` decides on the
        // host, this proves the decision is what `upsert` actually does.
        use crate::composite::ATLAS_TEXTURE_ID;
        let gpu = pollster::block_on(GpuRenderer::new()).expect("gpu");
        let page = TextureImage::new(4, 4, vec![7u8; 4 * 4 * 4]).expect("page");
        let before_edit = TextureStamp::for_atlas(4, 4, 1);
        assert_eq!(
            gpu.set_texture(ATLAS_TEXTURE_ID, before_edit, &page),
            Ok(UploadOutcome::Uploaded)
        );
        // Every following frame of an unedited scene: same id, same stamp.
        assert_eq!(
            gpu.set_texture(ATLAS_TEXTURE_ID, before_edit, &page),
            Ok(UploadOutcome::Resident),
            "an unchanged page must not be uploaded again"
        );
        assert_eq!(gpu.texture_count(), 1);
        // A glyph rasterized after the first upload.
        assert_eq!(
            gpu.set_texture(
                ATLAS_TEXTURE_ID,
                TextureStamp::for_atlas(4, 4, 2),
                &page
            ),
            Ok(UploadOutcome::Uploaded),
            "a text edit must reach the GPU"
        );
        assert_eq!(gpu.texture_count(), 1, "replaced in place, not accumulated");
    }
}
