// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.content.Context
import android.graphics.Bitmap
import android.net.Uri
import android.os.ParcelFileDescriptor
import java.io.File
import android.os.Handler
import android.os.Looper
import android.view.Choreographer
import android.view.Surface
import com.kerneldroid.rumo.data.AppLog
import com.kerneldroid.rumo.data.EffectStore
import com.kerneldroid.rumo.data.RumoBridge
import java.nio.IntBuffer
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.math.roundToInt
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

enum class LayerKindUi {
    SHAPE,
    TEXT,
    MEDIA,
    AUDIO,
}

val LayerPalette: List<Long> = listOf(
    0xFFFF9800,
    0xFF4DD0E1,
    0xFFF44336,
    0xFF4CAF50,
    0xFFE040FB,
    0xFFFFFFFF,
)

fun formatTime(ms: Long): String {
    val clamped = ms.coerceAtLeast(0L)
    val minutes = (clamped / 60_000L).toInt()
    val seconds = ((clamped / 1_000L) % 60L).toInt()
    val frames = (((clamped % 1_000L) * 30L) / 1_000L).toInt()
    return "%02d:%02d:%02d".format(minutes, seconds, frames)
}

data class LayerUi(
    val id: String,
    val kind: LayerKindUi,
    val name: String,
    val visible: Boolean,
    val argb: Long = 0L,
    val offsetX: Float = 0f,
    val offsetY: Float = 0f,
    /**
     * Uniform size multiplier, on top of each kind's base size (60% of frame
     * height). MEDIA/SHAPE only: the engine has no per-text scale, so the
     * overlay draws no scale handles for TEXT. Never 0 — a degenerate rect
     * makes Rust skip the draw and silently shift the effect-chain indices.
     */
    val scale: Float = 1f,
    val alpha: Float = 1f,
    val durationMs: Long = 5000L,
    val uri: String? = null,
    val text: String = "",
    /**
     * The text layer's font weight: 400 regular, 500 medium, 700 bold.
     * A family that has such a face provides it; for one that does not, the
     * engine thickens the bitmap, so the choice works on any device.
     */
    val textWeight: Int = 400,
    /**
     * The text layer's font family; empty means the built-in monospace face.
     *
     * The name is what `nativeFontRegister` returned when a font was installed
     * from the shop, not the name from the catalogue: the file is addressed in
     * the font database by its own name, and substituting the catalogue name
     * would lay the text out in a different face. An empty string, not a separate
     * "built-in" flag, because an empty name means "built-in" on both sides — in
     * `layoutTextFamily` and in `Family::Name`.
     */
    val textFamily: String = "",
    /** The text outline thickness in frame pixels; 0 means no outline. */
    val strokePx: Float = 0f,
    /** The outline colour, 0xAARRGGBB; drawn under the glyphs. */
    val strokeArgb: Long = 0xFF000000L,
    val keys: List<KeyframeUi> = listOf(KeyframeUi(0L, 0f)),
    /**
     * When the layer appears on the timeline, ms from the start of the project
     * (docs/11 §11.1). A field of the layer, not of its `extra`: this is a
     * position on the timeline, and it exists even for a layer without `extra`.
     * Always `>= 0` — a negative value is clamped to zero rather than dropped,
     * otherwise the layer would silently disappear.
     */
    val startMs: Long = 0L,
    /**
     * Animated horizontal offset (docs/11 §11.3). An empty track = "not
     * animated, take [offsetX]"; an empty track yielding 0.0 is exactly the
     * defect that looks like "the layer has vanished".
     */
    val xKeys: List<KeyframeUi> = emptyList(),
    /** Animated vertical offset; an empty track = the base [offsetY]. */
    val yKeys: List<KeyframeUi> = emptyList(),
    /** Animated size; an empty track = the base [scale]. */
    val scaleKeys: List<KeyframeUi> = emptyList(),
    /** Animated opacity; an empty track = the base [alpha]. */
    val alphaKeys: List<KeyframeUi> = emptyList(),
    /** The layer's effect chain (order = application order); a Rust contract. */
    val effects: List<Effect> = emptyList(),
    /** The incoming layer transition; null means no transition. Rust contract (tag 4). */
    val transition: TransitionUi? = null,
) {
    companion object {
        /** The allowed range of [LayerUi.scale]; also clamped on import. */
        const val MIN_SCALE = 0.05f
        const val MAX_SCALE = 8f
    }
}

/**
 * An incoming layer's transition: "over [startMs] from the start of the project I
 * appear on top of the layer beneath me".
 *
 * The engine has no graph executor: it composites the stack in order and blends
 * each layer by the alpha handed to it for the frame. So the transition is stored
 * as data on the *incoming* layer and expanded in
 * [EditorState.alphaMultipliers] into the frame's alphas — that is, it remains
 * genuine GPU blending of two layers, and nothing is baked in.
 */
data class TransitionUi(
    val startMs: Long = 0L,
    val durationMs: Long = 500L,
    /**
     * true — a cross-dissolve with the layer below (both alphas move);
     * false — appearance only, we leave the layer below alone.
     */
    val withPrevious: Boolean = true,
    val enabled: Boolean = true,
    /**
     * The shape of the ramp. The default, [EaseUi.SMOOTH], is **exactly** the
     * `3t² - 2t³` the ramp was before curves, so a project that never chose a
     * curve looks precisely as it did before.
     */
    val ease: EaseUi = EaseUi.SMOOTH,
) {
    /**
     * Progress 0..1 at time [t].
     *
     * Smoothed by default: a linear ramp in a dissolve reads as a kink at the
     * start and at the end. The formula has to be literally the same as in Rust's
     * `Transition::ramp_at` — this is one ramp, not two similar ones.
     */
    fun rampAt(t: Long): Float {
        val d = durationMs.coerceAtLeast(MIN_DURATION_MS).toFloat()
        val p = ((t - startMs).toFloat() / d).coerceIn(0f, 1f)
        return ease.at(p)
    }

    companion object {
        /** Zero would divide by zero in [rampAt]; also clamped on load. */
        const val MIN_DURATION_MS = 1L
        const val MAX_DURATION_MS = 60_000L
    }
}

/**
 * A single layer effect. [kindId] is a stable id from `nativeEffectCatalogue()`
 * (`EffectKind::id` in Rust); an unknown id is dropped by Rust on decode.
 * [params] is a flat vector of length `slots` for that kind.
 */
data class Effect(
    val id: String = java.util.UUID.randomUUID().toString(),
    val kindId: String,
    val enabled: Boolean = true,
    val params: List<Float> = emptyList(),
)

/** The default effect for a kind id (params = the catalogue defaults, length = slots). */
fun defaultEffectFor(kindId: String, customsJson: String = ""): Effect? {
    val params = RumoBridge.defaultEffectParams(kindId, customsJson) ?: return null
    return Effect(kindId = kindId, params = params)
}

/**
 * A single key: time, value and the curve **from this key to the next**.
 *
 * The curve is stored on the *outgoing* key, not on the pair — just like
 * `animation-timing-function` in CSS and like any keyframe editor: a key owns
 * how it leaves. On the last key the curve is unused — there is no segment after
 * it.
 */
/**
 * The curve in JSON: the flat shape `{"kind":"cubic","x1":…}`, one branch on read.
 *
 * The same shape Rust writes and reads (`project_json::ease_json`/`EaseDto`):
 * a file written by Kotlin has to be the same file Kotlin reads.
 */
private fun easeToJson(ease: EaseUi): JSONObject {
    val o = JSONObject().put(
        "kind",
        when (ease.kind) {
            EaseUi.Kind.LINEAR -> "linear"
            EaseUi.Kind.HOLD -> "hold"
            EaseUi.Kind.CUBIC -> "cubic"
        },
    )
    if (ease.kind == EaseUi.Kind.CUBIC) {
        o.put("x1", ease.x1.toDouble())
            .put("y1", ease.y1.toDouble())
            .put("x2", ease.x2.toDouble())
            .put("y2", ease.y2.toDouble())
    }
    return o
}

/**
 * The curve from JSON. `null`, an empty object and an unknown `kind` all mean a
 * straight line, that is, exactly what a document written before curves meant.
 */
private fun easeFromJson(o: JSONObject?): EaseUi {
    if (o == null) return EaseUi()
    return when (o.optString("kind", "linear")) {
        "hold" -> EaseUi(EaseUi.Kind.HOLD)
        "cubic" -> EaseUi(
            EaseUi.Kind.CUBIC,
            o.optDouble("x1", 0.0).toFloat(),
            o.optDouble("y1", 0.0).toFloat(),
            o.optDouble("x2", 0.0).toFloat(),
            o.optDouble("y2", 1.0).toFloat(),
        )
        else -> EaseUi()
    }
}

data class KeyframeUi(
    val timeMs: Long,
    val value: Float,
    val ease: EaseUi = EaseUi(),
)

data class MeshUi(
    val points: List<Pair<Float, Float>>,
) {
    companion object {
        val EMPTY = MeshUi(emptyList())
    }
}

class EditorState {
    companion object {
        const val DEFAULT_MIN_DURATION_MS = 5000L

        /**
         * The depth of the undo history. The old is discarded: 64 steps is
         * already "a lot", an unbounded history in a long session would eat
         * memory, and each step is a whole project document.
         */
        private const val UNDO_LIMIT = 64

        /**
         * Shortest layer that is still worth having: below this a layer is a
         * frame or two and cannot be grabbed in the timeline.
         */
        const val MIN_LAYER_DURATION_MS = 200L

        /**
         * Longest layer, six hours.
         *
         * This was ten minutes, and ten minutes is not a limit of anything —
         * not the engine, not the exporter, not the timeline: a layer's duration
         * is a `Long` and the project's length is the maximum over layers. It was
         * a number in one clamp, and it made a montage longer than ten minutes
         * impossible to build. The bound that remains exists so a typo cannot
         * ask for a million years of timeline.
         */
        const val MAX_LAYER_DURATION_MS = 6L * 60L * 60L * 1000L
        /**
         * The default canvas size. This is precisely a default, not a limit: a
         * project's canvas is set in the project itself (see [setCanvas]) —
         * 512×288 remains the size a new project starts from.
         */
        const val PREVIEW_W = 512
        const val PREVIEW_H = 288
        const val PREVIEW_BG = 0xFF141824

        /**
         * The background, as a layer.
         *
         * It is an ordinary SHAPE layer whose shape is `Frame`, which the engine
         * draws to cover the target instead of placing it inside
         * (`preview_shape_draws` special-cases it). That is the whole trick: the
         * background becomes a row in the layer list and therefore inherits
         * effects, opacity, keyframes and visibility without any of it being
         * written a second time — which is what it lacked when it was only the
         * colour the frame was cleared with.
         */
        fun backgroundLayer(argb: Long): LayerUi = LayerUi(
            id = "1",
            kind = LayerKindUi.SHAPE,
            name = BACKGROUND_SHAPE,
            visible = true,
            argb = argb,
        )

        /** The shape name that means "the frame itself"; see [backgroundLayer]. */
        const val BACKGROUND_SHAPE = "Frame"

        /** True for the layer the engine draws to the frame rather than in it. */
        fun isBackground(layer: LayerUi): Boolean =
            layer.kind == LayerKindUi.SHAPE && layer.name == BACKGROUND_SHAPE

        /** Canvas bounds: nothing smaller is worth showing, nothing larger fits in memory. */
        const val MIN_CANVAS = 16
        const val MAX_CANVAS = 8192

        /**
         * The layout font size as a fraction of the canvas height (32/288 at the
         * default). A fraction, not a constant: on a 1920 canvas text of the same
         * point size would occupy a sixth of the frame and look like a mistake.
         */
        const val TEXT_LAYOUT_RATIO = 32f / PREVIEW_H.toFloat()

        /** Text weight bounds: outside them the font base has not a single face. */
        /**
         * The text weight, 1..1000.
         *
         * Not 100..900: a variable font declares its own axis (Google Sans Flex is
         * 1..1000), and clipping to "nine named steps" would make both ends of the
         * axis unreachable. A static face works in this range as before — the
         * engine draws in the missing thickness.
         */
        const val MIN_TEXT_WEIGHT = 1
        const val MAX_TEXT_WEIGHT = 1000

        /**
         * The outline thickness limit, in frame pixels.
         *
         * An outline is a dilation of the glyph mask, that is, work per glyph;
         * twenty pixels at a hundred-point size already eats a letter whole, so
         * beyond that it stops being an outline and becomes a blot.
         */
        const val MAX_STROKE_PX = 20f

        /**
         * How many frames a texture being released survives: it may still take
         * part in a command that has not executed. The frame queue is short, so
         * two is enough with room to spare.
         */
        const val VIDEO_RETIRED_FRAMES = 2

        /**
         * Fit `naturalW`×`naturalH` into the `boxW`×`boxH` box while keeping the
         * aspect ratio, and return `[width, height]`.
         *
         * The longer side is fitted in full, the shorter one by what is left, so
         * the result never goes outside the box and never stretches the picture. A
         * missing or degenerate natural size gives the box itself: something has
         * to be drawn, and "I do not know the aspect ratio" is no reason to
         * stretch.
         *
         * Split out and stateless because it has to match in both renderers —
         * the Rust frame builder and the Kotlin fallback path; a divergence
         * between them shows as "one thing in the preview, another in the export".
         */
        fun fitInside(boxW: Float, boxH: Float, naturalW: Int?, naturalH: Int?): FloatArray {
            val nw = naturalW ?: 0
            val nh = naturalH ?: 0
            if (!(boxW > 0f) || !(boxH > 0f) || nw <= 0 || nh <= 0) {
                return floatArrayOf(boxW, boxH)
            }
            val k = minOf(boxW / nw.toFloat(), boxH / nh.toFloat())
            return floatArrayOf(nw * k, nh * k)
        }
    }

    /**
     * What the text layer's layout is made from: content, size, weight, outline.
     *
     * The layout lives in a Rust registry and is only ever recomputed whole, so
     * "the same" has to be recognisable: a change to any of these four is a new
     * handle, not the same one.
     */
    private data class TextLayoutStyle(
        val content: String,
        val sizePx: Float,
        val weight: Int,
        val strokePx: Int,
        /**
         * The family is part of the cache key: without it, changing the font would
         * not rebuild the layout, and the layer would stay set in the previous
         * face even though the field already shows the new one.
         */
        val family: String,
    )

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val mainHandler = Handler(Looper.getMainLooper())
    private val idSeq = AtomicLong(1L)
    private val shapeColorSeq = AtomicLong(1L)

    // --- VSYNC ticker (Choreographer instead of delay(50L)) ---
    @Volatile
    private var choreographer: Choreographer? = null

    @Volatile
    private var lastFrameNanos: Long = 0L

    private val frameCallback = object : Choreographer.FrameCallback {
        override fun doFrame(frameTimeNanos: Long) {
            if (!_isPlaying.value) return
            val last = lastFrameNanos
            lastFrameNanos = frameTimeNanos
            // The first frame after play: last == 0 → we take one 60fps frame.
            val dtMs = if (last == 0L) {
                16L
            } else {
                ((frameTimeNanos - last) / 1_000_000L).coerceIn(0L, 250L)
            }
            val end = _projectDurationMs.value
            // Audio is the master clock: the playhead = nativeAudioPosition.
            val next = audioMasterMs() ?: (_playheadMs.value + dtMs)
            if (next >= end) {
                _playheadMs.value = end
                pausePlayback()
            } else {
                _playheadMs.value = next.coerceAtLeast(0L)
                choreographer?.postFrameCallback(this)
            }
        }
    }

    // --- Engine handle (Surface path; 0 = no engine → bitmap fallback) ---
    @Volatile
    var engineHandle: Long = 0L
        private set

    private val _engineActive = MutableStateFlow(false)
    val engineActive: StateFlow<Boolean> = _engineActive.asStateFlow()

    /** Rust engine object exists (handle != 0): the surface path may be tried.
     * Kept separate from [engineActive] so the SurfaceView can be mounted
     * *before* the first surface callback — otherwise nothing would ever create
     * the surface (the old dead-code trap). */
    private val _engineReady = MutableStateFlow(false)
    val engineReady: StateFlow<Boolean> = _engineReady.asStateFlow()

    /** The engine answered with an error for a surface: stop offering the
     * SurfaceView and show the CPU frame instead. */
    private val _surfaceFailed = MutableStateFlow(false)
    val surfaceFailed: StateFlow<Boolean> = _surfaceFailed.asStateFlow()

    // --- Audio players: layerId -> native handle (0 impossible, open failed = null) ---
    private val audioHandles = ConcurrentHashMap<String, Long>()

    // --- Rust registries: staging of pictures/text for the engine ---
    /** uri -> texture id from nativeUploadImage. */
    private val textureIds = ConcurrentHashMap<String, Long>()

    /**
     * uri -> the texture's natural size (px), [0] = width, [1] = height.
     *
     * Needed so a photo is not stretched. Previously MEDIA always occupied a
     * square of `60% of the frame height × scale`, that is, the aspect ratio was
     * thrown away on import, and beyond that it is no longer "the wrong scale"
     * but the wrong pixels: a stretched photo cannot be put back together. The
     * layer scales around the centre of the box, which is computed from these
     * sizes, so `scale` remains "a fraction of the frame height".
     */
    private val textureSizes = ConcurrentHashMap<String, IntArray>()

    /** layerId -> the layout handle from nativeLayoutText. */
    private val layoutHandles = ConcurrentHashMap<String, Long>()

    /**
     * layerId -> the **outline** handle (nativeLayoutTextStyled with a thickness).
     *
     * The outline is a separate layout of the same text, thickened by its own
     * thickness, and it is drawn beneath the fill. That way the layer keeps one
     * draw call per handle, and the element-wise correspondence of effect chains
     * is not broken.
     */
    private val strokeHandles = ConcurrentHashMap<String, Long>()

    /**
     * layerId -> what the layout is made from (size, weight, outline thickness).
     *
     * Needed because the layout lives in a Rust registry and cannot simply be
     * "recomputed": when the canvas, weight or thickness changes, the old handle
     * has to be released and a new one requested, otherwise the text stays as it
     * was — which is exactly how a size change was lost on a canvas change.
     */
    private val layoutStyles = ConcurrentHashMap<String, TextLayoutStyle>()

    /** layerId -> a [w,h] cache from nativeLayoutBounds, for positioning. */
    private val layoutBoundsCache = ConcurrentHashMap<String, FloatArray>()

    // --- Video layers: handle + pfd + frame texture (by layer id) ---
    /** The editor's context (application) — needed to open video by uri. */
    @Volatile
    private var videoContext: Context? = null

    /** layerId -> the handle from nativeVideoOpenFd (>= 1). */
    private val videoHandles = ConcurrentHashMap<String, Long>()

    /**
     * layerId -> the ParcelFileDescriptor the Rust decoder reads through
     * /proc/self/fd. It is held open as long as the handle lives; it is closed
     * together with it (see [releaseVideo]) — otherwise the decoder would lose
     * the fd.
     */
    private val videoPfds = ConcurrentHashMap<String, ParcelFileDescriptor>()

    /** layerId -> the id of the latest frame texture (id != 0 only with a live frame). */
    private val videoTexIds = ConcurrentHashMap<String, Long>()

    /** layerId -> the clip frame number held in [videoTexIds]. */
    private val videoTexBuckets = ConcurrentHashMap<String, Long>()

    /** layerId -> the clip's fps, for [frameBucket]. */
    private val videoFps = ConcurrentHashMap<String, Float>()

    /**
     * Textures handed to the renderer but not yet released: they may take part in
     * a command that has not executed yet. The portion is how many frames to
     * survive before release; one is enough for a queue of two or three frames,
     * and this is taken with room to spare.
     */
    private val videoRetiredTex = ArrayDeque<Long>()

    /** layerId -> the uri the decoder was opened for (a uri change = reopen). */
    private val videoUris = ConcurrentHashMap<String, String>()

    /**
     * layerId -> the absolute path of the file **this app** wrote into the
     * project folder.
     *
     * Needed because a `content://` descriptor from a provider is rejected by
     * `AMediaExtractor_setDataSourceFd` even when it is seekable, while a
     * descriptor opened from a file is not. The layer keeps pointing at the
     * original uri (the material lives where it was taken from), while for
     * decoding we open the local copy.
     */
    private val videoLocalPaths = ConcurrentHashMap<String, String>()

    /** layerId -> video? (a cache of the mime classification; false = leave the decoder alone). */
    private val videoIsVideo = ConcurrentHashMap<String, Boolean>()

    /** layerIds whose open failed: a retry only on a uri change. */
    private val videoFailed = ConcurrentHashMap.newKeySet<String>()
    /** The uri for which video opening has ALREADY failed.
     *  Needed so that a uri change on a failed layer allows a retry again:
     *  [videoUris] is written only on success, so by it a failure cannot be told
     *  apart from "not tried yet". */
    private val videoAttemptedUris = ConcurrentHashMap<String, String>()

    /** Serialises texture free+request+store: the preview and the export come from different threads. */
    private val videoTexLock = Any()

    // --- SVG layers: geometry ids in the engine registry (by the file's uri) ---
    /**
     * The same application context as for video: the SVG file lives in the
     * project folder and is read through contentResolver. A separate binding
     * would be a second callback in EditorScreen for a value already there.
     */
    @Volatile
    private var assetContext: Context? = null

    /** uri -> (content fingerprint, id in the engine registry). */
    private val svgIds = ConcurrentHashMap<String, SvgEntry>()

    /** uris whose registration is happening right now: do not start a second one. */
    private val svgPending = ConcurrentHashMap.newKeySet<String>()

    /** uri -> why the SVG layer does not draw (no record = no error). */
    private val svgErrors = ConcurrentHashMap<String, String>()

    /** Registrations are serialised: reading a file + one JNI call per layer. */
    private val svgLock = Any()

    /**
     * The engine id, the content fingerprint and the document's own size.
     *
     * The width/height go here because the selection frame has no other source of
     * proportions: an SVG layer has neither a texture (`textureSizeFor`) nor a
     * mesh. The sizes come from the parse (see [ensureSvgRegistered]) and live
     * exactly as long as the id does.
     */
    private data class SvgEntry(
        val contentKey: Int,
        val id: Int,
        val width: Float,
        val height: Float,
    )

    // A new project is empty: no shapes, no text, no pictures.
    //
    // There used to be three demo layers here — an orange "Square", "Title" and
    // an invisible "images (4).jpeg". The last one was exactly that "invisible
    // (4).jpg" that appeared in every project: it was not drawn (visible = false)
    // but landed in the layer list, in `project_state`, in the saved file and in
    // the tool answers — that is, it looked like rubbish the user had not created
    // and had to delete by hand. An empty list is more honest: everything in the
    // project was put there by them.
    private val _layers = MutableStateFlow(listOf(backgroundLayer(PREVIEW_BG)))
    val layers: StateFlow<List<LayerUi>> = _layers.asStateFlow()

    private val _playheadMs = MutableStateFlow(0L)
    val playheadMs: StateFlow<Long> = _playheadMs.asStateFlow()

    // The project duration = max(layer ends), at least DEFAULT_MIN_DURATION_MS.
    // All layers start at 0, so a layer's end = its durationMs.
    private val _projectDurationMs = MutableStateFlow(DEFAULT_MIN_DURATION_MS)
    val projectDurationMs: StateFlow<Long> = _projectDurationMs.asStateFlow()

    private fun recalcProjectDuration(layers: List<LayerUi>) {
        // A layer's end = startMs + durationMs, not durationMs alone (docs/11
        // §11.1): a layer shifted to the right would otherwise be cut off by the
        // project end.
        val maxEnd = layers.maxOfOrNull { it.startMs + it.durationMs } ?: DEFAULT_MIN_DURATION_MS
        val total = maxOf(DEFAULT_MIN_DURATION_MS, maxEnd)
        _projectDurationMs.value = total

        // The background never ends before the project does. Left at the default
        // length it would stop in the middle of a longer edit, which reads as the
        // background vanishing for no reason the user gave — and a background is
        // the one layer whose whole job is to be behind everything. Its length is
        // therefore derived rather than chosen: stretched to cover the project,
        // never shortened, so the number the inspector shows cannot disagree with
        // what is drawn. To stop a background early, hide it or key its opacity.
        val stretched = layers.map { layer ->
            if (isBackground(layer) && (layer.startMs != 0L || layer.durationMs != total)) {
                layer.copy(startMs = 0L, durationMs = total)
            } else {
                layer
            }
        }
        // Written only when something actually changed: this runs after every
        // layer edit, and assigning an equal list would recompose the editor for
        // nothing.
        if (stretched != layers) _layers.value = stretched
    }

    private val _isPlaying = MutableStateFlow(false)
    val isPlaying: StateFlow<Boolean> = _isPlaying.asStateFlow()

    private val _engineVersion = MutableStateFlow("…")
    val engineVersion: StateFlow<String> = _engineVersion.asStateFlow()

    private val _shapeTris = MutableStateFlow<Map<Int, Int>>(emptyMap())
    val shapeTris: StateFlow<Map<Int, Int>> = _shapeTris.asStateFlow()

    private val _meshes = MutableStateFlow<Map<String, MeshUi>>(emptyMap())
    val meshes: StateFlow<Map<String, MeshUi>> = _meshes.asStateFlow()

    private val _selectedId = MutableStateFlow<String?>(null)
    val selectedId: StateFlow<String?> = _selectedId.asStateFlow()

    private val _hasEdits = MutableStateFlow(false)
    val hasEdits: StateFlow<Boolean> = _hasEdits.asStateFlow()

    private val _projectName = MutableStateFlow("New Project 1")
    val projectName: StateFlow<String> = _projectName.asStateFlow()

    private val _currentFileName = MutableStateFlow<String?>(null)
    val currentFileName: StateFlow<String?> = _currentFileName.asStateFlow()

    // Effects written in WGSL inside the project. They are stored in the project
    // itself, not in the app: someone else's project with someone else's effect
    // has to open whole, and conversely, the effect has to travel with the file.
    private val _customEffects = MutableStateFlow<List<CustomEffectUi>>(emptyList())
    val customEffects: StateFlow<List<CustomEffectUi>> = _customEffects.asStateFlow()

    /**
     * Effects installed from the shop.
     *
     * Separate from [customEffects], and this is not separation for separation's
     * sake: a project's effects travel **inside** the project (`customEffects` in
     * its JSON), while the installed ones belong to the device and are not
     * written into the project — otherwise someone else's effect would travel
     * with the file to a person who does not have it. In the engine catalogue they
     * meet, and there the order is single: the project beats the installed one
     * when the id matches.
     */
    private val _installedEffects = MutableStateFlow<List<CustomEffectUi>>(emptyList())

    /** Project and installed effects, in conflict-resolution order. */
    private fun mergedCustoms(): List<CustomEffectUi> {
        val project = _customEffects.value
        if (_installedEffects.value.isEmpty()) return project
        val taken = project.map { it.id }.toHashSet()
        return project + _installedEffects.value.filterNot { it.id in taken }
    }

    /**
     * Effects installed from the shop, as the caller sees them.
     *
     * Needed separately from [customEffects]: the assistant has to tell "this
     * project's effect" from "an effect available on the device", because the
     * first travels with the project and the second does not.
     */
    val installedEffects: List<CustomEffectUi> get() = _installedEffects.value

    /**
     * The same, but as a flow — for the interface.
     *
     * The getter is read without subscribing, so a menu that showed a removed
     * effect would not redraw: the list changes in the background after a shop
     * deletion.
     */
    val installedEffectsFlow: StateFlow<List<CustomEffectUi>> = _installedEffects.asStateFlow()

    /**
     * Read the installed effects from disk. The blocking work goes to IO.
     *
     * Called at app start, not when the shop is opened: an effect installed
     * yesterday has to be in the menu today too, when nobody opened the shop.
     */
    fun loadInstalledEffects(context: Context) {
        val app = context.applicationContext
        scope.launch {
            val list = EffectStore.installedEffects(app)
            _installedEffects.value = list
        }
    }

    // Which dock page is open and whether the dock is collapsed. It lives in
    // state, not in the editor screen's `remember`, because the panel is now
    // opened not only by the user's finger: the assistant also has the right to
    // show a panel.
    private val _dockPage = MutableStateFlow(DockPage.LAYERS)
    val dockPage: StateFlow<DockPage> = _dockPage.asStateFlow()

    private val _dockCollapsed = MutableStateFlow(false)
    val dockCollapsed: StateFlow<Boolean> = _dockCollapsed.asStateFlow()

    fun setDockPage(page: DockPage) {
        _dockPage.value = page
    }

    fun setDockCollapsed(collapsed: Boolean) {
        _dockCollapsed.value = collapsed
    }

    fun markSaved() {
        _hasEdits.value = false
    }

    // Replace the whole state with the template's seed: the layers wholesale, a
    // recomputation of duration/meshes, a reset of the edits, file name = seed name.
    fun loadSeed(seed: EditorSeed) {
        clearHistory()
        clearEngineResources()
        _projectName.value = seed.name.ifEmpty { "New Project 1" }
        // A template is not only layers: it has its own canvas and its own
        // background, otherwise a vertical template would arrive in a horizontal
        // frame.
        _canvasW.value = seed.canvasWidth.coerceIn(MIN_CANVAS, MAX_CANVAS)
        _canvasH.value = seed.canvasHeight.coerceIn(MIN_CANVAS, MAX_CANVAS)
        _bgArgb.value = seed.backgroundArgb and 0xFFFFFFFFL
        _layers.value = seed.layers
        recalcProjectDuration(seed.layers)
        _meshes.value = emptyMap()
        ensureMeshes(seed.layers.filter { it.visible && it.kind == LayerKindUi.SHAPE }.map { it.name })
        val maxNumeric = seed.layers.mapNotNull { it.id.toLongOrNull() }.maxOrNull() ?: 3L
        idSeq.set(maxNumeric + 1)
        _selectedId.value = seed.layers.firstOrNull { it.kind == LayerKindUi.SHAPE }?.id
        _playheadMs.value = 0L
        _currentFileName.value = seed.name
        _hasEdits.value = false
        prewarmAllLayouts(seed.layers)
    }

    /** The project name. Separate from the file name: the file is written from
     * it while there is no file name yet, and the assistant can name the project
     * before the first save. */
    fun setProjectName(name: String) {
        val clean = name.trim().take(64)
        if (clean.isEmpty() || clean == _projectName.value) return
        pushUndo("name")
        _projectName.value = clean
        _hasEdits.value = true
    }

    fun setCurrentFileName(fileName: String?) {
        _currentFileName.value = fileName
    }

    /**
     * Start a new project.
     *
     * The canvas size and background are project parameters, not app constants,
     * so they come in here: the canvas dialog and a template set them at once,
     * while the "new project" button leaves the default.
     */
    fun newProject(
        name: String,
        width: Int = PREVIEW_W,
        height: Int = PREVIEW_H,
        background: Long = PREVIEW_BG,
    ) {
        clearHistory()
        clearEngineResources()
        _projectName.value = name.ifEmpty { "New Project 1" }
        _canvasW.value = width.coerceIn(MIN_CANVAS, MAX_CANVAS)
        _canvasH.value = height.coerceIn(MIN_CANVAS, MAX_CANVAS)
        _bgArgb.value = background and 0xFFFFFFFFL
        // The project starts empty, not with three demo layers: see the comment
        // at `_layers`. The identifiers start at one, because none are taken any
        // more, and there is no selected layer either — there is nothing to select.
        _layers.value = listOf(backgroundLayer(background and 0xFFFFFFFFL))
        idSeq.set(2L)
        _selectedId.value = null
        recalcProjectDuration(_layers.value)
        _playheadMs.value = 0L
        _currentFileName.value = null
        _meshes.value = emptyMap()
        // A new project does not inherit someone else's effects: they belong to the file.
        _customEffects.value = emptyList()
        catalogueKey = null
        _hasEdits.value = false
    }

    // Editor JSON for the Rust codec:
    // {name, layers:[{id,kind,name,visible,argb,durationMs,uri,text,
    // offsetX,offsetY,scale,alpha,keys:[{t,v}],
    // effects:[{id,kind,enabled,params:[f32]}]}]}.
    fun toJson(): String {
        val root = JSONObject()
        root.put("name", _projectName.value)
        // The keys match the ones the Rust project document reads: otherwise the
        // canvas would be lost on save exactly as project effects used to be.
        root.put("canvasWidth", _canvasW.value)
        root.put("canvasHeight", _canvasH.value)
        root.put("backgroundArgb", _bgArgb.value)
        // The project's effects sit beside the layers, not inside them: the
        // project as a whole defines them, and one effect can be used by several
        // layers.
        //
        // To them are added the device's **used** effects. Without that, a
        // project that took an effect from the shop would travel without its
        // definition: on the recipient's side (and in a template that was shared)
        // the chain would refer to a kind they do not have, and the engine would
        // silently shorten it. Only the used ones — so the project does not
        // accumulate the definitions of everything that was ever installed.
        val usedIds = _layers.value.flatMap { it.effects }.map { it.kindId }.toHashSet()
        val projectEffects = _customEffects.value
        val projectIds = projectEffects.map { it.id }.toHashSet()
        val borrowed = _installedEffects.value.filter { it.id in usedIds && it.id !in projectIds }
        val customs = projectEffects + borrowed
        if (customs.isNotEmpty()) root.put("customEffects", CustomEffects.encode(customs))
        val layers = JSONArray()
        for (layer in _layers.value) {
            val keys = JSONArray()
            for (key in layer.keys) {
                keys.put(
                    JSONObject()
                        .put("t", key.timeMs)
                        .put("v", key.value.toDouble())
                        .put("ease", easeToJson(key.ease)),
                )
            }
            // Four animated tracks in the same shape as `keys` (docs/11 §11.3).
            // They are always written, as an empty array for "the property is not
            // animated", and Rust reads that as the layer's base value.
            fun trackJson(list: List<KeyframeUi>): JSONArray {
                val out = JSONArray()
                for (key in list) {
                    out.put(
                        JSONObject()
                            .put("t", key.timeMs)
                            .put("v", key.value.toDouble())
                            .put("ease", easeToJson(key.ease)),
                    )
                }
                return out
            }
            val effects = JSONArray()
            for (effect in layer.effects) {
                effects.put(effectToJson(effect))
            }
            layers.put(
                JSONObject()
                    .put("id", layer.id)
                    .put("kind", layer.kind.name)
                    .put("name", layer.name)
                    .put("visible", layer.visible)
                    .put("argb", layer.argb)
                    .put("durationMs", layer.durationMs)
                    .put("startMs", layer.startMs)
                    .put("uri", layer.uri ?: JSONObject.NULL)
                    .put("text", layer.text)
                    .put("textWeight", layer.textWeight)
                    .put("textFamily", layer.textFamily)
                    .put("strokePx", layer.strokePx.toDouble())
                    .put("strokeArgb", layer.strokeArgb)
                    .put("offsetX", layer.offsetX.toDouble())
                    .put("offsetY", layer.offsetY.toDouble())
                    .put("scale", layer.scale.toDouble())
                    .put("alpha", layer.alpha.toDouble())
                    .put(
                        "transition",
                        layer.transition?.let { tr ->
                            JSONObject()
                                .put("startMs", tr.startMs)
                                .put("durationMs", tr.durationMs)
                                .put("withPrevious", tr.withPrevious)
                                .put("enabled", tr.enabled)
                                .put("ease", easeToJson(tr.ease))
                        } ?: JSONObject.NULL,
                    )
                    .put("keys", keys)
                    .put("trackX", trackJson(layer.xKeys))
                    .put("trackY", trackJson(layer.yKeys))
                    .put("trackScale", trackJson(layer.scaleKeys))
                    .put("trackAlpha", trackJson(layer.alphaKeys))
                    .put("effects", effects),
            )
        }
        root.put("layers", layers)
        return root.toString()
    }

    fun loadFromJson(json: String): Boolean {
        // An opened document is a new point of reference: there is nowhere to undo its edits to.
        clearHistory()
        return loadDocument(json)
    }

    /**
     * Parse a document into the editor state. Without resetting the history,
     * because undo itself goes through this path: undo has to preserve the redo
     * stack.
     */
    /**
     * Accept a ready project document as one edit.
     *
     * A batch of operations from the engine arrives this way: Rust applies the
     * whole list in one call and hands back the new document entire. Here it
     * becomes the state — **one** entry in the history, not a hundred.
     *
     * Why not [loadFromJson]: that one starts a new point of reference and clears
     * the history, because opening another project has nowhere to be undone to. A
     * batch of edits is the opposite case: the user has to be able to undo it with
     * one press, and for that a snapshot is taken before the document is swapped.
     * The snapshot is taken by `beginUndoBatch`, not by each setter inside
     * `loadDocument`: by the time the second field is set, the document is already
     * half new, and an undo from it would return to a state nobody ever saw.
     */
    fun applyProjectDocument(json: String): Boolean {
        beginUndoBatch()
        val ok = try {
            loadDocument(json)
        } finally {
            endUndoBatch()
        }
        // A failed parse must not leave a snapshot in the history that
        // corresponds to nothing.
        if (!ok) undoStack.removeLastOrNull()
        syncUndoFlags()
        return ok
    }

    private fun loadDocument(json: String): Boolean {
        return try {
            val root = JSONObject(json)
            val name = root.optString("name", "").ifEmpty { return false }
            val rawLayers = root.optJSONArray("layers") ?: JSONArray()
            val layers = mutableListOf<LayerUi>()
            for (i in 0 until rawLayers.length()) {
                val o = rawLayers.optJSONObject(i) ?: continue
                val kind = try {
                    LayerKindUi.valueOf(o.optString("kind", "SHAPE"))
                } catch (_: IllegalArgumentException) {
                    LayerKindUi.SHAPE
                }
                val rawKeys = o.optJSONArray("keys") ?: JSONArray()
                val keys = mutableListOf<KeyframeUi>()
                for (j in 0 until rawKeys.length()) {
                    val k = rawKeys.optJSONObject(j) ?: continue
                    keys += KeyframeUi(
                        timeMs = k.optLong("t", 0L),
                        value = k.optDouble("v", 0.0).toFloat(),
                        ease = easeFromJson(k.optJSONObject("ease")),
                    )
                }
                // Four animated tracks. A missing array means "not animated",
                // that is, an empty track rather than a list of zero: a zero alpha
                // and a zero scale on an empty track are exactly what would look
                // like "the layer has vanished".
                fun readTrack(name: String): List<KeyframeUi> {
                    val raw = o.optJSONArray(name) ?: return emptyList()
                    val out = mutableListOf<KeyframeUi>()
                    for (j in 0 until raw.length()) {
                        val k = raw.optJSONObject(j) ?: continue
                        out += KeyframeUi(
                            timeMs = k.optLong("t", 0L).coerceAtLeast(0L),
                            value = k.optDouble("v", 0.0).toFloat(),
                            ease = easeFromJson(k.optJSONObject("ease")),
                        )
                    }
                    // The sampler assumes sorted unique times, otherwise a
                    // division by zero inside the interpolation.
                    return out.sortedBy { it.timeMs }.distinctBy { it.timeMs }
                }
                // Unknown effect kinds are kept as-is (Rust drops them on decode);
                // dropping them here would strip a newer project on re-save.
                val rawEffects = o.optJSONArray("effects") ?: JSONArray()
                val effects = mutableListOf<Effect>()
                for (j in 0 until rawEffects.length()) {
                    val e = rawEffects.optJSONObject(j) ?: continue
                    val effectKind = e.optString("kind", "")
                    if (effectKind.isEmpty()) continue
                    val rawParams = e.optJSONArray("params") ?: JSONArray()
                    val params = mutableListOf<Float>()
                    for (k in 0 until rawParams.length()) {
                        params += rawParams.optDouble(k, 0.0).toFloat()
                    }
                    effects += Effect(
                        id = e.optString("id", "")
                            .ifEmpty { java.util.UUID.randomUUID().toString() },
                        kindId = effectKind,
                        enabled = e.optBoolean("enabled", true),
                        params = params,
                    )
                }
                // SHAPE identity IS the name (shapeOrdinalOf). Repair names the
                // engine cannot resolve instead of silently dropping the layer:
                // older builds allowed a free-text rename of a shape.
                //
                // The exception is an SVG layer: its name (`logo.svg`) is
                // deliberately not an ordinal, and "repairing" it to `Square`
                // would erase the mark by which the layer is later recognised. The
                // uri is read here because it takes part in this decision.
                val rawUri = if (o.isNull("uri")) null else o.optString("uri")
                val rawName = o.optString("name", "Layer")
                val isSvgShape = svgUriOf(rawName, rawUri) != null
                val resolvedName =
                    if (kind == LayerKindUi.SHAPE && shapeOrdinalOf(rawName) < 0 && !isSvgShape) {
                        "Square"
                    } else {
                        rawName
                    }
                layers += LayerUi(
                    id = o.optString("id").ifEmpty { java.util.UUID.randomUUID().toString() },
                    kind = kind,
                    name = resolvedName,
                    visible = o.optBoolean("visible", true),
                    argb = o.optLong("argb", 0L),
                    offsetX = o.optDouble("offsetX", 0.0).toFloat(),
                    offsetY = o.optDouble("offsetY", 0.0).toFloat(),
                    // Clamped on the way in: a 0 or negative side would make
                    // Rust skip the draw and shift the effect-chain indices.
                    scale = o.optDouble("scale", 1.0).toFloat()
                        .takeIf { it.isFinite() }
                        ?.coerceIn(LayerUi.MIN_SCALE, LayerUi.MAX_SCALE)
                        ?: 1f,
                    alpha = o.optDouble("alpha", 1.0).toFloat().coerceIn(0f, 1f),
                    // Clamped here as well as in Rust: a zero-length ramp would
                    // divide by zero in `TransitionUi.rampAt`.
                    transition = o.optJSONObject("transition")?.let { tr ->
                        TransitionUi(
                            startMs = tr.optLong("startMs", 0L).coerceAtLeast(0L),
                            durationMs = tr.optLong("durationMs", 500L).coerceIn(
                                TransitionUi.MIN_DURATION_MS,
                                TransitionUi.MAX_DURATION_MS,
                            ),
                            withPrevious = tr.optBoolean("withPrevious", true),
                            enabled = tr.optBoolean("enabled", true),
                            // No curve — the very smoothed ramp the transition was
                            // before curves, not a straight line.
                            ease = tr.optJSONObject("ease")
                                ?.let { easeFromJson(it) }
                                ?: EaseUi.SMOOTH,
                        )
                    },
                    durationMs = o.optLong("durationMs", DEFAULT_MIN_DURATION_MS),
                    // A negative value is clamped to 0 rather than dropped:
                    // otherwise the layer would silently disappear from the project
                    // (docs/11 §11.1).
                    startMs = o.optLong("startMs", 0L).coerceAtLeast(0L),
                    uri = rawUri,
                    text = o.optString("text", ""),
                    // Tag ≤ 6 bytes carry no weight and no outline, and neither
                    // do projects the editor wrote before they existed: the
                    // defaults here are the same ones the codec fills in, so an
                    // old project opens as regular text without a contour.
                    textWeight = o.optInt("textWeight", 400)
                        .coerceIn(MIN_TEXT_WEIGHT, MAX_TEXT_WEIGHT),
                    textFamily = o.optString("textFamily", ""),
                    strokePx = o.optDouble("strokePx", 0.0).toFloat()
                        .coerceIn(0f, MAX_STROKE_PX),
                    strokeArgb = o.optLong("strokeArgb", 0xFF000000L) and 0xFFFFFFFFL,
                    keys = keys
                        .ifEmpty { listOf(KeyframeUi(0L, 0f)) }
                        // A safeguard against an externally edited .rumo:
                        // rotationAt assumes sorted unique times.
                        .sortedBy { it.timeMs }
                        .distinctBy { it.timeMs },
                    xKeys = readTrack("trackX"),
                    yKeys = readTrack("trackY"),
                    scaleKeys = readTrack("trackScale"),
                    alphaKeys = readTrack("trackAlpha"),
                    effects = effects,
                )
            }
            _projectName.value = name
            _canvasW.value = root.optInt("canvasWidth", PREVIEW_W)
                .coerceIn(MIN_CANVAS, MAX_CANVAS)
            _canvasH.value = root.optInt("canvasHeight", PREVIEW_H)
                .coerceIn(MIN_CANVAS, MAX_CANVAS)
            _bgArgb.value = root.optLong("backgroundArgb", PREVIEW_BG) and 0xFFFFFFFFL
            // Project effects are restored before the layers: a layer referring
            // to its effect has to find it already defined, otherwise the chain
            // would turn out to be an "unknown kind" and silently draw nothing.
            _customEffects.value = CustomEffects.decode(root.optJSONArray("customEffects"))
            catalogueKey = null
            // A project written before the background was a layer has no Frame
            // layer: its background is only the colour the frame was cleared
            // with, which cannot take an effect. Give it one carrying that colour,
            // so an old project opens with a background it can now edit rather
            // than with a hole where one should be.
            val withBackground = if (layers.none { isBackground(it) }) {
                // A fresh id, not the fixed "1" the constructor uses: the
                // document's own layers may well contain "1", and two layers
                // sharing an id makes selection, updates and deletion act on
                // whichever one the list happens to reach first.
                val taken = layers.mapNotNull { it.id.toLongOrNull() }.maxOrNull() ?: 0L
                listOf(backgroundLayer(_bgArgb.value).copy(id = (taken + 1).toString())) + layers
            } else {
                layers
            }
            _layers.value = withBackground
            ensureMeshes(
                withBackground.filter { it.visible && it.kind == LayerKindUi.SHAPE }.map { it.name },
            )
            // Numeric row ids must not collide with future addShape/addMedia ids.
            val maxNumeric = layers.mapNotNull { it.id.toLongOrNull() }.maxOrNull() ?: 3L
            idSeq.set(maxNumeric + 1)
            _selectedId.value = layers.firstOrNull { it.kind == LayerKindUi.SHAPE }?.id
            _playheadMs.value = 0L
            _hasEdits.value = false
            clearEngineResources()
            prewarmAllLayouts(layers)
            true
        } catch (_: Exception) {
            false
        }
    }

    /** One effect in the JSON shape of the Rust contract: id/kind/enabled/params. */
    private fun effectToJson(effect: Effect): JSONObject {
        val params = JSONArray()
        for (p in effect.params) params.put(p.toDouble())
        return JSONObject()
            .put("id", effect.id)
            .put("kind", effect.kindId)
            .put("enabled", effect.enabled)
            .put("params", params)
    }

    /**
     * Effect chains for the native compositor:
     * `{"shapes":[[{id,kind,enabled,params}]],"textures":[[...]]}`.
     * The group index = the draw call index; trailing empty groups are trimmed,
     * a missing/empty index means "no effects". "" — if no layer has any effects.
     */
    private fun effectsJsonFor(
        shapeGroups: List<List<Effect>>,
        textureGroups: List<List<Effect>>,
    ): String {
        // A fast exit: a typical frame without effects must not allocate JSON.
        val anyShapes = shapeGroups.any { g -> g.any { it.kindId.isNotEmpty() } }
        val anyTextures = textureGroups.any { g -> g.any { it.kindId.isNotEmpty() } }
        if (!anyShapes && !anyTextures) return ""
        val shapes = effectGroupsArray(shapeGroups)
        val textures = effectGroupsArray(textureGroups)
        val out = JSONObject()
            .put("shapes", shapes)
            .put("textures", textures)
        // The definitions travel in the same document: the engine looks for its
        // effect in `custom`, not in the built-in list, and without them a chain
        // with a custom effect is silently shortened to "unknown kind — skip".
        //
        // Specifically [mergedCustoms], not only the project's effects: an effect
        // installed from the shop lives in the device's list, and here it was
        // missing before — the panel showed it (the catalogue merges both lists),
        // while the engine received no definition and threw the chain away. That
        // is exactly what looked like "the effect is in the menu, but it is not in
        // the frame".
        val custom = CustomEffects.encode(mergedCustoms())
        if (custom.length() > 0) out.put("custom", custom)
        return out.toString()
    }

    /** Effect groups in draw-call order; trailing empty ones are trimmed. */
    private fun effectGroupsArray(groups: List<List<Effect>>): JSONArray {
        var last = groups.size
        while (last > 0 && groups[last - 1].none { it.kindId.isNotEmpty() }) last--
        val out = JSONArray()
        for (i in 0 until last) {
            val chain = JSONArray()
            for (e in groups[i]) {
                if (e.kindId.isEmpty()) continue
                chain.put(effectToJson(e))
            }
            out.put(chain)
        }
        return out
    }

    fun rotationAt(layer: LayerUi, tMs: Long): Float {
        // mirrors rumo_core::sample_value, per-layer keys
        val keys = layer.keys
        if (keys.isEmpty()) return 0f
        if (tMs <= keys.first().timeMs) return keys.first().value
        if (tMs >= keys.last().timeMs) return keys.last().value
        val i = keys.indexOfFirst { tMs <= it.timeMs }
        val a = keys[i - 1]
        val b = keys[i]
        val f = ((tMs - a.timeMs).toFloat() / (b.timeMs - a.timeMs).toFloat()).coerceIn(0f, 1f)
        // The curve belongs to the *outgoing* key: `a` decides how it leaves.
        // A linear curve does that with exactly the expression that was there
        // before curves — so all the old documents stay identical.
        return a.value + (b.value - a.value) * a.ease.at(f)
    }

    /**
     * `sample_value` for an animated property: the same linear choice as in
     * [rotationAt], but **an empty track yields [base], not 0.0** (docs/11 §11.3).
     *
     * The formula has to be literally the same as in Rust's `sample_or_base`:
     * before the first key — the first value, after the last — the last, in
     * between — linear in time. A divergence here and there gives a frame that
     * looks different in the preview and in the export.
     */
    private fun sampleOrBase(keys: List<KeyframeUi>, tMs: Long, base: Float): Float {
        if (keys.isEmpty()) return base
        if (tMs <= keys.first().timeMs) return keys.first().value
        if (tMs >= keys.last().timeMs) return keys.last().value
        val i = keys.indexOfFirst { tMs <= it.timeMs }
        val a = keys[i - 1]
        val b = keys[i]
        val f = ((tMs - a.timeMs).toFloat() / (b.timeMs - a.timeMs).toFloat()).coerceIn(0f, 1f)
        // The curve belongs to the *outgoing* key: `a` decides how it leaves.
        // A linear curve does that with exactly the expression that was there
        // before curves — so all the old documents stay identical.
        return a.value + (b.value - a.value) * a.ease.at(f)
    }

    fun xAt(layer: LayerUi, tMs: Long): Float = sampleOrBase(layer.xKeys, tMs, layer.offsetX)

    fun yAt(layer: LayerUi, tMs: Long): Float = sampleOrBase(layer.yKeys, tMs, layer.offsetY)

    /**
     * The layer's size at time [tMs]. Clamped, as in [previewCallAt]: a track can
     * yield 0, and Rust will skip a zero rectangle — and shift the effect chains
     * of all the following layers.
     */
    fun scaleAt(layer: LayerUi, tMs: Long): Float =
        sampleOrBase(layer.scaleKeys, tMs, layer.scale)
            .takeIf { it.isFinite() }
            ?.coerceIn(LayerUi.MIN_SCALE, LayerUi.MAX_SCALE)
            ?: 1f

    fun alphaBaseAt(layer: LayerUi, tMs: Long): Float =
        sampleOrBase(layer.alphaKeys, tMs, layer.alpha).coerceIn(0f, 1f)

    /**
     * The predicate "the layer belongs to the frame" (docs/11 §11.2) — the one
     * formula both Kotlin and Rust have to compute:
     * `t >= start_ms && t < start_ms + duration_ms`.
     *
     * Kotlin filters BEFORE putting records into the frame's arrays: otherwise a
     * dropped layer would keep a set of properties while its draw call would not,
     * and the effect chains would drift apart on all the following layers.
     */
    fun inFrame(layer: LayerUi, tMs: Long): Boolean =
        tMs >= layer.startMs && tMs < layer.startMs + layer.durationMs

    /**
     * Alpha multipliers from transitions at time [t]: a layer with an active
     * transition gets `ramp`, and the layer beneath it (with `withPrevious`) gets
     * `1 - ramp`. Absence from the map = 1.0, that is, "no transitions".
     *
     * The layer list is the drawing order (`first row paints first`), so "the
     * layer beneath it" is the previous one. If there are no transitions at all,
     * the map is empty and nothing is computed.
     */
    fun alphaMultipliers(t: Long): Map<String, Float> {
        val layers = _layers.value
        // Common case: no transitions at all. Bail out before allocating, since
        // this runs on every preview frame.
        if (layers.none { it.transition?.enabled == true }) return emptyMap()
        // "The layer below" means the previous layer the compositor actually
        // paints, so a hidden row can neither drive nor be driven by a fade.
        val drawn = layers.filter { it.visible }
        val out = HashMap<String, Float>()
        for ((i, l) in drawn.withIndex()) {
            val tr = l.transition ?: continue
            if (!tr.enabled) continue
            val ramp = tr.rampAt(t)
            out[l.id] = (out[l.id] ?: 1f) * ramp
            if (tr.withPrevious && i > 0) {
                val below = drawn[i - 1].id
                out[below] = (out[below] ?: 1f) * (1f - ramp)
            }
        }
        return out
    }

    /** The layer's alpha at time [t] with the track and transitions — what goes to the engine. */
    fun alphaAt(layer: LayerUi, t: Long): Float =
        (alphaBaseAt(layer, t) * (alphaMultipliers(t)[layer.id] ?: 1f)).coerceIn(0f, 1f)

    init {
        // Choreographer is bound to a Looper: we take the instance on the main
        // thread once; post/removeFrameCallback are thread-safe after that.
        mainHandler.post { choreographer = Choreographer.getInstance() }
        loadEngineVersion()
        loadShapeTris()
        recalcProjectDuration(_layers.value)
        scope.launch {
            _layers.collect { list ->
                recalcProjectDuration(list)
                ensureMeshes(
                    // An SVG layer is a SHAPE too, but it has no Material mesh: its
                    // geometry is held by the SVG registry, and requesting a mesh
                    // by the name `logo.svg` would be empty for certain.
                    list.filter { it.visible && it.kind == LayerKindUi.SHAPE && svgUriOf(it) == null }
                        .map { it.name },
                )
                reconcileVideoResources(list)
                syncSvgRegistry(list)
            }
        }
        scope.launch {
            combine(_layers, _playheadMs) { _, _ -> Unit }.collect {
                renderPreviewFrame()
            }
        }
    }

    fun loadEngineVersion() {
        scope.launch {
            _engineVersion.value = RumoBridge.engineVersion()
        }
    }

    // The preview frame comes only from the engine: Kotlin no longer holds the
    // frame's Bitmap nor reads it out of Rust (docs/12 §12.4). The pixels are
    // shown by the engine's SurfaceView, and a window without an engine says so
    // plainly.
    private val _renderPath = MutableStateFlow("cpu")
    val renderPath: StateFlow<String> = _renderPath.asStateFlow()

    /** The parsed nativeRenderDiagnostics() report; null = unavailable/failed. */
    private val _renderDiagnostics = MutableStateFlow<RumoBridge.RenderDiagnostics?>(null)
    val renderDiagnostics: StateFlow<RumoBridge.RenderDiagnostics?> =
        _renderDiagnostics.asStateFlow()

    /**
     * Read the native diagnostics and publish them. When there is a report, its
     * `path` becomes the path chip: the legacy [RumoBridge.renderPath] reflects
     * the bitmap path and lies on the Surface engine.
     *
     * Called on engine transitions/failures and when the diagnostics window is
     * opened — NOT on every animation frame (it is JNI + JSON parsing).
     */
    fun refreshRenderDiagnostics() {
        val d = RumoBridge.renderDiagnostics()
        _renderDiagnostics.value = d
        d?.path?.takeIf { it.isNotEmpty() }?.let { _renderPath.value = it }
    }

    /** Clear the native log and re-read the report. */
    fun clearRenderDiagnostics() {
        RumoBridge.clearRenderDiagnostics()
        refreshRenderDiagnostics()
    }

    // --- The project canvas: frame size and background ---
    private val _canvasW = MutableStateFlow(PREVIEW_W)
    val canvasWidth: StateFlow<Int> = _canvasW.asStateFlow()

    private val _canvasH = MutableStateFlow(PREVIEW_H)
    val canvasHeight: StateFlow<Int> = _canvasH.asStateFlow()

    private val _bgArgb = MutableStateFlow(PREVIEW_BG)
    val backgroundArgb: StateFlow<Long> = _bgArgb.asStateFlow()

    /**
     * Set the canvas size. The values are clamped rather than rejected: a canvas
     * of zero width is not an input error, it is a frame with nothing to draw, and
     * it cannot be produced by an accidental tap.
     */
    fun setCanvas(width: Int, height: Int) {
        val w = width.coerceIn(MIN_CANVAS, MAX_CANVAS)
        val h = height.coerceIn(MIN_CANVAS, MAX_CANVAS)
        if (w == _canvasW.value && h == _canvasH.value) return
        // After the no-op check: an entry in the history without a change to the
        // document would mean there is nothing to undo, while an undo consumes a
        // step anyway.
        pushUndo("canvas")
        _canvasW.value = w
        _canvasH.value = h
        // Text is laid out in canvas pixels, so a size change is a point-size
        // change: the old handles have to be released and the text laid out again.
        _layers.value.filter { it.kind == LayerKindUi.TEXT }.forEach { layer ->
            freeLayoutFor(layer.id)
            prewarmLayout(layer)
        }
        _hasEdits.value = true
    }

    /**
     * The background colour, which is two things at once.
     *
     * It is the colour the frame is cleared with *and* the colour of the
     * background layer. They are moved together on purpose: the clear is what
     * shows where the background layer is transparent, so setting only one of
     * them would make "make the background transparent" produce a transparent
     * layer over an opaque clear — that is, no visible change at all, which is
     * exactly the complaint that led here.
     */
    fun setBackground(argb: Long) {
        val clean = argb and 0xFFFFFFFFL
        val layer = _layers.value.firstOrNull { isBackground(it) }
        if (clean == _bgArgb.value && (layer == null || layer.argb == clean)) return
        pushUndo("canvas")
        _bgArgb.value = clean
        if (layer != null && layer.argb != clean) {
            _layers.value = _layers.value.map {
                if (isBackground(it)) it.copy(argb = clean) else it
            }
        }
        _hasEdits.value = true
    }

    /** Canvas and background in one operation: that is how the dialog, a template and the AI set it. */
    fun applyCanvas(width: Int, height: Int, background: Long) {
        setCanvas(width, height)
        setBackground(background)
    }

    /** The Canvas preview's size in px (for the offset scale); set by PreviewCard. */
    @Volatile
    var previewCanvasW: Float = PREVIEW_W.toFloat()
    @Volatile
    var previewCanvasH: Float = PREVIEW_H.toFloat()

    /** The frame's SHAPE composition at time t — one build for the bitmap path,
     * the engine push (nativeEngineSetLayers) and the MP4 export. */
    fun previewCallAt(t: Long): RumoBridge.PreviewCall {
        val layers = _layers.value.filter { inFrame(it, t) && it.visible && it.kind == LayerKindUi.SHAPE }
        val cw = _canvasW.value
        val ch = _canvasH.value
        val s = cw / previewCanvasW.coerceAtLeast(1f)
        val ord = mutableListOf<Int>()
        val col = mutableListOf<Int>()
        val dx = mutableListOf<Float>()
        val dy = mutableListOf<Float>()
        val rot = mutableListOf<Float>()
        val al = mutableListOf<Float>()
        val sc = mutableListOf<Float>()
        for (l in layers) {
            // An SVG layer hands the id of its geometry into the same slot: the
            // engine has one array of ordinals per SHAPE group, and ShapeSpec
            // learns the svgId from exactly there (Rust: `ShapeSpec.svg_id`). Not
            // registered — `-1`, and the layer is skipped rather than drawn with
            // someone else's shape.
            val o = shapeSlot(l)
            if (o < 0) continue
            ord.add(o)
            col.add(l.argb.toInt())
            dx.add(xAt(l, t) * s)
            dy.add(yAt(l, t) * s)
            rot.add(rotationAt(l, t))
            al.add(alphaAt(l, t))
            sc.add(scaleAt(l, t))
        }
        return RumoBridge.PreviewCall(
            cw, ch, _bgArgb.value.toInt(),
            ord.toIntArray(), col.toIntArray(),
            dx.toFloatArray(), dy.toFloatArray(),
            rot.toFloatArray(), al.toFloatArray(),
            sc.toFloatArray(),
        )
    }

    /** The full frame: SHAPE + text (layout handles) + pictures (texture id).
     * The geometry is 1:1 with the fallback overlays: text — centre + offset, a
     * picture — a "60% of height × scale" square at the frame's centre + the
     * layer's offset. The engine composites everything itself.
     * @param targetW/H the target (Surface) size; Rust's SHAPE geometry
     *   scales from the frame height, so the Kotlin coordinates are brought in
     *   by a uniform scale s = targetH/canvasH with centring (letterbox).
     *   When the target matches the canvas — identity. */
    fun previewFrameExAt(
        t: Long,
        targetW: Int = _canvasW.value,
        targetH: Int = _canvasH.value,
    ): RumoBridge.FrameEx {
        val call = previewCallAt(t)
        val layers = _layers.value
        val cw = _canvasW.value
        val ch = _canvasH.value
        val s = cw / previewCanvasW.coerceAtLeast(1f)
        val (ts, tox, toy) = targetMapping(targetW, targetH)
        // Effect chains by draw call index. The SHAPE order repeats
        // previewCallAt (a layer in the frame + a visible SHAPE with a resolvable
        // ordinal).
        val shapeFx = layers
            .filter { inFrame(it, t) && it.visible && it.kind == LayerKindUi.SHAPE && shapeSlot(it) >= 0 }
            .map { it.effects }
        // The time window of each draw call in the SHAPE group, parallel to its
        // arrays. Rust drops the layer by it itself; it is filled in here so that
        // both sides decide "does the layer belong to the frame" by one formula.
        val layerStarts = mutableListOf<Long>()
        val layerDurations = mutableListOf<Long>()
        // The global drawing order. The three groups (SHAPE, text, pictures) are
        // built by three separate passes, and the engine used to draw them in
        // groups — a picture could not lie under text or SVG. Here every draw call
        // gets the layer's number in the common list; Rust compares only these
        // numbers, merges the groups into one sequence and draws by it.
        //
        // The number is the layer's index in `layers`, doubled (`rank shl 1`): it
        // is monotone over the list, so the merge gives the layer order, whereas a
        // counter inside a single group would give the group order again (the
        // groups are built sequentially). The doubling leaves room for a text
        // layer's pair: outline and fill are two draw calls and get 2r and 2r+1,
        // so the outline stays beneath its own fill.
        val layerOrders = mutableListOf<Int>()
        for ((rank, l) in layers.withIndex()) {
            if (!inFrame(l, t) || !l.visible || l.kind != LayerKindUi.SHAPE) continue
            if (shapeSlot(l) < 0) continue
            layerStarts.add(l.startMs)
            layerDurations.add(l.durationMs)
            layerOrders.add(rank shl 1)
        }
        val th = mutableListOf<Long>()
        val tx = mutableListOf<Float>()
        val ty = mutableListOf<Float>()
        val ta = mutableListOf<Int>()
        val tal = mutableListOf<Float>()
        val tr = mutableListOf<Float>()
        val tsc = mutableListOf<Float>()
        val textStarts = mutableListOf<Long>()
        val textDurations = mutableListOf<Long>()
        val textFx = mutableListOf<List<Effect>>()
        val textOrders = mutableListOf<Int>()
        for ((rank, l) in layers.withIndex()) {
            if (!inFrame(l, t) || !l.visible || l.kind != LayerKindUi.TEXT) continue
            if (l.text.ifEmpty { l.name }.isEmpty()) continue
            val h = layoutHandles[l.id] ?: continue
            val b = layoutBoundsCache[l.id] ?: RumoBridge.layoutBounds(h)?.also {
                layoutBoundsCache[l.id] = it
            } ?: continue
            if (b.size < 2 || b[0] <= 0f || b[1] <= 0f) continue
            // dx/dy — the top-left corner of the UNstretched box: Rust stretches
            // the mesh around the centre of this box by `tsc` (§11.4.1), and baking
            // the scale into the offset here would move the layer away from the
            // position it was given.
            val x = cw / 2f + xAt(l, t) * s - b[0] / 2f
            val y = ch / 2f + yAt(l, t) * s - b[1] / 2f
            val a = alphaAt(l, t)
            val rot = rotationAt(l, t)
            val sc = scaleAt(l, t)
            // The outline goes first: it is the same layout thickened by its
            // thickness, and the fill on top of it is the outline. Both records
            // take the same effect chain — the outline is part of the same layer,
            // not a separate layer, and "blur only the fill" would be wrong.
            //
            // Their geometry is shared: bounds is the text's own box, not the
            // thickened mask, so centring does not shift the outline relative to
            // the fill. For the same reason the scale and the time window of both
            // records are IDENTICAL: one record — one handle, otherwise the effect
            // chains (and with them the starts) would drift apart.
            strokeHandles[l.id]?.let { sh ->
                th.add(sh)
                tx.add(x)
                ty.add(y)
                ta.add(l.strokeArgb.toInt())
                tal.add(a)
                tr.add(rot)
                tsc.add(sc)
                textStarts.add(l.startMs)
                textDurations.add(l.durationMs)
                textFx.add(l.effects)
                textOrders.add(rank shl 1)
            }
            th.add(h)
            tx.add(x)
            ty.add(y)
            ta.add(l.argb.toInt())
            tal.add(a)
            tr.add(rot)
            tsc.add(sc)
            textStarts.add(l.startMs)
            textDurations.add(l.durationMs)
            textFx.add(l.effects)
            textOrders.add((rank shl 1) + 1)
        }
        // MEDIA: the frame's centre + the layer's offset. The layer's box is
        // `60% of the frame height × scale`, but a photo is **fitted** into it
        // whole (contain) rather than stretched over it: the proportions come from
        // the texture's natural size. A square instead threw the proportions away
        // on import, and a stretched photo cannot be put back together — not by
        // scaling, not by a neural net.
        //
        // Exactly the same geometry as the Kotlin fallback draws
        // (drawPreviewContent) and as Rust expects (`image_mesh`). A photo's
        // rotation is not supported: Rust's texture group has no array of angles,
        // and `l.keys` is not applied to it.
        val ih = mutableListOf<Long>()
        val ixA = mutableListOf<Float>()
        val iyA = mutableListOf<Float>()
        val iwA = mutableListOf<Float>()
        val ihA = mutableListOf<Float>()
        val iaA = mutableListOf<Float>()
        val texStarts = mutableListOf<Long>()
        val texDurations = mutableListOf<Long>()
        val texFx = mutableListOf<List<Effect>>()
        val texOrders = mutableListOf<Int>()
        for ((rank, l) in layers.withIndex()) {
            if (!inFrame(l, t) || !l.visible || l.kind != LayerKindUi.MEDIA) continue
            val uri = l.uri ?: continue
            // Video hands over a frame texture; pictures — a staged texture by uri.
            // The classification is cached by layer id (it does not pull the
            // decoder in for pictures).
            val tex = if (isVideoLayer(l)) {
                videoTextureForLayer(l, t)
            } else {
                textureIds[uri] ?: 0L
            }
            // The group's ONLY filter: `tex` and `texFx` are added together only.
            // Rust skips a picture that is not in the registry (`texture_image`) or
            // whose `image_mesh` is degenerate; any such record would shift ALL
            // the following effect chains, so a degenerate rectangle is cut off
            // right here, not in Rust.
            if (tex == 0L) continue
            val sc = scaleAt(l, t)
            val side = ch * 0.6f * sc
            if (!(side > 0f)) continue
            // The natural proportions when the size is known; a square only when
            // the texture is not loaded yet, and then it is the best that can be
            // drawn. The known case (everything that went through stageTexture)
            // always follows the proportions.
            val natural = textureSizes[uri]
            val fit = fitInside(side, side, natural?.get(0), natural?.get(1))
            ih.add(tex)
            ixA.add(cw / 2f + xAt(l, t) * s - fit[0] / 2f)
            iyA.add(ch / 2f + yAt(l, t) * s - fit[1] / 2f)
            iwA.add(fit[0]); ihA.add(fit[1])
            iaA.add(alphaAt(l, t))
            texStarts.add(l.startMs)
            texDurations.add(l.durationMs)
            texFx.add(l.effects)
            texOrders.add(rank shl 1)
        }
        return RumoBridge.FrameEx(
            // The frame's canvas size has to be the target one, not
            // PREVIEW_W×PREVIEW_H: `copyScaled` recomputes only the offsets, while
            // `renderPreviewEx` hands exactly `call.width/height` to native. While
            // the export went to 512x288 the divergence was invisible; with
            // 1920x1080 chosen, native returned 147456 px while the Bitmap was
            // created 1920x1080 — and `copyPixelsFromBuffer` failed with "Buffer
            // not large enough for pixels". The native SHAPE geometry scales from
            // the frame height (`scale = height * 0.6 / 256`), so it needs the
            // target size too.
            call.copyScaled(ts, tox, toy).copy(width = targetW, height = targetH),
            th.toLongArray(), tx.mapScaled(ts, tox), ty.mapScaled(ts, toy),
            ta.toIntArray(), tal.toFloatArray(), tr.toFloatArray(),
            ih.toLongArray(), ixA.mapScaled(ts, tox), iyA.mapScaled(ts, toy),
            iwA.mapScaled(ts), ihA.mapScaled(ts), iaA.toFloatArray(),
            // The time window of each draw call of its group and the text mesh's
            // multiplier (docs/11 §11.4). Every array is filled from EXACTLY the
            // same pass that added the record to its group — otherwise the index
            // would slip and the layer would get someone else's time window.
            layerStarts.toLongArray(), layerDurations.toLongArray(),
            textStarts.toLongArray(), textDurations.toLongArray(),
            texStarts.toLongArray(), texDurations.toLongArray(),
            tsc.toFloatArray(),
            // The global drawing order by groups: draw calls can now pass above or
            // below other groups in layer-list order. The arrays are filled from
            // the same passes as the groups themselves, so the index in them
            // matches the draw call's index in its group.
            layerOrders.toIntArray(), textOrders.toIntArray(), texOrders.toIntArray(),
            // `textFx + texFx` has to stay element-wise aligned with Rust's
            // texture group (`build_ex_scene`: texts are pushed first, pictures
            // second, in layer-list order, and only the ones actually drawn). A
            // shifted index does not crash but silently gives a layer someone
            // else's effect chain — so the groups below are built from the SAME
            // filtered lists, and none of them adds a record after the other's
            // check.
            effectsJson = effectsJsonFor(shapeFx, textFx + texFx),
            timeMs = t,
        )
    }

    /**
     * One frame rendered with one layer's effect chain substituted.
     *
     * Needed to compare variants ("what a frame with this effect looks like") and
     * to check a freshly defined effect by fact rather than by whether the module
     * compiled. `effects == null` — the frame as it is. The substitution lives
     * inside one call: the layer is restored before the return, so the composition
     * does not get to see an intermediate state (the StateFlow conflates, the read
     * happens on a frame).
     *
     * `null` is returned only when there is nothing to substitute — there is no
     * layer with that id. There used to be an extra check here, "the chain did not
     * change → null", and it made `compare_effects` unusable by construction: "a
     * frame without effects" always passes exactly the layer's current chain, that
     * is, exactly the case the check rejected.
     */
    fun effectProbeAt(
        t: Long,
        targetW: Int,
        targetH: Int,
        layerId: String,
        effects: List<Effect>?,
    ): IntArray? {
        val saved = _layers.value
        if (effects != null) {
            if (saved.none { it.id == layerId }) return null
            _layers.value = saved.map { if (it.id == layerId) it.copy(effects = effects) else it }
        }
        return try {
            RumoBridge.renderPreviewEx(previewFrameExAt(t, targetW, targetH))
        } finally {
            _layers.value = saved
        }
    }

    /**
     * A frame with layers temporarily hidden — for the probe "did this layer get
     * drawn".
     *
     * The hiding lives inside the call, like the effect substitution in
     * [effectProbeAt]: the list is restored before the return, so the composition
     * does not get to see an intermediate state. Without that, `svg_paint` could
     * not tell "SVG parsed" from "SVG parsed and drawn".
     */
    fun probeFrameAt(t: Long, targetW: Int, targetH: Int, hidden: Set<String>): IntArray? {
        val saved = _layers.value
        if (hidden.isNotEmpty()) {
            _layers.value = saved.map { if (it.id in hidden) it.copy(visible = false) else it }
        }
        return try {
            RumoBridge.renderPreviewEx(previewFrameExAt(t, targetW, targetH))
        } finally {
            _layers.value = saved
        }
    }

    /**
     * Whether the layer's resource is ready without which it does not make it
     * into the frame: the text's layout handle and the picture's staged texture.
     * Needed by a caller that renders a frame outside the editor screen and has
     * to wait for these resources itself.
     */
    fun hasLayout(layerId: String): Boolean = layoutHandles.containsKey(layerId)

    fun hasTexture(key: String): Boolean = textureIds.containsKey(key)

    /**
     * The uniform scale of the project canvas to the target: SHAPE offsets, text,
     * pictures.
     *
     * Constants of 512×288 used to stand here, and that was an identity exactly
     * as long as there was one canvas. Now the canvas is a project property, so
     * the scale is taken from it: when the aspect ratios match (a 1080×1920 canvas
     * and a 1080×1920 target) it is an identity, when they do not, it is fitting
     * with margins on both axes, not only on X.
     */
    private fun targetMapping(targetW: Int, targetH: Int): Triple<Float, Float, Float> {
        val cw = _canvasW.value.toFloat()
        val ch = _canvasH.value.toFloat()
        val s = targetH.toFloat() / ch.coerceAtLeast(1f)
        val ox = (targetW.toFloat() - cw * s) / 2f
        val oy = (targetH.toFloat() - ch * s) / 2f
        return Triple(s, ox, oy)
    }

    private fun List<Float>.mapScaled(s: Float, o: Float = 0f): FloatArray =
        FloatArray(size) { i -> this[i] * s + o }

    private fun FloatArray.mapScaled(s: Float, o: Float = 0f): FloatArray =
        FloatArray(size) { i -> this[i] * s + o }

    private fun RumoBridge.PreviewCall.copyScaled(s: Float, ox: Float, oy: Float): RumoBridge.PreviewCall =
        copy(dxs = dxs.mapScaled(s, ox), dys = dys.mapScaled(s, oy))

    fun renderPreviewFrame() {
        scope.launch(Dispatchers.IO) {
            // The frame is drawn by the engine, and only by it: there is no longer
            // an offscreen render with a read-back and a Bitmap here (docs/12
            // §12.4). If there is no engine, or the Surface refused it, there will
            // be no frame at all — the window shows an explicit "GPU path
            // unavailable" rather than an approximation drawn by another path. Two
            // renderers means two pictures, and the second one was never the one
            // the user sees.
            //
            // The path of the last frame. The diagnostics (when present) are the
            // source of truth: the legacy renderPath() reflects the bitmap
            // function and lies on the Surface engine. We re-read the diagnostics
            // only on a path change — otherwise it would be JNI + JSON parsing on
            // every frame.
            val effective = _renderDiagnostics.value?.path.orEmpty()
                .ifEmpty { RumoBridge.renderPath() }
            if (effective != _renderPath.value) {
                _renderPath.value = effective
                refreshRenderDiagnostics()
            }
            // The engine path is active: we push the same frame into the Surface engine.
            if (_engineActive.value) pushEngineFrame()
        }
    }



    fun loadShapeTris() {
        scope.launch(Dispatchers.IO) {
            val map = mutableMapOf<Int, Int>()
            for (i in 0..34) {
                RumoBridge.shapeTris(i)?.let { map[i] = it }
            }
            _shapeTris.value = map
        }
    }

    private fun meshFor(shapeName: String, sizePx: Float): MeshUi {
        val ordinal = shapeOrdinalOf(shapeName)
        if (ordinal < 0) return MeshUi.EMPTY
        val flat = RumoBridge.shapeMesh(ordinal, sizePx, 0f, 0f) ?: return MeshUi.EMPTY
        if (flat.isEmpty()) return MeshUi.EMPTY
        val points = flat.toList().chunked(2).mapNotNull { if (it.size == 2) it[0] to it[1] else null }
        return MeshUi(points)
    }

    fun ensureMeshes(names: Collection<String>, sizePx: Float = 256f) {
        val missing = names.distinct().filter { it !in _meshes.value }
        if (missing.isEmpty()) return
        scope.launch(Dispatchers.IO) {
            val loaded = mutableMapOf<String, MeshUi>()
            for (name in missing) {
                if (name in _meshes.value) continue
                val mesh = meshFor(name, sizePx)
                if (mesh.points.isNotEmpty()) loaded[name] = mesh
            }
            if (loaded.isNotEmpty()) _meshes.update { it + loaded }
        }
    }

    fun loadMeshFor(shapeName: String, sizePx: Float = 256f) {
        ensureMeshes(listOf(shapeName), sizePx)
    }

    // --- Playback: a frame per VSYNC, the master clock is audio ---
    fun togglePlay() {
        if (_isPlaying.value) pausePlayback() else startPlayback()
    }

    fun startPlayback() {
        val limit = _projectDurationMs.value
        if (_playheadMs.value >= limit) {
            _playheadMs.value = 0L
        }
        // Audio in sync with the playhead: seek + play.
        val tSec = _playheadMs.value / 1000.0
        for (h in audioHandles.values) {
            RumoBridge.audioSeek(h, tSec)
            RumoBridge.audioPlay(h)
        }
        _isPlaying.value = true
        lastFrameNanos = 0L
        val cb = frameCallback
        // The choreographer is still coming from the main thread — we post there too.
        if (choreographer?.let { it.postFrameCallback(cb); true } != true) {
            mainHandler.post { choreographer?.postFrameCallback(cb) }
        }
    }

    fun pausePlayback() {
        try {
            choreographer?.removeFrameCallback(frameCallback)
        } catch (_: Exception) {
        }
        for (h in audioHandles.values) {
            RumoBridge.audioPause(h)
        }
        _isPlaying.value = false
    }

    /** The master clock: the position of the first live audio track in ms, or null. */
    private fun audioMasterMs(): Long? {
        for (h in audioHandles.values) {
            val pos = RumoBridge.audioPosition(h) ?: continue
            return (pos * 1000.0).toLong()
        }
        return null
    }

    fun jumpToStart() {
        seekTo(0L)
    }

    fun jumpToEnd() {
        pausePlayback()
        seekTo(_projectDurationMs.value)
    }

    fun seekTo(ms: Long) {
        val clamped = ms.coerceIn(0L, _projectDurationMs.value)
        _playheadMs.value = clamped
        val tSec = clamped / 1000.0
        for (h in audioHandles.values) {
            RumoBridge.audioSeek(h, tSec)
        }
    }

    // --- Engine handle + Surface callbacks (SurfacePreview) ---
    /** Create on entering the editor. Without the Rust symbols it returns null → fallback. */
    fun acquireEngine() {
        if (engineHandle != 0L) return
        val h = RumoBridge.engineCreate()
        if (h != null && h != 0L) {
            engineHandle = h
            _surfaceFailed.value = false
            _engineReady.value = true
            AppLog.info("engine", "created handle=$h")
        } else {
            // This used to silently mean "everything on the CPU"; now the reason is in the log.
            AppLog.error(
                "engine",
                "engineCreate returned ${h ?: 0} (native=${RumoBridge.isLoaded()}) — " +
                    "Surface engine disabled, bitmap path stays",
            )
        }
        refreshRenderDiagnostics()
    }

    /** Destroy on leaving the editor; it also stops playback. */
    fun releaseEngine() {
        pausePlayback()
        releaseVideoResources()
        _engineActive.value = false
        _engineReady.value = false
        _surfaceFailed.value = false
        val h = engineHandle
        engineHandle = 0L
        if (h != 0L) RumoBridge.engineDestroy(h)
    }

    fun engineSurfaceCreated(surface: Surface, w: Int, h: Int) {
        val e = engineHandle
        if (e == 0L) return
        val rc = RumoBridge.engineSurfaceCreated(e, surface, w, h)
        if (rc != null && rc == 0) {
            _engineActive.value = true
            _surfaceFailed.value = false
            pushEngineFrame(w, h)
        } else {
            // An engine-path failure — we stay on the bitmap path (rustFrame) and
            // no longer offer the SurfaceView in this editor session.
            AppLog.error(
                "engine",
                "surfaceCreated(${w}x$h) rc=${rc ?: "no JNI symbol"} — engine disabled, bitmap path",
            )
            _engineActive.value = false
            _surfaceFailed.value = true
        }
        // An engine transition/failure (not a per-frame path) — re-read the diagnostics.
        refreshRenderDiagnostics()
    }

    fun engineSurfaceChanged(w: Int, h: Int) {
        val e = engineHandle
        if (e == 0L) return
        if (RumoBridge.engineSurfaceChanged(e, w, h)) {
            pushEngineFrame(w, h)
        } else {
            AppLog.error("engine", "surfaceChanged(${w}x$h) failed — engine disabled, bitmap path")
            _engineActive.value = false
            _surfaceFailed.value = true
        }
        refreshRenderDiagnostics()
    }

    fun engineSurfaceDestroyed() {
        val e = engineHandle
        if (e == 0L) return
        RumoBridge.engineSurfaceDestroyed(e)
        _engineActive.value = false
    }

    /** The same Ex frame as on the bitmap path — into the engine + renderFrame.
     * The coordinates are scaled to the Surface's physical size. */
    private fun pushEngineFrame(w: Int = _canvasW.value, h: Int = _canvasH.value) {
        val e = engineHandle
        if (e == 0L || w <= 0 || h <= 0) return
        val f = previewFrameExAt(_playheadMs.value, w, h)
        val ok = RumoBridge.engineSetLayersEx(e, f.copy(call = f.call.copy(width = w, height = h)))
        if (!ok) {
            AppLog.error("engine", "engineSetLayersEx failed (${w}x$h) — engine disabled")
            _engineActive.value = false
            // See about `_surfaceFailed` in `engineRenderFrame` below: without
            // this the bitmap fallback is unreachable after an engine failure.
            _surfaceFailed.value = true
            // This is a per-frame path: we read the diagnostics only on a failure,
            // not every frame (JNI + JSON).
            refreshRenderDiagnostics()
            return
        }
        val rc = RumoBridge.engineRenderFrame(e)
        if (rc == null || rc != 0) {
            AppLog.error("engine", "engineRenderFrame rc=${rc ?: "no JNI symbol"} — engine disabled")
            _engineActive.value = false
            // The Surface is marked broken too. Otherwise the compositor would go
            // on believing in `useSurface = engineReady && !surfaceFailed`, go on
            // drawing into a dead surface and never return to the bitmap fallback —
            // the preview would simply freeze, with no way out but a restart.
            _surfaceFailed.value = true
            refreshRenderDiagnostics()
        }
    }

    // --- Audio: open when a track is added, close when it is removed ---
    /** Layers that already have a live native player (for the badge in the Audio panel). */
    private val _audioAttached = MutableStateFlow<Set<String>>(emptySet())
    val audioAttached: StateFlow<Set<String>> = _audioAttached.asStateFlow()

    /** Align the track to the current playhead (a native seek). */
    fun seekTrackToPlayhead(layerId: String) {
        audioHandles[layerId]?.let { RumoBridge.audioSeek(it, _playheadMs.value / 1000.0) }
    }

    /** Opens the player by fd (Rust reads /proc/self/fd); it holds the pfd only
     * for the duration of the open. Call it from the UI after addMediaLayer with
     * its id. */
    fun attachAudio(context: Context, layerId: String, uriString: String) {
        if (audioHandles.containsKey(layerId)) return
        scope.launch(Dispatchers.IO) {
            val handle = try {
                context.contentResolver.openFileDescriptor(Uri.parse(uriString), "r")?.use { pfd ->
                    RumoBridge.audioOpen(pfd.fd)
                }
            } catch (_: Exception) {
                null
            }
            if (handle != null && handle != 0L) {
                audioHandles[layerId] = handle
                _audioAttached.update { it + layerId }
                RumoBridge.audioSeek(handle, _playheadMs.value / 1000.0)
                if (_isPlaying.value) RumoBridge.audioPlay(handle)
            }
        }
    }

    private fun detachAudio(layerId: String) {
        audioHandles.remove(layerId)?.let { RumoBridge.audioClose(it) }
        _audioAttached.update { it - layerId }
    }

    // --- Textures/text: staging into the Rust registries for the engine ---
    /** RGBA staging of a picture (the id is cached by uri; the GPU layer picks it
     * up lazily through GpuRenderer::set_texture — see rumo-render/src/jni.rs). */
    fun stageTexture(key: String, decoded: RumoBridge.DecodedImage): Long? {
        freeTextureFor(key)
        val id = RumoBridge.uploadImage(decoded.width, decoded.height, decoded.rgba)
            ?: return null
        textureIds[key] = id
        textureSizes[key] = intArrayOf(decoded.width, decoded.height)
        return id
    }

    /** The layer texture's natural size, or null if it is not loaded yet. */
    fun textureSizeFor(uri: String): IntArray? = textureSizes[uri]

    /**
     * The video layer's size: the decoder's frame has the video's own
     * proportions, so the size from the probe is enough for it, not the size of
     * every frame.
     */
    fun noteVideoSize(uri: String, width: Int, height: Int) {
        if (width > 0 && height > 0) textureSizes[uri] = intArrayOf(width, height)
    }

    /** The path of the clip's local copy; the decoder will open it instead of the content uri. */
    fun noteLocalVideoPath(layerId: String, absolutePath: String) {
        videoLocalPaths[layerId] = absolutePath
    }

    /**
     * The clip's frame rate — needed to know what time interval one frame spans
     * and when a loaded texture is still valid. Without it, reuse is not enabled:
     * an extra decode is safer than a frame from the wrong moment.
     */
    fun noteVideoFps(layerId: String, fps: Float) {
        if (fps.isFinite() && fps > 0f) videoFps[layerId] = fps
    }

    fun freeTextureFor(key: String) {
        textureIds.remove(key)?.let { RumoBridge.freeTexture(it) }
        textureSizes.remove(key)
    }

    /** Delete textures whose uri is no longer used by any layer. */
    fun gcTextures(usedUris: Set<String>) {
        val stale = textureIds.keys - usedUris
        for (key in stale) freeTextureFor(key)
    }

    // --- SVG: geometry in the engine registry ---
    /**
     * The SVG layer's uri, or null.
     *
     * A layer is a SHAPE with a `uri`, and the engine has no separate kind (see
     * `Layer.uri` and `previewCallAt`), so "is this an SVG or a Material shape" is
     * decided here, not in Rust. It is recognised by name/path: a MediaStore uri
     * has no extension, but the file name keeps it, and it also becomes the
     * layer's name.
     */
    private fun svgUriOf(name: String, uri: String?): String? {
        if (uri == null) return null
        val path = uri.substringBefore('?').substringBefore('#')
        return if (path.endsWith(".svg", true) || name.endsWith(".svg", true)) uri else null
    }

    private fun svgUriOf(layer: LayerUi): String? =
        if (layer.kind == LayerKindUi.SHAPE) svgUriOf(layer.name, layer.uri) else null

    /** A layer with SVG geometry (for the Media panel and for the frame build). */
    fun isSvgLayer(layer: LayerUi): Boolean = svgUriOf(layer) != null

    /**
     * The draw call slot of the SHAPE group: for a Material shape — its ordinal,
     * for an SVG — the id in the engine registry. `-1` = the layer does not go
     * into the frame (an unknown shape or an SVG without an id). Both branches
     * have to live in one function: `ordinals` is the only array by which the
     * SHAPE group is addressed, and the build in `previewFrameExAt` goes by it
     * too.
     */
    private fun shapeSlot(layer: LayerUi): Int {
        val svg = svgUriOf(layer)
        if (svg != null) return svgIds[svg]?.id ?: -1
        return shapeOrdinalOf(layer.name)
    }

    /** Why the SVG layer was not drawn (for diagnostics/the tool). */
    fun svgFailure(uri: String): String? = svgErrors[uri]

    /**
     * The document's own proportions `[width, height]`, or null while it is not
     * parsed or the engine has not given the sizes.
     *
     * Needed by the selection frame (`EditorScreen.layerBoxPx`): the engine fits
     * the art so that the **longer** side of the document occupies the layer's box
     * (`svg_meshes`), so the frame has to know the proportion instead of drawing a
     * square.
     */
    fun svgSizeFor(uri: String): FloatArray? {
        val entry = svgIds[uri] ?: return null
        if (entry.width <= 0f || entry.height <= 0f) return null
        return floatArrayOf(entry.width, entry.height)
    }

    /**
     * Read and register an SVG file, idempotently by (uri, content).
     *
     * The content fingerprint, not the uri alone: a file in the project folder can
     * be overwritten, and then the same uri has to give new geometry while the old
     * id is released. A repeated call on unchanged content returns the same id and
     * registers nothing, so a "per frame" loop does not leak ids: registration is
     * called only on a change of the layer list and explicitly from the tool.
     */
    fun ensureSvgRegistered(uri: String): Int? {
        val ctx = assetContext ?: run {
            svgErrors[uri] = "no access to the file: the context is not bound"
            return null
        }
        val bytes = runCatching { readUriBytes(ctx, Uri.parse(uri)) }.getOrNull()
        if (bytes == null || bytes.isEmpty()) {
            failSvg(uri, "Cannot read the SVG: $uri")
            return null
        }
        val key = bytes.contentHashCode() * 31 + bytes.size
        synchronized(svgLock) {
            // A foreign registration could have happened between the read and the lock.
            val prev = svgIds[uri]
            if (prev != null && prev.contentKey == key) return prev.id
            val id = RumoBridge.svgRegister(bytes)
            if (id == null) {
                failSvg(uri, "the engine could not parse the SVG (no symbol or a parse error)")
                // The previous geometry no longer corresponds to the file: we release it.
                svgIds.remove(uri)?.let { RumoBridge.svgRelease(it.id) }
                return null
            }
            // We take the document's sizes from the parse: `svgRegister` gives only
            // the id, while the selection frame needs the proportion (`svgSizeFor`).
            // A second parse is the price of registration, not of a frame: we only
            // come here on a content change. No sizes — the frame stays a square.
            val size = RumoBridge.svgValidate(bytes)
            svgIds[uri] = SvgEntry(key, id, size?.width ?: 0f, size?.height ?: 0f)
            svgErrors.remove(uri)
            // The new id has taken the old one's place — the old one is released
            // right here, so every [svgIds] record accounts for exactly one live id.
            if (prev != null && prev.id != id) RumoBridge.svgRelease(prev.id)
        }
        // A layer without an id was skipped when the frame was built; now the id
        // exists, and the frame has to be rebuilt — a change of `_layers` does not
        // do that.
        renderPreviewFrame()
        return svgIds[uri]?.id
    }

    private fun failSvg(uri: String, reason: String) {
        svgErrors[uri] = reason
        AppLog.warn("svg", reason)
    }

    /**
     * Sort out the SVG layers: register the new ones, release the orphaned ones.
     *
     * Called both on a change of the layer list and after the context is bound
     * (otherwise a project opened before the binding would leave the SVG
     * unregistered). A failed attempt does not repeat by itself: a uri in
     * [svgErrors] is skipped until the content changes through
     * [ensureSvgRegistered] directly.
     */
    private fun syncSvgRegistry(layers: List<LayerUi>) {
        val used = layers.mapNotNull { svgUriOf(it) }.toSet()
        for (uri in svgIds.keys) {
            if (uri !in used) svgIds.remove(uri)?.let { RumoBridge.svgRelease(it.id) }
        }
        for (uri in svgErrors.keys) if (uri !in used) svgErrors.remove(uri)
        for (uri in used) {
            if (svgIds.containsKey(uri) || svgErrors.containsKey(uri)) continue
            if (!svgPending.add(uri)) continue
            scope.launch(Dispatchers.IO) {
                try {
                    ensureSvgRegistered(uri)
                } finally {
                    svgPending.remove(uri)
                }
            }
        }
    }

    // --- Video: lazy opening of the decoder by the layer's uri ---
    /** The application context for opening video (called from a UI effect). */
    fun bindVideoContext(context: Context?) {
        videoContext = context?.applicationContext
        // SVG files are read through the same application context, and there is
        // one binding per editor. SVGs opened before the binding are accounted for
        // here too.
        assetContext = context?.applicationContext
        syncSvgRegistry(_layers.value)
    }

    /** Mark the layer as video (from the picker) — it saves the mime classification. */
    fun markVideoLayer(layerId: String) {
        videoIsVideo[layerId] = true
    }

    /** Whether the layer is video; the mime is cached so the decoder is not touched for pictures. */
    private fun isVideoLayer(layer: LayerUi): Boolean {
        videoIsVideo[layer.id]?.let { return it }
        val ctx = videoContext ?: return false
        val uri = layer.uri ?: return false
        val video = try {
            ctx.contentResolver.getType(Uri.parse(uri))?.startsWith("video") == true
        } catch (_: Exception) {
            false
        }
        videoIsVideo[layer.id] = video
        return video
    }

    /**
     * Lazily open the layer's decoder. The pfd is NOT closed: the Rust decoder
     * reads the fd through /proc/self/fd for the whole life of the handle.
     * null = not video/failure. After a failure we do not retry (a perf trap),
     * only on a uri change.
     */
    private fun videoHandleFor(layer: LayerUi): Long? {
        val id = layer.id
        val uri = layer.uri ?: return null
        videoHandles[id]?.let { return it }
        if (id in videoFailed) return null
        if (!isVideoLayer(layer)) return null
        val ctx = videoContext ?: return null
        var opened: ParcelFileDescriptor? = null
        var handle = 0L
        val local = videoLocalPaths[id]
        try {
            // The app's own copy first: a descriptor opened from a path is what
            // AMediaExtractor accepts, while the provider's content descriptor is
            // refused with AMEDIA_ERROR_UNKNOWN even when it can seek.
            if (local != null) {
                val file = File(local)
                if (file.isFile) {
                    opened = ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
                }
            }
            if (opened == null) {
                opened = ctx.contentResolver.openFileDescriptor(Uri.parse(uri), "r")
            }
            // Length defaults to WHOLE_FILE (-1). Passing `0L` hands the
            // extractor an empty file: no tracks, no frames, ever.
            handle = opened?.let { RumoBridge.videoOpen(it.fd, 0L) } ?: 0L
        } catch (t: Exception) {
            AppLog.warn("video", "open failed for $uri: ${t.message}")
            handle = 0L
        }
        val pfd = opened
        if (pfd == null || handle == 0L) {
            runCatching { pfd?.close() }
            videoFailed.add(id)
            videoAttemptedUris[id] = uri
            return null
        }
        videoPfds[id] = pfd
        videoHandles[id] = handle
        videoUris[id] = uri
        return handle
    }

    /**
     * The video layer's frame texture: opening the handle, releasing the previous
     * frame and requesting a new one — under one lock, so that [releaseVideo] does
     * not close the handle between the open and the decode. The previous frame is
     * released BEFORE the new one is requested: by that time it has already been
     * drawn (the bitmap path reads the pixels synchronously inside
     * renderPreviewEx, the engine path draws the frame in
     * nativeEngineSetLayersEx/render). 0 = no frame.
     */
    private fun videoTextureForLayer(layer: LayerUi, t: Long): Long =
        synchronized(videoTexLock) {
            val handle = videoHandleFor(layer) ?: return 0L
            // One texture per *frame of the clip*, not per call. The compositor
            // asks for the same instant more than once per displayed frame (a
            // recomposition, a second surface, a scroll), and decoding a video
            // frame means a full YUV→RGBA pass plus a fresh GPU texture. Re-decoding
            // for an instant already on screen is what turned a 30 fps clip into
            // something visibly crawling.
            val bucket = frameBucket(layer, t)
            val held = videoTexBuckets[layer.id]
            val current = videoTexIds[layer.id]
            if (held == bucket && current != null && current != 0L) return current

            val id = RumoBridge.videoTextureAt(handle, t)
            if (id != 0L) {
                // The outgoing texture is **retired, not freed**. It was handed
                // to the renderer and may still be sampled by a command that has
                // not executed yet — freeing it here is a use-after-free, which
                // is what killed the process with nothing in any log.
                // Retirement: released one frame later, by which point the queue
                // has certainly drained past it.
                videoTexIds.put(layer.id, id)?.takeIf { it != 0L && it != id }?.let { old ->
                    videoRetiredTex.addLast(old)
                    while (videoRetiredTex.size > VIDEO_RETIRED_FRAMES) {
                        RumoBridge.freeTexture(videoRetiredTex.removeFirst())
                    }
                }
                videoTexBuckets[layer.id] = bucket
            }
            id
        }

    /**
     * Which frame of the clip `t` falls in, so that asking twice for the same
     * frame reuses the texture.
     *
     * A tenth of a millisecond is the floor, which is the finest step a frame of
     * any real clip can occupy; without a known rate the instant itself is used,
     * so a still image or an unknown rate simply never reuses — correct, only
     * not cheaper.
     */
    private fun frameBucket(layer: LayerUi, t: Long): Long {
        val fps = videoFps[layer.id] ?: return t
        val stepMs = (1_000.0f / fps.coerceAtLeast(1f)).coerceAtLeast(0.1f)
        return (t / stepMs).toLong()
    }

    /**
     * One line per lazily drawn layer: where it stopped.
     *
     * Video and SVG draw silently — neither path reports that a resource was
     * missing, and "the layer does not appear" without this line means guessing
     * between "the decoder did not open", "there is no file" and "the geometry is
     * not registered". One word per layer, and it distinguishes exactly these
     * cases.
     *
     * The function is historically called a video report and is consumed by the
     * diagnostics dialog as a single list of per-layer problems; the SVG lines go
     * into it too, so as not to start a second surface for the same question.
     */
    fun videoStatusReport(): List<String> =
        _layers.value
            .filter { it.kind == LayerKindUi.MEDIA && (isVideoLayer(it) || it.uri == null) }
            .map { layer ->
                val state = when {
                    layer.uri == null -> "no-uri"
                    !isVideoLayer(layer) -> "not-video"
                    layer.id in videoFailed -> "open-failed"
                    !videoHandles.containsKey(layer.id) -> "no-handle"
                    !videoTexIds.containsKey(layer.id) -> "no-frame"
                    else -> "ok"
                }
                "${layer.name} [$state] ${layer.uri?.let { uri -> uri.substringAfterLast('/') } ?: ""}"
            } + _layers.value
            .filter { it.kind == LayerKindUi.SHAPE && isSvgLayer(it) }
            .map { layer ->
                val uri = layer.uri.orEmpty()
                val state = when {
                    svgFailure(uri) != null -> "svg:failed"
                    svgIds.containsKey(uri) -> "svg:ok"
                    else -> "svg:pending"
                }
                val detail = svgFailure(uri) ?: uri.substringAfterLast('/')
                "${layer.name} [$state] $detail"
            }

    /** Release the layer's video state: handle -> pfd -> frame texture. */
    private fun releaseVideo(layerId: String) {
        synchronized(videoTexLock) {
            videoHandles.remove(layerId)?.let { RumoBridge.videoClose(it) }
            runCatching { videoPfds.remove(layerId)?.close() }
            videoTexIds.remove(layerId)?.let { if (it != 0L) RumoBridge.freeTexture(it) }
            videoTexBuckets.remove(layerId)
            videoFps.remove(layerId)
            // Nothing is rendering any more, so the retirement queue can go at
            // once instead of waiting for frames that will never come.
            while (videoRetiredTex.isNotEmpty()) RumoBridge.freeTexture(videoRetiredTex.removeFirst())
        }
        videoUris.remove(layerId)
        videoAttemptedUris.remove(layerId)
        videoLocalPaths.remove(layerId)
        videoIsVideo.remove(layerId)
        videoFailed.remove(layerId)
    }

    /** Close all video resources (leaving the editor, changing the project). */
    private fun releaseVideoResources() {
        val ids = HashSet<String>()
        ids += videoHandles.keys
        ids += videoPfds.keys
        ids += videoTexIds.keys
        ids += videoUris.keys
        ids += videoAttemptedUris.keys
        ids += videoFailed
        for (id in ids) releaseVideo(id)
    }

    /** Clear the video state of deleted layers and of layers whose uri changed. */
    private fun reconcileVideoResources(layers: List<LayerUi>) {
        val uriById = HashMap<String, String>()
        for (l in layers) {
            if (l.kind == LayerKindUi.MEDIA) l.uri?.let { uriById[l.id] = it }
        }
        val tracked = HashSet<String>()
        tracked += videoHandles.keys
        tracked += videoTexIds.keys
        tracked += videoTexBuckets.keys
        tracked += videoUris.keys
        tracked += videoAttemptedUris.keys
        tracked += videoFailed
        tracked += videoIsVideo.keys
        tracked += videoLocalPaths.keys
        for (id in tracked) {
            val uri = uriById[id]
            // A failed open records its uri in [videoAttemptedUris], so a uri
            // change releases the layer and clears the failure - otherwise the
            // "retry only on uri change" promise above could never be kept.
            val known = videoUris[id] ?: videoAttemptedUris[id]
            if (uri == null || known?.let { it != uri } == true) releaseVideo(id)
        }
    }

    /** Rust text layout: the handle + bounds are cached for the engine. */
    fun prewarmLayout(layer: LayerUi) {
        if (layer.kind != LayerKindUi.TEXT) return
        val content = layer.text.ifEmpty { layer.name }.take(140)
        if (content.isEmpty()) {
            freeLayoutFor(layer.id)
            return
        }
        val wanted = TextLayoutStyle(
            content = content,
            sizePx = _canvasH.value * TEXT_LAYOUT_RATIO,
            weight = layer.textWeight.coerceIn(MIN_TEXT_WEIGHT, MAX_TEXT_WEIGHT),
            strokePx = layer.strokePx.coerceIn(0f, MAX_STROKE_PX).toInt(),
            family = layer.textFamily,
        )
        if (layoutStyles[layer.id] == wanted) return
        freeLayoutFor(layer.id)
        val h = layoutStyled(
            content,
            wanted.sizePx,
            wanted.weight,
            0f,
            wanted.family,
        ) ?: return
        layoutHandles[layer.id] = h
        RumoBridge.layoutBounds(h)?.let { layoutBoundsCache[layer.id] = it }
        layoutStyles[layer.id] = wanted
        // The outline is requested only when it exists: a zero thickness would
        // return the same fill, while an extra draw call would shift the effect
        // chains relative to what the editor built.
        if (wanted.strokePx > 0) {
            layoutStyled(content, wanted.sizePx, wanted.weight, wanted.strokePx.toFloat(), wanted.family)
                ?.let { strokeHandles[layer.id] = it }
        }
    }

    /**
     * A layout with an optional family.
     *
     * An empty family goes to the old entry point: it already answers with the
     * built-in face, and a second branch here would only fork the behaviour.
     */
    private fun layoutStyled(
        text: String,
        sizePx: Float,
        weight: Int,
        strokePx: Float,
        family: String,
    ): Long? = if (family.isEmpty()) {
        RumoBridge.layoutTextStyled(text, sizePx, weight, strokePx)
    } else {
        RumoBridge.layoutTextFamily(text, sizePx, weight, strokePx, family)
    }

    fun layoutHandleFor(layerId: String): Long? = layoutHandles[layerId]

    /** The layer's outline handle, if it has one. */
    fun strokeHandleFor(layerId: String): Long? = strokeHandles[layerId]

    private fun freeLayoutFor(layerId: String) {
        layoutHandles.remove(layerId)?.let { RumoBridge.layoutFree(it) }
        strokeHandles.remove(layerId)?.let { RumoBridge.layoutFree(it) }
        layoutStyles.remove(layerId)
        layoutBoundsCache.remove(layerId)
    }

    private fun prewarmAllLayouts(layers: List<LayerUi>) {
        scope.launch(Dispatchers.IO) {
            for (l in layers) prewarmLayout(l)
        }
    }

    /** Close the players, release textures/layouts (changing the project). */
    private fun clearEngineResources() {
        pausePlayback()
        for ((_, h) in audioHandles) RumoBridge.audioClose(h)
        audioHandles.clear()
        _audioAttached.value = emptySet()
        for ((_, id) in textureIds) RumoBridge.freeTexture(id)
        textureIds.clear()
        for ((_, h) in layoutHandles) RumoBridge.layoutFree(h)
        layoutHandles.clear()
        for ((_, h) in strokeHandles) RumoBridge.layoutFree(h)
        strokeHandles.clear()
        layoutStyles.clear()
        layoutBoundsCache.clear()
        releaseVideoResources()
    }

    // ---------------------------------------------------------------- undo
    //
    // A snapshot is the project DOCUMENT (the same JSON that goes to Rust and to
    // disk), not the whole editor state: the player, the playhead, the panel
    // layout are not the result of an edit and there is no reason to restore them.
    // The snapshot is taken BEFORE the mutation, so undo is "put the previous one
    // back", without reconstructing the changes in reverse.


    private data class UndoSnapshot(
        val json: String,
        val selectedId: String?,
    )

    private val _canUndo = MutableStateFlow(false)
    private val _canRedo = MutableStateFlow(false)
    val canUndo: StateFlow<Boolean> = _canUndo.asStateFlow()
    val canRedo: StateFlow<Boolean> = _canRedo.asStateFlow()

    private val undoStack = ArrayDeque<UndoSnapshot>()
    private val redoStack = ArrayDeque<UndoSnapshot>()

    /**
     * A non-empty value means a gesture is in progress, and the next write with
     * the same key does NOT create a new entry. That is exactly how "dragged a
     * layer" gives one entry rather than two hundred, one per finger movement.
     */
    private var coalesceKey: String? = null

    /**
     * The batch depth: while it is non-zero, the internal setters do not write
     * entries. Needed by the assistant's tools — one `update_layer` call changes
     * text, colour and position, and without a batch that would be three undos
     * instead of one.
     *
     * The snapshot is taken by the batch entry (before the first mutation), not by
     * each setter: by the time the second field is set the document is already half
     * changed, and an undo from it would return to an intermediate state the user
     * never saw.
     */
    private var undoBatchDepth = 0

    fun beginUndoBatch() {
        if (undoBatchDepth == 0) pushUndo()
        undoBatchDepth++
    }

    fun endUndoBatch() {
        if (undoBatchDepth > 0) undoBatchDepth--
    }

    private fun pushUndo(key: String? = null) {
        if (undoBatchDepth > 0) return
        val top = undoStack.lastOrNull()
        if (key != null && key == coalesceKey && top != null) return
        undoStack.addLast(UndoSnapshot(toJson(), _selectedId.value))
        while (undoStack.size > UNDO_LIMIT) undoStack.removeFirst()
        redoStack.clear()
        coalesceKey = key
        syncUndoFlags()
    }

    /** Close the gesture: the next write with the same key will create an entry again. */
    fun endGesture() {
        coalesceKey = null
    }

    private fun syncUndoFlags() {
        _canUndo.value = undoStack.isNotEmpty()
        _canRedo.value = redoStack.isNotEmpty()
    }

    /** Clearing the history on a document change: an unopened project cannot be undone. */
    private fun clearHistory() {
        undoStack.clear()
        redoStack.clear()
        coalesceKey = null
        syncUndoFlags()
    }

    fun undo() {
        val snapshot = undoStack.removeLastOrNull() ?: return
        redoStack.addLast(UndoSnapshot(toJson(), _selectedId.value))
        while (redoStack.size > UNDO_LIMIT) redoStack.removeFirst()
        coalesceKey = null
        syncUndoFlags()
        restore(snapshot)
    }

    fun redo() {
        val snapshot = redoStack.removeLastOrNull() ?: return
        undoStack.addLast(UndoSnapshot(toJson(), _selectedId.value))
        while (undoStack.size > UNDO_LIMIT) undoStack.removeFirst()
        coalesceKey = null
        syncUndoFlags()
        restore(snapshot)
    }

    /**
     * Restore the document from a snapshot.
     *
     * Text layouts have to be rebuilt: the handles are cached by layer id, and a
     * restored document with the same ids may carry different text — otherwise
     * undo would show the glyphs of a layer no longer in the project.
     * `loadFromJson` does this itself (clearEngineResources + prewarm), but it
     * also sets the playhead to zero and selects the first SHAPE, so the selection
     * is restored from the snapshot.
     */
    private fun restore(snapshot: UndoSnapshot) {
        if (!loadDocument(snapshot.json)) return
        val restored = _layers.value.firstOrNull { it.id == snapshot.selectedId }
        _selectedId.value = restored?.id ?: _layers.value.firstOrNull()?.id
        // The document after the undo differs from what lies on disk.
        _hasEdits.value = true
    }

    /**
     * Audio sources for the export (docs/11 §11.5): layers `AUDIO` and `MEDIA`,
     * whose file can contain audio at all.
     *
     * `MEDIA` is included not "just in case" but because video almost always has
     * sound, and it cannot be decided here: the layers do not remember whether the
     * file was probed. Rust answers an attempt with the code `1` ("no track"), and
     * that is not an error — that is how the cost of a wrong guess is avoided.
     *
     * Hidden layers are skipped: the user switched them off, and they must not be
     * in the file.
     */
    fun audioSourcesForExport(): List<Pair<LayerUi, String>> =
        _layers.value
            .filter { it.visible && it.uri != null && (it.kind == LayerKindUi.AUDIO || it.kind == LayerKindUi.MEDIA) }
            .mapNotNull { layer -> layer.uri?.let { layer to it } }

    fun addKeyframe(id: String, value: Float? = null) {
        addKeyframeAt(id, _playheadMs.value, value)
    }

    /**
     * Put a key at a specific moment without moving the playhead.
     *
     * Needed by a caller that sets the time itself (the assistant): before, the
     * only way was to move the playhead and call `addKeyframe`, that is, a side
     * effect of putting a key was the user's cursor travelling.
     */
    fun addKeyframeAt(id: String, timeMs: Long, value: Float? = null, ease: EaseUi = EaseUi()) {
        pushUndo()
        _layers.update { layers ->
            layers.map { layer ->
                if (layer.id != id) {
                    layer
                } else {
                    val t = timeMs.coerceAtLeast(0L)
                    val v = value ?: rotationAt(layer, t)
                    // A duplicate time → replace the value, otherwise insert + sort by time.
                    val keys = (layer.keys.filterNot { it.timeMs == t } + KeyframeUi(t, v, ease))
                        .sortedBy { it.timeMs }
                    layer.copy(keys = keys)
                }
            }
        }
        _hasEdits.value = true
    }

    fun removeKeyframe(id: String, timeMs: Long) {
        pushUndo()
        _layers.update { layers ->
            layers.map { layer ->
                if (layer.id != id) {
                    layer
                } else {
                    layer.copy(keys = layer.keys.filterNot { it.timeMs == timeMs })
                }
            }
        }
        _hasEdits.value = true
    }

    /**
     * A key of an animated property (docs/11 §11.3). [ROTATION] is the former
     * [LayerUi.keys], the others are new tracks. The string names match the ones
     * the assistant's `keys` tool gives the model, so one place knows both the
     * track and what it is called from outside.
     */
    enum class KeyTrack(val wire: String) {
        ROTATION("rotation"),
        SCALE("scale"),
        POSITION_X("position_x"),
        POSITION_Y("position_y"),
        ALPHA("alpha"),
        ;

        companion object {
            fun fromWire(s: String): KeyTrack? = entries.firstOrNull { it.wire == s }
        }
    }

    /** The layer's track for a property; an empty list = "not animated". */
    fun keysOf(layer: LayerUi, track: KeyTrack): List<KeyframeUi> = when (track) {
        KeyTrack.ROTATION -> layer.keys
        KeyTrack.SCALE -> layer.scaleKeys
        KeyTrack.POSITION_X -> layer.xKeys
        KeyTrack.POSITION_Y -> layer.yKeys
        KeyTrack.ALPHA -> layer.alphaKeys
    }

    /**
     * A key on the property's track, replacing a key with the same time. The time
     * is clamped to `>= 0`, and after the insert the track stays sorted by time
     * with unique times — the sampler assumes that.
     */
    fun addPropertyKeyframe(
        id: String,
        timeMs: Long,
        track: KeyTrack,
        value: Float,
        ease: EaseUi = EaseUi(),
    ) {
        pushUndo()
        _layers.update { layers ->
            layers.map { layer ->
                if (layer.id != id) {
                    layer
                } else {
                    val t = timeMs.coerceAtLeast(0L)
                    val next = (
                        keysOf(layer, track).filterNot { it.timeMs == t } +
                            KeyframeUi(t, value, ease)
                        ).sortedBy { it.timeMs }
                    layer.withKeys(track, next)
                }
            }
        }
        _hasEdits.value = true
    }

    /**
     * The curve of a key that is already there.
     *
     * A separate operation, not a parameter of [addKeyframeAt]: the curve is
     * edited *after* the key is placed, and edited by eye — while
     * `addKeyframeAt` on an existing time overwrites the value, that is, setting
     * the curve would also reset the value if it went through it.
     *
     * Returns `false` if there is no key at that moment: it cannot be invented
     * here, a key has to have a value, and only the caller knows it.
     */
    fun setKeyEase(id: String, track: KeyTrack, timeMs: Long, ease: EaseUi): Boolean {
        val layer = _layers.value.firstOrNull { it.id == id } ?: return false
        if (keysOf(layer, track).none { it.timeMs == timeMs }) return false
        pushUndo()
        _layers.update { layers ->
            layers.map { l ->
                if (l.id != id) {
                    l
                } else {
                    l.withKeys(
                        track,
                        keysOf(l, track).map { if (it.timeMs == timeMs) it.copy(ease = ease) else it },
                    )
                }
            }
        }
        _hasEdits.value = true
        return true
    }

    /** Remove a key from the property's track; an empty track = the layer's base value. */
    fun removePropertyKeyframe(id: String, timeMs: Long, track: KeyTrack) {
        pushUndo()
        _layers.update { layers ->
            layers.map { layer ->
                if (layer.id != id) layer
                else layer.withKeys(track, keysOf(layer, track).filterNot { it.timeMs == timeMs })
            }
        }
        _hasEdits.value = true
    }

    /** Remove all the track's keys, leaving the layer's base value. */
    fun clearPropertyKeys(id: String, track: KeyTrack) {
        pushUndo()
        _layers.update { layers ->
            layers.map { layer ->
                if (layer.id == id) layer.withKeys(track, emptyList()) else layer
            }
        }
        _hasEdits.value = true
    }

    private fun LayerUi.withKeys(track: KeyTrack, next: List<KeyframeUi>): LayerUi = when (track) {
        KeyTrack.ROTATION -> copy(keys = next)
        KeyTrack.SCALE -> copy(scaleKeys = next)
        KeyTrack.POSITION_X -> copy(xKeys = next)
        KeyTrack.POSITION_Y -> copy(yKeys = next)
        KeyTrack.ALPHA -> copy(alphaKeys = next)
    }

    fun setColor(id: String, argb: Long) {
        pushUndo("color:$id")
        _layers.update { layers ->
            layers.map { layer ->
                if (layer.id != id) {
                    layer
                } else {
                    if (layer.kind != LayerKindUi.SHAPE && layer.kind != LayerKindUi.TEXT) {
                        layer
                    } else {
                        layer.copy(argb = argb)
                    }
                }
            }
        }
        _hasEdits.value = true
    }

    fun setAlpha(id: String, value: Float) {
        pushUndo("alpha:$id")
        _layers.update { layers ->
            layers.map { layer ->
                if (layer.id != id) {
                    layer
                } else {
                    layer.copy(alpha = value.coerceIn(0f, 1f))
                }
            }
        }
        _hasEdits.value = true
    }

    fun toggleVisibility(id: String) {
        pushUndo()
        _layers.update { layers ->
            layers.map { if (it.id == id) it.copy(visible = !it.visible) else it }
        }
        _hasEdits.value = true
    }

    fun addShape(name: String) {
        pushUndo()
        val color = LayerPalette[(shapeColorSeq.getAndIncrement() % LayerPalette.size).toInt()]
        val id = idSeq.getAndIncrement().toString()
        val layer = LayerUi(
            id = id,
            kind = LayerKindUi.SHAPE,
            name = name,
            visible = true,
            argb = color,
        )
        _layers.update { it + layer }
        _selectedId.value = id
        _hasEdits.value = true
        ensureMeshes(listOf(name))
    }

    fun addTextLayer(text: String) {
        pushUndo()
        val content = text.take(140)
        val id = idSeq.getAndIncrement().toString()
        val layer = LayerUi(
            id = id,
            kind = LayerKindUi.TEXT,
            name = content.ifBlank { "Text" },
            visible = true,
            argb = 0xFFFFFFFF,
            durationMs = DEFAULT_MIN_DURATION_MS,
            text = content,
        )
        _layers.update { it + layer }
        _selectedId.value = id
        _hasEdits.value = true
        scope.launch(Dispatchers.IO) { prewarmLayout(layer) }
    }

    /** Returns the layer id — the UI needs it for attachAudio. */
    fun addMediaLayer(name: String, kind: LayerKindUi, durationMs: Long, uri: String?): String {
        pushUndo()
        val mediaKind = if (kind == LayerKindUi.AUDIO) LayerKindUi.AUDIO else LayerKindUi.MEDIA
        val id = idSeq.getAndIncrement().toString()
        val layer = LayerUi(
            id = id,
            kind = mediaKind,
            name = name,
            visible = true,
            durationMs = durationMs.coerceAtLeast(1L),
            uri = uri,
        )
        _layers.update { it + layer }
        _selectedId.value = id
        _hasEdits.value = true
        return id
    }

    /**
     * Add an SVG layer: a SHAPE with a `uri` to a `.svg`.
     *
     * A SHAPE specifically, not a new kind: the engine knows no kinds, while the
     * layer gets a transform, keys, effects, transitions and the inspector without
     * a single new branch. `argb` is not set — an SVG carries its own colours, and
     * tinting it would substitute the picture. `name` has to end in `.svg`: that is
     * how the layer is recognised after the project is reloaded.
     */
    fun addSvgLayer(name: String, uri: String): String {
        pushUndo()
        val id = idSeq.getAndIncrement().toString()
        val layerName = if (name.endsWith(".svg", true)) name else "$name.svg"
        val layer = LayerUi(
            id = id,
            kind = LayerKindUi.SHAPE,
            name = layerName,
            visible = true,
            durationMs = DEFAULT_MIN_DURATION_MS,
            uri = uri,
        )
        _layers.update { it + layer }
        _selectedId.value = id
        _hasEdits.value = true
        // The registration need not be awaited: the id will appear in the cache,
        // and the next frame will pick it up (ensureSvgRegistered triggers
        // renderPreviewFrame itself).
        scope.launch(Dispatchers.IO) { ensureSvgRegistered(uri) }
        return id
    }

    fun removeLayer(id: String) {
        pushUndo()
        detachAudio(id)
        freeLayoutFor(id)
        releaseVideo(id)
        _layers.update { layers -> layers.filterNot { it.id == id } }
        gcTextures(_layers.value.filter { it.kind == LayerKindUi.MEDIA }.mapNotNull { it.uri }.toSet())
        if (_selectedId.value == id) {
            _selectedId.value = _layers.value.firstOrNull { it.kind == LayerKindUi.SHAPE }?.id
        }
        _hasEdits.value = true
    }

    fun selectLayer(id: String?) {
        _selectedId.value = id
    }

    // --- UI-only layer flags (not part of the .rumo contract: the Rust codec does not know about them) ---
    private val _locked = MutableStateFlow<Set<String>>(emptySet())
    val locked: StateFlow<Set<String>> = _locked.asStateFlow()

    fun toggleLocked(id: String) {
        _locked.update { if (id in it) it - id else it + id }
    }

    /** Node positions in the fullscreen graph: key -> (x, y) in canvas px. */
    private val _nodePos = MutableStateFlow(NodeLayoutRepo.read())
    val nodePos: StateFlow<Map<String, Pair<Float, Float>>> = _nodePos.asStateFlow()

    fun setNodePos(key: String, x: Float, y: Float) {
        pushUndo("node:$key")
        _nodePos.update { it + (key to (x to y)) }
        NodeLayoutRepo.write(_nodePos.value)
    }

    /** Reset a node's manual layout (or the whole map when key == null). */
    fun clearNodePos(key: String? = null) {
        pushUndo()
        if (key == null) _nodePos.value = emptyMap() else _nodePos.update { it - key }
        NodeLayoutRepo.write(_nodePos.value)
    }

    /**
     * Rename is a label change only. SHAPE layers are excluded on purpose: their
     * `name` is the shape kind the engine renders (`shapeOrdinalOf`), so a
     * rename would erase them from the frame — `setShapeName` is the SHAPE-
     * specific verb instead.
     */
    fun renameLayer(id: String, name: String) {
        pushUndo()
        val clean = name.trim().take(60)
        if (clean.isEmpty()) return
        _layers.update { layers ->
            layers.map {
                if (it.id == id && it.kind != LayerKindUi.SHAPE) it.copy(name = clean) else it
            }
        }
        _hasEdits.value = true
    }

    /** SHAPE-only: swap the geometry kind; `name` follows it (engine contract). */
    fun setShapeName(id: String, shapeName: String) {
        pushUndo()
        if (shapeName !in ShapeNames) return
        _layers.update { layers ->
            layers.map {
                if (it.id == id && it.kind == LayerKindUi.SHAPE) it.copy(name = shapeName) else it
            }
        }
        _hasEdits.value = true
        ensureMeshes(listOf(shapeName))
    }

    /** A z-order move: index from -> to (the list = drawing order). */
    fun moveLayerOrder(from: Int, to: Int) {
        pushUndo()
        val list = _layers.value
        if (from !in list.indices || to !in list.indices || from == to) return
        val mutable = list.toMutableList()
        val item = mutable.removeAt(from)
        mutable.add(to, item)
        _layers.value = mutable
        _hasEdits.value = true
    }

    fun setDuration(id: String, ms: Long) {
        pushUndo("duration:$id")
        _layers.update { layers ->
            layers.map {
                if (it.id == id) {
                    it.copy(durationMs = ms.coerceIn(MIN_LAYER_DURATION_MS, MAX_LAYER_DURATION_MS))
                } else {
                    it
                }
            }
        }
        _hasEdits.value = true
    }

    /**
     * The layer's start on the timeline, ms (docs/11 §11.1). A negative value is
     * clamped to zero rather than ignored: a layer cannot start before frame zero,
     * and a layer that "went negative" would simply disappear from the project.
     */
    fun setStartMs(id: String, startMs: Long) {
        pushUndo("start:$id")
        if (id in _locked.value) return
        val t = startMs.coerceAtLeast(0L)
        _layers.update { layers ->
            layers.map { if (it.id == id) it.copy(startMs = t) else it }
        }
        _hasEdits.value = true
    }

    /** The layer's offset in preview coordinates (Canvas/Surface px). */
    fun setOffset(id: String, x: Float, y: Float) {
        pushUndo("offset:$id")
        if (id in _locked.value) return
        _layers.update { layers ->
            layers.map {
                if (it.id == id) {
                    it.copy(
                        offsetX = x.coerceIn(-1000f, 1000f),
                        offsetY = y.coerceIn(-1000f, 1000f),
                    )
                } else {
                    it
                }
            }
        }
        _hasEdits.value = true
    }

    /** A TEXT layer's text: it rebuilds the Rust layout (the quad shapes change). */
    fun setText(id: String, text: String) {
        pushUndo("text:$id")
        val content = text.take(140)
        _layers.update { layers ->
            layers.map { if (it.id == id) it.copy(text = content) else it }
        }
        _hasEdits.value = true
        scope.launch(Dispatchers.IO) {
            freeLayoutFor(id)
            _layers.value.find { it.id == id }?.let { prewarmLayout(it) }
        }
    }

    /**
     * The text layer's font weight.
     *
     * The only place the weight is written, apart from a project import: the
     * layout was already created for the previous weight, so it has to be released
     * and requested afresh — otherwise choosing a weight would change the data but
     * not the picture.
     */
    fun setTextWeight(id: String, weight: Int) {
        pushUndo("weight:$id")
        if (id in _locked.value) return
        val clamped = weight.coerceIn(MIN_TEXT_WEIGHT, MAX_TEXT_WEIGHT)
        _layers.update { layers ->
            layers.map {
                if (it.id == id && it.kind == LayerKindUi.TEXT) it.copy(textWeight = clamped) else it
            }
        }
        _hasEdits.value = true
        scope.launch(Dispatchers.IO) { relayoutText(id) }
    }

    /**
     * The text layer's font family; empty means the built-in face.
     *
     * The name is not checked for existence: the font may have been deleted from
     * the shop after it was chosen in the project, and then the engine will draw
     * the layer in the built-in face (the documented fallback of
     * `layout_styled_family`). Refusing to set the name would be worse: the project
     * would open, but the text would disappear entirely instead of staying
     * readable.
     */
    fun setTextFamily(id: String, family: String) {
        pushUndo("family:$id")
        if (id in _locked.value) return
        val clean = family.trim()
        _layers.update { layers ->
            layers.map {
                if (it.id == id && it.kind == LayerKindUi.TEXT) it.copy(textFamily = clean) else it
            }
        }
        _hasEdits.value = true
        scope.launch(Dispatchers.IO) { relayoutText(id) }
    }

    /** The text outline thickness in frame pixels; 0 removes it. */
    fun setStrokePx(id: String, px: Float) {        pushUndo("stroke:$id")
        if (id in _locked.value) return
        if (!px.isFinite()) return
        val clamped = px.coerceIn(0f, MAX_STROKE_PX)
        _layers.update { layers ->
            layers.map {
                if (it.id == id && it.kind == LayerKindUi.TEXT) it.copy(strokePx = clamped) else it
            }
        }
        _hasEdits.value = true
        scope.launch(Dispatchers.IO) { relayoutText(id) }
    }

    /** The outline colour, 0xAARRGGBB. The layout does not depend on it — only the tint. */
    fun setStrokeArgb(id: String, argb: Long) {
        pushUndo("strokeColor:$id")
        if (id in _locked.value) return
        _layers.update { layers ->
            layers.map {
                if (it.id == id && it.kind == LayerKindUi.TEXT) {
                    it.copy(strokeArgb = argb and 0xFFFFFFFFL)
                } else {
                    it
                }
            }
        }
        _hasEdits.value = true
    }

    /** Rebuild the layer's layout after a change of what it is made from. */
    private fun relayoutText(id: String) {
        freeLayoutFor(id)
        _layers.value.find { it.id == id }?.let { prewarmLayout(it) }
    }

    /**
     * [LayerUi.scale] mutator: clamped to [[LayerUi.MIN_SCALE], [LayerUi.MAX_SCALE]],
     * so the rectangle never becomes zero (Rust would skip such a picture and
     * shift the effect chains). The only place scale is written, apart from a
     * project import.
     */
    fun setScale(id: String, value: Float) {
        pushUndo("scale:$id")
        if (id in _locked.value) return
        if (!value.isFinite()) return
        val clamped = value.coerceIn(LayerUi.MIN_SCALE, LayerUi.MAX_SCALE)
        _layers.update { layers ->
            layers.map { if (it.id == id) it.copy(scale = clamped) else it }
        }
        _hasEdits.value = true
    }

    /**
     * The incoming layer's transition. `null` removes the transition. The duration
     * is clamped to [[TransitionUi.MIN_DURATION_MS], [TransitionUi.MAX_DURATION_MS]]
     * — zero would divide by zero in `rampAt`. The only place the transition is
     * written, apart from loading a project.
     */
    fun setTransition(id: String, transition: TransitionUi?) {
        pushUndo()
        val safe = transition?.copy(
            startMs = transition.startMs.coerceAtLeast(0L),
            durationMs = transition.durationMs.coerceIn(
                TransitionUi.MIN_DURATION_MS,
                TransitionUi.MAX_DURATION_MS,
            ),
        )
        _layers.update { layers ->
            layers.map { if (it.id == id) it.copy(transition = safe) else it }
        }
        _hasEdits.value = true
    }

    /** The layer's texture layout bounds (scene layout px), or null until warmed up. */
    fun textBoundsFor(layerId: String): FloatArray? = layoutBoundsCache[layerId]
    fun moveLayer(id: String, dx: Float, dy: Float) {
        pushUndo("move:$id")
        if (id in _locked.value) return
        _layers.update { layers ->
            layers.map { layer ->
                if (layer.id != id) {
                    layer
                } else {
                    layer.copy(
                        offsetX = (layer.offsetX + dx).coerceIn(-1000f, 1000f),
                        offsetY = (layer.offsetY + dy).coerceIn(-1000f, 1000f),
                    )
                }
            }
        }
        _hasEdits.value = true
    }

    // --- The layer's effect chain -------------------------------------------------
    // Edits go through the same copy-on-write as the other mutators:
    // `_layers.update` is the only trigger for the preview redraw (init:
    // combine(_layers, _playheadMs) -> renderPreviewFrame), a separate
    // invalidate is not needed. The list order = the application order in the
    // engine.

    /**
     * The parsed effect catalogue (JNI + JSON). Cached by the key "the project's
     * effect list": rebuilding the catalogue on every slider drag would be an
     * extra JNI call and JSON parse per frame, but `by lazy` is no longer possible
     * either — after a custom effect is defined the catalogue has to change. An
     * empty list = Rust unavailable/failed.
     */
    private var catalogueKey: String? = null
    private var catalogueValue: List<RumoBridge.EffectDescriptor> = emptyList()

    val effectCatalogue: List<RumoBridge.EffectDescriptor>
        get() {
            val key = customsJson()
            if (key != catalogueKey) {
                catalogueKey = key
                catalogueValue = RumoBridge.effectCatalogue(key)
            }
            return catalogueValue
        }

    /**
     * The effect catalogue as a flow — what the interface subscribes to.
     *
     * Separate from [effectCatalogue], because that one is an ordinary getter:
     * reading `StateFlow.value` does not register a Compose subscription, so an
     * effect installed in the shop would appear in the menu only after a restart —
     * which is exactly what used to happen. The flow is rebuilt when either list
     * changes (the project's effects and the device's effects), and the panel
     * reads it.
     *
     * The initial value is the catalogue without user effects: that way the panel
     * does not flash empty on the first frame, and the first build happens right
     * after it.
     */
    val effectCatalogueFlow: StateFlow<List<RumoBridge.EffectDescriptor>> =
        combine(_customEffects, _installedEffects) { project, installed ->
            val taken = project.map { it.id }.toHashSet()
            val merged = project + installed.filterNot { it.id in taken }
            if (merged.isEmpty()) "" else CustomEffects.encode(merged).toString()
        }
            .map { key -> RumoBridge.effectCatalogue(key) }
            .stateIn(scope, SharingStarted.Lazily, RumoBridge.effectCatalogue())

    /** The project's effects in the shape the engine reads (`custom` in the chains). */
    fun customsJson(): String {
        val list = mergedCustoms()
        return if (list.isEmpty()) "" else CustomEffects.encode(list).toString()
    }

    /**
     * Accept a project effect: first a quick check by the engine (WGSL parsing and
     * validation, without an adapter), and only then does it land in the project.
     * Returns a rejection message, or null on success.
     */
    fun defineCustomEffect(effect: CustomEffectUi): String? {
        CustomEffects.shapeError(effect)?.let { return it }
        if (RumoBridge.effectCatalogue().any { it.id == effect.id }) {
            return "`${effect.id}` is a built-in effect id"
        }
        val reply = RumoBridge.effectValidate(effect.toJson().toString())
            ?: return "the engine could not check the effect (no validator in this build)"
        if (!reply.ok) {
            return reply.error.ifEmpty { "the engine rejected the effect" }
        }
        pushUndo()
        val replaced = _customEffects.value.filterNot { it.id == effect.id }
        _customEffects.value = replaced + effect
        _hasEdits.value = true
        catalogueKey = null
        return null
    }

    /** Whether the project already defines an effect under this id. */
    fun hasCustomEffect(id: String): Boolean = mergedCustoms().any { it.id == id }

    /** Forget a project effect and every use of it. */
    fun removeCustomEffect(id: String) {
        if (_customEffects.value.none { it.id == id }) return
        pushUndo()
        _customEffects.value = _customEffects.value.filterNot { it.id == id }
        // Instances of it would otherwise stay in layer chains and be skipped at
        // draw time while still showing up in the panel.
        _layers.value = _layers.value.map { layer ->
            if (layer.effects.none { it.kindId == id }) {
                layer
            } else {
                layer.copy(effects = layer.effects.filterNot { it.kindId == id })
            }
        }
        _hasEdits.value = true
        catalogueKey = null
    }

    /** An effect kind's parameter from the catalogue; null = the kind/key is unknown. */
    private fun effectParamFor(kindId: String, key: String): RumoBridge.EffectParam? =
        effectCatalogue.firstOrNull { it.id == kindId }?.params?.firstOrNull { it.key == key }

    /** The clamp of one component — a mirror of `ParamSpec::clamp` in rumo-core. */
    private fun clampParamValue(
        param: RumoBridge.EffectParam,
        component: Int,
        value: Float,
    ): Float {
        if (!value.isFinite()) return param.default.getOrElse(0) { 0f }
        val v = when (param.kind.lowercase()) {
            "int" -> value.roundToInt().toFloat().coerceIn(param.min, param.max)
            "bool" -> if (value >= 0.5f) 1f else 0f
            "choice" -> value.roundToInt().toFloat()
                .coerceIn(0f, (param.choices.size - 1).coerceAtLeast(0).toFloat())
            else -> value.coerceIn(param.min, param.max)
        }
        // The colour's alpha lives in 0..1 regardless of the parameter's min/max.
        return if (param.kind.equals("color", ignoreCase = true) && component == 3) {
            v.coerceIn(0f, 1f)
        } else {
            v
        }
    }

    /** An edit of the layer's effect chain; false = nothing changed (a no-op). */
    private fun updateEffects(
        layerId: String,
        coalesceKey: String? = null,
        transform: (List<Effect>) -> List<Effect>,
    ): Boolean {
        var changed = false
        _layers.update { layers ->
            layers.map { layer ->
                if (layer.id != layerId) {
                    layer
                } else {
                    val next = transform(layer.effects)
                    if (next == layer.effects) {
                        layer
                    } else {
                        changed = true
                        layer.copy(effects = next)
                    }
                }
            }
        }
        if (changed) _hasEdits.value = true
        return changed
    }

    /** Append an effect to the end of the chain; an unknown kind is a no-op. */
    /**
     * Append an effect and answer with its instance id, or null when the kind is
     * unknown.
     *
     * The id is the point: a caller that has just added an effect almost always
     * wants to select it, and re-deriving the id by diffing the chain afterwards
     * is how the panel ended up editing the *first* effect instead of the one the
     * user had just added (docs/10 §3 D1). Handing the id back removes the guess.
     */
    fun addEffect(layerId: String, kindId: String): String? {
        val effect = defaultEffectFor(kindId, customsJson()) ?: return null
        updateEffects(layerId) { it + effect }
        return effect.id
    }

    fun removeEffect(layerId: String, effectId: String) {
        updateEffects(layerId) { chain -> chain.filterNot { it.id == effectId } }
    }

    /** A move of an effect inside the chain (order = application order), clamped. */
    fun moveEffect(layerId: String, effectId: String, delta: Int) {
        if (delta == 0) return
        updateEffects(layerId) { chain ->
            val from = chain.indexOfFirst { it.id == effectId }
            if (from < 0) return@updateEffects chain
            val to = (from + delta).coerceIn(0, chain.lastIndex)
            if (to == from) {
                chain
            } else {
                chain.toMutableList().apply { add(to, removeAt(from)) }
            }
        }
    }

    fun toggleEffect(layerId: String, effectId: String) {
        updateEffects(layerId) { chain ->
            chain.map { if (it.id == effectId) it.copy(enabled = !it.enabled) else it }
        }
    }

    /**
     * Write the values of the parameter [key] into the flat vector at its `slot`
     * (values are clamped as in Rust). The length of [values] has to match the
     * parameter's number of slots — otherwise a no-op (the engine checks arity).
     */
    fun setEffectParam(layerId: String, effectId: String, key: String, values: List<Float>) {
        // The key is per effect instance: dragging one slider is one undo entry,
        // not one per value pixel by pixel.
        updateEffects(layerId, "effectParam:$effectId:$key") { chain ->
            chain.map { effect ->
                if (effect.id != effectId) return@map effect
                val param = effectParamFor(effect.kindId, key) ?: return@map effect
                if (values.size != param.slots) return@map effect
                val params = effect.params.toMutableList()
                for (i in 0 until param.slots) {
                    val idx = param.slot + i
                    if (idx < 0) continue
                    while (params.size <= idx) params += 0f
                    params[idx] = clampParamValue(param, i, values[i])
                }
                effect.copy(params = params)
            }
        }
    }

    /** One component (a colour channel/scalar) of a parameter — a wrapper over [setEffectParam]. */
    fun setEffectParamValue(
        layerId: String,
        effectId: String,
        key: String,
        component: Int,
        value: Float,
    ) {
        val effect = _layers.value.firstOrNull { it.id == layerId }
            ?.effects?.firstOrNull { it.id == effectId } ?: return
        val param = effectParamFor(effect.kindId, key) ?: return
        if (component !in 0 until param.slots) return
        val values = MutableList(param.slots) { i ->
            effect.params.getOrElse(param.slot + i) { 0f }
        }
        values[component] = value
        setEffectParam(layerId, effectId, key, values)
    }
}
