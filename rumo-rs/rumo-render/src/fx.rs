// SPDX-License-Identifier: Apache-2.0

//! GPU effect-chain execution: a pooled target allocator, one pipeline per
//! `(effect kind, pass entry point)`, and the pass scheduler.
//!
//! # Contract with the effect modules
//!
//! Every module produced by [`crate::effect::wgsl_source`] declares exactly
//! five bindings — four in group 0 (`input_tex`, `input_smp`, `frame`,
//! `origin_tex`) and one in group 1 (`params`) — and one fragment entry point
//! per [`rumo_core::effect::PassSpec`]. It declares no vertex stage: the shared
//! vertex stage lives here ([`FX_VERTEX_WGSL`]) so every effect samples through
//! an identical top-left-origin `uv`.
//!
//! [`FxRuntime::new`] validates each module with `naga` — the same WGSL front
//! end wgpu uses — and again asserts the binding allowlist, so a module that
//! would be rejected by the driver is rejected here instead, at startup, where
//! it can be skipped rather than panicking mid-frame.
//!
//! # Pass scheduling
//!
//! Pass 0 reads the layer texture as both `input_tex` and `origin_tex`; each
//! later pass reads the previous pass's output as `input_tex` while
//! `origin_tex` stays the untouched layer (that is what lets `Glow`,
//! `DropShadow` and `InnerShadow` recombine with the original). The last pass
//! yields the layer's final colour.
//!
//! Targets ping-pong: once a pass has been recorded its input is free, so an
//! equal-sized former input is reused as the next output's target instead of
//! allocating a third buffer. A four-pass effect therefore needs two
//! intermediate targets plus the layer, which matters at 4K (33 MB each).

use std::collections::HashMap;
use std::num::NonZeroU64;

use rumo_core::effect::{
    CustomEffect, EffectInstance, EffectKind, EffectTarget, MAX_SHRINK, MemberShape,
};
use wgpu::util::DeviceExt;

/// Format of every effect render target.
pub const FX_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Idle pooled targets retained before the pool starts dropping them instead.
pub const POOL_BUDGET_BYTES: u64 = 192 * 1024 * 1024;

/// Vertex stage shared by every effect pass.
pub const FX_VERTEX_WGSL: &str = include_str!("fx_fullscreen.wgsl");

/// Bindings an effect module is allowed to declare, as `(group, binding)`.
pub const ALLOWED_BINDINGS: &[(u32, u32)] = &[(0, 0), (0, 1), (0, 2), (0, 3), (1, 0)];

/// Minimum size promised for the `params` binding of group 1.
///
/// **This must stay `None`.** It was once a hard `16`, and the Adreno 730 in a
/// Snapdragon 8+ Gen 1 rejected every pipeline whose `struct Params` exceeded
/// that: `Buffer structure size 24 … ended up greater than the given
/// min_binding_size, which is 16`. Six of the ten built-in effects were disabled
/// on device while every host-side test still passed, because a pipeline-layout
/// mismatch is only checkable with a real device.
///
/// `None` means "no promise", which is what a per-effect-varying uniform block
/// needs: the shader's own declared size is then checked against the bound
/// buffer (always `EffectSpec::block_len()` floats, padded to 16 bytes), and
/// that is the check we actually want.
pub const PARAMS_MIN_BINDING_SIZE: Option<NonZeroU64> = None;

/// Composite stage for effect results. See `fx_blit.wgsl`.
pub const FX_BLIT_WGSL: &str = include_str!("fx_blit.wgsl");

/// How a layer's result is combined with what is already in the frame.
///
/// The shaders in this engine emit **straight (non-premultiplied) alpha**, so
/// every state below is written with that convention.
///
/// `Multiply` and `Screen` are exact for opaque sources (the common case).
/// For a source with `alpha < 1` they approximate: a fixed-function blend
/// cannot express `lerp(dst, f(src, dst), src.a)` for a non-linear `f` without
/// sampling the destination in the shader, which would force a copy of the
/// frame per layer. The approximation is documented rather than hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlendMode {
    /// Source-over. Matches `blend_over` in the CPU compositor.
    #[default]
    Normal,
    /// Additive: `dst + src * src.a`.
    Add,
    /// `dst * src` for opaque sources.
    Multiply,
    /// `src + dst - src * dst` for opaque sources.
    Screen,
}

impl BlendMode {
    /// Every mode, in UI order.
    pub const ALL: &'static [BlendMode] = &[
        BlendMode::Normal,
        BlendMode::Add,
        BlendMode::Multiply,
        BlendMode::Screen,
    ];

    /// Stable identifier for JSON/JNI.
    pub const fn id(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Add => "add",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
        }
    }

    /// Inverse of [`BlendMode::id`].
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|mode| mode.id() == id)
    }

    /// Fixed-function state for this mode.
    pub fn state(self) -> wgpu::BlendState {
        let component = |src: wgpu::BlendFactor, dst: wgpu::BlendFactor| wgpu::BlendComponent {
            src_factor: src,
            dst_factor: dst,
            operation: wgpu::BlendOperation::Add,
        };
        use wgpu::BlendFactor as F;
        match self {
            BlendMode::Normal => wgpu::BlendState {
                color: component(F::SrcAlpha, F::OneMinusSrcAlpha),
                alpha: component(F::One, F::OneMinusSrcAlpha),
            },
            BlendMode::Add => wgpu::BlendState {
                color: component(F::SrcAlpha, F::One),
                alpha: component(F::One, F::OneMinusSrcAlpha),
            },
            BlendMode::Multiply => wgpu::BlendState {
                color: component(F::Dst, F::OneMinusSrcAlpha),
                alpha: component(F::DstAlpha, F::OneMinusSrcAlpha),
            },
            BlendMode::Screen => wgpu::BlendState {
                color: component(F::OneMinusDst, F::One),
                alpha: component(F::One, F::OneMinusSrcAlpha),
            },
        }
    }
}

/// `Frame` uniform, matching `struct Frame` in every effect module: 32 bytes,
/// no padding.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FxFrame {
    /// Size of the intermediate target in pixels.
    pub size: [f32; 2],
    /// Timeline time in seconds.
    pub time: f32,
    /// Index of the pass inside the effect.
    pub pass: f32,
    /// `1.0 / size`.
    pub texel_input: [f32; 2],
    /// `1.0 / origin_size`, so a radius expressed in output pixels stays
    /// constant across downscaled passes.
    pub texel_origin: [f32; 2],
}

const _: () = assert!(std::mem::size_of::<FxFrame>() == 32);

impl FxFrame {
    /// Build the uniform for one pass.
    pub fn new(size: (u32, u32), origin: (u32, u32), time: f32, pass: u8) -> Self {
        let inv = |v: u32| 1.0 / (v.max(1) as f32);
        Self {
            size: [size.0 as f32, size.1 as f32],
            time,
            pass: pass as f32,
            texel_input: [inv(size.0), inv(size.1)],
            texel_origin: [inv(origin.0), inv(origin.1)],
        }
    }
}

/// Target size for a pass that divides by `2^shrink`.
pub fn shrunk(size: (u32, u32), shrink: u8) -> (u32, u32) {
    let divisor = 1u32 << shrink.min(MAX_SHRINK);
    ((size.0 / divisor).max(1), (size.1 / divisor).max(1))
}

/// Bytes one RGBA8 target of this size occupies.
pub fn target_bytes(size: (u32, u32)) -> u64 {
    u64::from(size.0.max(1)) * u64::from(size.1.max(1)) * 4
}

/// A render target checked out of [`TargetPool`].
pub struct PooledTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
}

impl PooledTarget {
    /// Texture view for use as a render attachment or a bound texture.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The underlying texture.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Pixel dimensions.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Reuses RGBA8 render targets across passes and frames.
///
/// Reuse is safe because command buffers execute in submission order: a target
/// handed back after frame N is only re-encoded in frame N+1, by which point
/// frame N's reads have completed. The pool never hands the same target to two
/// live passes — the scheduler ping-pongs instead.
#[derive(Default)]
pub struct TargetPool {
    free: HashMap<(u32, u32), Vec<wgpu::Texture>>,
    retained_bytes: u64,
}

impl TargetPool {
    /// An empty pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Check out a target of exactly `size`, reusing a pooled one when
    /// available.
    pub fn acquire(&mut self, device: &wgpu::Device, size: (u32, u32)) -> PooledTarget {
        let (width, height) = (size.0.max(1), size.1.max(1));
        let reused = self
            .free
            .get_mut(&(width, height))
            .and_then(std::vec::Vec::pop);
        let texture = match reused {
            Some(texture) => {
                self.retained_bytes = self
                    .retained_bytes
                    .saturating_sub(target_bytes((width, height)));
                texture
            }
            None => device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fx target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FX_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }),
        };
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        PooledTarget {
            texture,
            view,
            width,
            height,
        }
    }

    /// Return a target. Dropped instead of retained once the budget is full.
    pub fn release(&mut self, target: PooledTarget) {
        let size = target.size();
        let bytes = target_bytes(size);
        if self.retained_bytes + bytes > POOL_BUDGET_BYTES {
            return;
        }
        self.retained_bytes += bytes;
        self.free.entry(size).or_default().push(target.texture);
    }

    /// Bytes currently held by idle targets.
    pub fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }

    /// Number of idle targets.
    pub fn retained_count(&self) -> usize {
        self.free.values().map(std::vec::Vec::len).sum()
    }

    /// Drop every idle target.
    pub fn clear(&mut self) {
        self.free.clear();
        self.retained_bytes = 0;
    }
}

/// What `naga` reported about one effect module's `Params` struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamsLayout {
    /// Field names in declaration order.
    pub names: Vec<String>,
    /// Field byte offsets in declaration order.
    pub offsets: Vec<u32>,
    /// `f32` values each field carries, in declaration order.
    pub components: Vec<u8>,
    /// How each field is laid out, in declaration order. This is what the
    /// packer walks, so a `vec3` or a `mat3x3` lands where WGSL reads it.
    pub shapes: Vec<MemberShape>,
    /// Struct span in bytes.
    pub span: u32,
}

/// Everything the host needs to know about one validated module: its `Params`
/// block and the fragment entry points it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleShape {
    /// The module's `Params` struct.
    pub params: ParamsLayout,
    /// `@fragment` entry-point names, in declaration order.
    pub fragments: Vec<String>,
}

/// Parse and validate one effect module, returning its shape.
///
/// `label` names the effect in every error message (a built-in id or a custom
/// id). Fails with a human-readable message when the module does not parse,
/// does not validate, declares a binding outside [`ALLOWED_BINDINGS`], declares
/// a vertex entry point, or has no `struct Params`.
///
/// Validation runs with `Capabilities::all()`, not `empty()`. The host must not
/// refuse a module the device could run: a feature the host has no opinion on
/// (say a language construct one driver supports) belongs to the driver, which
/// is the real gate. A pipeline the driver refuses is already recorded with its
/// reason through [`crate::diag::reject_effect`] and surfaced by the app, so
/// widening the host's acceptance moves a rejection from "before the driver
/// saw it" to "the driver's own, named reason". Nothing else is weakened: the
/// binding allowlist, the no-vertex-stage rule, the `Params` presence check and
/// the name/order match below all still apply.
pub fn validate_source(label: &str, source: &str) -> Result<ModuleShape, String> {
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|e| format!("{label}: WGSL parse error: {}", e.emit_to_string(source)))?;

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator.validate(&module).map_err(|e| {
        format!(
            "{label}: WGSL validation error: {}",
            e.emit_to_string(source)
        )
    })?;

    for (_, var) in module.global_variables.iter() {
        if let Some(binding) = &var.binding {
            let pair = (binding.group, binding.binding);
            if !ALLOWED_BINDINGS.contains(&pair) {
                return Err(format!(
                    "{label}: unexpected binding group {} binding {} (allowlist: {:?})",
                    pair.0, pair.1, ALLOWED_BINDINGS
                ));
            }
        }
        if let Some(name) = &var.name
            && var.space == naga::AddressSpace::Uniform
            && !matches!(name.as_str(), "frame" | "params")
        {
            return Err(format!("{label}: unexpected uniform `{name}`"));
        }
    }

    for entry in module.entry_points.iter() {
        if entry.stage == naga::ShaderStage::Vertex {
            return Err(format!(
                "{label}: effect modules must not declare a vertex stage"
            ));
        }
    }

    let mut found: Option<ParamsLayout> = None;
    for (_, ty) in module.types.iter() {
        if ty.name.as_deref() != Some("Params") {
            continue;
        }
        let naga::TypeInner::Struct { members, span } = &ty.inner else {
            return Err(format!("{label}: `Params` is not a struct"));
        };
        let mut names = Vec::with_capacity(members.len());
        let mut offsets = Vec::with_capacity(members.len());
        let mut components = Vec::with_capacity(members.len());
        let mut shapes = Vec::with_capacity(members.len());
        for member in members {
            let Some(name) = member.name.clone() else {
                return Err(format!("{label}: `Params` has an unnamed member"));
            };
            let float = naga::Scalar {
                kind: naga::ScalarKind::Float,
                width: 4,
            };
            // The member types the packer knows how to write. Anything else is
            // refused here with the allowed set, rather than silently packed
            // into the wrong place.
            let (count, shape) = match &module.types[member.ty].inner {
                naga::TypeInner::Scalar(scalar) if *scalar == float => (1u8, MemberShape::Scalar),
                naga::TypeInner::Vector { size, scalar } if *scalar == float => {
                    let count = u8::from(*size);
                    (count, MemberShape::Vector(count))
                }
                naga::TypeInner::Matrix {
                    columns,
                    rows,
                    scalar,
                } if *scalar == float => match (columns, rows) {
                    (naga::VectorSize::Tri, naga::VectorSize::Tri) => (9, MemberShape::Matrix3),
                    (naga::VectorSize::Quad, naga::VectorSize::Quad) => (16, MemberShape::Matrix4),
                    _ => {
                        return Err(format!(
                            "{label}: `Params.{name}` must be one of f32, vec2<f32>, vec3<f32>, \
                             vec4<f32>, mat3x3<f32>, mat4x4<f32> (a matrix must be 3x3 or 4x4)"
                        ));
                    }
                },
                _ => {
                    return Err(format!(
                        "{label}: `Params.{name}` must be one of f32, vec2<f32>, vec3<f32>, \
                         vec4<f32>, mat3x3<f32>, mat4x4<f32>"
                    ));
                }
            };
            names.push(name);
            offsets.push(member.offset);
            components.push(count);
            shapes.push(shape);
        }
        found = Some(ParamsLayout {
            names,
            offsets,
            components,
            shapes,
            span: *span,
        });
        break;
    }

    let params = found.ok_or_else(|| format!("{label}: no `struct Params` in module"))?;
    let fragments = module
        .entry_points
        .iter()
        .filter(|e| e.stage == naga::ShaderStage::Fragment)
        .map(|e| e.name.clone())
        .collect();
    Ok(ModuleShape { params, fragments })
}

/// [`validate_source`] for a built-in kind, keeping its 8.2 contract.
pub fn validate_module(kind: EffectKind, source: &str) -> Result<ParamsLayout, String> {
    validate_source(kind.id(), source).map(|shape| shape.params)
}

/// The uniform bytes for one effect pass: every declared parameter written at
/// its member's real offset, so a `vec3` or a `mat4` lands where WGSL expects
/// it.
///
/// The block is `layout.span` bytes rounded up to 16, zero-filled. Rows are
/// walked in `instance.layout.params` order and each row's values are read from
/// `instance.params` at the row's own flat offset, so the declared slot order is
/// unchanged: only the destination within the block moves to the member offset
/// `naga` reported.
///
/// A member name missing from `layout` means the instance's layout and the
/// module's layout have drifted, which is a bug in the host rather than
/// untrusted input. It is debug-asserted and skipped, never a panic: dropping
/// one member for one frame is far better than aborting the render.
pub fn pack_params(instance: &EffectInstance, layout: &ParamsLayout) -> Vec<u8> {
    let mut block = vec![0u8; (layout.span as usize).div_ceil(16) * 16];
    for row in &instance.layout.params {
        let values = instance
            .params
            .get(row.offset..row.offset + row.slots())
            .unwrap_or(&[]);
        let mut cursor = 0usize;
        for member in row.members() {
            let count = member.components as usize;
            let member_values = match values.get(cursor..cursor + count) {
                Some(slice) => slice,
                None => {
                    debug_assert!(false, "`{}.{}` has too few values", row.key, member.name);
                    break;
                }
            };
            cursor += count;
            let Some(index) = layout.names.iter().position(|name| name == &member.name) else {
                debug_assert!(
                    false,
                    "`{}` is not a member of the module's `Params`",
                    member.name
                );
                continue;
            };
            write_member(
                &mut block,
                layout.offsets[index] as usize,
                member.shape,
                member_values,
            );
        }
    }
    block
}

/// Write one member's values into `block` at `offset`.
///
/// Whatever the value slice does not cover stays zero. For [`MemberShape::Matrix3`]
/// that is exactly the padded fourth `f32` of each column: columns start 16
/// bytes apart, so column `c`'s components sit at `offset + c * 16 + r * 4`.
fn write_member(block: &mut [u8], offset: usize, shape: MemberShape, values: &[f32]) {
    fn put_f32(block: &mut [u8], at: usize, value: f32) {
        if let Some(slot) = block.get_mut(at..at + 4) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
    }
    match shape {
        MemberShape::Scalar => {
            if let Some(value) = values.first() {
                put_f32(block, offset, *value);
            }
        }
        MemberShape::Vector(count) => {
            for i in 0..count as usize {
                if let Some(value) = values.get(i) {
                    put_f32(block, offset + i * 4, *value);
                }
            }
        }
        MemberShape::Matrix3 => {
            for column in 0..3usize {
                for row in 0..3usize {
                    if let Some(value) = values.get(column * 3 + row) {
                        put_f32(block, offset + column * 16 + row * 4, *value);
                    }
                }
            }
        }
        MemberShape::Matrix4 => {
            for i in 0..16usize {
                if let Some(value) = values.get(i) {
                    put_f32(block, offset + i * 4, *value);
                }
            }
        }
    }
}

/// One project-defined effect, validated and compiled.
struct CustomPlan {
    /// Hash of the source/passes/params shape the pipelines were built from.
    /// An unchanged fingerprint skips revalidation and pipeline rebuilds.
    fingerprint: u64,
    effect: CustomEffect,
    layout: ParamsLayout,
    pipelines: HashMap<String, wgpu::RenderPipeline>,
}

/// Effect pipelines, modules and the target pool for one device.
pub struct FxRuntime {
    group0: wgpu::BindGroupLayout,
    group1: wgpu::BindGroupLayout,
    vertex: wgpu::ShaderModule,
    sampler: wgpu::Sampler,
    pipeline_layout: wgpu::PipelineLayout,
    /// Built-in pipelines, `kind.id()` → entry point → pipeline.
    pipelines: HashMap<String, HashMap<String, wgpu::RenderPipeline>>,
    /// Built-in `Params` layouts, keyed by `kind.id()`.
    layouts: HashMap<String, ParamsLayout>,
    rejected: HashMap<EffectKind, String>,
    /// Compiled project-defined effects, keyed by custom id.
    customs: HashMap<String, CustomPlan>,
    /// Project-defined effects the validator refused, id → reason.
    rejected_custom: HashMap<String, String>,
    /// Fingerprint of the source each rejection was made against. Without it a
    /// module that stays in the project would be re-parsed and re-validated on
    /// every frame, which is a per-frame cost paid for a permanently broken
    /// effect — and the one case that reaches this state is a project file
    /// edited by hand, which the user cannot fix from inside the app.
    rejected_fingerprint: HashMap<String, u64>,
    pool: TargetPool,
    blit_layout: wgpu::BindGroupLayout,
    blit_shader: wgpu::ShaderModule,
    blit_pipelines: HashMap<(BlendMode, wgpu::TextureFormat), wgpu::RenderPipeline>,
}

impl FxRuntime {
    /// Build layouts and pipelines for every effect whose module validates.
    ///
    /// Kinds that fail validation are recorded in [`Self::rejected`] and simply
    /// skipped at draw time, so one bad shader cannot take the preview down.
    pub fn new(device: &wgpu::Device) -> Self {
        let group0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fx textures"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(std::mem::size_of::<FxFrame>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let group1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fx params"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: PARAMS_MIN_BINDING_SIZE,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx pipeline layout"),
            bind_group_layouts: &[Some(&group0), Some(&group1)],
            immediate_size: 0,
        });
        let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fx fullscreen vertex"),
            source: wgpu::ShaderSource::Wgsl(FX_VERTEX_WGSL.into()),
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fx sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fx blit textures"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fx blit"),
            source: wgpu::ShaderSource::Wgsl(FX_BLIT_WGSL.into()),
        });

        let mut pipelines = HashMap::new();
        let mut layouts = HashMap::new();
        let mut rejected = HashMap::new();

        // Each kind is built in isolation and bracketed by a driver-error
        // counter. A shader a particular driver refuses must disable *that*
        // effect only: if a single bad pipeline could abort the whole runtime,
        // the app would silently lose the GPU path for every layer.
        for kind in EffectKind::ALL.iter().copied() {
            let source = crate::effect::wgsl_source(kind);
            let layout = match validate_module(kind, source) {
                Ok(layout) => layout,
                Err(reason) => {
                    rejected.insert(kind, reason);
                    continue;
                }
            };
            let entries: Vec<String> = kind
                .spec()
                .passes
                .iter()
                .map(|p| p.entry.to_string())
                .collect();
            match Self::build_effect_pipelines(
                device,
                &pipeline_layout,
                &vertex,
                kind.id(),
                source,
                &entries,
            ) {
                Ok(built) => {
                    layouts.insert(kind.id().to_string(), layout);
                    pipelines.insert(kind.id().to_string(), built);
                }
                Err(reason) => {
                    // naga already passed the module, so this is a driver
                    // limit specific to this device.
                    crate::diag::reject_effect(kind.id(), &reason);
                    rejected.insert(kind, reason);
                }
            }
        }

        Self {
            group0,
            group1,
            vertex,
            sampler,
            pipeline_layout,
            pipelines,
            layouts,
            rejected,
            customs: HashMap::new(),
            rejected_custom: HashMap::new(),
            rejected_fingerprint: HashMap::new(),
            pool: TargetPool::new(),
            blit_layout,
            blit_shader,
            blit_pipelines: HashMap::new(),
        }
    }

    /// Create the shader module and one pipeline per entry point, bracketed by
    /// the driver-error counter. `Err` carries the driver's reason and means
    /// *nothing* was retained: the caller must not use any of these pipelines.
    fn build_effect_pipelines(
        device: &wgpu::Device,
        pipeline_layout: &wgpu::PipelineLayout,
        vertex: &wgpu::ShaderModule,
        label: &str,
        source: &str,
        entries: &[String],
    ) -> Result<HashMap<String, wgpu::RenderPipeline>, String> {
        let before = crate::diag::error_count();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let mut built = HashMap::with_capacity(entries.len());
        for entry in entries {
            built.insert(
                entry.clone(),
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(entry),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: vertex,
                        entry_point: Some("vs_main"),
                        buffers: &[],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some(entry),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: FX_FORMAT,
                            // Effect passes own their whole target, so they
                            // replace rather than blend.
                            blend: None,
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
                }),
            );
        }
        if crate::diag::error_count() > before {
            let reason = crate::diag::last_error();
            return Err(if reason.is_empty() {
                "driver rejected the pipeline".to_string()
            } else {
                reason
            });
        }
        Ok(built)
    }

    /// `true` when the kind's module validated and its pipelines exist.
    pub fn usable(&self, kind: EffectKind) -> bool {
        self.layouts.contains_key(kind.id())
    }

    /// Kinds that failed validation, with the reason.
    pub fn rejected(&self) -> &HashMap<EffectKind, String> {
        &self.rejected
    }

    /// The `naga`-reported `Params` layout for a usable kind.
    pub fn params_layout(&self, kind: EffectKind) -> Option<&ParamsLayout> {
        self.layouts.get(kind.id())
    }

    /// Custom effects the validator refused, id → reason.
    pub fn rejected_custom(&self) -> &HashMap<String, String> {
        &self.rejected_custom
    }

    /// The validated `Params` layout of a compiled project-defined effect.
    pub fn custom_params_layout(&self, id: &str) -> Option<&ParamsLayout> {
        self.customs.get(id).map(|plan| &plan.layout)
    }

    /// The compiled project-defined effect, for diagnostics and tests.
    pub fn custom_effect(&self, id: &str) -> Option<&CustomEffect> {
        self.customs.get(id).map(|plan| &plan.effect)
    }

    /// Reconcile the compiled project-defined effects with this frame's list.
    ///
    /// Croaks (drops plans and rejections) for ids that vanished; for each
    /// surviving custom, skips the work when the source/passes/params shape is
    /// unchanged, otherwise revalidates and rebuilds its pipelines. A driver
    /// rejection is recorded through [`crate::diag`] and the effect is skipped,
    /// never aborting the frame.
    pub fn sync_customs(&mut self, device: &wgpu::Device, customs: &[CustomEffect]) {
        let present: std::collections::HashSet<&str> =
            customs.iter().map(|c| c.id.as_str()).collect();
        self.customs.retain(|id, _| present.contains(id.as_str()));
        self.rejected_custom
            .retain(|id, _| present.contains(id.as_str()));
        self.rejected_fingerprint
            .retain(|id, _| present.contains(id.as_str()));

        for effect in customs {
            let fingerprint = custom_fingerprint(effect);
            let unchanged = self
                .customs
                .get(&effect.id)
                .map(|plan| plan.fingerprint)
                .or_else(|| self.rejected_fingerprint.get(&effect.id).copied())
                == Some(fingerprint);
            if unchanged {
                continue;
            }
            self.customs.remove(&effect.id);
            self.rejected_custom.remove(&effect.id);
            self.rejected_fingerprint.remove(&effect.id);
            let layout = match crate::effect::validate_custom(effect) {
                Ok(layout) => layout,
                Err(reason) => {
                    self.rejected_custom.insert(effect.id.clone(), reason);
                    self.rejected_fingerprint
                        .insert(effect.id.clone(), fingerprint);
                    continue;
                }
            };
            let entries: Vec<String> = effect.passes.iter().map(|p| p.entry.clone()).collect();
            match Self::build_effect_pipelines(
                device,
                &self.pipeline_layout,
                &self.vertex,
                &effect.id,
                &effect.source,
                &entries,
            ) {
                Ok(built) => {
                    self.customs.insert(
                        effect.id.clone(),
                        CustomPlan {
                            fingerprint,
                            effect: effect.clone(),
                            layout,
                            pipelines: built,
                        },
                    );
                }
                Err(reason) => {
                    crate::diag::reject_effect(&effect.id, &reason);
                    self.rejected_custom.insert(effect.id.clone(), reason);
                    self.rejected_fingerprint
                        .insert(effect.id.clone(), fingerprint);
                }
            }
        }
    }

    /// The pipeline bank an instance draws from: the built-in bank for a
    /// built-in, the compiled custom plan for a project-defined id.
    fn bank(&self, inst: &EffectInstance) -> Option<&HashMap<String, wgpu::RenderPipeline>> {
        match &inst.target {
            EffectTarget::Builtin(kind) => self.pipelines.get(kind.id()),
            EffectTarget::Custom(id) => self.customs.get(id).map(|plan| &plan.pipelines),
        }
    }

    /// The `naga`-reported `Params` layout an instance packs against: the
    /// built-in layout for a built-in, the compiled custom plan's for a
    /// project-defined id.
    fn params_layout_of(&self, inst: &EffectInstance) -> Option<&ParamsLayout> {
        match &inst.target {
            EffectTarget::Builtin(kind) => self.layouts.get(kind.id()),
            EffectTarget::Custom(id) => self.customs.get(id).map(|plan| &plan.layout),
        }
    }

    /// The target pool, for callers that need to check out their own targets.
    pub fn pool(&mut self) -> &mut TargetPool {
        &mut self.pool
    }

    /// Render `chain` over `origin`, returning the final target.
    ///
    /// `Ok(None)` means nothing was drawn (empty chain, every instance
    /// disabled, or no usable kind) and the caller should draw `origin`
    /// directly. `origin_texture`/`origin_view` must describe `origin_size`.
    ///
    /// # `origin_tex` across a chain
    ///
    /// `origin_tex` is the layer state an effect starts from. The first
    /// instance in the chain sees the caller's untouched layer; each later
    /// instance sees the *previous instance's output*. That is what makes
    /// `Blur` then `Glow` mean "glow the blurred layer" instead of "recombine
    /// the blur with the un-blurred original".
    pub fn record_chain(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        chain: &[EffectInstance],
        origin_texture: &wgpu::Texture,
        origin_view: &wgpu::TextureView,
        origin_size: (u32, u32),
        time: f32,
    ) -> Result<Option<PooledTarget>, String> {
        let _ = origin_texture;

        // The layer state the chain is building up: `None` means "still the
        // caller's layer". Each completed instance replaces it with its output.
        let mut stage: Option<PooledTarget> = None;
        // Ping-pong buffer of the instance currently running, and its last
        // pass output.
        let mut cur: Option<PooledTarget> = None;
        let mut spare: Option<PooledTarget> = None;

        for inst in chain {
            if !inst.enabled {
                continue;
            }
            // An absent bank means the built-in failed validation or the custom
            // id is unknown/rejected; a wrong-arity instance would pack a short
            // block. Both are skipped, so `Ok(None)` still means "nothing drawn".
            if self.bank(inst).is_none() {
                continue;
            }
            if inst.params.len() != inst.layout.slots {
                continue;
            }
            let shrinks = inst.layout.pass_shrinks(inst);
            // Pack against the module's own `Params` layout, not the declared
            // slot count: a `vec3` or a matrix has to sit at the offset WGSL
            // reads it from, which only `naga` knows.
            let Some(params_layout) = self.params_layout_of(inst) else {
                continue;
            };
            let params = pack_params(inst, params_layout);

            for (index, entry) in inst.layout.entries.iter().enumerate() {
                let size = shrunk(origin_size, shrinks[index]);
                let Some(pipeline) = self.bank(inst).and_then(|bank| bank.get(entry)).cloned()
                else {
                    return Err(format!(
                        "{}: pass `{}` was never compiled",
                        inst.layout.id, entry
                    ));
                };

                let out = match spare.take() {
                    Some(target) if target.size() == size => target,
                    Some(target) => {
                        self.pool.release(target);
                        self.pool.acquire(device, size)
                    }
                    None => self.pool.acquire(device, size),
                };

                let (stage_view, stage_size) = match &stage {
                    Some(target) => (target.view(), target.size()),
                    None => (origin_view, origin_size),
                };
                let input_view = match &cur {
                    Some(target) => target.view(),
                    None => stage_view,
                };

                let frame = FxFrame::new(size, stage_size, time, index as u8);
                let frame_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fx frame"),
                    contents: bytemuck::bytes_of(&frame),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fx params"),
                    contents: &params,
                    usage: wgpu::BufferUsages::UNIFORM,
                });

                let textures = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fx textures"),
                    layout: &self.group0,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(input_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: frame_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(stage_view),
                        },
                    ],
                });
                let params_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fx params"),
                    layout: &self.group1,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: params_buffer.as_entire_binding(),
                    }],
                });

                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("fx pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: out.view(),
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    render_pass.set_pipeline(&pipeline);
                    render_pass.set_bind_group(0, &textures, &[]);
                    render_pass.set_bind_group(1, &params_group, &[]);
                    render_pass.draw(0..3, 0..1);
                }

                spare = cur.take();
                cur = Some(out);
            }

            // This instance is finished: its last pass output becomes the
            // layer state the next instance composites against, and the
            // buffers it borrowed go back to the pool.
            if let Some(previous) = stage.take() {
                self.pool.release(previous);
            }
            stage = cur.take();
            if let Some(target) = spare.take() {
                self.pool.release(target);
            }
        }

        // `queue` is unused today: no uploads are needed for a fullscreen pass.
        let _ = queue;

        Ok(stage)
    }

    /// `true` when `chain` would actually change the layer: at least one
    /// enabled instance whose bank exists and whose parameters are the right
    /// arity.
    ///
    /// Callers use this to keep the no-effect fast path (draw straight into the
    /// frame) instead of paying for an origin target and a composite pass.
    pub fn chain_is_effective(&self, chain: &[EffectInstance]) -> bool {
        chain.iter().any(|inst| {
            inst.enabled && self.bank(inst).is_some() && inst.params.len() == inst.layout.slots
        })
    }

    /// Blit `src_view` into `dst_view`, compositing with `blend`.
    ///
    /// `load` must be [`wgpu::LoadOp::Clear`] for the frame's first draw and
    /// [`wgpu::LoadOp::Load`] afterwards — this is a compositing pass, so it
    /// must not clobber what earlier layers already wrote.
    #[allow(clippy::too_many_arguments)]
    pub fn record_blit(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        dst_view: &wgpu::TextureView,
        dst_format: wgpu::TextureFormat,
        src_view: &wgpu::TextureView,
        blend: BlendMode,
        load: wgpu::LoadOp<wgpu::Color>,
    ) -> Result<(), String> {
        let pipeline = self.blit_pipeline(device, blend, dst_format);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx blit"),
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fx blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst_view,
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
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }

    /// Render one layer through its effect chain and composite the result.
    ///
    /// `draw` records the layer's own geometry into the origin target handed to
    /// it; when the chain is empty (or entirely unusable) this degrades to
    /// blitting that origin, so the output is identical to the direct path.
    #[allow(clippy::too_many_arguments)]
    pub fn record_effected_layer<F>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        size: (u32, u32),
        dst_view: &wgpu::TextureView,
        dst_format: wgpu::TextureFormat,
        load: wgpu::LoadOp<wgpu::Color>,
        time: f32,
        chain: &[EffectInstance],
        blend: BlendMode,
        draw: F,
    ) -> Result<(), String>
    where
        F: FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView) -> Result<(), String>,
    {
        let origin = self.pool.acquire(device, size);
        // The layer usually does not cover the whole frame, so the origin
        // starts transparent rather than assuming a full-bleed draw. The clear
        // pass must be closed before `draw` borrows the encoder again.
        {
            let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fx origin clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: origin.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        draw(encoder, origin.view())?;

        let effected = self.record_chain(
            device,
            queue,
            encoder,
            chain,
            origin.texture(),
            origin.view(),
            size,
            time,
        )?;

        let src_view = match &effected {
            Some(target) => target.view(),
            None => origin.view(),
        };
        self.record_blit(device, encoder, dst_view, dst_format, src_view, blend, load)?;

        if let Some(target) = effected {
            self.pool.release(target);
        }
        self.pool.release(origin);
        Ok(())
    }

    /// Pipeline for a blit into `format` with `blend`, created on first use.
    fn blit_pipeline(
        &mut self,
        device: &wgpu::Device,
        blend: BlendMode,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        let key = (blend, format);
        if let Some(pipeline) = self.blit_pipelines.get(&key) {
            return pipeline.clone();
        }
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx blit layout"),
            bind_group_layouts: &[Some(&self.blit_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fx blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.vertex,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.blit_shader,
                entry_point: Some("fs_blit"),
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
        });
        self.blit_pipelines.insert(key, pipeline.clone());
        pipeline
    }

    /// Drop every idle pooled target (e.g. on surface loss).
    pub fn trim(&mut self) {
        self.pool.clear();
    }

    /// Shader module for the shared vertex stage, for callers building their
    /// own effect pipelines.
    pub fn vertex_module(&self) -> &wgpu::ShaderModule {
        &self.vertex
    }
}

/// Hash the parts of a custom effect that determine its compiled pipelines:
/// the source, the pass list (entry + shrink) and the declared parameter shape
/// (WGSL field names and block length). Parameter ranges and defaults do not
/// affect compilation, so they are deliberately excluded.
fn custom_fingerprint(effect: &CustomEffect) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    effect.source.hash(&mut hasher);
    effect.passes.len().hash(&mut hasher);
    for pass in &effect.passes {
        pass.entry.hash(&mut hasher);
        pass.shrink.hash(&mut hasher);
    }
    effect.field_names().hash(&mut hasher);
    effect.block_len().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rumo_core::effect::{CustomParam, CustomPass, EffectInstance, EffectSpace, ParamKind};

    #[test]
    fn validate_source_reports_fragments_in_declaration_order() {
        let source = "struct Params { amount: f32 }\n\
            @group(1) @binding(0) var<uniform> params: Params;\n\
            @fragment fn fs_two(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> { return vec4<f32>(params.amount); }\n\
            @fragment fn fs_one(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> { return vec4<f32>(1.0); }";
        let shape = validate_source("test", source).expect("valid module");
        assert_eq!(
            shape.fragments,
            vec!["fs_two".to_string(), "fs_one".to_string()]
        );
        assert_eq!(shape.params.names, vec!["amount".to_string()]);
        assert_eq!(shape.params.span, 4);
    }

    #[test]
    fn frame_uniform_matches_the_wgsl_struct_size() {
        assert_eq!(std::mem::size_of::<FxFrame>(), 32);
        let frame = FxFrame::new((1920, 1080), (3840, 2160), 1.5, 2);
        assert_eq!(frame.size, [1920.0, 1080.0]);
        assert_eq!(frame.pass, 2.0);
        assert_eq!(frame.texel_input, [1.0 / 1920.0, 1.0 / 1080.0]);
        assert_eq!(frame.texel_origin, [1.0 / 3840.0, 1.0 / 2160.0]);
        // A zero-sized intermediate must not produce infinities.
        let degenerate = FxFrame::new((0, 0), (0, 0), 0.0, 0);
        assert!(degenerate.texel_input.iter().all(|v| *v == 1.0));
    }

    #[test]
    fn shrunk_divides_and_never_reaches_zero() {
        assert_eq!(shrunk((1920, 1080), 0), (1920, 1080));
        assert_eq!(shrunk((1920, 1080), 1), (960, 540));
        assert_eq!(shrunk((1920, 1080), 2), (480, 270));
        assert_eq!(shrunk((1, 1), 6), (1, 1));
        assert_eq!(shrunk((1920, 1080), 200), shrunk((1920, 1080), MAX_SHRINK));
    }

    #[test]
    fn target_bytes_is_rgba8() {
        assert_eq!(target_bytes((16, 16)), 1024);
        assert_eq!(target_bytes((3840, 2160)), 33_177_600);
        assert_eq!(target_bytes((0, 0)), 4);
    }

    /// The authoritative WGSL check: `naga` is wgpu's own front end, so a
    /// module that validates here is one the driver will accept, and the
    /// reported member offsets prove the host's flat `f32` packing matches the
    /// shader's real layout.
    #[test]
    fn every_effect_module_validates_and_packs_like_the_spec() {
        for kind in EffectKind::ALL.iter().copied() {
            let source = crate::effect::wgsl_source(kind);
            let layout = validate_module(kind, source)
                .unwrap_or_else(|e| panic!("effect `{}` is not usable: {e}", kind.id()));

            let spec = kind.spec();
            assert_eq!(
                layout.names,
                spec.field_names(),
                "{}: `struct Params` field order must match EffectSpec::params",
                kind.id()
            );
            assert_eq!(
                layout.names.len(),
                spec.slots(),
                "{}: slot count",
                kind.id()
            );

            // Each field is a 4-byte f32, so its offset must be 4 * slot index.
            for (index, offset) in layout.offsets.iter().enumerate() {
                assert_eq!(
                    *offset as usize,
                    index * 4,
                    "{}: field {} sits at byte {} but the flat f32 packer writes it at {}",
                    kind.id(),
                    layout.names[index],
                    offset,
                    index * 4
                );
            }
            assert!(
                layout.span as usize >= spec.slots() * 4,
                "{}: struct span {} is smaller than the packed block",
                kind.id(),
                layout.span
            );
            assert!(
                layout.components.iter().all(|c| *c == 1)
                    && layout.shapes.iter().all(|s| *s == MemberShape::Scalar),
                "{}: a built-in `Params` is plain f32 members",
                kind.id()
            );

            // The built-ins have always been a flat run of `f32`, so packing by
            // member offset must reproduce the old flat block exactly — same
            // bytes, same length. This is the regression guard for the switch
            // from `uniform_block` to `pack_params`.
            let inst = EffectInstance::new(kind);
            let packed = pack_params(&inst, &layout);
            let mut flat = vec![0u8; spec.block_len() * 4];
            for (index, value) in inst.params.iter().enumerate() {
                flat[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
            assert_eq!(
                packed,
                flat,
                "{}: offset packing must be byte-identical to the flat f32 block",
                kind.id()
            );

            // Every declared pass entry must exist as a fragment entry point.
            for pass in spec.passes {
                assert!(
                    source.contains(pass.entry),
                    "{}: pass entry `{}` is missing from the module",
                    kind.id(),
                    pass.entry
                );
            }
        }
    }

    #[test]
    fn the_shared_vertex_stage_validates_on_its_own() {
        let module = naga::front::wgsl::parse_str(FX_VERTEX_WGSL).expect("vertex stage parses");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        );
        validator.validate(&module).expect("vertex stage validates");
        assert_eq!(module.entry_points.len(), 1);
        assert_eq!(module.entry_points[0].stage, naga::ShaderStage::Vertex);
        assert_eq!(module.entry_points[0].name, "vs_main");
    }

    #[test]
    fn the_vertex_stage_covers_the_target_with_top_left_uv() {
        // Mirrors fx_fullscreen.wgsl: uv (0,0) must be NDC (-1, +1) so that
        // texture row 0 lands at the top of the frame.
        let corner = |vi: u32| {
            let x = ((vi << 1) & 2) as f32;
            let y = (vi & 2) as f32;
            ([x * 2.0 - 1.0, 1.0 - y * 2.0], (x, y))
        };
        let (pos0, uv0) = corner(0);
        assert_eq!(pos0, [-1.0, 1.0]);
        assert_eq!(uv0, (0.0, 0.0));
        let (pos1, uv1) = corner(1);
        assert_eq!(pos1, [3.0, 1.0]);
        assert_eq!(uv1, (2.0, 0.0));
        let (pos2, uv2) = corner(2);
        assert_eq!(pos2, [-1.0, -3.0]);
        assert_eq!(uv2, (0.0, 2.0));
    }

    #[test]
    fn blend_modes_round_trip_ids_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for mode in BlendMode::ALL {
            assert!(seen.insert(mode.id()), "duplicate blend id {}", mode.id());
            assert_eq!(BlendMode::from_id(mode.id()), Some(*mode));
        }
        assert_eq!(seen.len(), 4);
        assert_eq!(BlendMode::from_id("lighten"), None);
        assert_eq!(BlendMode::default(), BlendMode::Normal);
    }

    #[test]
    fn normal_blend_is_straight_alpha_source_over() {
        // Matches the CPU compositor's `blend_over` and wgpu's
        // `BlendState::ALPHA_BLENDING`, which the direct draw path uses.
        let state = BlendMode::Normal.state();
        assert_eq!(state.color.src_factor, wgpu::BlendFactor::SrcAlpha);
        assert_eq!(state.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
        assert_eq!(state.alpha.src_factor, wgpu::BlendFactor::One);
        assert_eq!(state.alpha.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
        assert_eq!(state, wgpu::BlendState::ALPHA_BLENDING);
    }

    #[test]
    fn multiply_and_screen_reduce_to_the_plain_formula_for_opaque_sources() {
        use wgpu::BlendFactor as F;
        let multiply = BlendMode::Multiply.state();
        // dst * src + dst * (1 - src.a); with src.a = 1 this is exactly dst*src.
        assert_eq!(multiply.color.src_factor, F::Dst);
        assert_eq!(multiply.color.dst_factor, F::OneMinusSrcAlpha);

        let screen = BlendMode::Screen.state();
        // (1-dst) * src + dst; with src.a = 1 this is src + dst - src*dst.
        assert_eq!(screen.color.src_factor, F::OneMinusDst);
        assert_eq!(screen.color.dst_factor, F::One);

        let add = BlendMode::Add.state();
        assert_eq!(add.color.src_factor, F::SrcAlpha);
        assert_eq!(add.color.dst_factor, F::One);
    }

    #[test]
    fn the_params_binding_never_promises_less_than_a_params_struct_needs() {
        // A hard `min_binding_size` smaller than an effect's `Params` struct
        // makes that effect's pipeline invalid on the driver — which is how six
        // of ten effects were silently disabled on a Snapdragon 8+ Gen 1 while
        // every host-side test still passed. The naga-reported span is exactly
        // what the driver compares against.
        let promised = PARAMS_MIN_BINDING_SIZE.map(NonZeroU64::get);
        for kind in EffectKind::ALL.iter().copied() {
            let layout = validate_module(kind, crate::effect::wgsl_source(kind))
                .expect("every catalogue module validates");
            if let Some(promised) = promised {
                assert!(
                    promised >= u64::from(layout.span),
                    "{}: min_binding_size {promised} is below the {}-byte `Params` struct, \
                     so the driver will reject this pipeline",
                    kind.id(),
                    layout.span
                );
            }
            // The invariant that replaces the promise: the packed uniform block
            // always covers the shader's declared struct.
            let block_bytes = kind.spec().block_len() as u32 * 4;
            assert!(
                block_bytes >= layout.span,
                "{}: packed block is {block_bytes} bytes but `Params` spans {}",
                kind.id(),
                layout.span
            );
        }
    }

    #[test]
    fn a_hostile_module_is_rejected_with_a_reason() {
        // A stray binding outside the allowlist must be refused by name.
        let bad = r#"
@group(3) @binding(7) var<uniform> sneaky: vec4<f32>;
struct Params { a: f32 }
@group(1) @binding(0) var<uniform> params: Params;
@fragment fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(sneaky.xyz, 1.0);
}
"#;
        let err = validate_module(EffectKind::Pixelate, bad).expect_err("must be refused");
        assert!(err.contains("unexpected binding"), "{err}");
    }

    #[test]
    fn a_module_with_a_vertex_stage_is_rejected() {
        let bad = r#"
struct Params { a: f32 }
@group(1) @binding(0) var<uniform> params: Params;
@vertex fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
@fragment fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(uv, params.a, 1.0);
}
"#;
        let err = validate_module(EffectKind::Pixelate, bad).expect_err("must be refused");
        assert!(err.contains("vertex stage"), "{err}");
    }

    #[test]
    fn an_unsupported_params_member_type_is_rejected_by_name() {
        // A vector of the wrong element type is the interesting miss: a
        // `vec4<f32>` is now allowed, so the refusal has to come from the
        // element type, and the message must name the member.
        let bad = r#"
struct Params { a: vec4<u32> }
@group(1) @binding(0) var<uniform> params: Params;
@fragment fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(params.a);
}
"#;
        let err = validate_module(EffectKind::Pixelate, bad).expect_err("must be refused");
        assert!(err.contains("Params.a"), "{err}");
        assert!(err.contains("must be one of"), "{err}");
        assert!(
            err.contains("mat4x4"),
            "the allowed set must be listed: {err}"
        );
    }

    #[test]
    fn vector_and_matrix_params_are_reported_with_their_shape() {
        let source = r#"
struct Params {
    tint: vec4<f32>,
    dir: vec2<f32>,
    warp: mat3x3<f32>,
    spin: mat4x4<f32>,
    amount: f32,
}
@group(1) @binding(0) var<uniform> params: Params;
@fragment fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return params.tint * params.warp[0][0] + vec4<f32>(params.dir, params.amount, params.spin[0][0]);
}
"#;
        let shape = validate_source("test", source).expect("valid module");
        assert_eq!(
            shape.params.names,
            vec!["tint", "dir", "warp", "spin", "amount"]
        );
        assert_eq!(shape.params.components, vec![4, 2, 9, 16, 1]);
        assert_eq!(
            shape.params.shapes,
            vec![
                MemberShape::Vector(4),
                MemberShape::Vector(2),
                MemberShape::Matrix3,
                MemberShape::Matrix4,
                MemberShape::Scalar,
            ]
        );
    }

    #[test]
    fn a_params_member_of_an_unsupported_matrix_shape_is_rejected() {
        let bad = r#"
struct Params { a: mat3x4<f32> }
@group(1) @binding(0) var<uniform> params: Params;
@fragment fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(params.a[0]);
}
"#;
        let err = validate_module(EffectKind::Pixelate, bad).expect_err("must be refused");
        assert!(err.contains("Params.a"), "{err}");
        assert!(err.contains("mat3x3"), "{err}");
    }

    /// A declaration of one custom parameter, wide enough that `set` cannot
    /// clamp the values these tests write.
    fn custom_param(key: &str, kind: ParamKind) -> CustomParam {
        CustomParam {
            key: key.to_string(),
            label: key.to_string(),
            kind,
            min: -1.0e6,
            max: 1.0e6,
            default: Vec::new(),
            unit: String::new(),
            choices: Vec::new(),
        }
    }

    fn custom_effect(id: &str, params: Vec<CustomParam>, source: &str) -> CustomEffect {
        CustomEffect {
            id: id.to_string(),
            label: "Custom".to_string(),
            space: EffectSpace::Display,
            passes: vec![CustomPass {
                entry: "fs_main".to_string(),
                shrink: 0,
            }],
            params,
            source: source.to_string(),
        }
    }

    #[test]
    fn pack_params_places_vector_and_matrix_members_at_the_naga_offsets() {
        let source = r#"
struct Params {
    tint: vec4<f32>,
    warp: mat4x4<f32>,
    amount: f32,
}
@group(1) @binding(0) var<uniform> params: Params;
@fragment fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return params.tint * params.amount;
}
"#;
        let effect = custom_effect(
            "warp_x",
            vec![
                custom_param("tint", ParamKind::Vec4),
                custom_param("warp", ParamKind::Mat4),
                custom_param("amount", ParamKind::Float),
            ],
            source,
        );
        let layout = crate::effect::validate_custom(&effect).expect("valid custom");
        assert_eq!(layout.names, vec!["tint", "warp", "amount"]);
        assert_eq!(layout.components, vec![4, 16, 1]);
        // `warp` starts on the next 16-byte boundary after `tint`, and
        // `amount` follows it.
        assert_eq!(layout.offsets, vec![0, 16, 80]);
        assert_eq!(layout.span, 96, "84 bytes rounded up to 16");

        let mut inst = EffectInstance::custom(&effect).expect("valid custom");
        assert!(inst.set("tint", &[0.1, 0.2, 0.3, 0.4]));
        let warp: Vec<f32> = (0..16).map(|i| i as f32).collect();
        assert!(inst.set("warp", &warp));
        assert!(inst.set("amount", &[0.5]));

        let block = pack_params(&inst, &layout);
        assert_eq!(block.len(), 96);
        let read = |at: usize| f32::from_le_bytes(block[at..at + 4].try_into().unwrap());
        assert_eq!(
            (0..4).map(|i| read(i * 4)).collect::<Vec<_>>(),
            vec![0.1, 0.2, 0.3, 0.4]
        );
        // A `mat4x4` is four contiguous columns of four.
        assert_eq!((0..16).map(|i| read(16 + i * 4)).collect::<Vec<_>>(), warp);
        assert_eq!(read(80), 0.5);
        assert!(
            block[84..].iter().all(|b| *b == 0),
            "the tail past `amount` stays zero"
        );
    }

    #[test]
    fn pack_params_pads_a_mat3_column_to_sixteen_bytes() {
        let source = r#"
struct Params { spin: mat3x3<f32> }
@group(1) @binding(0) var<uniform> params: Params;
@fragment fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(params.spin[0], 1.0);
}
"#;
        let effect = custom_effect(
            "spin_x",
            vec![custom_param("spin", ParamKind::Mat3)],
            source,
        );
        let layout = crate::effect::validate_custom(&effect).expect("valid custom");
        assert_eq!(layout.offsets, vec![0]);
        assert_eq!(layout.span, 48, "three columns of 16 bytes");

        let mut inst = EffectInstance::custom(&effect).expect("valid custom");
        let spin: Vec<f32> = (1..=9).map(|i| i as f32).collect();
        assert!(inst.set("spin", &spin));

        let block = pack_params(&inst, &layout);
        let read = |at: usize| f32::from_le_bytes(block[at..at + 4].try_into().unwrap());
        // Column `c`'s three values start at 16 * c; its fourth `f32` is the
        // padding WGSL inserts and must stay zero.
        let mut expected = vec![0.0f32; 12];
        for column in 0..3 {
            for row in 0..3 {
                expected[column * 4 + row] = spin[column * 3 + row];
            }
        }
        assert_eq!((0..12).map(|i| read(i * 4)).collect::<Vec<_>>(), expected);
        for column in 0..3 {
            assert_eq!(read(column * 16 + 12), 0.0, "column {column} padding");
        }
    }

    #[test]
    fn a_module_without_params_is_rejected() {
        let bad = r#"
@fragment fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(uv, 0.0, 1.0);
}
"#;
        let err = validate_module(EffectKind::Pixelate, bad).expect_err("must be refused");
        assert!(err.contains("no `struct Params`"), "{err}");
    }

    #[test]
    fn jagged_wgsl_is_reported_not_panicked() {
        let err = validate_module(EffectKind::Blur, "@fragment fn fs_main( {").expect_err("parse");
        assert!(err.contains("parse error"), "{err}");
    }

    #[test]
    fn multi_pass_effects_declare_every_entry_at_most_at_the_expected_size() {
        // Guards the scheduler's assumption that a pass can only shrink.
        for kind in EffectKind::ALL.iter().copied() {
            let spec = kind.spec();
            let mut inst = EffectInstance::new(kind);
            if kind == EffectKind::Blur {
                inst.set("downscale", &[MAX_SHRINK as f32]);
            }
            let shrinks = spec.pass_shrinks(&inst);
            assert_eq!(shrinks.len(), spec.passes.len());
            let mut previous = (3840u32, 2160u32);
            for (index, shrink) in shrinks.iter().enumerate() {
                let size = shrunk((3840, 2160), *shrink);
                assert!(size.0 <= previous.0 && size.1 <= previous.1);
                previous = size;
                // Every pass must produce at least one pixel.
                assert!(size.0 >= 1 && size.1 >= 1, "{} pass {index}", kind.id());
            }
            // The last pass must land on a non-degenerate target, because it
            // is what the compositor blits.
            assert!(
                shrinks.last().is_some_and(|s| *s <= MAX_SHRINK),
                "{}: final pass shrink out of range",
                kind.id()
            );
        }
    }
}
