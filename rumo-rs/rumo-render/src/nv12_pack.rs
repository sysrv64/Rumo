// SPDX-License-Identifier: Apache-2.0

//! NV12 packing on the GPU — the export's last CPU pass, moved.
//!
//! The export used to render a frame on the GPU, read it back as RGBA8, hand it
//! to Java as an `0xAARRGGBB` array, take it back and convert it to NV12 with a
//! scalar loop over every pixel (`rumo_export::nv12`). This module replaces all
//! of that with one compute pass: the composited frame is already a texture, the
//! pass packs its bytes into a storage buffer, and one map returns exactly
//! `nv12_len` bytes for the encoder.
//!
//! # Why a storage buffer, and not `copy_texture_to_buffer`
//!
//! `shader_nv12.wgsl` carries the reasoning; the short version is that a texture
//! copy needs `bytes_per_row` to be a multiple of 256 while an NV12 frame has no
//! row stride at all. A storage buffer has none either, so the frame arrives
//! contiguous and no CPU pass has to strip padding.
//!
//! # What is verified where
//!
//! There is no GPU adapter in the build container, so the compiled pass cannot
//! be run here. What the host tests *can* pin, and do: that the module parses and
//! validates (naga), that it holds no colour constants of its own, that the
//! dispatch geometry matches the shader's workgroup size, and that the index
//! arithmetic written out in Rust puts every byte where the encoder expects it.
//! The arithmetic itself is compared against the CPU reference in
//! `rumo_export::nv12`, where both are visible. Whether the driver accepts the
//! bind group is what the device answers (docs/12 §12.6).

use std::time::Duration;

use wgpu::util::DeviceExt as _;

/// The pack shader, kept as a constant so tests can read it.
pub const SHADER_NV12_WGSL: &str = include_str!("shader_nv12.wgsl");

/// Invocations per workgroup, matching `@workgroup_size` in the shader. A test
/// ties the two together so the dispatch cannot be off by a factor.
pub const WORKGROUP: u32 = 64;

/// The NV12 matrix, as the pack shader consumes it.
///
/// Deliberately a plain value with no constants of its own: the numbers live in
/// `rumo_export::nv12`, next to the CPU reference they are compared against, and
/// merely travel through here — the same arrangement as the YUV matrix
/// (docs/12 §12.3). A shader holding its own matrix would show a green or pink
/// export and run clean.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Nv12Coeffs {
    /// BT.601 full-range luma coefficients, applied to the pixel's RGB.
    pub luma: [f32; 3],
    /// Chroma U coefficients, applied to the 2x2 block average.
    pub chroma_u: [f32; 3],
    /// Chroma V coefficients, applied to the 2x2 block average.
    pub chroma_v: [f32; 3],
    /// Where chroma is centred (128 for BT.601 full range).
    pub bias: f32,
    /// The scale that turns a target's 0..=1 texel back into its byte, 0..=255.
    pub scale: f32,
}

/// Byte length of one NV12 frame, or `None` for zero or odd dimensions.
///
/// The single implementation of the layout: `rumo_export::nv12::nv12_len` is a
/// thin call to this one, so the buffer this module sizes and the frame the
/// encoder is handed cannot disagree about how long a frame is.
pub fn nv12_len(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return None;
    }
    let y = width as usize * height as usize;
    let uv = (width as usize / 2) * (height as usize / 2) * 2;
    Some(y + uv)
}

/// Words of four frame bytes needed to hold `len` bytes.
///
/// A frame is `3/2 * width * height` bytes, which is not always a multiple of
/// four (2x2 is six), so the last word can carry padding. The host hands the
/// encoder `nv12_len` bytes and drops the rest.
pub fn nv12_word_count(len: usize) -> usize {
    len.div_ceil(4)
}

/// Uniform for one pack dispatch; matches `struct Nv12Uniforms` in the shader.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Nv12Uniforms {
    luma: [f32; 4],
    chroma_u: [f32; 4],
    chroma_v: [f32; 4],
    /// x = chroma bias, y = sample scale.
    range: [f32; 4],
    /// x = width, y = height.
    size: [u32; 4],
}

impl Nv12Uniforms {
    fn new(width: u32, height: u32, coeffs: &Nv12Coeffs) -> Self {
        let v4 = |c: [f32; 3]| [c[0], c[1], c[2], 0.0];
        Self {
            luma: v4(coeffs.luma),
            chroma_u: v4(coeffs.chroma_u),
            chroma_v: v4(coeffs.chroma_v),
            range: [coeffs.bias, coeffs.scale, 0.0, 0.0],
            size: [width, height, 0, 0],
        }
    }
}

/// The compiled pack pass, and the buffers one frame needs.
///
/// Created once per renderer and reused for every frame: building a compute
/// pipeline per frame would recompile the shader 300 times over a 10-second
/// export, and the two buffers below would be 6 MiB of driver allocation churn
/// per frame on top of that.
pub struct Nv12Pack {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    /// Buffers of the last packed frame, kept for the next one.
    scratch: Option<Nv12Scratch>,
}

/// One frame's worth of pack buffers.
///
/// Reuse is safe because there is exactly one caller — the export worker, which
/// blocks on the map before returning — so a frame's buffers are never touched
/// again until the next frame asks for them. Recreated only when the frame size
/// changes, which for an export happens when the config changes and not per
/// frame.
struct Nv12Scratch {
    size: wgpu::BufferAddress,
    /// What the compute pass writes: `STORAGE | COPY_SRC`, never mappable.
    storage: wgpu::Buffer,
    /// Where the frame lands for the host: `MAP_READ | COPY_DST`.
    staging: wgpu::Buffer,
}

impl Nv12Pack {
    /// Compile the pack shader and lay out its bind group.
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12 pack shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_NV12_WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nv12 pack bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        // `textureLoad` only, and no sampler binding at all: the
                        // byte the engine rendered is the byte this reads, with
                        // no filter and no `value / 255 * 255` round trip that
                        // could land a code value low.
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nv12 pack pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("nv12 pack pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("cs_nv12"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self {
            pipeline,
            layout,
            scratch: None,
        }
    }

    /// Append the pack pass and its staging copy to `encoder`, submit it, and
    /// return exactly `nv12_len(width, height)` bytes.
    ///
    /// `source` must be an RGBA8 texture holding the composited frame, with
    /// `TEXTURE_BINDING` — the engine's offscreen target, straight from the
    /// render pass, so the frame is never read back as RGBA.
    pub fn pack(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        width: u32,
        height: u32,
        coeffs: &Nv12Coeffs,
        timeout: Duration,
    ) -> Result<Vec<u8>, String> {
        let len = nv12_len(width, height)
            .ok_or_else(|| format!("NV12 needs even, non-zero dimensions, got {width}x{height}"))?;
        // The buffer is word-sized; the tail beyond `len` is padding that never
        // reaches the encoder.
        let buffer_size = (nv12_word_count(len) * 4) as wgpu::BufferAddress;

        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("nv12 uniforms"),
            contents: bytemuck::cast_slice(std::slice::from_ref(&Nv12Uniforms::new(
                width, height, coeffs,
            ))),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        // `STORAGE | COPY_SRC`, never `MAP_READ` as well: without
        // `MAPPABLE_PRIMARY_BUFFERS` a mappable buffer's only other usage may be
        // `COPY_DST`, so the frame is staged through a second buffer.
        //
        // Both buffers are kept between frames of the same size; a size change
        // (a new export config, not a new frame) replaces them.
        if self.scratch.as_ref().map(|s| s.size) != Some(buffer_size) {
            self.scratch = Some(Nv12Scratch {
                size: buffer_size,
                storage: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("nv12 storage"),
                    size: buffer_size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                staging: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("nv12 staging"),
                    size: buffer_size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
            });
        }
        let scratch = self.scratch.as_ref().ok_or("nv12 buffers vanished")?;
        let (storage, staging) = (&scratch.storage, &scratch.staging);
        debug_assert_eq!(scratch.size, buffer_size);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12 pack bind group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: storage.as_entire_binding(),
                },
            ],
        });

        let mut encoder = encoder;
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("nv12 pack"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                (nv12_word_count(len) as u32).div_ceil(WORKGROUP),
                1,
                1,
            );
        }
        encoder.copy_buffer_to_buffer(storage, 0, staging, 0, buffer_size);
        queue.submit(Some(encoder.finish()));

        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        // Every exit from here on unmaps, and the buffer is reused by the next
        // frame: a failure that left it mapped would make that frame's
        // `map_async` invalid, and one bad frame would break the GPU path for the
        // rest of the export. `unmap` terminates an outstanding map request, so
        // it is the right call whether the map completed or not.
        let outcome = device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(timeout),
            })
            .map_err(|e| format!("nv12 gpu poll failed: {e:?}"))
            .and_then(|_status| {
                rx.recv_timeout(timeout + Duration::from_secs(5))
                    .map_err(|e| format!("nv12 map timed out: {e:?}"))?
                    .map_err(|e| format!("nv12 buffer map failed: {e:?}"))
            })
            .and_then(|()| {
                let data = slice
                    .get_mapped_range()
                    .map_err(|e| format!("nv12 mapped range failed: {e:?}"))?;
                let mut out = vec![0u8; len];
                out.copy_from_slice(&data[..len]);
                drop(data);
                Ok(out)
            });
        staging.unmap();
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module the driver will be handed must at least parse and validate.
    /// naga is wgpu's own front end, pinned to the version wgpu bundles, so this
    /// is the same check the device would make — without an adapter.
    #[test]
    fn the_nv12_shader_parses_and_validates() {
        let module = naga::front::wgsl::parse_str(SHADER_NV12_WGSL)
            .unwrap_or_else(|e| panic!("shader_nv12.wgsl does not parse: {}", e.emit_to_string(SHADER_NV12_WGSL)));
        let entry_points: Vec<&str> = module
            .entry_points
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert!(entry_points.contains(&"cs_nv12"), "{entry_points:?}");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("shader_nv12.wgsl does not validate: {e:?}"));
    }

    /// The matrix must arrive in the uniform, not in the source: a wrong
    /// coefficient baked into the shader shows as a tinted export and runs
    /// perfectly clean, so there is deliberately nowhere to put one.
    #[test]
    fn the_nv12_shader_holds_no_colour_constants() {
        for forbidden in [
            "0.299", "0.587", "0.114", "0.168736", "0.331264", "0.418688", "0.081312", "128.0",
        ] {
            assert!(
                !SHADER_NV12_WGSL.contains(forbidden),
                "shader_nv12.wgsl hard-codes {forbidden}; the matrix must come from the uniform"
            );
        }
        for expected in ["u.luma.xyz", "u.chroma_u.xyz", "u.chroma_v.xyz", "u.range.x"] {
            assert!(
                SHADER_NV12_WGSL.contains(expected),
                "shader_nv12.wgsl no longer reads {expected}; the uniform is the only source"
            );
        }
    }

    /// The arithmetic the shader performs is pinned by name, so a rewrite that
    /// silently changes the rounding or the sampling has to change this test too.
    #[test]
    fn the_nv12_shader_keeps_the_pinned_arithmetic() {
        for expected in [
            // No sampler, no filtering: the decoded byte itself.
            "textureLoad(src",
            // The CPU reference's rounding rule, transcribed.
            "clamp(round(v), 0.0, u.range.y)",
            // Luma is f32 in the reference (`rumo_export::nv12`), so it is the
            // same expression here rather than an integer reformulation.
            "dot(u.luma.xyz, rgb)",
            // The 2x2 block average, as the CPU pass reads it.
            "* 0.25",
        ] {
            assert!(
                SHADER_NV12_WGSL.contains(expected),
                "shader_nv12.wgsl no longer contains {expected:?}"
            );
        }
    }

    /// The dispatch counts workgroups of `WORKGROUP` invocations; the shader
    /// declares its own size. A mismatch would silently pack part of a frame.
    #[test]
    fn the_dispatch_geometry_matches_the_shader() {
        assert!(
            SHADER_NV12_WGSL.contains(&format!("@workgroup_size({WORKGROUP})")),
            "the shader's workgroup size is no longer {WORKGROUP}"
        );
    }

    /// One invocation per four bytes, rounded up: a frame is `3/2 * w * h` bytes
    /// and not always a multiple of four.
    #[test]
    fn words_round_up_to_whole_words() {
        assert_eq!(nv12_word_count(0), 0);
        assert_eq!(nv12_word_count(1), 1);
        assert_eq!(nv12_word_count(4), 1);
        assert_eq!(nv12_word_count(5), 2);
        // 2x2 is the smallest frame, and the one that needs padding: 4 luma
        // bytes plus one chroma pair is six bytes, so the second word is half
        // padding.
        let len = nv12_len(2, 2).unwrap();
        assert_eq!(len, 6);
        assert_eq!(nv12_word_count(len), 2);
        // 1080p: 1920 * 1080 * 3 / 2, exactly a whole number of words.
        let len = nv12_len(1920, 1080).unwrap();
        assert_eq!(len, 3_110_400);
        assert_eq!(nv12_word_count(len), 777_600);
        assert_eq!(len % 4, 0);
    }

    /// The layout the encoder is handed: luma row-major for `w * h` bytes, then
    /// interleaved U/V, one pair per 2x2 block, with no row padding anywhere.
    #[test]
    fn the_frame_layout_is_contiguous_planes() {
        let (w, h) = (6u32, 4u32);
        let len = nv12_len(w, h).unwrap();
        assert_eq!(len, 24 + 12);
        assert_eq!(len, (w * h) as usize + (w as usize / 2) * (h as usize / 2) * 2);
        // Both dimensions even is the whole reason the layout holds; odd or zero
        // is refused rather than silently truncated.
        assert_eq!(nv12_len(3, 4), None);
        assert_eq!(nv12_len(4, 3), None);
        assert_eq!(nv12_len(0, 4), None);
    }

    /// The shader's `frame_byte` index arithmetic, transcribed.
    ///
    /// Returns the plane (0 luma, 1 U, 2 V) and, for luma, the pixel the byte
    /// belongs to; for chroma, the 2x2 block it belongs to. This is a
    /// transcription of `shader_nv12.wgsl`, so it proves the *formulation* rather
    /// than the compiled module — what the device answers is a separate question
    /// (docs/12 §12.6). What it does catch is the class of mistake that survives
    /// review: a transposed x/y, a plane offset one block out, a byte that
    /// straddles the luma/chroma boundary.
    fn model_index(index: u32, w: u32, h: u32) -> (u8, u32, u32) {
        let y_len = w * h;
        if index < y_len {
            return (0, index % w, index / w);
        }
        let chroma = index - y_len;
        let cw = w / 2;
        let block = chroma / 2;
        let component = if chroma % 2 == 0 { 1 } else { 2 };
        (component, block % cw, block / cw)
    }

    /// Every byte of a frame belongs to exactly one place, and every place gets
    /// exactly one byte: the luma plane is a complete row-major image, the
    /// chroma plane a complete set of 2x2 blocks with U before V.
    ///
    /// A transpose, an off-by-one plane split or a word that straddles the
    /// boundary all break one of those two counts, which is exactly what a
    /// mis-specified index expression does in practice.
    #[test]
    fn every_frame_byte_lands_in_one_place_and_every_place_gets_one() {
        for (w, h) in [(2u32, 2u32), (6, 4), (16, 2), (10, 10), (14, 6)] {
            let len = nv12_len(w, h).unwrap() as u32;
            let mut luma = vec![false; (w * h) as usize];
            let mut chroma = vec![false; ((w / 2) * (h / 2) * 2) as usize];
            for index in 0..len {
                let (plane, x, y) = model_index(index, w, h);
                match plane {
                    0 => {
                        assert!(x < w && y < h, "{w}x{h}: luma {x},{y} out of frame");
                        let slot = (y * w + x) as usize;
                        assert!(!luma[slot], "{w}x{h}: luma {x},{y} written twice");
                        luma[slot] = true;
                    }
                    1 | 2 => {
                        assert!(x < w / 2 && y < h / 2, "{w}x{h}: chroma {x},{y} out of frame");
                        let slot = ((y * (w / 2) + x) * 2 + (u32::from(plane) - 1)) as usize;
                        assert!(!chroma[slot], "{w}x{h}: chroma {x},{y} p{plane} twice");
                        chroma[slot] = true;
                    }
                    other => panic!("{w}x{h}: unknown plane {other}"),
                }
            }
            assert!(luma.iter().all(|seen| *seen), "{w}x{h}: luma plane has a hole");
            assert!(
                chroma.iter().all(|seen| *seen),
                "{w}x{h}: chroma plane has a hole"
            );
        }
    }

    /// The word expression in `cs_nv12` is little-endian, which is the order the
    /// host maps the buffer in. Byte-swapped, a whole export would arrive as
    /// noise — and nothing else in the pipeline would notice.
    #[test]
    fn the_packed_word_is_little_endian() {
        // The shader's expression, transcribed: b0 in the low byte.
        let pack = |bytes: [u8; 4]| {
            u32::from(bytes[0])
                | (u32::from(bytes[1]) << 8)
                | (u32::from(bytes[2]) << 16)
                | (u32::from(bytes[3]) << 24)
        };
        for bytes in [[0u8, 0, 0, 0], [1, 2, 3, 4], [255, 0, 255, 0], [7, 7, 7, 7]] {
            assert_eq!(pack(bytes), u32::from_le_bytes(bytes), "{bytes:?}");
        }
    }

    /// The uniform is what the shader reads; a field landing in the wrong slot
    /// would shift the whole matrix. 16-byte alignment, no padding surprises.
    #[test]
    fn the_uniform_has_the_shader_layout() {
        assert_eq!(std::mem::size_of::<Nv12Uniforms>(), 80);
        assert_eq!(std::mem::size_of::<Nv12Uniforms>() % 16, 0);
        let u = Nv12Uniforms::new(
            1920,
            1080,
            &Nv12Coeffs {
                luma: [1.0, 2.0, 3.0],
                chroma_u: [4.0, 5.0, 6.0],
                chroma_v: [7.0, 8.0, 9.0],
                bias: 128.0,
                scale: 255.0,
            },
        );
        assert_eq!(u.luma, [1.0, 2.0, 3.0, 0.0]);
        assert_eq!(u.chroma_u, [4.0, 5.0, 6.0, 0.0]);
        assert_eq!(u.chroma_v, [7.0, 8.0, 9.0, 0.0]);
        assert_eq!(u.range[0], 128.0, "bias is the first range slot");
        assert_eq!(u.range[1], 255.0, "scale is the second");
        assert_eq!(u.size[0], 1920);
        assert_eq!(u.size[1], 1080);
    }
}
