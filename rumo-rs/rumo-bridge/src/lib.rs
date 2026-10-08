// SPDX-License-Identifier: Apache-2.0
use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::jni_str;
use jni::objects::{JByteArray, JClass, JObject, JString, Reference as _};
use jni::sys::{jbyteArray, jfloat, jfloatArray, jint, jlong, jstring};

/// Version of the underlying `rumo_core` engine.
pub fn bridge_version() -> &'static str {
    rumo_core::RUMO_CORE_VERSION
}

/// Build a new project with `name` and return its encoded bytes.
pub fn encode_new_project(name: &str) -> Vec<u8> {
    let project = rumo_core::model::Project::new(name);
    rumo_core::codec::encode(&project)
}

/// Number of fill triangles for the shape at `ordinal` in
/// `rumo_render::all_shapes()` order with default params.
///
/// Out-of-range ordinals (and a `None` path, which default params
/// should never produce) yield -1.
pub fn shape_tris(ordinal: usize) -> i32 {
    let shapes = rumo_render::all_shapes();
    let kind = match shapes.get(ordinal) {
        Some(k) => *k,
        None => return -1,
    };
    match rumo_render::shape_path(kind, rumo_render::ShapeParams::default()) {
        Some(path) => rumo_render::tessellate(&path) as i32,
        None => -1,
    }
}

/// Triangulated 2D shape geometry in a `size_px` box (origin top-left).
pub struct Mesh {
    pub xs: Vec<f32>,
    pub ys: Vec<f32>,
    pub tris: Vec<u32>,
}

/// Fill-tessellates the shape at `ordinal` (`rumo_render::all_shapes()` order)
/// with the given pixel-space params into a [`Mesh`].
///
/// Positions are the tessellator's centred vertices shifted into `0..size_px`.
/// Out-of-range ordinals, invalid params, and tessellation failures yield `None`.
pub fn shape_mesh(ordinal: usize, size_px: f32, rounding: f32, rotation_deg: f32) -> Option<Mesh> {
    let kind = *rumo_render::all_shapes().get(ordinal)?;
    let params = rumo_render::ShapeParams {
        size_px,
        corner_rounding: rounding,
        rotation_deg,
    }
    .validated()?;
    let path = rumo_render::shape_path(kind, params)?;

    let tol = f64::from(size_px) / 512.0;
    let mut flat: Vec<kurbo::PathEl> = Vec::new();
    kurbo::flatten(path.elements().iter().cloned(), tol, |el| {
        flat.push(el);
    });

    use lyon_tessellation::path::PathEvent;
    use lyon_tessellation::path::math::Point;
    let mut events: Vec<PathEvent> = Vec::new();
    let mut first: Option<Point> = None;
    let mut current: Option<Point> = None;
    for el in flat {
        match el {
            kurbo::PathEl::MoveTo(p) => {
                if let (Some(f), Some(c)) = (first, current) {
                    events.push(PathEvent::End {
                        last: c,
                        first: f,
                        close: false,
                    });
                }
                let at = Point::new(p.x as f32, p.y as f32);
                events.push(PathEvent::Begin { at });
                first = Some(at);
                current = Some(at);
            }
            kurbo::PathEl::LineTo(p) => {
                let to = Point::new(p.x as f32, p.y as f32);
                match current {
                    Some(from) => events.push(PathEvent::Line { from, to }),
                    None => {
                        events.push(PathEvent::Begin { at: to });
                        first = Some(to);
                    }
                }
                current = Some(to);
            }
            kurbo::PathEl::ClosePath => {
                if let (Some(f), Some(c)) = (first, current) {
                    events.push(PathEvent::End {
                        last: c,
                        first: f,
                        close: true,
                    });
                }
                first = None;
                current = None;
            }
            _ => {}
        }
    }
    if let (Some(f), Some(c)) = (first, current) {
        events.push(PathEvent::End {
            last: c,
            first: f,
            close: false,
        });
    }
    if events.is_empty() {
        return None;
    }

    let mut buffers: lyon_tessellation::VertexBuffers<Point, u32> =
        lyon_tessellation::VertexBuffers::new();
    {
        let mut builder = lyon_tessellation::BuffersBuilder::new(
            &mut buffers,
            |v: lyon_tessellation::FillVertex| v.position(),
        );
        let mut tess = lyon_tessellation::FillTessellator::new();
        if tess
            .tessellate(
                events,
                &lyon_tessellation::FillOptions::default(),
                &mut builder,
            )
            .is_err()
        {
            return None;
        }
    }
    if buffers.indices.is_empty() {
        return None;
    }
    let half = size_px / 2.0;
    let xs: Vec<f32> = buffers.vertices.iter().map(|v| v.x + half).collect();
    let ys: Vec<f32> = buffers.vertices.iter().map(|v| v.y + half).collect();
    Some(Mesh {
        xs,
        ys,
        tris: buffers.indices,
    })
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeVersion (static via @JvmStatic).
///
/// Errors throw `java/lang/RuntimeException` and return null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeVersion(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        let s = env.new_string(bridge_version())?;
        Ok(s.as_raw() as jstring)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeEncodeProject (static via @JvmStatic).
///
/// Errors throw `java/lang/RuntimeException` and return null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEncodeProject(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    name: JString<'_>,
) -> jbyteArray {
    env.with_env(|env| -> jni::errors::Result<jbyteArray> {
        let name = name.try_to_string(env)?;
        let bytes = encode_new_project(&name);
        let arr = env.byte_array_from_slice(&bytes)?;
        Ok(arr.as_raw() as jbyteArray)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// Minimal JSON string escaping for an error message.
///
/// Local, following the same reasoning as `rumo_media::audio_jni`'s copy: the
/// reason can quote a layer name or a UUID from model-written JSON, and a quote
/// or a newline in it must not be able to break the object the app then parses.
fn json_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if (c as u32) < 0x20 => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeApplyEdl (static via @JvmStatic).
///
/// Applies a batch of editing operations to a project in one pass and returns the
/// new project JSON.
///
/// ## Why one call instead of ten
///
/// Every narrow edit tool is a round trip and an undo step of its own. A hundred
/// cuts on a long timeline is a hundred of each, and the failure that matters is
/// not speed: it is that the model's picture of the timeline and the timeline
/// itself drift apart between calls, so cut ninety is computed against a document
/// that no longer exists. One call takes the project and the whole operation list
/// together, so the batch is atomic and the answer is the document the next step
/// is actually computed against.
///
/// Errors — unparseable project JSON, unparseable ops JSON — throw
/// `java/lang/RuntimeException` and return null. Individual operations that cannot
/// apply do **not** fail the batch: they are reported in the result's `skipped`
/// array with a reason, because a batch of a hundred cuts should not be thrown
/// away over one bad index.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeApplyEdl(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    project_json: JString<'_>,
    ops_json: JString<'_>,
) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        let project_json = project_json.try_to_string(env)?;
        let ops_json = ops_json.try_to_string(env)?;
        // A panic must never unwind into the JVM, and this entry runs model-written
        // JSON through the whole codec, so the guard is not decoration.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rumo_core::edl::apply_edl_json(&project_json, &ops_json)
        }));
        let payload = match result {
            Ok(Ok(json)) => json,
            Ok(Err(reason)) => format!(
                "{{\"ok\":false,\"error\":{}}}",
                json_escape(&reason)
            ),
            Err(_) => "{\"ok\":false,\"error\":\"the editor core panicked while applying the batch\"}"
                .to_string(),
        };
        let out = env.new_string(payload)?;
        Ok(out.as_raw() as jstring)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeShapeTris (static via @JvmStatic).
///
/// Negative ordinals throw `java/lang/RuntimeException` and return -1;
/// out-of-range non-negative ordinals return -1 without throwing.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeShapeTris(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    ordinal: jint,
) -> jint {
    if ordinal < 0 {
        let _ = env
            .with_env(|env| -> jni::errors::Result<()> {
                env.throw_new(
                    jni_str!("java/lang/RuntimeException"),
                    jni_str!("negative shape ordinal"),
                )?;
                Ok(())
            })
            .resolve::<ThrowRuntimeExAndDefault>();
        return -1;
    }
    shape_tris(ordinal as usize)
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeShapeMesh (static via @JvmStatic).
///
/// Flat `[x0,y0,x1,y1,...]` of the triangulated vertices in triangle order
/// (indices expanded: Kotlin draws `drawPath` triangles with no index buffer).
/// Errors throw `java/lang/RuntimeException` and return null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeShapeMesh(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    ordinal: jint,
    size_px: jfloat,
    rounding: jfloat,
    rotation_deg: jfloat,
) -> jfloatArray {
    if ordinal < 0 {
        let _ = env
            .with_env(|env| -> jni::errors::Result<()> {
                env.throw_new(
                    jni_str!("java/lang/RuntimeException"),
                    jni_str!("negative shape ordinal"),
                )?;
                Ok(())
            })
            .resolve::<ThrowRuntimeExAndDefault>();
        return std::ptr::null_mut();
    }
    let mesh = match shape_mesh(ordinal as usize, size_px, rounding, rotation_deg) {
        Some(m) => m,
        None => {
            let _ = env
                .with_env(|env| -> jni::errors::Result<()> {
                    env.throw_new(
                        jni_str!("java/lang/RuntimeException"),
                        jni_str!("shape mesh failed"),
                    )?;
                    Ok(())
                })
                .resolve::<ThrowRuntimeExAndDefault>();
            return std::ptr::null_mut();
        }
    };
    let mut flat: Vec<jfloat> = Vec::with_capacity(mesh.tris.len() * 2);
    for i in mesh.tris {
        flat.push(mesh.xs[i as usize]);
        flat.push(mesh.ys[i as usize]);
    }
    env.with_env(|env| -> jni::errors::Result<jfloatArray> {
        let arr = env.new_float_array(flat.len())?;
        arr.set_region(env, 0, &flat)?;
        Ok(arr.as_raw() as jfloatArray)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeRenderPath (static via @JvmStatic).
///
/// Which path rendered the last preview: `"gpu"` or `"cpu"`. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeRenderPath(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
) -> jni::sys::jstring {
    env.with_env(|env| -> jni::errors::Result<jni::sys::jstring> {
        let s = env.new_string(render_path())?;
        Ok(s.as_raw() as jni::sys::jstring)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeProjectFromJson (static via @JvmStatic).
///
/// Parses editor JSON into `.rumo` codec bytes.
/// Errors throw `java/lang/RuntimeException` and return null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeProjectFromJson(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    json: JString<'_>,
) -> jbyteArray {
    env.with_env(|env| -> jni::errors::Result<jbyteArray> {
        let json = json.try_to_string(env)?;
        let bytes = rumo_core::project_json::project_from_json(&json)
            .map_err(|_| jni::errors::Error::JniCall(jni::errors::JniError::Unknown))?;
        let arr = env.byte_array_from_slice(&bytes)?;
        Ok(arr.as_raw() as jbyteArray)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeProjectToJson (static via @JvmStatic).
///
/// Decodes `.rumo` bytes back into editor JSON.
/// Errors throw `java/lang/RuntimeException` and return null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeProjectToJson(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    data: JByteArray<'_>,
) -> jstring {
    env.with_env(|env| -> jni::errors::Result<jstring> {
        let bytes = env.convert_byte_array(&data)?;
        let json = rumo_core::project_json::project_to_json(&bytes)
            .map_err(|_| jni::errors::Error::JniCall(jni::errors::JniError::Unknown))?;
        let s = env.new_string(json)?;
        Ok(s.as_raw() as jstring)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// Pack a decoded image as flat `[w:u32 LE][h:u32 LE][rgba...]` for Kotlin.
pub fn pack_image_flat(img: &rumo_media::RgbaImage) -> Vec<u8> {
    let mut flat = Vec::with_capacity(8 + img.rgba.len());
    flat.extend_from_slice(&img.width.to_le_bytes());
    flat.extend_from_slice(&img.height.to_le_bytes());
    flat.extend_from_slice(&img.rgba);
    flat
}

/// Parse the `[w:u32 LE][h:u32 LE]` header of [`pack_image_flat`] output.
pub fn parse_image_flat_header(flat: &[u8]) -> Option<(u32, u32)> {
    if flat.len() < 8 {
        return None;
    }
    let w = u32::from_le_bytes(flat[0..4].try_into().ok()?);
    let h = u32::from_le_bytes(flat[4..8].try_into().ok()?);
    Some((w, h))
}

/// Decode a whole image file to flat `[w LE][h LE][rgba...]` pixels,
/// or `None` when the bytes are not a decodable image.
pub fn decode_image_flat(bytes: &[u8], max_side: u32) -> Option<Vec<u8>> {
    let img = rumo_media::decode_image_rgba(bytes, max_side)?;
    Some(pack_image_flat(&img))
}

/// Duration of the audio visible through `fd` (opened by Kotlin) in
/// milliseconds, or -1 when unknown. Missing duration is not fatal,
/// so callers must not throw on -1.
pub fn audio_duration_for_fd(fd: i32) -> i64 {
    if fd < 0 {
        return -1;
    }
    let path = format!("/proc/self/fd/{fd}");
    match rumo_media::probe_audio_file(&path) {
        Some(ms) => ms as i64,
        None => -1,
    }
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeDecodeImage (static via @JvmStatic).
///
/// Input is a whole image file; output is flat `[w:u32 LE][h:u32 LE][rgba...]`.
/// Errors throw `java/lang/RuntimeException` and return null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeDecodeImage(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    bytes: JByteArray<'_>,
    max_side: jint,
) -> jbyteArray {
    if max_side <= 0 {
        let _ = env
            .with_env(|env| -> jni::errors::Result<()> {
                env.throw_new(
                    jni_str!("java/lang/RuntimeException"),
                    jni_str!("non-positive maxSide"),
                )?;
                Ok(())
            })
            .resolve::<ThrowRuntimeExAndDefault>();
        return std::ptr::null_mut();
    }
    env.with_env(|env| -> jni::errors::Result<jbyteArray> {
        let input = env.convert_byte_array(&bytes)?;
        let flat = decode_image_flat(&input, max_side as u32)
            .ok_or(jni::errors::Error::JniCall(jni::errors::JniError::Unknown))?;
        let arr = env.byte_array_from_slice(&flat)?;
        Ok(arr.as_raw() as jbyteArray)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeAudioDurationFd (static via @JvmStatic).
///
/// Reads `/proc/self/fd/<fd>` (opened by Kotlin) and returns the audio
/// duration in milliseconds, or -1 when unknown. Never throws: a missing
/// duration is not fatal.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioDurationFd(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    fd: jint,
) -> jlong {
    audio_duration_for_fd(fd) as jlong
}

/// Map a `rumo_media` packing onto the renderer's own enum.
///
/// The two crates cannot depend on each other (`rumo_media` must not pull in
/// `wgpu`, and `rumo_render` must not pull in a decoder), so the packing is
/// stated twice and mapped here — exhaustively, so a new packing is a compile
/// error at this one `match` rather than a silently wrong swizzle in a shader.
fn yuv_format(format: rumo_media::video::yuv::YuvFormat) -> rumo_render::texture::YuvFormat {
    match format {
        rumo_media::video::yuv::YuvFormat::I420 => rumo_render::texture::YuvFormat::I420,
        rumo_media::video::yuv::YuvFormat::Nv12 => rumo_render::texture::YuvFormat::Nv12,
        rumo_media::video::yuv::YuvFormat::Nv21 => rumo_render::texture::YuvFormat::Nv21,
    }
}

/// Turn a decoded frame's planes into the renderer's texture payload.
///
/// What crosses this boundary is a description, not pixels: where each plane
/// starts, how wide its rows are, the crop, the coefficients the GPU has to
/// multiply with, and the four UV corners that fold in the decoder's rotation.
/// The buffer itself is shared by reference — this converts nothing.
fn yuv_payload(
    planes: &rumo_media::video::yuv::YuvPlanes,
) -> Result<rumo_render::texture::YuvTexture, String> {
    let desc = *planes.desc();
    let corners = planes.corner_uvs()?;
    // The description has already resolved every packing's plane geometry — the
    // interleaved ones read chroma with the *luma* stride and have no third
    // plane — so the renderer is handed that resolution verbatim.
    let mut plane_width = [0u32; 3];
    let mut plane_height = [0u32; 3];
    for index in 0..3 {
        plane_width[index] = desc.row_stride[index];
        plane_height[index] = desc.plane_rows[index];
    }
    rumo_render::texture::YuvTexture::new(
        yuv_format(desc.format),
        desc.width,
        desc.height,
        corners,
        planes.coeffs(),
        std::sync::Arc::clone(planes.bytes()),
        desc.plane_offset,
        desc.plane_len,
        plane_width,
        plane_height,
    )
}

/// `RumoBridge.nativeVideoTextureAt(handle, timeMs)`: decode the frame covering
/// `timeMs` and register it in the renderer's texture registry, returning the
/// texture id (`0` on any failure).
///
/// This is where the two halves of video support meet: `rumo-media` owns the
/// decoder and `rumo-render` owns the texture registry, and neither crate
/// depends on the other, so the glue lives in this cdylib. Decoding and
/// registering in one step also keeps a decoded frame out of a Java byte array
/// — a 1080p RGBA frame is 8 MB and the preview loop would otherwise copy it
/// twice per frame.
///
/// The frame is registered **as planes**: the decoder's own buffers plus a
/// description, so the YCbCr → RGBA8 conversion happens on the GPU while the
/// frame is already being sampled, instead of on the CPU for 2.07M pixels
/// before it gets there (docs/12 §12.3). When this build's decoder cannot hand
/// its planes over — no backend has installed a planes reader, see
/// [`rumo_media::video_jni::set_planes_reader`] — the RGBA8 path below runs
/// instead, which is what every clip looked like until this step.
///
/// The returned id belongs to the caller and must be released with
/// `nativeFreeTexture`; a handle closed with `nativeVideoClose` yields `0`.
///
/// Installs the planes reader on first use, once per process. The reader is a
/// plain function pointer behind a `OnceLock`, so installing it here means the
/// decoder and the renderer cannot disagree about which path a clip takes: one
/// decision, made once, at the first video texture.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoTextureAt(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    time_ms: jlong,
) -> jlong {
    // Install the planes reader **before** the first attempt, not after it.
    // Doing it afterwards meant the first video frame of the process was decoded
    // twice — once through the RGBA path (which converts every pixel) and once
    // through the planes path that replaced it — because the first attempt ran
    // against a decoder that had no reader yet. One frame per process is a small
    // bill, but it is paid in the worst place: the moment a clip first appears.
    rumo_media::video_jni::set_planes_reader(|handle, time_ms| {
        rumo_media::video_jni::planes_for_frame(handle, time_ms)
    });

    // A panic must never cross the FFI boundary; a failed decode is 0.
    let as_planes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rumo_media::video_jni::planes_for_compositor(handle, time_ms)
    }));
    if let Ok(Some(planes)) = as_planes {
        match yuv_payload(&planes) {
            Ok(frame) => {
                if let Some(id) = rumo_render::jni::upload_yuv_frame(frame) {
                    return id as jlong;
                }
            }
            Err(reason) => {
                // Refusing a frame we cannot sample safely must not be silent —
                // the converted path below still draws it — but it also must not
                // fill the diagnostics ring once per layer per frame, so the
                // first refusal is the one that gets recorded.
                static ONCE: std::sync::Once = std::sync::Once::new();
                ONCE.call_once(|| {
                    rumo_render::diag::warn("video_planes_refused", reason);
                });
            }
        }
    }
    // The planes path did not answer, so the frame is fetched as pixels — the
    // legacy shape, and the only one left that costs a conversion.
    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rumo_media::video_jni::frame_for_compositor(handle, time_ms)
    }));
    let Ok(Some((width, height, rgba))) = decoded else {
        return 0;
    };
    rumo_render::jni::upload_image(width, height, rgba).unwrap_or(0) as jlong
}

/// One SHAPE layer for [`render_preview`]: shape ordinal in
/// `rumo_render::all_shapes()` order plus pixel-space placement.
pub struct PreviewLayer {    pub ordinal: usize,
    pub argb: u32,
    pub dx: f32,
    pub dy: f32,
    pub rotation_deg: f32,
    pub alpha: f32,
}

/// 0xAARRGGBB (like Kotlin `Color(argb)`) → linear f32 RGBA.
fn argb_to_f32(argb: u32, alpha_mul: f32) -> [f32; 4] {
    let a = ((argb >> 24) & 0xFF) as f32 / 255.0 * alpha_mul.clamp(0.0, 1.0);
    [
        ((argb >> 16) & 0xFF) as f32 / 255.0,
        ((argb >> 8) & 0xFF) as f32 / 255.0,
        (argb & 0xFF) as f32 / 255.0,
        a,
    ]
}

/// 0xAARRGGBB → the engine's packed LE-RGBA `u32` (R in the low byte).
/// Test oracle for the boundary pair: production goes through `argb_to_f32`,
/// the reverse pack goes through [`rgba_u32_to_argb`] once, on JNI output.
#[cfg(test)]
fn argb_to_rgba_u32(argb: u32) -> u32 {
    let a = (argb >> 24) & 0xFF;
    let r = (argb >> 16) & 0xFF;
    let g = (argb >> 8) & 0xFF;
    let b = argb & 0xFF;
    r | (g << 8) | (b << 16) | (a << 24)
}

/// Linear f32 RGBA → the engine's packed LE-RGBA `u32`.
fn f32_to_rgba_u32(c: [f32; 4]) -> u32 {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    ch(c[0]) | (ch(c[1]) << 8) | (ch(c[2]) << 16) | (ch(c[3]) << 24)
}

/// The engine's packed LE-RGBA `u32` → 0xAARRGGBB.
///
/// The ONLY place pixels are converted toward Android:
/// the engine's surface path is zero-copy with no shuffles (RGBA8 from shader to
/// swapchain), while legacy `nativeRenderPreview` has to return an `int[]` for
/// `Bitmap.Config.ARGB_8888` — so the pack is applied once on JNI output,
/// never inside the engine's per-frame paths.
fn rgba_u32_to_argb(px: u32) -> u32 {
    let r = px & 0xFF;
    let g = (px >> 8) & 0xFF;
    let b = (px >> 16) & 0xFF;
    let a = (px >> 24) & 0xFF;
    (a << 24) | (r << 16) | (g << 8) | b
}

/// Pixel-space → NDC column-major matrix for a `w`×`h` frame.
fn ndc_matrix(w: f32, h: f32) -> [[f32; 4]; 4] {
    [
        [2.0 / w, 0.0, 0.0, 0.0],
        [0.0, -2.0 / h, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0, 1.0],
    ]
}

/// Tessellate + place SHAPE `layers` for a `width`×`height` frame:
/// each layer at 256px, scaled to 60% of frame height, rotated, moved to
/// frame centre + (`dx`,`dy`), with its NDC matrix and f32 color.
/// Unknown ordinals are skipped.
fn preview_meshes(
    width: u32,
    height: u32,
    layers: &[PreviewLayer],
) -> Vec<(rumo_render::renderer::MeshData, [f32; 4], [[f32; 4]; 4])> {
    use rumo_render::renderer::MeshData;
    let mat = ndc_matrix(width as f32, height as f32);
    let base = 256.0f32;
    let scale = height as f32 * 0.6 / base;
    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;
    let mut out = Vec::new();
    for l in layers {
        let mesh = match shape_mesh(l.ordinal, base, 0.0, l.rotation_deg) {
            Some(m) if !m.tris.is_empty() => m,
            _ => continue,
        };
        let verts: Vec<[f32; 2]> = mesh
            .xs
            .iter()
            .zip(mesh.ys.iter())
            .map(|(x, y)| {
                [
                    (x - base / 2.0) * scale + cx + l.dx,
                    (y - base / 2.0) * scale + cy + l.dy,
                ]
            })
            .collect();
        out.push((
            MeshData {
                vertices: verts,
                indices: mesh.tris,
            },
            argb_to_f32(l.argb, l.alpha),
            mat,
        ));
    }
    out
}

/// CPU compositor: reference pixels, always available. Input `bg` is
/// linear RGBA, output is engine RGBA8 `u32` (R in the low byte) — no ARGB
/// anywhere inside.
fn render_preview_cpu(
    width: u32,
    height: u32,
    bg: [f32; 4],
    layers: &[PreviewLayer],
) -> Vec<u32> {
    use rumo_render::renderer::{RenderConfig, blend_over, render_frame_cpu};
    let n = width as usize * height as usize;
    let mut frame = vec![f32_to_rgba_u32(bg); n];
    if width == 0 || height == 0 {
        return frame;
    }
    let cfg = RenderConfig {
        width,
        height,
        clear_color: [0.0, 0.0, 0.0, 0.0],
    };
    for (md, color, mat) in preview_meshes(width, height, layers) {
        let layer_px = render_frame_cpu(&md, &cfg, &mat, color);
        blend_over(&mut frame, &layer_px);
    }
    frame
}

use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};

/// One GPU preview job for the worker thread below.
struct PreviewJob {
    width: u32,
    height: u32,
    bg: [f32; 4],
    triples: Vec<(
        rumo_render::renderer::MeshData,
        [f32; 4],
        [[f32; 4]; 4],
    )>,
    reply: Sender<Option<Vec<u32>>>,
}

/// Sender to the single preview worker thread (lazily spawned). The GPU
/// context lives on that thread — never a global `static GPU`, never a
/// `block_on` on a JNI thread.
fn preview_tx() -> Option<Sender<PreviewJob>> {
    static CELL: std::sync::OnceLock<Mutex<Sender<PreviewJob>>> = std::sync::OnceLock::new();
    let sender = CELL.get_or_init(|| {
        let (tx, rx) = channel::<PreviewJob>();
        std::thread::Builder::new()
            .name("rumo-preview".into())
            .spawn(move || {
                let mut gpu: Option<rumo_render::renderer::GpuRenderer> = None;
                while let Ok(job) = rx.recv() {
                    let out = render_preview_gpu_on_thread(
                        &mut gpu,
                        job.width,
                        job.height,
                        job.bg,
                        &job.triples,
                    );
                    let _ = job.reply.send(out);
                }
            })
            .expect("preview worker thread must spawn");
        Mutex::new(tx)
    });
    sender.lock().ok().map(|guard| guard.clone())
}

/// Run one GPU frame on the preview worker thread: lazy init, one submit,
/// bounded readback wait. `None` on any failure (no driver, timeout,
/// validation) — callers fall back to [`render_preview_cpu`]. A dead
/// context is dropped so the next frame retries instead of hanging.
fn render_preview_gpu_on_thread(
    gpu: &mut Option<rumo_render::renderer::GpuRenderer>,
    width: u32,
    height: u32,
    bg: [f32; 4],
    triples: &[(
        rumo_render::renderer::MeshData,
        [f32; 4],
        [[f32; 4]; 4],
    )],
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    if gpu.is_none() {
        // block_on lives ONLY on this worker thread, never on JNI.
        match pollster::block_on(rumo_render::renderer::GpuRenderer::new()) {
            Ok(ctx) => *gpu = Some(ctx),
            Err(_) => return None,
        }
    }
    let ctx = gpu.as_ref()?;
    let timeout = std::time::Duration::from_secs(5);
    match pollster::block_on(ctx.render_layers(width, height, bg, triples, timeout)) {
        Ok(px) => Some(px),
        Err(_) => {
            *gpu = None;
            None
        }
    }
}
/// Which path rendered the last preview: true = GPU, false = CPU.
static LAST_GPU_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// GPU compositor via the preview worker thread. The JNI thread only
/// builds mesh triples (fast CPU tessellation) and waits at most 6s for
/// the answer; expiry (or any worker failure) yields `None` and the caller
/// falls back to [`render_preview_cpu`]. Never blocks indefinitely.
fn render_preview_gpu(
    width: u32,
    height: u32,
    bg: [f32; 4],
    layers: &[PreviewLayer],
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let triples = preview_meshes(width, height, layers);
    let (reply_tx, reply_rx) = channel();
    let tx = preview_tx()?;
    tx.send(PreviewJob {
        width,
        height,
        bg,
        triples,
        reply: reply_tx,
    })
    .ok()?;
    reply_rx
        .recv_timeout(std::time::Duration::from_secs(6))
        .ok()
        .flatten()
}

/// Composite SHAPE `layers` over linear-RGBA `bg` into a `width`×`height`
/// frame of engine RGBA8 `u32` (R in the low byte): GPU first (worker
/// thread, bounded wait), CPU reference on any GPU failure. Partly
/// off-frame triangles are skipped whole (no Sutherland clipping —
/// preview-grade, not export).
///
/// `try_gpu = false` pins the CPU path (tests: no driver exists in the
/// container, so unit tests must never touch the GPU).
pub fn render_preview_inner(
    width: u32,
    height: u32,
    bg: [f32; 4],
    layers: &[PreviewLayer],
    try_gpu: bool,
) -> Vec<u32> {
    if try_gpu {
        if let Some(px) = render_preview_gpu(width, height, bg, layers) {
            LAST_GPU_OK.store(true, std::sync::atomic::Ordering::Relaxed);
            return px;
        }
        LAST_GPU_OK.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    render_preview_cpu(width, height, bg, layers)
}

pub fn render_preview(
    width: u32,
    height: u32,
    bg: [f32; 4],
    layers: &[PreviewLayer],
) -> Vec<u32> {
    render_preview_inner(width, height, bg, layers, true)
}

/// Keep the extended JNI entries from `rumo_render::jni` linked into this
/// cdylib: a `#[used]` address table defeats dead-code elimination of the
/// `#[no_mangle]` objects, same guarantee the legacy entries rely on.
#[used]
static LINK_RENDER_JNI_EX: [std::sync::atomic::AtomicPtr<()>; 7] = [
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeRenderPreviewEx as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeEngineSetLayersEx as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_export::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeExportBegin as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_export::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeExportLastError
            as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_export::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeExportEnd as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_export::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeExportAudioTrack as *mut (),
    ),
    // The export's GPU path (docs/12 §12.3): the frame is composited and packed
    // into NV12 without leaving the device. Kotlin reaches it directly and
    // nothing in Rust calls it, so it needs the same anchor as the rest.
    std::sync::atomic::AtomicPtr::new(
        rumo_export::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeExportWriteFrameGpu
            as *mut (),
    ),
];

/// Keep the text-layout JNI entries from `rumo_render::jni` linked in: Kotlin
/// reaches them directly and nothing in Rust calls them, so without an anchor
/// the linker may drop the objects and Java would see
/// `UnsatisfiedLinkError` — the layout handle is what the whole text path
/// hangs off, so losing it would take text down with it.
#[used]
static LINK_RENDER_JNI_TEXT: [std::sync::atomic::AtomicPtr<()>; 10] = [
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutText as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutTextStyled
            as *mut (),
    ),
    // The shop's three entries: registering a downloaded face, shaping with an
    // explicit family, and rasterising a preview. Same reason as the rest —
    // Kotlin calls them and Rust does not.
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeFontRegister as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutTextFamily
            as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeFontPreview as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutQuadCount
            as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutQuad as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutBounds as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutAtlas as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeLayoutFree as *mut (),
    ),
];

/// Keep the resolution/effect-catalogue JNI entries from `rumo_render::jni`
/// linked in: nothing in Rust calls them, so without an anchor the linker may
/// drop the objects and Java would see `UnsatisfiedLinkError`.
#[used]
static LINK_RENDER_JNI_INFO: [std::sync::atomic::AtomicPtr<()>; 7] = [
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeRenderDiagnostics
            as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeClearRenderDiagnostics
            as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeResolutionPresets
            as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeBitrateFor as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeEffectCatalogue
            as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeEffectValidate
            as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeEffectCatalogueEx
            as *mut (),
    ),
];

/// Keep the SVG-registry JNI entries from `rumo_render::jni` linked in: Kotlin
/// reaches them directly and nothing in Rust calls them, so without an anchor
/// the linker may drop the objects and Java would see `UnsatisfiedLinkError`.
/// A document is parsed once and then drawn from the registry every frame, so
/// losing the register entry would take every SVG layer down with it. The
/// rasterising fallback is anchored here too: Kotlin reaches it directly, and a
/// document the vector path refuses would otherwise lose its whole layer. The
/// validate entry anchors the `svg_paint` tool's pre-flight check, which must
/// answer honestly on an old `.so` rather than appear to succeed.
#[used]
static LINK_RENDER_JNI_SVG: [std::sync::atomic::AtomicPtr<()>; 4] = [
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeSvgRegister as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeSvgRelease as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeSvgRasterize as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_render::jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeSvgValidate as *mut (),
    ),
];

/// Keep the video-decode JNI entries from `rumo_media::video_jni` linked into
/// this cdylib. Same `#[used]` trick as [`LINK_RENDER_JNI_EX`]: the entries are
/// reachable only from Java, so nothing in Rust references them and the linker
/// would otherwise drop them from the shared object.
#[used]
static LINK_MEDIA_JNI: [std::sync::atomic::AtomicPtr<()>; 5] = [
    std::sync::atomic::AtomicPtr::new(
        rumo_media::video_jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoProbeFd as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_media::video_jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoOpenFd as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_media::video_jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoInfo as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_media::video_jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoFrameAt as *mut (),
    ),
    std::sync::atomic::AtomicPtr::new(
        rumo_media::video_jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeVideoClose as *mut (),
    ),
];

/// Keep the beat-analysis JNI entry from `rumo_media::audio_jni` linked into
/// this cdylib. Same `#[used]` trick as [`LINK_MEDIA_JNI`]: Java calls it, so
/// nothing in Rust references it and the linker would otherwise drop it from
/// the shared object.
#[used]
static LINK_MEDIA_BEATS_JNI: [std::sync::atomic::AtomicPtr<()>; 2] = [
    std::sync::atomic::AtomicPtr::new(
        rumo_media::audio_jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioAnalyzeBeats
            as *mut (),
    ),
    // Speech/silence segmentation. It sits in the same table as beats because
    // both are "listen to this audio and answer with times" and both come from
    // `rumo_media::audio_jni`; a second table for one more symbol would be noise.
    std::sync::atomic::AtomicPtr::new(
        rumo_media::audio_jni::Java_com_kerneldroid_rumo_data_RumoBridge_nativeAudioAnalyzeSpeech
            as *mut (),
    ),
];

/// Keep the batch-EDL JNI entry linked into this cdylib. Same `#[used]` trick as
/// [`LINK_MEDIA_BEATS_JNI`]: Java calls it, so nothing in Rust references it.
///
/// The entry lives here rather than in `rumo-core` because that crate has no JNI
/// module and should not grow one: it is the project model, and it stays free of
/// `jni` so it can be used from a host tool or a test without a JVM. The bridge
/// is the crate whose whole job is the JNI surface.
#[used]
static LINK_EDL_JNI: [std::sync::atomic::AtomicPtr<()>; 1] =
    [std::sync::atomic::AtomicPtr::new(
        Java_com_kerneldroid_rumo_data_RumoBridge_nativeApplyEdl as *mut (),
    )];

/// Which path rendered the last frame: `"gpu"` or `"cpu"`.
///
/// This is the *single* source of truth, so it is answered by the diagnostics
/// state ([`rumo_render::diag`]), which every path updates: the legacy bitmap
/// compositor here, the offscreen preview, and the Surface engine.
///
/// It used to read only this module's [`LAST_GPU_OK`], which the legacy bitmap
/// path sets and the editor never calls — so on a perfectly working GPU it
/// reported `"cpu"` forever. That is what made a Snapdragon 8+ Gen 1 look like
/// it was rendering on the CPU.
pub fn render_path() -> &'static str {
    let legacy_ok = LAST_GPU_OK.load(std::sync::atomic::Ordering::Relaxed);
    if legacy_ok || rumo_render::diag::path_is_gpu() {
        "gpu"
    } else {
        "cpu"
    }
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeRenderPreview (static via @JvmStatic).
///
/// Composites SHAPE layers to a `width`×`height` 0xAARRGGBB int array
/// (for `Bitmap.Config.ARGB_8888`).
/// All six layer arrays must share one length. Errors throw
/// `java/lang/RuntimeException` and return null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeRenderPreview(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    width: jint,
    height: jint,
    bg_argb: jint,
    ordinals: jni::objects::JIntArray<'_>,
    argbs: jni::objects::JIntArray<'_>,
    dxs: jni::objects::JFloatArray<'_>,
    dys: jni::objects::JFloatArray<'_>,
    rotations: jni::objects::JFloatArray<'_>,
    alphas: jni::objects::JFloatArray<'_>,
) -> jni::sys::jintArray {
    env.with_env(|env| -> jni::errors::Result<jni::sys::jintArray> {
        let bad = || jni::errors::Error::JniCall(jni::errors::JniError::Unknown);
        if width <= 0 || height <= 0 {
            return Err(bad());
        }
        let layers = read_preview_layers(env, &ordinals, &argbs, &dxs, &dys, &rotations, &alphas)?;
        // Engine-internal pixels are RGBA8; the ONLY ARGB conversion is
        // this exit pack for Bitmap.Config.ARGB_8888 (see rgba_u32_to_argb).
        let px = render_preview(
            width as u32,
            height as u32,
            argb_to_f32(bg_argb as u32, 1.0),
            &layers,
        );
        let out: Vec<jint> = px.into_iter().map(|v| rgba_u32_to_argb(v) as jint).collect();
        let arr = jni::objects::JIntArray::new(env, out.len())?;
        arr.set_region(env, 0, &out)?;
        Ok(arr.as_raw() as jni::sys::jintArray)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

// ---------------------------------------------------------------------------
// Explicit render engine (surface path): no global GPU static, no block_on
// on JNI threads. Handles are Box<Engine> raw pointers as jlong.
// ---------------------------------------------------------------------------

/// Borrow the engine behind `handle` (created by `nativeEngineCreate`,
/// alive until `nativeEngineDestroy`). `None` for a null handle.
fn engine_from_handle(handle: jlong) -> Option<&'static rumo_render::Engine> {
    if handle == 0 {
        return None;
    }
    // SAFETY: Kotlin only passes handles returned by nativeEngineCreate
    // and never calls after nativeEngineDestroy; the Box keeps the engine
    // alive in between.
    unsafe { (handle as *const rumo_render::Engine).as_ref() }
}

/// Read the six parallel layer arrays of `nativeEngineSetLayers` /
/// `nativeRenderPreview` into [`PreviewLayer`]s. `Err` on any length
/// mismatch (the caller throws).
fn read_preview_layers(
    env: &mut jni::Env<'_>,
    ordinals: &jni::objects::JIntArray<'_>,
    argbs: &jni::objects::JIntArray<'_>,
    dxs: &jni::objects::JFloatArray<'_>,
    dys: &jni::objects::JFloatArray<'_>,
    rotations: &jni::objects::JFloatArray<'_>,
    alphas: &jni::objects::JFloatArray<'_>,
) -> jni::errors::Result<Vec<PreviewLayer>> {
    let bad = || jni::errors::Error::JniCall(jni::errors::JniError::Unknown);
    let n = ordinals.len(env)? as usize;
    if argbs.len(env)? as usize != n
        || dxs.len(env)? as usize != n
        || dys.len(env)? as usize != n
        || rotations.len(env)? as usize != n
        || alphas.len(env)? as usize != n
    {
        return Err(bad());
    }
    let mut o = vec![0i32; n];
    let mut a = vec![0i32; n];
    let mut dx = vec![0f32; n];
    let mut dy = vec![0f32; n];
    let mut rot = vec![0f32; n];
    let mut al = vec![0f32; n];
    ordinals.get_region(env, 0, &mut o)?;
    argbs.get_region(env, 0, &mut a)?;
    dxs.get_region(env, 0, &mut dx)?;
    dys.get_region(env, 0, &mut dy)?;
    rotations.get_region(env, 0, &mut rot)?;
    alphas.get_region(env, 0, &mut al)?;
    Ok((0..n)
        .map(|i| PreviewLayer {
            ordinal: o[i].max(0) as usize,
            argb: a[i] as u32,
            dx: dx[i],
            dy: dy[i],
            rotation_deg: rot[i],
            alpha: al[i],
        })
        .collect())
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeEngineCreate (static via @JvmStatic).
///
/// Spawns the render worker thread (GPU init stays lazy inside it) and
/// returns the engine handle (`Box::into_raw` → jlong); `0` only when the
/// worker thread itself cannot spawn. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEngineCreate(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
) -> jlong {
    let engine = Box::new(rumo_render::Engine::new());
    Box::into_raw(engine) as jlong
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeEngineDestroy (static via @JvmStatic).
///
/// Signals worker shutdown and frees the handle. Idempotent for `0`;
/// calling twice with the same live handle is a Kotlin bug (use-after-free)
/// and is not guarded. Never throws.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEngineDestroy(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    engine: jlong,
) {
    if engine == 0 {
        return;
    }
    // SAFETY: inverse of nativeEngineCreate's into_raw; Kotlin never calls
    // after destroy, so this runs exactly once per handle.
    unsafe {
        drop(Box::from_raw(engine as *mut rumo_render::Engine));
    }
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeEngineSetLayers (static via @JvmStatic).
///
/// Stores the scene the engine renders on each `nativeEngineRenderFrame`:
/// same parameters as `nativeRenderPreview`. Tessellation runs on the
/// calling thread (fast CPU work); only GPU submission happens on the
/// worker. Errors throw `java/lang/RuntimeException`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEngineSetLayers(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    engine: jlong,
    width: jint,
    height: jint,
    bg_argb: jint,
    ordinals: jni::objects::JIntArray<'_>,
    argbs: jni::objects::JIntArray<'_>,
    dxs: jni::objects::JFloatArray<'_>,
    dys: jni::objects::JFloatArray<'_>,
    rotations: jni::objects::JFloatArray<'_>,
    alphas: jni::objects::JFloatArray<'_>,
) {
    let result = env.with_env(|env| -> jni::errors::Result<()> {
        let bad = || jni::errors::Error::JniCall(jni::errors::JniError::Unknown);
        if width <= 0 || height <= 0 {
            return Err(bad());
        }
        let Some(engine) = engine_from_handle(engine) else {
            return Err(bad());
        };
        let layers = read_preview_layers(env, &ordinals, &argbs, &dxs, &dys, &rotations, &alphas)?;
        // Colors enter the engine as linear RGBA f32 — no ARGB shuffling
        // inside; the surface path stays RGBA8 zero-copy throughout.
        let triples = preview_meshes(width as u32, height as u32, &layers);
        let scene = rumo_render::EngineScene::new(
            width as u32,
            height as u32,
            argb_to_f32(bg_argb as u32, 1.0),
            triples,
        );
        if engine.set_scene(scene) != rumo_render::ENGINE_OK {
            return Err(bad());
        }
        Ok(())
    });
    let _ = result.resolve::<ThrowRuntimeExAndDefault>();
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeEngineSurfaceCreated (static via @JvmStatic).
///
/// Attaches an `android.view.Surface`: acquires the `ANativeWindow`,
/// creates the wgpu surface from the worker's own instance, picks
/// `Bgra8UnormSrgb`/`Rgba8UnormSrgb` from the capabilities and configures
/// the swapchain (alpha `Auto`). Returns [`rumo_render::ENGINE_OK`] or a
/// nonzero code (Kotlin falls back to bitmap preview). Never throws; a
/// null engine is [`rumo_render::ENGINE_ERR_BAD_ARG`].
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEngineSurfaceCreated(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    engine: jlong,
    surface: JObject<'_>,
    width: jint,
    height: jint,
) -> jint {
    let Some(engine) = engine_from_handle(engine) else {
        return rumo_render::ENGINE_ERR_BAD_ARG;
    };
    if width <= 0 || height <= 0 {
        return rumo_render::ENGINE_ERR_BAD_ARG;
    }
    env.with_env(|env| -> jni::errors::Result<jint> {
        let jni_env = env.get_raw() as *mut std::ffi::c_void;
        let surface_obj = surface.as_raw() as *mut std::ffi::c_void;
        if jni_env.is_null() || surface_obj.is_null() {
            return Ok(rumo_render::ENGINE_ERR_BAD_ARG);
        }
        Ok(engine.surface_created(jni_env, surface_obj, width as u32, height as u32))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeEngineSurfaceChanged (static via @JvmStatic).
///
/// Reconfigures the swapchain for a new surface size. Fire-and-forget:
/// a missing surface (`NO_SURFACE`) is a benign race with surface events —
/// the next `surfaceCreated`/`renderFrame` sorts it out — so only
/// programmer errors (null engine, non-positive size) throw.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEngineSurfaceChanged(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    engine: jlong,
    width: jint,
    height: jint,
) {
    let result = env.with_env(|_| -> jni::errors::Result<()> {
        let bad = || jni::errors::Error::JniCall(jni::errors::JniError::Unknown);
        if width <= 0 || height <= 0 {
            return Err(bad());
        }
        let Some(engine) = engine_from_handle(engine) else {
            return Err(bad());
        };
        // NO_SURFACE/TIMEOUT here are benign: renderFrame reports the code.
        let _ = engine.surface_changed(width as u32, height as u32);
        Ok(())
    });
    let _ = result.resolve::<ThrowRuntimeExAndDefault>();
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeEngineSurfaceDestroyed (static via @JvmStatic).
///
/// Detaches the surface; the device/queue are kept for the next surface.
/// Idempotent and never throws (null engine is a no-op).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEngineSurfaceDestroyed(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    engine: jlong,
) {
    if let Some(engine) = engine_from_handle(engine) {
        let _ = engine.surface_destroyed();
    }
}

/// com.kerneldroid.rumo.data.RumoBridge.nativeEngineRenderFrame (static via @JvmStatic).
///
/// Renders the stored scene to the surface and presents. Returns
/// [`rumo_render::ENGINE_OK`] or a nonzero code (no surface/GPU →
/// Kotlin falls back to bitmap preview). Never throws; a null engine is
/// [`rumo_render::ENGINE_ERR_BAD_ARG`].
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kerneldroid_rumo_data_RumoBridge_nativeEngineRenderFrame(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    engine: jlong,
) -> jint {
    let Some(engine) = engine_from_handle(engine) else {
        return rumo_render::ENGINE_ERR_BAD_ARG;
    };
    engine.render_frame()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_version_matches_core() {
        assert_eq!(bridge_version(), rumo_core::RUMO_CORE_VERSION);
        assert_eq!(bridge_version(), rumo_core::core_version());
    }

    #[test]
    fn encode_roundtrips_through_codec() {
        let name = "hello bridge";
        let bytes = encode_new_project(name);
        let decoded = rumo_core::codec::decode(&bytes).expect("decode");
        assert_eq!(decoded.name, name);
    }

    #[test]
    fn shape_tris_all_nonneg() {
        for ordinal in 0..35 {
            let tris = shape_tris(ordinal);
            assert!(tris >= 1, "ordinal {ordinal} tessellated to {tris}");
        }
    }

    #[test]
    fn shape_tris_oob_is_minus1() {
        // One past the last shape, derived rather than written down: this test
        // used the literal 35, which stopped being out of range the moment a
        // 36th shape was appended, and the failure it produced named an
        // assertion rather than the number that had gone stale.
        assert_eq!(shape_tris(rumo_render::all_shapes().len()), -1);
        assert_eq!(shape_tris(usize::MAX), -1);
    }

    #[test]
    fn mesh_circle_covers_center() {
        let size = 256.0;
        let mesh = shape_mesh(0, size, 0.0, 0.0).expect("circle mesh");
        assert!(!mesh.tris.is_empty());
        assert_eq!(mesh.tris.len() % 3, 0);
        let n = mesh.xs.len() as f32;
        let cx: f32 = mesh.xs.iter().sum::<f32>() / n;
        let cy: f32 = mesh.ys.iter().sum::<f32>() / n;
        let dx = cx - size / 2.0;
        let dy = cy - size / 2.0;
        assert!(
            dx.hypot(dy) <= size / 4.0,
            "mean vertex ({cx},{cy}) too far from center"
        );
    }

    #[test]
    fn mesh_oob_none() {
        assert!(shape_mesh(rumo_render::all_shapes().len(), 256.0, 0.0, 0.0).is_none());
        assert!(shape_mesh(usize::MAX, 256.0, 0.0, 0.0).is_none());
        assert!(shape_mesh(0, 0.0, 0.0, 0.0).is_none());
        assert!(shape_mesh(0, 256.0, 0.9, 0.0).is_none());
    }

    #[test]
    fn image_flat_header_parse() {
        let img = rumo_media::RgbaImage {
            width: 0x0102_0304,
            height: 0x0506_0708,
            rgba: vec![9, 10, 11, 12],
        };
        let flat = pack_image_flat(&img);
        assert_eq!(
            &flat[0..8],
            &[0x04, 0x03, 0x02, 0x01, 0x08, 0x07, 0x06, 0x05]
        );
        assert_eq!(
            parse_image_flat_header(&flat),
            Some((0x0102_0304, 0x0506_0708))
        );
        assert_eq!(parse_image_flat_header(&flat[..7]), None);
        assert_eq!(parse_image_flat_header(b""), None);
    }

    #[test]
    fn decode_flat_roundtrip_and_garbage() {
        // Reuse the media encoder path: solid PNG in memory.
        use image::ExtendedColorType;
        use image::ImageEncoder;
        use image::codecs::png::PngEncoder;
        let pixels = vec![0x7Fu8; 3 * 2 * 3];
        let mut buf = Vec::new();
        PngEncoder::new(&mut buf)
            .write_image(&pixels, 3, 2, ExtendedColorType::Rgb8)
            .expect("encode test png");
        let flat = decode_image_flat(&buf, 1024).expect("must decode");
        let (w, h) = parse_image_flat_header(&flat).expect("header");
        assert_eq!((w, h), (3, 2));
        assert_eq!(flat.len(), 8 + 3 * 2 * 4);
        assert!(decode_image_flat(b"garbage............................", 512).is_none());
        assert!(decode_image_flat(&buf, 0).is_none());
    }

    #[test]
    fn probe_garbage_bytes_none() {
        assert!(rumo_media::probe_audio_bytes(b"").is_none());
        assert!(rumo_media::probe_audio_bytes(&[0xABu8; 64]).is_none());
        assert!(rumo_media::probe_audio_bytes(b"not audio.............").is_none());
    }

    #[test]
    fn json_bridge_roundtrip() {
        let json = serde_json::json!({
            "name": "bridge trip",
            "layers": [{
                "id": "7",
                "kind": "TEXT",
                "name": "Title",
                "visible": true,
                "argb": 4294967295u32,
                "durationMs": 6000,
                "uri": serde_json::Value::Null,
                "offsetX": 1.5,
                "offsetY": 2.5,
                "alpha": 0.75,
                "keys": [{"t": 0, "v": 0.0}],
            }],
        })
        .to_string();
        let bytes = rumo_core::project_json::project_from_json(&json).expect("from json");
        let back = rumo_core::project_json::project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&back).expect("reparse");
        assert_eq!(v["name"], "bridge trip");
        assert_eq!(v["layers"][0]["kind"], "TEXT");
        assert_eq!(v["layers"][0]["argb"], 4294967295u32);
        assert!((v["layers"][0]["alpha"].as_f64().unwrap() - 0.75).abs() < 1e-6);
        assert!(rumo_core::project_json::project_to_json(b"garbage").is_err());
    }

    fn preview_layer(ordinal: usize, argb: u32) -> PreviewLayer {
        PreviewLayer {
            ordinal,
            argb,
            dx: 0.0,
            dy: 0.0,
            rotation_deg: 0.0,
            alpha: 1.0,
        }
    }

    #[test]
    fn preview_empty_layers_all_bg() {
        let bg_f32 = argb_to_f32(0xFF141824, 1.0);
        let px = render_preview_inner(64, 36, bg_f32, &[], false);
        assert_eq!(px.len(), 64 * 36);
        // Engine-internal pixels are RGBA8, not 0xAARRGGBB.
        assert!(px.iter().all(|&p| p == argb_to_rgba_u32(0xFF141824)));
    }

    #[test]
    fn preview_circle_paints_centre() {
        let bg = argb_to_f32(0xFF141824, 1.0);
        let orange = 0xFFFF9800u32;
        let px = render_preview_inner(64, 36, bg, &[preview_layer(0, orange)], false);
        let centre = px[18 * 64 + 32];
        let bg_rgba = argb_to_rgba_u32(0xFF141824);
        assert_ne!(centre, bg_rgba, "circle must paint frame centre");
        // Opaque orange over opaque bg composites exactly (RGBA8 order).
        assert_eq!(centre, argb_to_rgba_u32(orange));
        assert_eq!(px[0], bg_rgba, "corner stays background");
    }

    #[test]
    fn rgba_boundary_pack_is_single_and_reversible() {
        // The ONLY ARGB conversion: legacy bitmap-JNI exit pack.
        assert_eq!(argb_to_rgba_u32(0xFFFF9800), 0xFF0098FF);
        assert_eq!(rgba_u32_to_argb(0xFF0098FF), 0xFFFF9800);
        for argb in [0xFF141824u32, 0xFFFF0000, 0xFF00FF00, 0xFF0000FF, 0x80010203, 0x00000000] {
            assert_eq!(rgba_u32_to_argb(argb_to_rgba_u32(argb)), argb, "roundtrip {argb:#X}");
        }
        // f32 path agrees with the packed path on opaque colors.
        assert_eq!(f32_to_rgba_u32(argb_to_f32(0xFFFF9800, 1.0)), 0xFF0098FF);
    }

    #[test]
    fn argb_channels_not_swapped() {
        // 0xAARRGGBB like Kotlin Color(argb): pure red must come out red.
        let c = argb_to_f32(0xFFFF0000, 1.0);
        assert!((c[0] - 1.0).abs() < 1e-6, "R must be 1, got {:?}", c);
        assert!(c[1].abs() < 1e-6 && c[2].abs() < 1e-6);
        let c = argb_to_f32(0xFF0000FF, 1.0);
        assert!((c[2] - 1.0).abs() < 1e-6, "B must be 1, got {:?}", c);
    }

    #[test]
    fn preview_oob_ordinal_skipped() {
        let bg_f32 = argb_to_f32(0xFF141824, 1.0);
        let bg_rgba = argb_to_rgba_u32(0xFF141824);
        let px = render_preview_inner(64, 36, bg_f32, &[preview_layer(usize::MAX, 0xFFFF0000)], false);
        assert!(px.iter().all(|&p| p == bg_rgba));
    }

    #[test]
    fn render_path_defaults_to_cpu() {
        // No preview rendered yet in this process (GPU never touched here).
        assert_eq!(render_path(), "cpu");
    }

    #[test]
    fn preview_ex_empty_textured_matches_legacy() {
        // Empty text/image sets must render byte-for-byte like the legacy
        // SHAPE-only path: same triples through rumo_render::composite_preview
        // as through render_preview_inner (CPU pinned, no driver here).
        let bg = argb_to_f32(0xFF141824, 1.0);
        let layers = vec![preview_layer(0, 0xFFFF9800), preview_layer(2, 0xFF00FF00)];
        let old = render_preview_inner(64, 36, bg, &layers, false);
        let triples = preview_meshes(64, 36, &layers);
        let new = rumo_render::composite_preview(64, 36, bg, &triples, &[]);
        assert_eq!(old, new);
    }

    #[test]
    fn preview_ex_staged_image_composites_over_shape() {
        // Cross-crate flow: stage via rumo_render::jni, composite via
        // rumo_render::composite — the same pieces nativeRenderPreviewEx uses.
        let id = rumo_render::jni::upload_image(2, 2, vec![255, 0, 0, 255].repeat(4))
            .expect("stage red 2x2");
        let image = rumo_render::jni::texture_image(id).expect("staged");
        let mesh = rumo_render::image_mesh(0.0, 0.0, 2.0, 2.0);
        let bg = argb_to_f32(0xFF141824, 1.0);
        let layers = vec![preview_layer(0, 0xFFFF9800)];
        let triples = preview_meshes(64, 36, &layers);
        let px = rumo_render::composite_preview(64, 36, bg, &triples, &[(&image, &mesh, [1.0; 4])]);
        // Opaque red texels win over whatever the shape painted there.
        assert_eq!(px[0], 0xFF0000FF, "top-left must be opaque red (LE-RGBA)");
        assert_eq!(px[64 + 1], 0xFF0000FF);
        // Far corner untouched by the 2x2 stamp: circle or bg, never red.
        let far = px[35 * 64 + 63];
        assert_ne!(far, 0xFF0000FF);
        assert!(rumo_render::jni::free_texture(id));
    }

    #[test]
    fn engine_handle_roundtrip_without_surface() {
        use rumo_render::{
            ENGINE_ERR_BAD_ARG, ENGINE_ERR_NO_SURFACE, ENGINE_OK, Engine, EngineScene,
        };
        // Box::into_raw -> jlong -> borrow -> destroy: the exact JNI flow,
        // with no surface and no GPU in the container. Codes only, no panic.
        let engine = Box::new(Engine::new());
        let handle = Box::into_raw(engine) as jlong;
        assert_ne!(handle, 0);
        let borrowed = engine_from_handle(handle).expect("live handle must borrow");
        assert_eq!(
            borrowed.set_scene(EngineScene::new(64, 36, [0.08, 0.09, 0.14, 1.0], vec![])),
            ENGINE_OK
        );
        assert_eq!(borrowed.render_frame(), ENGINE_ERR_NO_SURFACE);
        assert_eq!(borrowed.surface_changed(64, 36), ENGINE_ERR_NO_SURFACE);
        assert_eq!(borrowed.surface_destroyed(), ENGINE_OK);
        assert!(engine_from_handle(0).is_none());
        // SAFETY: inverse of into_raw above, runs exactly once.
        unsafe {
            drop(Box::from_raw(handle as *mut Engine));
        }
        // Null-handle JNI mapping (no engine): BAD_ARG, never a crash.
        assert!(engine_from_handle(0).is_none());
        let _ = ENGINE_ERR_BAD_ARG;
    }

    #[test]
    fn audio_fd_missing_returns_minus1() {
        assert_eq!(audio_duration_for_fd(-1), -1);
        assert_eq!(audio_duration_for_fd(i32::MIN), -1);
        // No test fd is open at this number; even if the open fails,
        // the contract is -1 without throwing.
        assert_eq!(audio_duration_for_fd(4096), -1);
    }

    // -----------------------------------------------------------------------
    // The plane hand-across
    // -----------------------------------------------------------------------

    use rumo_media::video::yuv::{
        Matrix as YuvMatrix, Range as YuvRange, YuvFormat, YuvLayout, YuvPlanes,
    };
    use rumo_render::texture::{YuvFormat as RenderFormat, YuvSlot, YuvUniforms};

    /// A padded 4:2:0 planar buffer: 8×6 luma, 4×3 U, 4×3 V.
    fn i420_planes(format: YuvFormat) -> YuvPlanes {
        let layout = YuvLayout {
            stride: 8,
            slice_height: 6,
        };
        let bytes: std::sync::Arc<[u8]> = vec![128u8; 8 * 6 + 2 * 4 * 3].into();
        YuvPlanes::new(
            format,
            layout,
            4,
            4,
            YuvMatrix::Bt601,
            YuvRange::Limited,
            0,
            bytes,
        )
        .expect("valid frame")
    }

    /// The two crates cannot see each other's types, so the description a decoded
    /// frame is handed as is pinned here: same packing, same crop, same strides,
    /// same rotation UVs, and the CPU's own coefficients on their way to the GPU.
    #[test]
    fn a_decoded_frame_becomes_a_gpu_plane_description() {
        let planes = i420_planes(YuvFormat::I420);
        let texture = yuv_payload(&planes).expect("hand-across");
        assert_eq!(texture.format(), RenderFormat::I420);
        assert_eq!((texture.width(), texture.height()), (4, 4));
        assert_eq!(texture.plane_size(0), (8, 6), "stride padding is carried, not repacked");
        assert_eq!(texture.plane_size(1), (4, 3));
        assert_eq!(texture.plane_size(2), (4, 3));
        assert_eq!(texture.uploaded_bytes(), planes.bytes().len());
        assert_eq!(texture.corners(), planes.corner_uvs().expect("uv"));
        // The uniform the shader multiplies with is the converter's coefficients,
        // in the order `coeffs_f32` produced them.
        let uniforms = YuvUniforms::for_draw(
            &YuvSlot::of(&texture),
            rumo_render::texture::identity(),
            [1.0; 4],
        );
        assert_eq!(&uniforms.luma[..3], &planes.coeffs()[..3]);
        assert_eq!(uniforms.chroma, planes.coeffs()[3..]);
        assert_eq!(uniforms.luma[3], RenderFormat::I420.code());
        assert_eq!(&uniforms.size[..2], &[4.0, 4.0][..], "the crop, not the plane");
    }

    /// The interleaved packings read chroma with the luma stride and have no
    /// third plane, and the mapping has to survive that intact.
    #[test]
    fn an_interleaved_frame_keeps_its_packing() {
        for (media, render) in [
            (YuvFormat::Nv12, RenderFormat::Nv12),
            (YuvFormat::Nv21, RenderFormat::Nv21),
        ] {
            let planes = i420_planes(media);
            let texture = yuv_payload(&planes).expect("hand-across");
            assert_eq!(texture.format(), render);
            assert_eq!(texture.plane_size(1), (8, 3), "chroma keeps the luma stride");
            assert_eq!(
                texture.plane_size(2),
                (0, 0),
                "and the third plane is empty, which is what the shader's swizzle \
                 and the bind group's dummy both rely on"
            );
            assert_eq!(texture.uploaded_bytes(), 8 * 6 + 8 * 3);
        }
    }

    /// A rotation the converter would also refuse never reaches the GPU, and one
    /// it accepts arrives as the same corner UVs the CPU test pins.
    #[test]
    fn rotation_travels_as_uvs_not_as_pixels() {
        let layout = YuvLayout {
            stride: 8,
            slice_height: 6,
        };
        let bytes: std::sync::Arc<[u8]> = vec![128u8; 8 * 6 + 2 * 4 * 3].into();
        let rotated = YuvPlanes::new(
            YuvFormat::I420,
            layout,
            4,
            4,
            YuvMatrix::Bt601,
            YuvRange::Limited,
            90,
            std::sync::Arc::clone(&bytes),
        )
        .expect("valid frame");
        let texture = yuv_payload(&rotated).expect("hand-across");
        assert_eq!(texture.corners(), rotated.corner_uvs().expect("uv"));
        assert_ne!(
            texture.corners(),
            i420_planes(YuvFormat::I420)
                .corner_uvs()
                .expect("uv"),
            "a quarter turn has to change the UVs; the pixels are untouched"
        );
        // 45° is refused by the media side, so the renderer's own guard is only
        // ever reached by a caller that skipped it.
        assert!(YuvPlanes::new(
            YuvFormat::I420,
            layout,
            4,
            4,
            YuvMatrix::Bt601,
            YuvRange::Limited,
            45,
            bytes,
        )
        .is_err());
    }
}
