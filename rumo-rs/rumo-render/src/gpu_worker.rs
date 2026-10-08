// SPDX-License-Identifier: Apache-2.0

//! Offscreen GPU rendering on a dedicated thread.
//!
//! The extended preview (`nativeRenderPreviewEx`) and the export loop both ask
//! for a composited frame from the caller's thread, and both were served by the
//! CPU reference compositor — so an editor session that only ever used this path
//! never touched the GPU at all, no matter how capable the device was. This
//! module gives that path a real GPU implementation.
//!
//! # Why a thread
//!
//! `rumo-render`'s contract with the JNI layer forbids blocking on a GPU
//! initialisation from a JNI thread (see `AGENTS.md`): device creation can take
//! hundreds of milliseconds and must never stall the platform's calling thread.
//! So exactly one worker thread owns the offscreen `GpuRenderer`, renders jobs
//! one at a time, and every request waits with a bounded timeout. A timeout or a
//! worker failure returns `Err`, which the caller turns into the CPU fallback —
//! and [`crate::diag`] records why.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::renderer::{
    GpuRenderer, LayerDraw, MeshData, SceneDraw, TexturedQuad, ordered_layer_draws,
};
use crate::texture::SceneTexture;
use rumo_core::effect::EffectChains;

/// How long a caller waits for one offscreen frame before giving up.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(6);

/// One frame request. Owns everything it needs so it can cross the thread
/// boundary; [`LayerDraw`] itself borrows and therefore cannot be sent.
pub struct FrameRequest {
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Background colour (linear RGBA).
    pub bg: [f32; 4],
    /// Solid-shape draws, in draw order.
    pub shapes: Vec<(MeshData, [f32; 4], [[f32; 4]; 4])>,
    /// Textured draws, in draw order (text first, then images).
    pub textured: Vec<TexturedQuad>,
    /// The scene's global draw order, naming a draw in `shapes` or in
    /// `textured` per element. Empty means "no order information": the draws
    /// are then emitted in group order (every mesh, then every textured draw),
    /// which is what a hand-built request always produced.
    pub draw_order: Vec<SceneDraw>,
    /// Effect chains, indexed the same way as `shapes` and `textured`.
    pub chains: EffectChains,
    /// The textures the draw list references: id, content stamp and pixels.
    ///
    /// Payloads rather than bare ids, because the worker owns the only
    /// wgpu-side cache and has to be the one deciding an upload is due. Two
    /// things fall out of that: the pixels are shared (`Arc`) instead of being
    /// copied out of the registry or across the thread boundary — a 1080p RGBA
    /// image is 8 MB, and cloning one per frame at 60 fps is half a gigabyte
    /// per second of memcpy — and the stamp lets an unchanged still image or
    /// atlas page skip the transfer while a video frame, which arrives under a
    /// fresh id and therefore a fresh stamp, never does.
    pub textures: Vec<SceneTexture>,
    /// Timeline position handed to effect shaders, in milliseconds.
    pub time_ms: i64,
}

impl FrameRequest {
    /// Every draw of this request as an effect-aware [`LayerDraw`], in the
    /// scene's global draw order: the merged order when the scene carries one,
    /// the group order otherwise.
    pub fn layer_draws(&self) -> Vec<LayerDraw<'_>> {
        ordered_layer_draws(
            &self.shapes,
            &self.textured,
            &self.draw_order,
            &self.chains,
            self.time_ms as f32 / 1000.0,
        )
    }
}

/// One frame request, and the shape of the answer the caller wants.
///
/// Both kinds composite the same scene with the same textures; they differ only
/// in what leaves the GPU — RGBA8 pixels for the preview and the export's
/// fallback, or an NV12 frame packed by `shader_nv12.wgsl` for the export.
enum Job {
    Pixels {
        request: FrameRequest,
        reply: Sender<Result<Vec<u32>, String>>,
    },
    Nv12 {
        request: FrameRequest,
        coeffs: crate::nv12_pack::Nv12Coeffs,
        reply: Sender<Result<Vec<u8>, String>>,
    },
}

static WORKER: OnceLock<Mutex<Option<Sender<Job>>>> = OnceLock::new();

fn sender() -> Option<Sender<Job>> {
    let cell = WORKER.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(tx) = guard.as_ref() {
        return Some(tx.clone());
    }
    let (tx, rx) = channel::<Job>();
    std::thread::Builder::new()
        .name("rumo-offscreen-gpu".to_string())
        .spawn(move || worker_loop(rx))
        .ok()?;
    *guard = Some(tx.clone());
    Some(tx)
}

/// How many GPU-side textures the offscreen worker keeps before evicting.
///
/// Video playback uploads a new id per frame, so an unbounded cache would leak a
/// texture per frame; a small LRU covers the handful of ids that cycle during
/// playback and bounds the rest.
const MAX_CACHED_TEXTURES: usize = 32;

/// Which texture ids the offscreen worker has kept on the GPU, oldest first.
///
/// Deliberately *only* the LRU: whether an id's pixels still have to be
/// transferred is the renderer's cache's business (see
/// [`crate::texture::GpuTextureCache::claim`]), so there is one residency rule
/// in the crate rather than one per path.
#[derive(Default)]
struct Uploads {
    order: std::collections::VecDeque<u64>,
}

impl Uploads {
    fn touch(&mut self, id: u64) {
        if let Some(pos) = self.order.iter().position(|cached| *cached == id) {
            self.order.remove(pos);
        }
        self.order.push_back(id);
    }

    /// Ids to evict, oldest first, to get back under the cap.
    fn evictions(&self, cap: usize) -> Vec<u64> {
        if self.order.len() <= cap {
            return Vec::new();
        }
        self.order
            .iter()
            .take(self.order.len() - cap)
            .copied()
            .collect()
    }

    fn forget(&mut self, id: u64) {
        if let Some(pos) = self.order.iter().position(|cached| *cached == id) {
            self.order.remove(pos);
        }
    }
}

fn worker_loop(rx: Receiver<Job>) {
    // The renderer is created on first use and lives for the process: creating a
    // device per frame would be ruinous, and the driver may only allow a couple.
    let mut renderer: Option<GpuRenderer> = None;
    let mut uploads = Uploads::default();
    while let Ok(job) = rx.recv() {
        // A caller that gave up is not an error: the worker keeps serving the
        // next request, which is usually the same scene a moment later.
        match job {
            Job::Pixels { request, reply } => {
                let result = prepare(&mut renderer, &mut uploads, &request).and_then(|gpu| {
                    let draws = request.layer_draws();
                    pollster::block_on(gpu.render_scene_ex(
                        request.width,
                        request.height,
                        request.bg,
                        &draws,
                        request.chains.customs(),
                        DEFAULT_TIMEOUT,
                    ))
                });
                let _ = reply.send(result);
            }
            Job::Nv12 {
                request,
                coeffs,
                reply,
            } => {
                let result = prepare(&mut renderer, &mut uploads, &request).and_then(|gpu| {
                    let draws = request.layer_draws();
                    pollster::block_on(gpu.render_scene_ex_nv12(
                        request.width,
                        request.height,
                        request.bg,
                        &draws,
                        request.chains.customs(),
                        &coeffs,
                        DEFAULT_TIMEOUT,
                    ))
                });
                let _ = reply.send(result);
            }
        }
    }
}

/// Bring the worker's renderer and its texture cache up to date for `request`,
/// then hand the renderer back.
///
/// Hand every referenced texture to the cache. The cache — not this loop — owns
/// the residency decision: an unchanged stamp is a no-op there, so the
/// steady state of a static scene transfers nothing, while a video frame (new
/// id, new stamp) always uploads.
fn prepare<'a>(
    renderer: &'a mut Option<GpuRenderer>,
    uploads: &mut Uploads,
    request: &FrameRequest,
) -> Result<&'a GpuRenderer, String> {
    if renderer.is_none() {
        match pollster::block_on(GpuRenderer::new()) {
            Ok(gpu) => *renderer = Some(gpu),
            Err(e) => {
                crate::diag::error("preview_gpu_init", e.clone());
                return Err(e);
            }
        }
    }
    let gpu = renderer.as_ref().ok_or("GPU renderer vanished")?;

    for texture in &request.textures {
        gpu.set_texture(texture.id, texture.stamp, &texture.image)
            .map_err(|e| format!("texture {} upload failed: {e}", texture.id))?;
        uploads.touch(texture.id);
    }
    for stale in uploads.evictions(MAX_CACHED_TEXTURES) {
        gpu.remove_texture(stale);
        uploads.forget(stale);
    }
    Ok(gpu)
}

/// Render one frame offscreen on the GPU, blocking the caller up to `timeout`.
///
/// `Err` means the caller must fall back to the CPU compositor; the reason is
/// already recorded in [`crate::diag`] for the diagnostics window.
pub fn render(request: FrameRequest, timeout: Duration) -> Result<Vec<u32>, String> {
    if request.width == 0 || request.height == 0 {
        return Err("zero-sized frame".to_string());
    }
    let Some(tx) = sender() else {
        let why = "offscreen GPU worker thread could not be started".to_string();
        crate::diag::error("preview_gpu_init", why.clone());
        return Err(why);
    };
    let (reply_tx, reply_rx) = channel();
    if let Err(e) = tx.send(Job::Pixels {
        request,
        reply: reply_tx,
    }) {
        let why = format!("offscreen GPU worker is gone: {e}");
        crate::diag::error("preview_gpu_init", why.clone());
        return Err(why);
    }
    match reply_rx.recv_timeout(timeout) {
        Ok(Ok(pixels)) => {
            crate::diag::note_preview_ok(true);
            crate::diag::note_gpu_path();
            Ok(pixels)
        }
        Ok(Err(e)) => {
            crate::diag::note_preview_ok(false);
            crate::diag::note_cpu_path("preview_gpu_render", e.clone());
            Err(e)
        }
        Err(_) => {
            let why = format!("offscreen GPU frame timed out after {timeout:?}");
            crate::diag::note_preview_ok(false);
            crate::diag::note_cpu_path("preview_gpu_timeout", why.clone());
            Err(why)
        }
    }
}

/// Render one frame offscreen **and pack it into NV12 on the GPU**, blocking the
/// caller up to `timeout`.
///
/// The export's replacement for "read the frame back as RGBA, convert it on the
/// CPU": the frame leaves the GPU already in the layout the encoder takes, and
/// never becomes an `0xAARRGGBB` array on the way (docs/12 §12.3).
///
/// Deliberately does **not** touch the diagnostics render path. `renderPath`
/// describes the *preview*, and an export whose pack failed says nothing about
/// which path the preview took; the caller records this failure under its own
/// code, so the two questions stay separable. `Err` means the caller must fall
/// back to [`render`] plus the CPU conversion.
pub fn render_nv12(
    request: FrameRequest,
    coeffs: crate::nv12_pack::Nv12Coeffs,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    if request.width == 0 || request.height == 0 {
        return Err("zero-sized frame".to_string());
    }
    let Some(tx) = sender() else {
        let why = "offscreen GPU worker thread could not be started".to_string();
        crate::diag::error("export_gpu_nv12", why.clone());
        return Err(why);
    };
    let (reply_tx, reply_rx) = channel();
    if let Err(e) = tx.send(Job::Nv12 {
        request,
        coeffs,
        reply: reply_tx,
    }) {
        let why = format!("offscreen GPU worker is gone: {e}");
        crate::diag::error("export_gpu_nv12", why.clone());
        return Err(why);
    }
    match reply_rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => Err(format!("offscreen GPU NV12 frame timed out after {timeout:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::LayerShape;
    use rumo_core::effect::EffectKind;

    fn mesh() -> MeshData {
        MeshData {
            vertices: vec![[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]],
            indices: vec![0, 1, 2],
        }
    }

    fn request() -> FrameRequest {
        FrameRequest {
            width: 16,
            height: 16,
            bg: [0.0, 0.0, 0.0, 1.0],
            shapes: vec![(mesh(), [1.0, 0.0, 0.0, 1.0], [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ])],
            textured: Vec::new(),
            draw_order: Vec::new(),
            chains: EffectChains::default(),
            textures: Vec::new(),
            time_ms: 0,
        }
    }

    #[test]
    fn a_zero_sized_frame_is_rejected_without_touching_the_gpu() {
        let mut req = request();
        req.width = 0;
        let err = render(req, Duration::from_millis(50)).expect_err("must reject");
        assert!(err.contains("zero-sized"), "{err}");
    }

    #[test]
    fn layer_draws_follow_the_shape_then_texture_order_and_carry_chains() {
        // No GPU needed: this pins the ordering contract the effect chains are
        // indexed by, which is the part that would silently mismatch.
        let mut chains = EffectChains::default();
        chains.shapes = vec![
            vec![rumo_core::effect::EffectInstance::new(EffectKind::Blur)],
            Vec::new(),
        ];
        let mut req = request();
        req.chains = chains;
        req.shapes.push((mesh(), [0.0, 1.0, 0.0, 1.0], req.shapes[0].2));
        let mut quad_mesh = crate::texture::TexturedMesh::new();
        quad_mesh.push_quad(0.0, 0.0, 4.0, 4.0, [0.0, 0.0, 1.0, 1.0]);
        req.textured.push(TexturedQuad {
            mesh: quad_mesh,
            color: [1.0, 1.0, 1.0, 1.0],
            transform: req.shapes[0].2,
            texture_id: 7,
        });

        let draws = req.layer_draws();
        assert_eq!(draws.len(), 3);
        assert!(matches!(draws[0].shape, LayerShape::Mesh { .. }));
        assert_eq!(draws[0].effects.len(), 1, "shape 0 keeps its chain");
        assert_eq!(draws[1].effects.len(), 0, "shape 1 has no chain");
        assert!(
            matches!(draws[2].shape, LayerShape::Textured(_)),
            "textured draws come last"
        );
        assert_eq!(draws[0].blend, crate::fx::BlendMode::Normal);
        assert_eq!(draws[0].time, 0.0);
    }

    #[test]
    fn the_upload_lru_is_bounded_and_evicts_oldest_first() {
        let mut uploads = Uploads::default();
        for id in 0..5 {
            uploads.touch(id);
        }
        assert!(uploads.order.contains(&0));
        assert!(uploads.evictions(3) == vec![0, 1], "oldest two go first");
        uploads.forget(0);
        assert!(!uploads.order.contains(&0));
        assert_eq!(uploads.order.len(), 4, "forget removes exactly one entry");
        // Re-touching an existing id moves it to the back, not duplicates it.
        uploads.touch(4);
        assert_eq!(uploads.order.len(), 4);
        assert_eq!(uploads.order.back().copied(), Some(4));
    }

    #[test]
    fn every_textured_draw_travels_with_the_payload_it_samples() {
        // The atlas has no registry id — it is ATLAS_TEXTURE_ID, a constant that
        // `jni::texture_image` can never resolve — so the offscreen path only
        // ever shows text because the page itself rides along in the request.
        // An id-only list silently dropped every glyph; this pins the contract.
        use crate::composite::ATLAS_TEXTURE_ID;
        use crate::texture::{TextureImage, TextureStamp};
        let mut quad_mesh = crate::texture::TexturedMesh::new();
        quad_mesh.push_quad(0.0, 0.0, 4.0, 4.0, [0.0, 0.0, 1.0, 1.0]);
        let mut req = request();
        req.textured = vec![
            TexturedQuad {
                mesh: quad_mesh.clone(),
                color: [1.0, 1.0, 1.0, 1.0],
                transform: req.shapes[0].2,
                texture_id: ATLAS_TEXTURE_ID,
            },
            TexturedQuad {
                mesh: quad_mesh,
                color: [1.0, 1.0, 1.0, 1.0],
                transform: req.shapes[0].2,
                texture_id: 11,
            },
        ];
        req.textures = vec![
            SceneTexture::new(
                ATLAS_TEXTURE_ID,
                TextureStamp::for_atlas(2, 2, 3),
                TextureImage::new(2, 2, vec![5u8; 16]).expect("page"),
            ),
            SceneTexture::new(
                11,
                TextureStamp::for_content(11),
                TextureImage::new(2, 2, vec![6u8; 16]).expect("photo"),
            ),
        ];
        for quad in &req.textured {
            let payload = req
                .textures
                .iter()
                .find(|texture| texture.id == quad.texture_id)
                .unwrap_or_else(|| panic!("texture {} has no payload", quad.texture_id));
            assert!(payload.image.width > 0 && payload.image.height > 0);
        }
    }

    #[test]
    fn time_is_handed_to_effects_in_seconds() {
        let mut req = request();
        req.time_ms = 1500;
        assert_eq!(req.layer_draws()[0].time, 1.5);
    }
}
