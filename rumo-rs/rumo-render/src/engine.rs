// SPDX-License-Identifier: Apache-2.0

//! Dedicated render-thread engine for the Android preview surface.
//!
//! Two rules this module enforces:
//!
//! 1. **No `block_on` on JNI threads, ever.** GPU initialization and all
//!    wgpu work run on a single worker thread owned by [`Engine`]. JNI
//!    callers send a request over a channel and wait with
//!    [`ENGINE_OP_TIMEOUT`]; expiry yields [`ENGINE_ERR_TIMEOUT`], never a
//!    hang. A worker stuck inside a driver call cannot block Java — the JNI
//!    side has already returned.
//! 2. **No global GPU static.** Each [`Engine`] owns its worker thread and
//!    its channels; the JNI layer holds it as `Box::into_raw` → `jlong`
//!    and destroys it explicitly. Dropping the engine signals shutdown and
//!    detaches; the worker drains and exits on its own.
//!
//! Pixel order inside the engine is strictly RGBA8 (R in the low byte,
//! matching [`crate::renderer`] and the export path). The surface path is
//! zero-copy with no channel shuffling; the only ARGB conversion in the
//! whole codebase lives at the legacy bitmap-JNI boundary in `rumo_bridge`.
//!
//! Without a surface (or without a GPU) every render call returns a
//! nonzero code so Kotlin falls back to the bitmap preview; nothing here
//! panics on missing hardware.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::renderer::{GpuRenderer, LayerDraw, MeshData, SceneDraw, TexturedQuad, ordered_layer_draws};
use crate::texture::SceneTexture;
use rumo_core::effect::EffectChains;

/// Success code for every `nativeEngine*` call that returns `int`.
pub const ENGINE_OK: i32 = 0;
/// Bad argument (zero size, null pointer, empty handle).
pub const ENGINE_ERR_BAD_ARG: i32 = -1;
/// No live surface: `surfaceCreated` was never called (or it failed), or
/// the surface was lost/destroyed. Kotlin falls back to bitmap preview.
pub const ENGINE_ERR_NO_SURFACE: i32 = -2;
/// GPU unavailable: adapter/device request failed on the worker thread.
/// Kotlin falls back to bitmap preview.
pub const ENGINE_ERR_NO_GPU: i32 = -3;
/// The worker did not answer in time. The request may still complete in
/// the background; the JNI thread never blocks longer than this.
pub const ENGINE_ERR_TIMEOUT: i32 = -4;
/// Transient per-frame failure (acquire timeout, occluded window,
/// validation): skip the frame and retry later.
pub const ENGINE_ERR_FRAME: i32 = -5;

/// Upper bound any JNI thread waits for the worker thread.
pub const ENGINE_OP_TIMEOUT: Duration = Duration::from_secs(5);

/// One placed shape layer: tessellated geometry, linear RGBA color and a
/// pixel-space → NDC transform. Same triple the offscreen compositor uses.
pub type EngineLayer = (MeshData, [f32; 4], [[f32; 4]; 4]);

/// Stored scene: frame size, linear-RGBA background, SHAPE layers, textured
/// draws (atlas glyphs, photos) and the CPU-side image snapshots the worker
/// uploads under their texture ids before presenting.
#[derive(Debug, Default)]
pub struct EngineScene {
    pub width: u32,
    pub height: u32,
    pub bg: [f32; 4],
    pub layers: Vec<EngineLayer>,
    pub textured: Vec<TexturedQuad>,
    pub textures: Vec<SceneTexture>,
    /// The scene's global draw order across `layers` and `textured`, naming a
    /// draw per element. Empty (the default) means "no order information": the
    /// draws are then emitted in group order, every mesh before every textured
    /// draw, which is what a scene built by hand always produced.
    pub draw_order: Vec<SceneDraw>,
    /// Per-draw effect chains: `shapes[i]` applies to `layers[i]`,
    /// `textures[i]` applies to `textured[i]`. Empty (the default) means the
    /// scene renders on the direct draw path.
    pub chains: EffectChains,
    /// Timeline position of this scene in milliseconds; handed to effect
    /// shaders as seconds so time-varying effects animate with playback.
    pub time_ms: i64,
}

impl EngineScene {
    pub fn new(width: u32, height: u32, bg: [f32; 4], layers: Vec<EngineLayer>) -> Self {
        Self {
            width,
            height,
            bg,
            layers,
            textured: Vec::new(),
            textures: Vec::new(),
            draw_order: Vec::new(),
            chains: EffectChains::default(),
            time_ms: 0,
        }
    }

    /// Attach per-draw effect chains. See [`EngineScene::chains`].
    pub fn with_chains(mut self, chains: EffectChains) -> Self {
        self.chains = chains;
        self
    }

    /// Attach the scene's global draw order across shapes and textured draws.
    /// See [`EngineScene::draw_order`]. Without it the draws keep the group
    /// order (every mesh, then every textured draw).
    pub fn with_draw_order(mut self, draw_order: Vec<SceneDraw>) -> Self {
        self.draw_order = draw_order;
        self
    }

    /// Extended scene with textured draws plus their backing images.
    /// `textures` holds the payloads the worker uploads under the ids the
    /// quads reference, so no draw is ever left pointing at an unknown id.
    /// `ATLAS_TEXTURE_ID` (`crate::composite`) backs every text quad with the
    /// one atlas page, and every payload carries the [`crate::texture::TextureStamp`]
    /// of its pixels: a scene that is re-presented every frame (and the whole
    /// point of this path is that it is) then re-uploads only what actually
    /// changed since the previous frame.
    pub fn new_ex(
        width: u32,
        height: u32,
        bg: [f32; 4],
        layers: Vec<EngineLayer>,
        textured: Vec<TexturedQuad>,
        textures: Vec<SceneTexture>,
    ) -> Self {
        Self {
            width,
            height,
            bg,
            layers,
            textured,
            textures,
            draw_order: Vec::new(),
            chains: EffectChains::default(),
            time_ms: 0,
        }
    }

    /// Every draw of this scene as an effect-aware [`LayerDraw`], in the
    /// scene's global draw order: the merged order across shapes and textured
    /// draws when the scene carries one, the group order (solid shapes first,
    /// then textured draws) otherwise.
    ///
    /// Effect shaders receive [`EngineScene::time_ms`] as seconds, so a
    /// time-varying effect animates exactly as far as the timeline has moved.
    pub fn layer_draws(&self) -> Vec<LayerDraw<'_>> {
        ordered_layer_draws(
            &self.layers,
            &self.textured,
            &self.draw_order,
            &self.chains,
            self.time_ms as f32 / 1000.0,
        )
    }

    fn valid(&self) -> bool {
        self.width != 0 && self.height != 0
    }
}

type Ack = Sender<i32>;

/// Presentation-surface handle travelling from the JNI thread to the worker.
///
/// `ANativeWindow_fromSurface` is a JNI call, so it may only run on a thread the
/// JVM knows about. The worker is deliberately a plain `std::thread` (no JNI
/// env, no `block_on` on a JNI thread), so it cannot resolve the window itself:
/// CheckJNI aborts such a thread with "making JNI calls without being attached".
/// The JNI thread resolves the window instead and hands over the *owned*
/// reference; the worker only wraps it for wgpu and releases it on drop.
#[cfg(target_os = "android")]
struct SurfaceHandle(ndk::native_window::NativeWindow);

/// Off Android there is no window at all; the worker answers
/// [`ENGINE_ERR_NO_SURFACE`] without touching this.
#[cfg(not(target_os = "android"))]
struct SurfaceHandle(());

enum Request {
    SetScene {
        scene: EngineScene,
        ack: Ack,
    },
    SurfaceCreated {
        handle: SurfaceHandle,
        width: u32,
        height: u32,
        ack: Ack,
    },
    SurfaceChanged {
        width: u32,
        height: u32,
        ack: Ack,
    },
    SurfaceDestroyed {
        ack: Ack,
    },
    RenderFrame {
        ack: Ack,
    },
    Shutdown,
}

// SAFETY: requests cross to the single worker thread. Every payload is `Send`
// on its own — the scene owns plain data, the surface travels as an owned
// `SurfaceHandle` (ndk's `NativeWindow` is `Send`) — so this impl only asserts
// that no request type smuggles a non-`Send` JNI reference across.
unsafe impl Send for Request {}

/// Live presentation surface (Android only).
///
/// Drop order matters: `surface` is declared first so it is destroyed
/// before `_window` releases the `ANativeWindow`.
#[cfg(target_os = "android")]
struct LiveSurface {
    surface: wgpu::Surface<'static>,
    _window: ndk::native_window::NativeWindow,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

/// Explicit render engine: a worker thread plus its request channel.
///
/// `Send + Sync` so a `Box<Engine>` behind a `jlong` is callable from any
/// JNI thread; the mutex only guards the join handle, all state lives on
/// the worker.
pub struct Engine {
    tx: Sender<Request>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Engine {
    /// Spawn the worker thread. Never touches the GPU: initialization is
    /// lazy on first surface/render request, inside the worker.
    pub fn new() -> Self {
        let (tx, rx) = channel::<Request>();
        let thread = std::thread::Builder::new()
            .name("rumo-render".into())
            .spawn(move || worker_loop(rx))
            .expect("render worker thread must spawn");
        Self {
            tx,
            thread: Mutex::new(Some(thread)),
        }
    }

    /// Store the scene to render. `ENGINE_OK`, or `ENGINE_ERR_BAD_ARG` for
    /// a zero-sized scene, or `ENGINE_ERR_TIMEOUT` when the worker is busy.
    pub fn set_scene(&self, scene: EngineScene) -> i32 {
        if !scene.valid() {
            return ENGINE_ERR_BAD_ARG;
        }
        let (ack_tx, ack_rx) = channel();
        if self
            .tx
            .send(Request::SetScene {
                scene,
                ack: ack_tx,
            })
            .is_err()
        {
            return ENGINE_ERR_TIMEOUT;
        }
        ack_rx.recv_timeout(ENGINE_OP_TIMEOUT).unwrap_or(ENGINE_ERR_TIMEOUT)
    }

    /// Attach an `android.view.Surface` (`surface_obj`, raw `jobject`) seen
    /// through `jni_env` (raw `*mut JNIEnv`) at `width`×`height`.
    ///
    /// The worker acquires the `ANativeWindow` (+1 ref), creates the wgpu
    /// surface from the renderer's own instance, picks
    /// `Bgra8UnormSrgb`/`Rgba8UnormSrgb` from the capabilities and
    /// configures the swapchain (alpha `Auto`). Non-Android builds return
    /// [`ENGINE_ERR_NO_SURFACE`]: no window can exist there.
    ///
    /// The `ANativeWindow` is resolved **on the calling thread** and only then
    /// handed to the worker: `ANativeWindow_fromSurface` is a JNI call, and the
    /// worker is not attached to the JVM. This is the whole reason the parameter
    /// is a raw env + Surface instead of a plain size.
    pub fn surface_created(
        &self,
        jni_env: *mut c_void,
        surface_obj: *mut c_void,
        width: u32,
        height: u32,
    ) -> i32 {
        if width == 0 || height == 0 || jni_env.is_null() || surface_obj.is_null() {
            return ENGINE_ERR_BAD_ARG;
        }
        #[cfg(target_os = "android")]
        let handle = {
            // SAFETY: `jni_env` is the calling thread's own JNIEnv and
            // `surface_obj` is a live local reference to an
            // `android.view.Surface` for the duration of this call; both come
            // straight from the JNI entry point. `from_surface` acquires a
            // reference to the native window, so the handle owns it from here
            // and can be moved to the worker.
            match unsafe {
                ndk::native_window::NativeWindow::from_surface(
                    jni_env.cast(),
                    surface_obj.cast(),
                )
            } {
                Some(window) => SurfaceHandle(window),
                // Dead Surface (native window already released) → no surface.
                None => return ENGINE_ERR_BAD_ARG,
            }
        };
        #[cfg(not(target_os = "android"))]
        let handle = SurfaceHandle(());
        // Host builds take the same channel path; the worker answers
        // NO_SURFACE there because no window can exist off Android.
        let (ack_tx, ack_rx) = channel();
        if self
            .tx
            .send(Request::SurfaceCreated {
                handle,
                width,
                height,
                ack: ack_tx,
            })
            .is_err()
        {
            return ENGINE_ERR_TIMEOUT;
        }
        ack_rx.recv_timeout(ENGINE_OP_TIMEOUT).unwrap_or(ENGINE_ERR_TIMEOUT)
    }

    /// Reconfigure the swapchain for a new surface size. `ENGINE_OK`;
    /// `ENGINE_ERR_NO_SURFACE` when no surface is attached (the next
    /// `surfaceCreated` rebuilds everything); `ENGINE_ERR_BAD_ARG` on zero
    /// size. The stored scene is *not* re-tessellated: callers re-issue
    /// `setScene` at the new size when geometry must follow.
    pub fn surface_changed(&self, width: u32, height: u32) -> i32 {
        if width == 0 || height == 0 {
            return ENGINE_ERR_BAD_ARG;
        }
        // Same channel path on every target; without an attached surface
        // the worker answers NO_SURFACE.
        let (ack_tx, ack_rx) = channel();
        if self
            .tx
            .send(Request::SurfaceChanged {
                width,
                height,
                ack: ack_tx,
            })
            .is_err()
        {
            return ENGINE_ERR_TIMEOUT;
        }
        ack_rx.recv_timeout(ENGINE_OP_TIMEOUT).unwrap_or(ENGINE_ERR_TIMEOUT)
    }

    /// Detach the surface (idempotent, always `ENGINE_OK` unless the worker
    /// is unreachable). The device/queue are kept for the next surface.
    pub fn surface_destroyed(&self) -> i32 {
        let (ack_tx, ack_rx) = channel();
        if self
            .tx
            .send(Request::SurfaceDestroyed { ack: ack_tx })
            .is_err()
        {
            return ENGINE_ERR_TIMEOUT;
        }
        ack_rx.recv_timeout(ENGINE_OP_TIMEOUT).unwrap_or(ENGINE_ERR_TIMEOUT)
    }

    /// Render the stored scene to the surface and present. `ENGINE_OK` on
    /// success; `NO_SURFACE`/`NO_GPU` when the path is unavailable (Kotlin
    /// falls back to bitmap); `FRAME` on transient acquire failures (skip
    /// and retry); `TIMEOUT` when the worker is busy past the deadline.
    pub fn render_frame(&self) -> i32 {
        #[cfg(not(target_os = "android"))]
        {
            // No window can exist off Android; answer without touching GPU.
            // The worker is not even woken, so this is infallible.
            let (ack_tx, ack_rx) = channel();
            if self.tx.send(Request::RenderFrame { ack: ack_tx }).is_err() {
                return ENGINE_ERR_TIMEOUT;
            }
            return ack_rx.recv_timeout(ENGINE_OP_TIMEOUT).unwrap_or(ENGINE_ERR_TIMEOUT);
        }
        #[cfg(target_os = "android")]
        {
            let (ack_tx, ack_rx) = channel();
            if self.tx.send(Request::RenderFrame { ack: ack_tx }).is_err() {
                return ENGINE_ERR_TIMEOUT;
            }
            ack_rx.recv_timeout(ENGINE_OP_TIMEOUT).unwrap_or(ENGINE_ERR_TIMEOUT)
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Best-effort shutdown; the worker also exits when the last sender
        // (us) disappears and the queue drains. Joined only when already
        // finished: a worker stuck inside a driver call must not hang the
        // thread that destroys the engine — the detached thread exits on
        // its own once the in-flight op completes.
        let _ = self.tx.send(Request::Shutdown);
        if let Ok(mut slot) = self.thread.try_lock() {
            let finished = slot.as_ref().map(JoinHandle::is_finished).unwrap_or(true);
            if finished {
                let _ = slot.take().map(JoinHandle::join);
            }
        }
    }
}

fn reply(ack: &Ack, code: i32) {
    let _ = ack.send(code);
}

/// Lazily create the GPU context on the worker thread. `block_on` lives
/// only here — never on a JNI thread.
#[cfg(target_os = "android")]
fn ensure_gpu(gpu: &mut Option<GpuRenderer>) -> Result<&GpuRenderer, i32> {
    if gpu.is_none() {
        match pollster::block_on(GpuRenderer::new()) {
            Ok(g) => *gpu = Some(g),
            Err(e) => {
                // The whole engine path lives or dies here; without this the
                // reason only ever surfaced as a bare `ENGINE_ERR_NO_GPU`.
                crate::diag::error("gpu_init", e);
                return Err(ENGINE_ERR_NO_GPU);
            }
        }
    }
    Ok(gpu.as_ref().expect("GPU context was just created"))
}

#[cfg(any(target_os = "android", test))]
fn is_outdated(err: &str) -> bool {
    err.contains("outdated")
}

#[cfg(any(target_os = "android", test))]
fn is_lost(err: &str) -> bool {
    err.contains("lost")
}

#[cfg(target_os = "android")]
fn on_surface_created(
    gpu: &mut Option<GpuRenderer>,
    surface_slot: &mut Option<LiveSurface>,
    SurfaceHandle(window): SurfaceHandle,
    width: u32,
    height: u32,
) -> i32 {
    use crate::renderer::{make_surface_config, pick_surface_format};

    let gpu = match ensure_gpu(gpu) {
        Ok(g) => g,
        Err(code) => return code,
    };
    // SAFETY: `window` is live and is stored next to the surface with
    // matching drop order (surface first), satisfying create_android_surface.
    let surface = match unsafe { gpu.create_android_surface(window.ptr().as_ptr().cast()) } {
        Ok(s) => s,
        Err(e) => {
            crate::diag::error("surface_create", format!("ANativeWindow -> surface: {e}"));
            return ENGINE_ERR_NO_SURFACE;
        }
    };
    let caps = gpu.surface_capabilities(&surface);
    let format = match pick_surface_format(&caps.formats) {
        Some(f) => f,
        None => {
            crate::diag::error(
                "surface_format",
                format!(
                    "no usable swapchain format among {:?} (need an sRGB or 8-bit unorm target)",
                    caps.formats
                ),
            );
            return ENGINE_ERR_NO_GPU;
        }
    };
    let config = match make_surface_config(width, height, format) {
        Some(c) => c,
        None => {
            crate::diag::error(
                "surface_config",
                format!("cannot configure a {width}x{height} surface as {format:?}"),
            );
            return ENGINE_ERR_BAD_ARG;
        }
    };
    gpu.configure_surface(&surface, &config);
    crate::diag::info(
        "surface_ready",
        format!("Surface engine configured: {width}x{height} {format:?}"),
    );
    *surface_slot = Some(LiveSurface {
        surface,
        _window: window,
        width,
        height,
        format,
    });
    ENGINE_OK
}

#[cfg(target_os = "android")]
fn on_render_frame(
    gpu: &mut Option<GpuRenderer>,
    surface_slot: &mut Option<LiveSurface>,
    scene: &EngineScene,
) -> i32 {
    use crate::renderer::make_surface_config;

    let slot = match surface_slot.as_ref() {
        Some(s) => s,
        None => return ENGINE_ERR_NO_SURFACE,
    };
    let gpu = match ensure_gpu(gpu) {
        Ok(g) => g,
        Err(code) => return code,
    };
    // Upload the scene's image payloads (atlas + photos) under their ids so
    // the textured draws below never reference an unknown texture. The stamp
    // decides it: a frame that repeats the previous one transfers nothing.
    for texture in &scene.textures {
        if let Err(e) = gpu.set_texture(texture.id, texture.stamp, &texture.image) {
            crate::diag::error("texture_upload", format!("texture {}: {e}", texture.id));
            return ENGINE_ERR_NO_GPU;
        }
    }
    let draws = scene.layer_draws();
    match gpu.render_to_surface_draws(&slot.surface, scene.bg, &draws, scene.chains.customs()) {
        Ok(()) => {
            crate::diag::note_engine_ok(true);
            crate::diag::note_gpu_path();
            ENGINE_OK
        }
        Err(e) => {
            if is_outdated(&e) {
                // Swapchain no longer matches: reconfigure once and retry.
                crate::diag::warn("surface_outdated", format!("reconfiguring: {e}"));
                if let Some(config) = make_surface_config(slot.width, slot.height, slot.format) {
                    gpu.configure_surface(&slot.surface, &config);
                    return match gpu.render_to_surface_draws(&slot.surface, scene.bg, &draws, scene.chains.customs()) {
                        Ok(()) => {
                            crate::diag::note_engine_ok(true);
                            crate::diag::note_gpu_path();
                            ENGINE_OK
                        }
                        Err(retry) => {
                            crate::diag::note_engine_ok(false);
                            crate::diag::note_cpu_path("engine_frame_retry", retry);
                            ENGINE_ERR_FRAME
                        }
                    };
                }
                crate::diag::note_engine_ok(false);
                crate::diag::note_cpu_path("engine_frame_config", "surface reconfiguration failed");
                return ENGINE_ERR_FRAME;
            }
            if is_lost(&e) {
                // Surface needs recreation from Java; drop the slot so the
                // next frame reports NO_SURFACE instead of error-spinning.
                *surface_slot = None;
                crate::diag::note_engine_ok(false);
                crate::diag::note_cpu_path("surface_lost", e);
                return ENGINE_ERR_NO_SURFACE;
            }
            crate::diag::note_engine_ok(false);
            crate::diag::note_cpu_path("engine_frame", e);
            ENGINE_ERR_FRAME
        }
    }
}

/// The worker owns everything GPU-related: context, scene, surface.
/// Every arm replies exactly once; unknown-hardware states are codes,
/// never panics.
fn worker_loop(rx: Receiver<Request>) {
    let mut gpu: Option<GpuRenderer> = None;
    let mut scene = EngineScene::default();
    #[cfg(target_os = "android")]
    let mut surface_slot: Option<LiveSurface> = None;

    while let Ok(req) = rx.recv() {
        match req {
            Request::SetScene { scene: next, ack } => {
                scene = next;
                reply(&ack, ENGINE_OK);
            }
            Request::SurfaceCreated {
                handle,
                width,
                height,
                ack,
            } => {
                #[cfg(target_os = "android")]
                {
                    let code = on_surface_created(
                        &mut gpu,
                        &mut surface_slot,
                        handle,
                        width,
                        height,
                    );
                    reply(&ack, code);
                }
                #[cfg(not(target_os = "android"))]
                {
                    let _ = (handle, width, height);
                    reply(&ack, ENGINE_ERR_NO_SURFACE);
                }
            }
            Request::SurfaceChanged { width, height, ack } => {
                #[cfg(target_os = "android")]
                {
                    use crate::renderer::make_surface_config;
                    match surface_slot.as_mut() {
                        None => reply(&ack, ENGINE_ERR_NO_SURFACE),
                        Some(slot) => {
                            let Some(config) =
                                make_surface_config(width, height, slot.format)
                            else {
                                reply(&ack, ENGINE_ERR_BAD_ARG);
                                continue;
                            };
                            let Some(g) = gpu.as_ref() else {
                                reply(&ack, ENGINE_ERR_NO_GPU);
                                continue;
                            };
                            g.configure_surface(&slot.surface, &config);
                            slot.width = width;
                            slot.height = height;
                            reply(&ack, ENGINE_OK);
                        }
                    }
                }
                #[cfg(not(target_os = "android"))]
                {
                    let _ = (width, height);
                    reply(&ack, ENGINE_ERR_NO_SURFACE);
                }
            }
            Request::SurfaceDestroyed { ack } => {
                #[cfg(target_os = "android")]
                {
                    surface_slot = None;
                }
                reply(&ack, ENGINE_OK);
            }
            Request::RenderFrame { ack } => {
                #[cfg(target_os = "android")]
                {
                    let code = on_render_frame(&mut gpu, &mut surface_slot, &scene);
                    reply(&ack, code);
                }
                #[cfg(not(target_os = "android"))]
                {
                    // Host builds cannot present; report before touching GPU.
                    // `scene` is only borrowed to keep the arm symmetric.
                    let _ = &scene;
                    let _ = &mut gpu;
                    reply(&ack, ENGINE_ERR_NO_SURFACE);
                }
            }
            Request::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{make_surface_config, pick_present_mode, pick_surface_format};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn engine_is_send_sync_for_jlong_handles() {
        assert_send_sync::<Engine>();
    }

    #[test]
    fn codes_are_distinct_and_nonzero_on_failure() {
        assert_eq!(ENGINE_OK, 0);
        let codes = [
            ENGINE_ERR_BAD_ARG,
            ENGINE_ERR_NO_SURFACE,
            ENGINE_ERR_NO_GPU,
            ENGINE_ERR_TIMEOUT,
            ENGINE_ERR_FRAME,
        ];
        assert!(codes.iter().all(|c| *c != 0));
        let mut sorted = codes.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "error codes must be distinct");
    }

    #[test]
    fn full_lifecycle_without_surface_never_panics() {
        // No GPU, no window in the container: every call must answer with a
        // code, never panic or block.
        let engine = Engine::new();
        // Zero-sized scene is rejected before the worker is even involved.
        assert_eq!(
            engine.set_scene(EngineScene::new(0, 0, [0.0; 4], vec![])),
            ENGINE_ERR_BAD_ARG
        );
        assert_eq!(
            engine.set_scene(EngineScene::new(64, 36, [0.08, 0.09, 0.14, 1.0], vec![])),
            ENGINE_OK
        );
        assert_eq!(engine.render_frame(), ENGINE_ERR_NO_SURFACE);
        assert_eq!(engine.surface_changed(64, 36), ENGINE_ERR_NO_SURFACE);
        // Idempotent destroy always succeeds.
        assert_eq!(engine.surface_destroyed(), ENGINE_OK);
        assert_eq!(engine.surface_destroyed(), ENGINE_OK);
        // Null / zero args are BAD_ARG even on host.
        assert_eq!(
            engine.surface_created(std::ptr::null_mut(), std::ptr::null_mut(), 64, 36),
            ENGINE_ERR_BAD_ARG
        );
        assert_eq!(
            engine.surface_created(1 as *mut c_void, 1 as *mut c_void, 0, 36),
            ENGINE_ERR_BAD_ARG
        );
        // Non-null but window-less on host: no surface can exist.
        #[cfg(not(target_os = "android"))]
        assert_eq!(
            engine.surface_created(1 as *mut c_void, 1 as *mut c_void, 64, 36),
            ENGINE_ERR_NO_SURFACE
        );
        // Still usable after errors; drop joins nothing and must not hang.
        assert_eq!(engine.render_frame(), ENGINE_ERR_NO_SURFACE);
        drop(engine);
    }

    #[test]
    fn scene_ex_stores_textured_and_snapshots() {
        use crate::composite::ATLAS_TEXTURE_ID;
        use crate::texture::{SceneTexture, TextureImage, TextureStamp, TexturedMesh};
        let img = TextureImage::new(2, 2, vec![9u8; 16]).expect("image");
        let mut mesh = TexturedMesh::new();
        mesh.push_quad(0.0, 0.0, 2.0, 2.0, [0.0, 0.0, 1.0, 1.0]);
        let quad = crate::renderer::TexturedQuad {
            mesh,
            color: [1.0; 4],
            transform: crate::composite::ndc_matrix(64.0, 36.0),
            texture_id: ATLAS_TEXTURE_ID,
        };
        let stamp = TextureStamp::for_atlas(2, 2, 1);
        let scene = EngineScene::new_ex(
            64,
            36,
            [0.0; 4],
            vec![],
            vec![quad],
            vec![SceneTexture::new(ATLAS_TEXTURE_ID, stamp, img.clone())],
        );
        assert_eq!(scene.textured.len(), 1);
        assert_eq!(scene.textures.len(), 1);
        assert_eq!(scene.textures[0].id, ATLAS_TEXTURE_ID);
        assert_eq!(scene.textures[0].stamp, stamp);
        assert_eq!(*scene.textures[0].image, img);
        // Legacy constructor leaves the extended fields empty.
        let legacy = EngineScene::new(64, 36, [0.0; 4], vec![]);
        assert!(legacy.textured.is_empty() && legacy.textures.is_empty());
        // Still a valid scene for the worker protocol.
        let engine = Engine::new();
        assert_eq!(engine.set_scene(scene), ENGINE_OK);
        assert_eq!(engine.render_frame(), ENGINE_ERR_NO_SURFACE);
    }

    #[test]
    fn many_engines_are_independent() {
        let a = Engine::new();
        let b = Engine::new();
        assert_eq!(
            a.set_scene(EngineScene::new(32, 32, [1.0, 0.0, 0.0, 1.0], vec![])),
            ENGINE_OK
        );
        // b never got a scene: both still report NO_SURFACE, neither
        // interferes with the other (no global GPU static anymore).
        assert_eq!(a.render_frame(), ENGINE_ERR_NO_SURFACE);
        assert_eq!(b.render_frame(), ENGINE_ERR_NO_SURFACE);
    }

    #[test]
    fn surface_format_avoids_srgb_so_photos_survive() {
        use wgpu::TextureFormat::*;
        // Mock capability lists: no GPU involved.
        //
        // The rule is that a value must not be sRGB-encoded twice. Picking an
        // `-Srgb` swapchain next to `Rgba8Unorm` textures and an `Rgba8Unorm`
        // offscreen target does exactly that to the final write, and it looks
        // like the *photo* is wrong rather than like the format is.
        assert_eq!(
            pick_surface_format(&[Bgra8UnormSrgb, Rgba8UnormSrgb, Bgra8Unorm, Rgba8Unorm]),
            Some(Bgra8Unorm),
            "the plain formats must win over their sRGB twins"
        );
        assert_eq!(
            pick_surface_format(&[Rgba8UnormSrgb, Rgba8Unorm]),
            Some(Rgba8Unorm)
        );
        // Only sRGB offered: take it rather than fail. It is still the driver's
        // first choice and a frame beats no frame, but it means the final write
        // re-encodes, which `renderDiagnostics` reports through the adapter.
        assert_eq!(
            pick_surface_format(&[Rgba8UnormSrgb]),
            Some(Rgba8UnormSrgb),
            "an sRGB-only surface is usable, if not colour-exact"
        );
        assert_eq!(pick_surface_format(&[Rgba8Unorm]), Some(Rgba8Unorm));
        assert_eq!(pick_surface_format(&[]), None);
    }

    #[test]
    fn surface_config_fields_and_zero_rejection() {
        use wgpu::{CompositeAlphaMode, PresentMode, TextureFormat, TextureUsages};
        let config = make_surface_config(1280, 720, TextureFormat::Bgra8UnormSrgb)
            .expect("valid size must configure");
        assert_eq!((config.width, config.height), (1280, 720));
        assert_eq!(config.format, TextureFormat::Bgra8UnormSrgb);
        assert_eq!(config.usage, TextureUsages::RENDER_ATTACHMENT);
        assert_eq!(config.alpha_mode, CompositeAlphaMode::Auto);
        assert_eq!(config.present_mode, PresentMode::Fifo);
        assert_eq!(config.desired_maximum_frame_latency, 2);
        assert!(config.view_formats.is_empty());
        assert_eq!(make_surface_config(0, 720, TextureFormat::Bgra8UnormSrgb), None);
        assert_eq!(make_surface_config(1280, 0, TextureFormat::Bgra8UnormSrgb), None);
    }

    #[test]
    fn present_mode_prefers_fifo() {
        use wgpu::PresentMode::*;
        assert_eq!(pick_present_mode(&[Mailbox, Fifo, Immediate]), Some(Fifo));
        assert_eq!(pick_present_mode(&[Immediate, Mailbox]), Some(Immediate));
        assert_eq!(pick_present_mode(&[]), None);
    }

    #[test]
    fn outdated_and_lost_classifiers_match_our_literals() {
        assert!(is_outdated("surface outdated"));
        assert!(!is_outdated("surface lost"));
        assert!(is_lost("surface lost"));
        assert!(!is_lost("surface outdated"));
    }
}
