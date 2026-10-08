// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import android.view.Surface
import org.json.JSONArray
import org.json.JSONObject

object RumoBridge {
    fun isLoaded(): Boolean = runCatching {
        System.loadLibrary("rumo_bridge")
    }.isSuccess

    @JvmStatic
    external fun nativeVersion(): String

    @JvmStatic
    external fun nativeEncodeProject(name: String): ByteArray

    @JvmStatic
    external fun nativeShapeTris(ordinal: Int): Int

    @JvmStatic
    external fun nativeShapeMesh(ordinal: Int, sizePx: Float, rounding: Float, rotationDeg: Float): FloatArray

    @JvmStatic
    external fun nativeDecodeImage(data: ByteArray, maxSide: Int): ByteArray

    @JvmStatic
    external fun nativeAudioDurationFd(fd: Int): Long

    @JvmStatic
    external fun nativeRenderPreview(
        width: Int,
        height: Int,
        bgArgb: Int,
        ordinals: IntArray,
        argbs: IntArray,
        dxs: FloatArray,
        dys: FloatArray,
        rotations: FloatArray,
        alphas: FloatArray,
    ): IntArray

    @JvmStatic
    external fun nativeRenderPath(): String

    @JvmStatic
    external fun nativeProjectFromJson(json: String): ByteArray

    @JvmStatic
    external fun nativeProjectToJson(data: ByteArray): String

    // --- Textures (rumo-render/src/jni.rs) ---
    @JvmStatic
    external fun nativeUploadImage(width: Int, height: Int, rgba: ByteArray): Long

    @JvmStatic
    external fun nativeFreeTexture(id: Long)

    // --- Text (rumo-render/src/jni.rs) ---
    @JvmStatic
    external fun nativeLayoutText(text: String, sizePx: Float): Long

    /**
     * Layout with style: face weight (400 regular, 700 bold) and the outline
     * thickness in pixels. With `strokePx > 0` this is the glyph's **outline** — drawn
     * under the normal layout of the same text.
     */
    @JvmStatic
    external fun nativeLayoutTextStyled(
        text: String,
        sizePx: Float,
        weight: Int,
        strokePx: Float,
    ): Long

    @JvmStatic
    external fun nativeLayoutQuadCount(handle: Long): Int

    /**
     * Registers a downloaded face (TTF/OTF/TTC) in the engine and returns the family
     * name, **read from the file itself**; `null` means the bytes were not parsed.
     * The name does not come from the caller: the shop hands over what it downloaded,
     * and only the file itself knows what it calls itself.
     */
    @JvmStatic
    external fun nativeFontRegister(bytes: ByteArray): String?

    /** The same as [nativeLayoutTextStyled], but with an explicit font family. */
    @JvmStatic
    external fun nativeLayoutTextFamily(
        text: String,
        sizePx: Float,
        weight: Int,
        strokePx: Float,
        family: String,
    ): Long

    /**
     * A flat text preview in the given font: `[width:u32 LE][height:u32 LE]
     * [rgba8…]`. `fontBytes` is the downloaded face, `argb` is `0xAARRGGBB` (like
     * `Color`), `pad` is the transparent margin in pixels. `null` if the bytes do not
     * register, the text is empty or the rectangle is degenerate.
     *
     * The bytes are passed on every call rather than registered once: the preview engine
     * is **one-shot and bounded** — it keeps the most recent faces and rebuilds beyond
     * the limit, so that browsing the catalogue does not pin the whole catalogue in
     * memory. An installed font goes to [nativeFontRegister].
     */
    @JvmStatic
    external fun nativeFontPreview(
        family: String,
        fontBytes: ByteArray,
        text: String,
        sizePx: Float,
        weight: Int,
        argb: Int,
        pad: Int,
    ): ByteArray?

    @JvmStatic
    external fun nativeLayoutQuad(handle: Long, index: Int): FloatArray?

    @JvmStatic
    external fun nativeLayoutBounds(handle: Long): FloatArray?

    @JvmStatic
    external fun nativeLayoutAtlas(handle: Long): ByteArray?

    @JvmStatic
    external fun nativeLayoutFree(handle: Long)

    // --- Audio player (rumo-media/src/audio_jni.rs; handle mode, 0 = error) ---
    @JvmStatic
    external fun nativeAudioOpen(fd: Int): Long

    @JvmStatic
    external fun nativeAudioDuration(handle: Long): Double

    @JvmStatic
    external fun nativeAudioPlay(handle: Long)

    @JvmStatic
    external fun nativeAudioPause(handle: Long)

    @JvmStatic
    external fun nativeAudioSeek(handle: Long, seconds: Double)

    @JvmStatic
    external fun nativeAudioPosition(handle: Long): Double

    @JvmStatic
    external fun nativeAudioClose(handle: Long)

    // --- MP4 export (rumo-export/src/jni.rs) ---
    @JvmStatic
    external fun nativeExportBegin(outPath: String, width: Int, height: Int, fps: Float, bitrate: Int): Long

    /**
     * A frame is drawn **and** encoded in one call without leaving the GPU: the scene
     * is composited into an offscreen target, a compute pass packs it into NV12, and
     * the bytes go to the encoder directly (docs/12 §12.3). The arguments are the same
     * as [nativeRenderPreviewEx]'s, plus a handle and a timestamp: both entry points
     * read them with one piece of code, so an export frame is the same frame as in the
     * preview.
     */
    @JvmStatic
    external fun nativeExportWriteFrameGpu(
        handle: Long,
        width: Int,
        height: Int,
        bgArgb: Int,
        ordinals: IntArray?,
        argbs: IntArray?,
        dxs: FloatArray?,
        dys: FloatArray?,
        rotations: FloatArray?,
        alphas: FloatArray?,
        shapeScales: FloatArray?,
        textHandles: LongArray?,
        textX: FloatArray?,
        textY: FloatArray?,
        textArgb: IntArray?,
        textAlpha: FloatArray?,
        textRot: FloatArray?,
        texIds: LongArray?,
        texX: FloatArray?,
        texY: FloatArray?,
        texW: FloatArray?,
        texH: FloatArray?,
        texAlpha: FloatArray?,
        layerStarts: LongArray?,
        layerDurations: LongArray?,
        textStarts: LongArray?,
        textDurations: LongArray?,
        texStarts: LongArray?,
        texDurations: LongArray?,
        textScales: FloatArray?,
        layerOrders: IntArray?,
        textOrders: IntArray?,
        texOrders: IntArray?,
        effectsJson: String?,
        timeMs: Long,
        ptsUs: Long,
    ): Int


    /**
     * Mix one audio source into the export's single audio track
     * (docs/11 §11.5). Called AFTER [nativeExportBegin] and BEFORE the first
     * video frame: the muxer requires all tracks before the start, and the audio format
     * is only known after the encoder's first output.
     *
     * Codes: `0` — accepted and mixed, `1` — the file has no audio track (an ordinary
     * case for video without sound, NOT an error), `-2`/`-3` — failure.
     */
    @JvmStatic
    external fun nativeExportAudioTrack(
        handle: Long,
        fd: Int,
        startMs: Long,
        durationMs: Long,
        gain: Float,
    ): Int

    @JvmStatic
    external fun nativeExportEnd(handle: Long): Int

    /**
     * The cause of the last export failure from Rust (codec/muxer/config) — the very
     * string nobody used to read. "" = the last step succeeded.
     */
    @JvmStatic
    external fun nativeExportLastError(): String

    // --- Video import/probe (rumo-media) ---
    @JvmStatic
    external fun nativeVideoProbeFd(fd: Int, offset: Long, length: Long): String

    @JvmStatic
    external fun nativeVideoOpenFd(fd: Int, offset: Long, length: Long): Long

    @JvmStatic
    external fun nativeVideoInfo(handle: Long): String

    @JvmStatic
    external fun nativeVideoFrameAt(handle: Long, timeMs: Long): ByteArray?

    @JvmStatic
    external fun nativeVideoTextureAt(handle: Long, timeMs: Long): Long

    @JvmStatic
    external fun nativeVideoClose(handle: Long)

    // --- Export presets/bitrate (rumo-export) ---
    @JvmStatic
    external fun nativeResolutionPresets(): String

    @JvmStatic
    external fun nativeBitrateFor(width: Int, height: Int, fps: Int): Int

    /**
     * Beat analysis in audio: the answer `{"ok":true,"bpm":…,"confidence":…,"beats":[…]}`.
     * Decoding (symphonia) and all the arithmetic are in Rust; only the file's bytes
     * come here, because Kotlin has nothing to parse the container with.
     */
    @JvmStatic
    external fun nativeAudioAnalyzeBeats(bytes: ByteArray): String
    external fun nativeAudioAnalyzeSpeech(bytes: ByteArray): String
    external fun nativeApplyEdl(projectJson: String, opsJson: String): String

    // --- SVG registry (rumo-render) ---
    /**
     * Parse and tessellate an SVG, put the geometry into the engine registry.
     * Returns an id for [ShapeSpec.svgId] / -1 on a parse error. The symbol is absent
     * in an old .so — the [svgRegister] wrapper then returns null.
     */
    @JvmStatic
    external fun nativeSvgRegister(bytes: ByteArray): Int

    /** Release SVG geometry by id. A repeated/foreign id is ignored by the engine. */
    @JvmStatic
    external fun nativeSvgRelease(id: Int)

    /**
     * SVG validation without registering: the answer
     * `{"ok":…,"error":…,"width":…,"height":…,"shapes":…,
     * "flattenedGradients":…,"skipped":…}`. Needed by the `svg_paint` tool
     * to reject a source before it is written.
     */
    @JvmStatic
    external fun nativeSvgValidate(bytes: ByteArray): String

    /**
     * Legacy fallback: rasterise the whole SVG (resvg), including what the vector path
     * cannot do. The answer is a flat `[width:u32 LE][height:u32 LE][rgba8…]` (straight
     * alpha), or null on an error/missing symbol. Wrapper — [svgRasterize].
     */
    @JvmStatic
    external fun nativeSvgRasterize(bytes: ByteArray, sizePx: Int): ByteArray?

    // --- Effect catalogue (rumo-render) ---
    @JvmStatic
    external fun nativeEffectCatalogue(): String

    /**
     * The catalogue plus the project's effects: customsJson is an array of CustomEffect
     * objects (see CustomEffects.encode) or an empty string. A separate symbol rather
     * than a parameter on nativeEffectCatalogue: otherwise an old .so would stop
     * resolving and the catalogue would disappear entirely.
     */
    @JvmStatic
    external fun nativeEffectCatalogueEx(customsJson: String): String

    /**
     * A quick check of one WGSL effect before it reaches a frame.
     * The answer: {"ok":true,…} or {"ok":false,"error":"…"}. No adapter is needed:
     * the check is parsing and validating the module (naga), without a driver.
     */
    @JvmStatic
    external fun nativeEffectValidate(json: String): String

    // --- Engine/Surface: rumo-rs has NO such symbols (verified by grepping for
    // Java_com_kerneldroid — only Version/EncodeProject/ShapeTris/ShapeMesh/
    // RenderPath/ProjectFromJson/ProjectToJson/DecodeImage/AudioDurationFd/
    // RenderPreview + UploadImage/FreeTexture/Layout*/Audio*/Export*).
    // Declared strictly per the spec; until the Rust agent adds them, any call
    // throws UnsatisfiedLinkError and the wrappers return a fallback (the engine path
    // is off, the bitmap path via rustFrame keeps working).
    @JvmStatic
    external fun nativeEngineCreate(): Long

    @JvmStatic
    external fun nativeEngineDestroy(engine: Long)

    @JvmStatic
    external fun nativeEngineSetLayers(
        engine: Long,
        width: Int,
        height: Int,
        bgArgb: Int,
        ordinals: IntArray,
        argbs: IntArray,
        dxs: FloatArray,
        dys: FloatArray,
        rotations: FloatArray,
        alphas: FloatArray,
    )

    @JvmStatic
    external fun nativeEngineSurfaceCreated(engine: Long, surface: Surface, width: Int, height: Int): Int

    @JvmStatic
    external fun nativeEngineSurfaceChanged(engine: Long, width: Int, height: Int)

    @JvmStatic
    external fun nativeEngineSurfaceDestroyed(engine: Long)

    @JvmStatic
    external fun nativeEngineRenderFrame(engine: Long): Int

    // --- Render-path diagnostics (rumo_bridge::render_diagnostics) ---
    // JSON; "" on failure (never null). For the format see the data class
    // [RenderDiagnostics]. While the Rust symbols are absent, the call throws
    // UnsatisfiedLinkError and the wrappers degrade to null.
    @JvmStatic
    external fun nativeRenderDiagnostics(): String

    @JvmStatic
    external fun nativeClearRenderDiagnostics()

    // --- Ex composite: SHAPE + text (layout handles) + images (texture ids).
    // Rust: rumo-render/src/jni.rs (nativeRenderPreviewEx /
    // nativeEngineSetLayersEx). Arrays of one group have one length; null = no layers.
    // effectsJson — effect chains per group of draw calls:
    // {"shapes":[[{id,kind,enabled,params}]],"textures":[[...]]}; "" = no
    // effects. timeMs is the playhead position for time-varying effects.
    //
    // Six new parallel arrays (docs/11 §11.4) are inserted between
    // texAlpha and effectsJson in exactly this order: a layer gained
    // `start_ms`, and Rust has to know the window of every draw call. The time
    // (startMs/durationMs) arrives as separate LongArrays rather than a flag, for
    // two reasons: an empty array reads as "time is unbounded"
    // (an old call does not break), and a short one — as "unbounded" for the
    // missing tails, because dropping a frame over a call defect is
    // worse than drawing extra. textScales is a FloatArray parallel to
    // the text records: the mesh is scaled around the centre of the box, while dx/dy
    // remain the top-left of the UNstretched box (§11.4.1).
    @JvmStatic
    external fun nativeRenderPreviewEx(
        width: Int,
        height: Int,
        bgArgb: Int,
        ordinals: IntArray?,
        argbs: IntArray?,
        dxs: FloatArray?,
        dys: FloatArray?,
        rotations: FloatArray?,
        alphas: FloatArray?,
        shapeScales: FloatArray?,
        textHandles: LongArray?,
        textX: FloatArray?,
        textY: FloatArray?,
        textArgb: IntArray?,
        textAlpha: FloatArray?,
        textRot: FloatArray?,
        texIds: LongArray?,
        texX: FloatArray?,
        texY: FloatArray?,
        texW: FloatArray?,
        texH: FloatArray?,
        texAlpha: FloatArray?,
        layerStarts: LongArray?,
        layerDurations: LongArray?,
        textStarts: LongArray?,
        textDurations: LongArray?,
        texStarts: LongArray?,
        texDurations: LongArray?,
        textScales: FloatArray?,
        // Global draw order: three arrays parallel to the three groups
        // (SHAPE, text, images). The values are only compared with each other
        // (a layer's position in the list), so an empty or short array = "no order"
        // → the engine draws by groups, as before the field appeared. Without this
        // an image, text and SVG could not pass over another group.
        layerOrders: IntArray?,
        textOrders: IntArray?,
        texOrders: IntArray?,
        effectsJson: String?,
        timeMs: Long,
    ): IntArray

    @JvmStatic
    external fun nativeEngineSetLayersEx(
        engine: Long,
        width: Int,
        height: Int,
        bgArgb: Int,
        ordinals: IntArray?,
        argbs: IntArray?,
        dxs: FloatArray?,
        dys: FloatArray?,
        rotations: FloatArray?,
        alphas: FloatArray?,
        shapeScales: FloatArray?,
        textHandles: LongArray?,
        textX: FloatArray?,
        textY: FloatArray?,
        textArgb: IntArray?,
        textAlpha: FloatArray?,
        textRot: FloatArray?,
        texIds: LongArray?,
        texX: FloatArray?,
        texY: FloatArray?,
        texW: FloatArray?,
        texH: FloatArray?,
        texAlpha: FloatArray?,
        layerStarts: LongArray?,
        layerDurations: LongArray?,
        textStarts: LongArray?,
        textDurations: LongArray?,
        texStarts: LongArray?,
        texDurations: LongArray?,
        textScales: FloatArray?,
        // See nativeRenderPreviewEx: global draw order across the three
        // groups, empty/short = group-wise drawing.
        layerOrders: IntArray?,
        textOrders: IntArray?,
        texOrders: IntArray?,
        effectsJson: String?,
        timeMs: Long,
    )

    data class DecodedImage(val width: Int, val height: Int, val rgba: ByteArray) {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is DecodedImage) return false
            return width == other.width && height == other.height && rgba.contentEquals(other.rgba)
        }

        override fun hashCode(): Int {
            var result = width
            result = 31 * result + height
            result = 31 * result + rgba.contentHashCode()
            return result
        }
    }

    fun engineVersion(): String =
        if (isLoaded()) {
            try {
                nativeVersion()
            } catch (_: Throwable) {
                "fallback"
            }
        } else {
            "fallback"
        }

    fun encodeProject(name: String): ByteArray? =
        if (isLoaded()) {
            try {
                nativeEncodeProject(name)
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    fun shapeTris(ordinal: Int): Int? =
        if (isLoaded()) {
            try {
                val v = nativeShapeTris(ordinal)
                if (v < 0) null else v
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    fun shapeMesh(ordinal: Int, sizePx: Float, rounding: Float, rotationDeg: Float): FloatArray? =
        if (isLoaded()) {
            try {
                val v = nativeShapeMesh(ordinal, sizePx, rounding, rotationDeg)
                if (v.isEmpty()) null else v
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    fun decodeImage(data: ByteArray, maxSide: Int = 1024): DecodedImage? {
        if (!isLoaded()) return null
        return try {
            // JNI returns null on an error — for a non-null ByteArray that
            // arrives as an NPE at the boundary, caught below and turned into null.
            flatImageToDecoded(nativeDecodeImage(data, maxSide))
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * Legacy fallback for SVG: rasterise the whole document (`resvg`) and return
     * its pixels. The flat answer `[width:u32 LE][height:u32 LE][rgba8…]` is the
     * same format as [DecodedImage] and [nativeDecodeImage], so the result
     * works both in [uploadImage]/[stageTexture] and as a PNG for an image layer.
     *
     * `sizePx` sets the **larger** side (see `svg_legacy` in Rust). Returns
     * null when the symbol is absent (an old .so), rasterisation failed or the answer
     * is corrupt; the caller is obliged to say exactly that rather than show emptiness.
     */
    fun svgRasterize(bytes: ByteArray, sizePx: Int): DecodedImage? {
        if (!isLoaded() || bytes.isEmpty() || sizePx <= 0) return null
        return try {
            flatImageToDecoded(nativeSvgRasterize(bytes, sizePx))
        } catch (_: UnsatisfiedLinkError) {
            null
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * Parsing the flat `[width:u32 LE][height:u32 LE][rgba8…]` into [DecodedImage].
     *
     * One parse for two inputs — an image and an SVG raster: their format is common, and
     * a second copy of the length check would diverge from the first at the first edit.
     */
    private fun flatImageToDecoded(flat: ByteArray?): DecodedImage? {
        if (flat == null || flat.size < 8) return null
        val w = (flat[0].toInt() and 0xFF) or
            ((flat[1].toInt() and 0xFF) shl 8) or
            ((flat[2].toInt() and 0xFF) shl 16) or
            ((flat[3].toInt() and 0xFF) shl 24)
        val h = (flat[4].toInt() and 0xFF) or
            ((flat[5].toInt() and 0xFF) shl 8) or
            ((flat[6].toInt() and 0xFF) shl 16) or
            ((flat[7].toInt() and 0xFF) shl 24)
        if (w <= 0 || h <= 0) return null
        if (flat.size.toLong() != 8L + w.toLong() * h.toLong() * 4L) return null
        return DecodedImage(w, h, flat.copyOfRange(8, flat.size))
    }

    fun audioDurationMs(fd: Int): Long? {
        if (!isLoaded()) return null
        return try {
            val v = nativeAudioDurationFd(fd)
            if (v < 0) null else v
        } catch (_: Throwable) {
            null
        }
    }

    data class PreviewCall(
        val width: Int,
        val height: Int,
        val bgArgb: Int,
        val ordinals: IntArray,
        val argbs: IntArray,
        val dxs: FloatArray,
        val dys: FloatArray,
        val rotations: FloatArray,
        val alphas: FloatArray,
        /**
         * Per-layer uniform SHAPE size multiplier (empty array = all
         * 1.0). Rust reads it leniently: missing/non-numeric entries
         * degrade to 1.0, so an old call keeps working.
         * The legacy `nativeRenderPreview` path does not take it and draws 1.0.
         */
        val shapeScales: FloatArray = floatArrayOf(),
    )

    /** A SHAPE frame from Rust (LE-RGBA pixels) or null. All arrays have one length. */
    fun renderPreview(call: PreviewCall): IntArray? =
        if (isLoaded()) {
            try {
                val px = nativeRenderPreview(
                    call.width, call.height, call.bgArgb,
                    call.ordinals, call.argbs, call.dxs, call.dys,
                    call.rotations, call.alphas,
                )
                if (px.size != call.width * call.height) null else px
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    /** "gpu" or "cpu" — which path rendered the last preview. */
    fun renderPath(): String =
        if (isLoaded()) {
            try {
                nativeRenderPath()
            } catch (_: Throwable) {
                "cpu"
            }
        } else {
            "cpu"
        }

    // --- Render-path diagnostics ---
    /** One native render log record ([level] — info|warn|error). */
    data class RenderLogEntry(
        val seq: Long,
        val level: String,
        val code: String,
        val text: String,
    )

    /**
     * The parsed nativeRenderDiagnostics() report: the last frame's path,
     * GPU adapter/backend, a short hint, the effect kinds whose shader
     * failed to compile, and log records (newest at the end). Empty adapter/backend
     * strings mean "no GPU device".
     */
    data class RenderDiagnostics(
        val path: String,
        val adapter: String,
        val backend: String,
        val hint: String,
        val rejectedEffects: List<String>,
        val entries: List<RenderLogEntry>,
        /**
         * The surface engine (live preview) drew the last frame on the GPU.
         * Separate from [previewOk]: the editor uses both paths, and "which one
         * exactly is on the CPU" is the main question when debugging.
         */
        val engineOk: Boolean = false,
        /** The offscreen preview (bitmap + export) drew the last frame on the GPU. */
        val previewOk: Boolean = false,
    )

    /**
     * The render diagnostics report; null = no JNI, an empty answer or the body did not
     * parse. Parsing is defensive: a missing key / unknown level /
     * unreadable JSON is not a crash but a default/skip.
     */
    fun renderDiagnostics(): RenderDiagnostics? {
        if (!isLoaded()) return null
        val json = try {
            nativeRenderDiagnostics()
        } catch (_: UnsatisfiedLinkError) {
            return null
        } catch (_: Throwable) {
            return null
        }
        if (json.isEmpty()) return null
        return try {
            val root = JSONObject(json)
            val entries = mutableListOf<RenderLogEntry>()
            root.optJSONArray("entries")?.let { arr ->
                for (i in 0 until arr.length()) {
                    val o = arr.optJSONObject(i) ?: continue
                    entries += RenderLogEntry(
                        seq = o.optLong("seq", 0L),
                        level = normalizeLevel(o.optString("level", "info")),
                        code = o.optString("code", ""),
                        text = o.optString("text", ""),
                    )
                }
            }
            val rejected = mutableListOf<String>()
            root.optJSONArray("rejectedEffects")?.let { arr ->
                for (i in 0 until arr.length()) {
                    val v = arr.optString(i, "")
                    if (v.isNotEmpty()) rejected += v
                }
            }
            RenderDiagnostics(
                path = normalizePath(root.optString("path", "")),
                adapter = root.optString("adapter", ""),
                backend = root.optString("backend", ""),
                hint = root.optString("hint", ""),
                rejectedEffects = rejected,
                entries = entries,
                engineOk = root.optBoolean("engineOk", false),
                previewOk = root.optBoolean("previewOk", false),
            )
        } catch (_: Throwable) {
            null
        }
    }

    /** Clear the native log; a no-op when JNI is absent. */
    fun clearRenderDiagnostics() {
        if (!isLoaded()) return
        try {
            nativeClearRenderDiagnostics()
        } catch (_: UnsatisfiedLinkError) {
        } catch (_: Throwable) {
        }
    }

    /**
     * "gpu"/"cpu" from the diagnostics, or null if the report is unavailable/did not
     * parse. Prefer [renderPath]: that one reflects the legacy bitmap function and
     * lies on the surface engine.
     */
    fun renderPathFromDiagnostics(): String? =
        renderDiagnostics()?.path?.takeIf { it.isNotEmpty() }

    /** "gpu"/"cpu" (lowercase) or "" for an unknown value. */
    private fun normalizePath(raw: String): String =
        when (raw.trim().lowercase()) {
            "gpu" -> "gpu"
            "cpu" -> "cpu"
            else -> ""
        }

    /** Exactly info|warn|error; an unknown value degrades to "info". */
    private fun normalizeLevel(raw: String): String =
        when (raw.trim().lowercase()) {
            "warn" -> "warn"
            "error" -> "error"
            else -> "info"
        }

    /** A full frame: SHAPE composition + text + images (the engine composites). */
    data class FrameEx(
        val call: PreviewCall,
        val textHandles: LongArray = longArrayOf(),
        val textX: FloatArray = floatArrayOf(),
        val textY: FloatArray = floatArrayOf(),
        val textArgb: IntArray = intArrayOf(),
        val textAlpha: FloatArray = floatArrayOf(),
        val textRot: FloatArray = floatArrayOf(),
        val texIds: LongArray = longArrayOf(),
        val texX: FloatArray = floatArrayOf(),
        val texY: FloatArray = floatArrayOf(),
        val texW: FloatArray = floatArrayOf(),
        val texH: FloatArray = floatArrayOf(),
        val texAlpha: FloatArray = floatArrayOf(),
        /**
         * The time window of each draw call of the SHAPE group, parallel to its
         * arrays (docs/11 §11.4). Rust discards a layer outside the window. An empty
         * array = "time is unbounded", a short one — the missing tails
         * read as unbounded.
         */
        val layerStarts: LongArray = longArrayOf(),
        val layerDurations: LongArray = longArrayOf(),
        /** The same for text records — one per handle (outline, then
         *  fill), otherwise the effect chains would diverge again. */
        val textStarts: LongArray = longArrayOf(),
        val textDurations: LongArray = longArrayOf(),
        /** The same for images, parallel to texIds/texX/…. */
        val texStarts: LongArray = longArrayOf(),
        val texDurations: LongArray = longArrayOf(),
        /**
         * The mesh multiplier of a text record around the centre of its box (§11.4.1).
         * Text layout is done once per point size, so it is the mesh that is moved by
         * the size, not the layout: the point size keeps the same font.
         */
        val textScales: FloatArray = floatArrayOf(),
        /**
         * The global draw order of the SHAPE group: the position of each of its
         * draw calls in the overall layer list (docs/14). Rust merges the three groups
         * (this one, [textOrders], [texOrders]) into one sequence and draws
         * by it, so an image can lie under text or an SVG — over it.
         * The values are only compared; an empty array = "no order" →
         * group-wise drawing, as before.
         */
        val layerOrders: IntArray = intArrayOf(),
        /** The same for text records: text with an outline has two records
         *  (outline, then fill) — two adjacent values, the outline first. */
        val textOrders: IntArray = intArrayOf(),
        /** The same for images, parallel to texIds/texX/…. */
        val texOrders: IntArray = intArrayOf(),
        /** Effect chains per group of draw calls; "" = no effects. */
        val effectsJson: String = "",
        /** The frame's playhead position in ms (for time-varying effects). */
        val timeMs: Long = 0L,
    )

    /** An Ex frame from Rust; on any error — the legacy SHAPE path (no text/images). */
    fun renderPreviewEx(f: FrameEx): IntArray? =
        if (isLoaded()) {
            try {
                val px = nativeRenderPreviewEx(
                    f.call.width, f.call.height, f.call.bgArgb,
                    f.call.ordinals, f.call.argbs, f.call.dxs, f.call.dys,
                    f.call.rotations, f.call.alphas, f.call.shapeScales,
                    f.textHandles, f.textX, f.textY, f.textArgb, f.textAlpha, f.textRot,
                    f.texIds, f.texX, f.texY, f.texW, f.texH, f.texAlpha,
                    f.layerStarts, f.layerDurations,
                    f.textStarts, f.textDurations,
                    f.texStarts, f.texDurations,
                    f.textScales,
                    f.layerOrders, f.textOrders, f.texOrders,
                    f.effectsJson, f.timeMs,
                )
                if (px.size != f.call.width * f.call.height) renderPreview(f.call) else px
            } catch (_: Throwable) {
                renderPreview(f.call)
            }
        } else {
            null
        }

    /**
     * One export frame: render and encode in one call, without an IntArray of
     * `width*height` in both directions across JNI (docs/12 §12.3).
     *
     * Returns the Rust code: `0` — the frame is encoded, negative — an error
     * (the cause is in [exportLastError]). Throws only when the symbol is absent from
     * the loaded `.so`; the export no longer has a fallback CPU route
     * (docs/12 §12.4), so both that and a negative code are an export failure.
     */
    fun exportWriteFrameGpu(handle: Long, f: FrameEx, ptsUs: Long): Int =
        nativeExportWriteFrameGpu(
            handle, f.call.width, f.call.height, f.call.bgArgb,
            f.call.ordinals, f.call.argbs, f.call.dxs, f.call.dys,
            f.call.rotations, f.call.alphas, f.call.shapeScales,
            f.textHandles, f.textX, f.textY, f.textArgb, f.textAlpha, f.textRot,
            f.texIds, f.texX, f.texY, f.texW, f.texH, f.texAlpha,
            f.layerStarts, f.layerDurations,
            f.textStarts, f.textDurations,
            f.texStarts, f.texDurations,
            f.textScales,
            f.layerOrders, f.textOrders, f.texOrders,
            f.effectsJson, f.timeMs, ptsUs,
        )

    fun projectFromJson(json: String): ByteArray? =
        if (isLoaded()) {
            try {
                nativeProjectFromJson(json)
            } catch (t: Throwable) {
                AppLog.error("project", "nativeProjectFromJson threw", t)
                null
            }
        } else {
            AppLog.error("project", "nativeProjectFromJson: librumo_bridge not loaded")
            null
        }

    fun projectToJson(bytes: ByteArray): String? =
        if (isLoaded()) {
            try {
                nativeProjectToJson(bytes)
            } catch (t: Throwable) {
                AppLog.error("project", "nativeProjectToJson threw", t)
                null
            }
        } else {
            AppLog.error("project", "nativeProjectToJson: librumo_bridge not loaded")
            null
        }

    // --- Wrappers: textures ---
    /** RGBA8 staging in Rust; id >= 1 or null (0/exception = error). */
    fun uploadImage(width: Int, height: Int, rgba: ByteArray): Long? =
        if (isLoaded()) {
            try {
                val id = nativeUploadImage(width, height, rgba)
                if (id == 0L) null else id
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    fun freeTexture(id: Long) {
        if (!isLoaded()) return
        try {
            nativeFreeTexture(id)
        } catch (_: UnsatisfiedLinkError) {
        } catch (_: Throwable) {
        }
    }

    // --- Wrappers: text ---
    /** A layout handle >= 1 or null. */
    fun layoutText(text: String, sizePx: Float): Long? =
        if (isLoaded()) {
            try {
                val h = nativeLayoutText(text, sizePx)
                if (h == 0L) null else h
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    /**
     * A layout handle with style, or null.
     *
     * Separate from [layoutText], not instead of it: this is a new entry point, and
     * the old layout stays what it was — otherwise already saved projects
     * would shift along with it.
     */
    fun layoutTextStyled(text: String, sizePx: Float, weight: Int, strokePx: Float): Long? =
        if (isLoaded()) {
            try {
                val h = nativeLayoutTextStyled(text, sizePx, weight, strokePx)
                if (h == 0L) null else h
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    /**
     * A layout with an explicit font family, or null.
     *
     * A separate entry point rather than a field in [layoutTextStyled]: the layout style
     * is `Copy` and sits in the hot path for every layer, and a `String` in it would force
     * copying it on every call and answering the question "which font?" where
     * there is no answer.
     */
    fun layoutTextFamily(
        text: String,
        sizePx: Float,
        weight: Int,
        strokePx: Float,
        family: String,
    ): Long? =
        if (isLoaded()) {
            try {
                val h = nativeLayoutTextFamily(text, sizePx, weight, strokePx, family)
                if (h == 0L) null else h
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    /**
     * Registers a font from bytes; returns the family name or null.
     *
     * Garbage bytes are an expected outcome of a network download, so this is "a name or
     * null" rather than an exception.
     */
    fun fontRegister(bytes: ByteArray): String? =
        if (isLoaded() && bytes.isNotEmpty()) {
            try {
                nativeFontRegister(bytes)?.takeIf { it.isNotEmpty() }
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    /**
     * A text preview in the given font, or null.
     *
     * The format is the same flat `[w][h][rgba]` as an image and an SVG raster —
     * the parse is shared with them ([flatImageToDecoded]), and a second copy of the
     * length check would diverge from the first at the first edit.
     */
    fun fontPreview(
        family: String,
        fontBytes: ByteArray,
        text: String,
        sizePx: Float,
        weight: Int,
        argb: Int,
        pad: Int,
    ): DecodedImage? {
        if (!isLoaded() || family.isEmpty() || text.isEmpty() || sizePx <= 0f) return null
        if (fontBytes.isEmpty()) return null
        return try {
            flatImageToDecoded(
                nativeFontPreview(family, fontBytes, text, sizePx, weight, argb, pad),
            )
        } catch (_: UnsatisfiedLinkError) {
            null
        } catch (_: Throwable) {
            null
        }
    }

    fun layoutQuadCount(handle: Long): Int =
        if (isLoaded()) {
            try {
                nativeLayoutQuadCount(handle)
            } catch (_: UnsatisfiedLinkError) {
                0
            } catch (_: Throwable) {
                0
            }
        } else {
            0
        }

    /** One quad [x,y,w,h,u0,v0,u1,v1] or null. */
    fun layoutQuad(handle: Long, index: Int): FloatArray? =
        if (isLoaded()) {
            try {
                nativeLayoutQuad(handle, index)
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    /** The layout's [width,height] or null. */
    fun layoutBounds(handle: Long): FloatArray? =
        if (isLoaded()) {
            try {
                nativeLayoutBounds(handle)
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    /** An atlas [w:u32 LE][h:u32 LE][rgba...] or null. */
    fun layoutAtlas(handle: Long): ByteArray? =
        if (isLoaded()) {
            try {
                nativeLayoutAtlas(handle)
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    fun layoutFree(handle: Long) {
        if (!isLoaded()) return
        try {
            nativeLayoutFree(handle)
        } catch (_: UnsatisfiedLinkError) {
        } catch (_: Throwable) {
        }
    }

    // --- Wrappers: audio player ---
    /** A player handle (>= 1) or null (0/exception = open failed). */
    fun audioOpen(fd: Int): Long? =
        if (isLoaded()) {
            try {
                val h = nativeAudioOpen(fd)
                if (h == 0L) null else h
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    /** The track duration in seconds, or null (< 0/exception = unknown). */
    fun audioDuration(handle: Long): Double? =
        if (isLoaded()) {
            try {
                val v = nativeAudioDuration(handle)
                if (v < 0.0) null else v
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    fun audioPlay(handle: Long) {
        if (!isLoaded()) return
        try {
            nativeAudioPlay(handle)
        } catch (_: UnsatisfiedLinkError) {
        } catch (_: Throwable) {
        }
    }

    fun audioPause(handle: Long) {
        if (!isLoaded()) return
        try {
            nativeAudioPause(handle)
        } catch (_: UnsatisfiedLinkError) {
        } catch (_: Throwable) {
        }
    }

    fun audioSeek(handle: Long, seconds: Double) {
        if (!isLoaded()) return
        try {
            nativeAudioSeek(handle, seconds)
        } catch (_: UnsatisfiedLinkError) {
        } catch (_: Throwable) {
        }
    }

    /** The position in seconds, or null (< 0/exception = unknown). Master clock. */
    fun audioPosition(handle: Long): Double? =
        if (isLoaded()) {
            try {
                val v = nativeAudioPosition(handle)
                if (v < 0.0) null else v
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    fun audioClose(handle: Long) {
        if (!isLoaded()) return
        try {
            nativeAudioClose(handle)
        } catch (_: UnsatisfiedLinkError) {
        } catch (_: Throwable) {
        }
    }

    // --- Wrappers: MP4 export ---
    /** An exporter handle (>= 1) or null. */
    fun exportBegin(outPath: String, width: Int, height: Int, fps: Float, bitrate: Int): Long? =
        if (isLoaded()) {
            try {
                val h = nativeExportBegin(outPath, width, height, fps, bitrate)
                if (h == 0L) null else h
            } catch (_: UnsatisfiedLinkError) {
                AppLog.error("export", "nativeExportBegin: JNI symbol missing")
                null
            } catch (t: Throwable) {
                AppLog.error("export", "nativeExportBegin threw", t)
                null
            }
        } else {
            AppLog.error("export", "nativeExportBegin: librumo_bridge not loaded")
            null
        }

    /**
     * Hand one audio source to the export's audio track (docs/11 §11.5).
     *
     * `0` — accepted, `1` — the file has no sound (an ordinary case, not an error),
     * negative — a source failure. `null` if the symbol is absent from this build of
     * `.so` or it threw: the caller is obliged to continue video-only.
     */
    fun exportAudioTrack(
        handle: Long,
        fd: Int,
        startMs: Long,
        durationMs: Long,
        gain: Float,
    ): Int? =
        if (isLoaded()) {
            try {
                nativeExportAudioTrack(handle, fd, startMs, durationMs, gain)
            } catch (_: UnsatisfiedLinkError) {
                // An old .so without the audio symbol: the video must be saved rather
                // than the export failing (as with any other missing symbol in this
                // file).
                AppLog.error("export", "nativeExportAudioTrack: JNI symbol missing")
                null
            } catch (t: Throwable) {
                AppLog.error("export", "nativeExportAudioTrack threw", t)
                null
            }
        } else {
            null
        }

    fun exportEnd(handle: Long): Int =
        if (isLoaded()) {
            try {
                nativeExportEnd(handle)
            } catch (_: UnsatisfiedLinkError) {
                AppLog.error("export", "nativeExportEnd: JNI symbol missing")
                Int.MIN_VALUE
            } catch (t: Throwable) {
                AppLog.error("export", "nativeExportEnd threw", t)
                Int.MIN_VALUE
            }
        } else {
            AppLog.error("export", "nativeExportEnd: librumo_bridge not loaded")
            Int.MIN_VALUE
        }

    /**
     * The real cause of the last export failure from Rust, "" when the cause is
     * unknown (no JNI, an empty answer or the last step succeeded).
     */
    fun exportLastError(): String =
        if (isLoaded()) {
            try {
                nativeExportLastError()
            } catch (_: UnsatisfiedLinkError) {
                ""
            } catch (_: Throwable) {
                ""
            }
        } else {
            ""
        }

    // --- Wrappers: engine (no Rust symbols yet — always a fallback) ---
    fun engineCreate(): Long? =
        if (isLoaded()) {
            try {
                val h = nativeEngineCreate()
                if (h == 0L) null else h
            } catch (_: UnsatisfiedLinkError) {
                AppLog.error("engine", "nativeEngineCreate: JNI symbol missing")
                null
            } catch (t: Throwable) {
                AppLog.error("engine", "nativeEngineCreate threw", t)
                null
            }
        } else {
            AppLog.error("engine", "nativeEngineCreate: librumo_bridge not loaded")
            null
        }

    fun engineDestroy(engine: Long): Boolean =
        if (isLoaded()) {
            try {
                nativeEngineDestroy(engine)
                true
            } catch (_: UnsatisfiedLinkError) {
                false
            } catch (_: Throwable) {
                false
            }
        } else {
            false
        }

    fun engineSetLayers(
        engine: Long,
        width: Int,
        height: Int,
        bgArgb: Int,
        ordinals: IntArray,
        argbs: IntArray,
        dxs: FloatArray,
        dys: FloatArray,
        rotations: FloatArray,
        alphas: FloatArray,
    ): Boolean =
        if (isLoaded()) {
            try {
                nativeEngineSetLayers(
                    engine, width, height, bgArgb,
                    ordinals, argbs, dxs, dys, rotations, alphas,
                )
                true
            } catch (_: UnsatisfiedLinkError) {
                false
            } catch (_: Throwable) {
                false
            }
        } else {
            false
        }

    /** An Ex layer push into the engine; on error — the legacy SHAPE push. */
    fun engineSetLayersEx(engine: Long, f: FrameEx): Boolean =
        if (isLoaded()) {
            try {
                nativeEngineSetLayersEx(
                    engine, f.call.width, f.call.height, f.call.bgArgb,
                    f.call.ordinals, f.call.argbs, f.call.dxs, f.call.dys,
                    f.call.rotations, f.call.alphas, f.call.shapeScales,
                    f.textHandles, f.textX, f.textY, f.textArgb, f.textAlpha, f.textRot,
                    f.texIds, f.texX, f.texY, f.texW, f.texH, f.texAlpha,
                    f.layerStarts, f.layerDurations,
                    f.textStarts, f.textDurations,
                    f.texStarts, f.texDurations,
                    f.textScales,
                    f.layerOrders, f.textOrders, f.texOrders,
                    f.effectsJson, f.timeMs,
                )
                true
            } catch (_: UnsatisfiedLinkError) {
                val c = f.call
                engineSetLayers(
                    engine, c.width, c.height, c.bgArgb,
                    c.ordinals, c.argbs, c.dxs, c.dys, c.rotations, c.alphas,
                )
            } catch (_: Throwable) {
                false
            }
        } else {
            false
        }

    /** 0 = OK, null = no JNI/exception. */
    fun engineSurfaceCreated(engine: Long, surface: Surface, width: Int, height: Int): Int? =
        if (isLoaded()) {
            try {
                nativeEngineSurfaceCreated(engine, surface, width, height)
            } catch (_: UnsatisfiedLinkError) {
                AppLog.error("engine", "nativeEngineSurfaceCreated: JNI symbol missing")
                null
            } catch (t: Throwable) {
                AppLog.error("engine", "nativeEngineSurfaceCreated threw", t)
                null
            }
        } else {
            AppLog.error("engine", "nativeEngineSurfaceCreated: librumo_bridge not loaded")
            null
        }

    fun engineSurfaceChanged(engine: Long, width: Int, height: Int): Boolean =
        if (isLoaded()) {
            try {
                nativeEngineSurfaceChanged(engine, width, height)
                true
            } catch (_: UnsatisfiedLinkError) {
                AppLog.error("engine", "nativeEngineSurfaceChanged: JNI symbol missing")
                false
            } catch (t: Throwable) {
                AppLog.error("engine", "nativeEngineSurfaceChanged threw", t)
                false
            }
        } else {
            AppLog.error("engine", "nativeEngineSurfaceChanged: librumo_bridge not loaded")
            false
        }

    fun engineSurfaceDestroyed(engine: Long): Boolean =
        if (isLoaded()) {
            try {
                nativeEngineSurfaceDestroyed(engine)
                true
            } catch (_: UnsatisfiedLinkError) {
                false
            } catch (_: Throwable) {
                false
            }
        } else {
            false
        }

    /** 0 = OK, null = no JNI/exception. */
    fun engineRenderFrame(engine: Long): Int? =
        if (isLoaded()) {
            try {
                nativeEngineRenderFrame(engine)
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    // --- Resolution/bitrate presets ---
    /** One export preset (id is stable, label is for the UI). */
    data class ResolutionPreset(
        val id: String,
        val label: String,
        val width: Int,
        val height: Int,
    )

    /** The project's aspect ratio ([w]:[h]); from nativeResolutionPresets(). */
    data class AspectPreset(
        val label: String,
        val w: Int,
        val h: Int,
    )

    /** The last resort: without a Rust catalogue the export must still work. */
    private const val DEFAULT_BITRATE = 4_000_000

    private val FallbackPresets: List<ResolutionPreset> = listOf(
        ResolutionPreset("480p", "480p", 854, 480),
        ResolutionPreset("480p_portrait", "480p Portrait", 480, 854),
        ResolutionPreset("720p", "720p", 1280, 720),
        ResolutionPreset("720p_portrait", "720p Portrait", 720, 1280),
        ResolutionPreset("1080p", "1080p", 1920, 1080),
        ResolutionPreset("1080p_portrait", "1080p Portrait", 1080, 1920),
        ResolutionPreset("1440p", "1440p", 2560, 1440),
        ResolutionPreset("1440p_portrait", "1440p Portrait", 1440, 2560),
        ResolutionPreset("2160p", "2160p", 3840, 2160),
        ResolutionPreset("2160p_portrait", "2160p Portrait", 2160, 3840),
    )

    private val FallbackFps: List<Int> = listOf(24, 25, 30, 50, 60)

    private val FallbackAspects: List<AspectPreset> = listOf(
        AspectPreset("16:9", 16, 9),
        AspectPreset("9:16", 9, 16),
    )

    /** The parsed catalogue: presets + aspects + fps. null = no Rust/failure. */
    private data class ResolutionCatalogue(
        val presets: List<ResolutionPreset>,
        val aspects: List<AspectPreset>,
        val fps: List<Int>,
    )

    private fun nativeResolutionCatalogue(): ResolutionCatalogue? {
        if (!isLoaded()) return null
        val json = try {
            nativeResolutionPresets()
        } catch (_: UnsatisfiedLinkError) {
            return null
        } catch (_: Throwable) {
            return null
        }
        if (json.isEmpty()) return null
        return try {
            val root = JSONObject(json)
            val presets = mutableListOf<ResolutionPreset>()
            root.optJSONArray("presets")?.let { arr ->
                for (i in 0 until arr.length()) {
                    val o = arr.optJSONObject(i) ?: continue
                    val w = o.optInt("width", 0)
                    val h = o.optInt("height", 0)
                    if (w <= 0 || h <= 0) continue
                    presets += ResolutionPreset(
                        id = o.optString("id", "${w}x$h"),
                        label = o.optString("label", "${h}p"),
                        width = w,
                        height = h,
                    )
                }
            }
            val aspects = mutableListOf<AspectPreset>()
            root.optJSONArray("aspects")?.let { arr ->
                for (i in 0 until arr.length()) {
                    val o = arr.optJSONObject(i) ?: continue
                    val w = o.optInt("w", 0)
                    val h = o.optInt("h", 0)
                    if (w <= 0 || h <= 0) continue
                    aspects += AspectPreset(o.optString("label", "$w:$h"), w, h)
                }
            }
            val fps = mutableListOf<Int>()
            root.optJSONArray("fps")?.let { arr ->
                for (i in 0 until arr.length()) {
                    val v = arr.optInt(i, 0)
                    if (v > 0) fps += v
                }
            }
            ResolutionCatalogue(presets, aspects, fps)
        } catch (_: Throwable) {
            null
        }
    }

    /** Resolution presets; on a failure/empty answer — the built-in list. */
    fun resolutionPresets(): List<ResolutionPreset> =
        nativeResolutionCatalogue()?.presets?.takeIf { it.isNotEmpty() } ?: FallbackPresets

    /** The fps options; on a failure — 24/25/30/50/60. */
    fun frameRateOptions(): List<Int> =
        nativeResolutionCatalogue()?.fps?.takeIf { it.isNotEmpty() } ?: FallbackFps

    /** The project's aspects; on a failure — 16:9 + 9:16. */
    fun aspectPresets(): List<AspectPreset> =
        nativeResolutionCatalogue()?.aspects?.takeIf { it.isNotEmpty() } ?: FallbackAspects

    /** The bitrate from Rust; a constant fallback on a failure or an invalid answer. */
    fun bitrateFor(width: Int, height: Int, fps: Int): Int =
        if (isLoaded()) {
            try {
                val v = nativeBitrateFor(width, height, fps)
                if (v > 0) v else DEFAULT_BITRATE
            } catch (_: UnsatisfiedLinkError) {
                DEFAULT_BITRATE
            } catch (_: Throwable) {
                DEFAULT_BITRATE
            }
        } else {
            DEFAULT_BITRATE
        }

    // --- Video: probe by fd + handle mode ---
    /** Video metadata (RGBA frames on demand). */
    data class VideoInfo(
        val width: Int,
        val height: Int,
        val durationMs: Long,
        /** Whether the container also holds a decodable audio track. */
        val hasAudio: Boolean = false,
        /**
         * Nominal frame rate; `0f` when the container does not report one.
         *
         * Needed to know how much time one frame occupies, so an instant that is
         * still showing the decoded frame is not decoded again.
         */
        val fps: Float = 0f,
    )

    private fun parseVideoInfo(json: String): VideoInfo? {
        if (json.isEmpty()) return null
        return try {
            val o = JSONObject(json)
            val w = firstPositiveInt(o, "width", "w")
            val h = firstPositiveInt(o, "height", "h")
            if (w <= 0 || h <= 0) return null
            VideoInfo(
                w,
                h,
                firstNonNegativeLong(o, "durationMs", "duration_ms", "duration"),
                // The probe emits these already; an older .so simply omits them,
                // which costs nothing but a missing sound layer and a missing
                // frame rate.
                o.optBoolean("hasAudio", false),
                o.optDouble("fps", 0.0).toFloat(),
            )
        } catch (_: Throwable) {
            null
        }
    }

    private fun firstPositiveInt(o: JSONObject, vararg keys: String): Int {
        for (k in keys) {
            val v = o.optInt(k, 0)
            if (v > 0) return v
        }
        return 0
    }

    private fun firstNonNegativeLong(o: JSONObject, vararg keys: String): Long {
        for (k in keys) {
            if (o.has(k)) return o.optLong(k, 0L).coerceAtLeast(0L)
        }
        return 0L
    }

    /** A video probe by fd without opening a decoder; null = Rust silent/failure. */
    /**
     * `length` of `-1L` means "to the end of the descriptor".
     *
     * Not `0`: `AMediaExtractor_setDataSourceFd` takes a *byte count*, so `0` is
     * an empty data source — no tracks, no metadata, a probe that can never
     * succeed and an open that can never return a handle. Both video call sites
     * passed `0` explicitly, which is why video produced nothing at all while
     * looking like a decoder problem, and why the parameter is now unreachable
     * from outside: changing a default does nothing to a caller that names the
     * argument.
     */
    fun videoInfoFor(fd: Int, offset: Long = 0L): VideoInfo? = videoInfoForRange(fd, offset, WHOLE_FILE)

    /**
     * The same probe with an explicit byte range. Not reachable by accident:
     * the public entry point above has no `length` parameter at all, so a
     * caller cannot pass `0` meaning "everything" — which is exactly what both
     * call sites did, and why video never opened a container.
     */
    private fun videoInfoForRange(fd: Int, offset: Long, length: Long): VideoInfo? {
        if (!isLoaded()) return null
        val json = try {
            nativeVideoProbeFd(fd, offset, length)
        } catch (_: UnsatisfiedLinkError) {
            AppLog.error(TAG, "videoInfoFor: JNI symbol missing")
            return null
        } catch (t: Throwable) {
            AppLog.error(TAG, "videoInfoFor threw", t)
            return null
        }
        if (json.isEmpty()) {
            AppLog.error(TAG, "videoInfoFor: probe returned nothing for fd=$fd")
            return null
        }
        val info = parseVideoInfo(json)
        if (info == null) {
            // The probe's own reason, when it has one. This is the line that was
            // missing: a failed probe used to disappear into a `null` and a
            // toast that said "will retry at render", forever.
            AppLog.error(TAG, "videoInfoFor: probe gave no usable info: $json")
        }
        return info
    }

    /** A decoder handle (>= 1) or null. */
    fun videoOpen(fd: Int, offset: Long = 0L): Long? = videoOpenRange(fd, offset, WHOLE_FILE)

    /** Range-taking open; see [`videoInfoForRange`] for why it is private. */
    private fun videoOpenRange(fd: Int, offset: Long, length: Long): Long? =
        if (isLoaded()) {
            try {
                val h = nativeVideoOpenFd(fd, offset, length)
                if (h == 0L) null else h
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    /** The info of an already-open handle; null on failure. */
    fun videoInfo(handle: Long): VideoInfo? =
        if (isLoaded()) {
            try {
                parseVideoInfo(nativeVideoInfo(handle))
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    /** An RGBA8 frame (w*h*4) at timeMs; null on failure. */
    fun videoFrameAt(handle: Long, timeMs: Long): ByteArray? =
        if (isLoaded()) {
            try {
                nativeVideoFrameAt(handle, timeMs)
            } catch (_: UnsatisfiedLinkError) {
                null
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }

    fun videoClose(handle: Long) {
        if (!isLoaded()) return
        try {
            nativeVideoClose(handle)
        } catch (_: UnsatisfiedLinkError) {
        } catch (_: Throwable) {
        }
    }

    /**
     * A video frame texture (id >= 1) for the `texIds` group, or 0 on failure.
     * The caller owns the id: release it via [freeTexture].
     */
    fun videoTextureAt(handle: Long, timeMs: Long): Long =
        if (isLoaded() && handle != 0L) {
            try {
                val id = nativeVideoTextureAt(handle, timeMs)
                if (id <= 0L) 0L else id
            } catch (_: UnsatisfiedLinkError) {
                0L
            } catch (_: Throwable) {
                0L
            }
        } else {
            0L
        }

    // --- Effect catalogue ---
    /** One effect parameter; min/max/default are in the slots (Color = 4 values). */
    data class EffectParam(
        val key: String,
        val label: String,
        val kind: String,
        val min: Float,
        val max: Float,
        val default: List<Float>,
        val choices: List<String>,
        val unit: String,
        /**
         * The parameter's offset in the shared `Effect.params` vector, as computed by
         * Rust (`"slot"` in the catalogue) — by the same traversal that owns the order.
         * It cannot be recomputed in Kotlin by summing `slots`: an error in the
         * arithmetic silently writes one parameter's value into another's slot,
         * and Rust only checks the vector's arity, not the intent.
         */
        val slot: Int,
    ) {
        /**
         * The number of f32 slots of the parameter. Scalars take one, `color` — four
         * (they are four separate fields `key_r/_g/_b/_a`, that is how the built-in
         * effects are built), while vector and matrix kinds take as many as they have
         * components. The list has to match `ParamKind` in Rust: diverging,
         * it would give the sliders the wrong values.
         */
        val slots: Int
            get() = when (kind.lowercase()) {
                "color" -> 4
                "vec2" -> 2
                "vec3" -> 3
                "vec4" -> 4
                "mat3" -> 9
                "mat4" -> 16
                else -> 1
            }

        /** The default values, length exactly [slots]. */
        fun defaultSlots(): List<Float> {
            val out = ArrayList<Float>(slots)
            for (i in 0 until slots) out += default.getOrElse(i) { 0f }
            return out
        }
    }

    /** The description of one effect kind from nativeEffectCatalogue(). */
    data class EffectDescriptor(
        val id: String,
        val label: String,
        val cost: String,
        val space: String,
        val slots: Int,
        val blockLen: Int,
        val passes: Int,
        val params: List<EffectParam>,
        /**
         * A project effect rather than a built-in one: its WGSL came from the user
         * (or from the assistant) and lives with the project. It reaches the catalogue
         * the same way, so the effects panel does not know the difference.
         */
        val custom: Boolean = false,
    )

    /** Parsing the nativeEffectValidate() answer. */
    data class EffectValidation(
        val ok: Boolean,
        val error: String,
        val slots: Int,
        val blockLen: Int,
        val passes: Int,
        val fields: List<String>,
    )

    /**
     * Check one project effect. Returns null if the symbol is absent
     * (an old .so) — then the effect can be neither accepted nor rejected meaningfully.
     */
    fun effectValidate(customsJson: String): EffectValidation? {
        if (!isLoaded()) return null
        val json = try {
            nativeEffectValidate(customsJson)
        } catch (_: UnsatisfiedLinkError) {
            return null
        } catch (_: Throwable) {
            return null
        }
        if (json.isEmpty()) return null
        return try {
            val root = JSONObject(json)
            val fields = mutableListOf<String>()
            root.optJSONArray("fields")?.let { fs ->
                for (i in 0 until fs.length()) fields += fs.optString(i, "")
            }
            EffectValidation(
                ok = root.optBoolean("ok", false),
                error = root.optString("error", ""),
                slots = root.optInt("slots", 0),
                blockLen = root.optInt("block_len", 0),
                passes = root.optInt("passes", 0),
                fields = fields,
            )
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * Beat analysis in an audio file. Returns the engine's raw JSON, or null if the
     * symbol is absent (an old .so) or the call failed.
     */
    fun audioAnalyzeBeats(bytes: ByteArray): String? {
        if (!isLoaded() || bytes.isEmpty()) return null
        return try {
            nativeAudioAnalyzeBeats(bytes)
        } catch (_: UnsatisfiedLinkError) {
            null
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * Speech and pauses in an audio file: speech segments, silence gaps and points where
     * a cut is possible. Returns the engine's raw JSON, or null if the symbol is absent
     * (an old .so) or the call failed.
     *
     * Separate from [audioAnalyzeBeats], although both listen to the same file:
     * beats are about rhythm, speech is about where a phrase ends, and all they share
     * is the decoding path.
     */
    fun audioAnalyzeSpeech(bytes: ByteArray): String? {
        if (!isLoaded() || bytes.isEmpty()) return null
        return try {
            nativeAudioAnalyzeSpeech(bytes)
        } catch (_: UnsatisfiedLinkError) {
            null
        } catch (_: Throwable) {
            null
        }
    }

    /** Parsing the [svgValidate] answer. */
    data class SvgValidation(
        val ok: Boolean,
        val error: String,
        val width: Float,
        val height: Float,
        val shapes: Int,
        /** How many gradients were flattened to a single colour: the colour is still
         *  visible, but the transition is not, and that has to be named, not hushed up. */
        val flattenedGradients: Int,
        /** How many parsed elements are unsupported. The field may arrive as a number
         *  or as a list of reasons — it is counted either way. */
        val skipped: Int,
    )

    /**
     * Check an SVG source without registering anything. Returns null if the
     * symbol is absent (an old .so) or the call failed — then there is nothing to judge
     * the SVG's suitability by, and the tool is obliged to say exactly that.
     */
    fun svgValidate(bytes: ByteArray): SvgValidation? {
        if (!isLoaded() || bytes.isEmpty()) return null
        val json = try {
            nativeSvgValidate(bytes)
        } catch (_: UnsatisfiedLinkError) {
            return null
        } catch (_: Throwable) {
            return null
        }
        if (json.isEmpty()) return null
        return try {
            val root = JSONObject(json)
            SvgValidation(
                ok = root.optBoolean("ok", false),
                error = root.optString("error", ""),
                width = root.optDouble("width", 0.0).toFloat(),
                height = root.optDouble("height", 0.0).toFloat(),
                shapes = root.optInt("shapes", 0),
                flattenedGradients = root.optInt("flattenedGradients", 0),
                skipped = countOrArray(root, "skipped"),
            )
        } catch (_: Throwable) {
            null
        }
    }

    /** `skipped` as a number or as a list of reasons: both answers are valid. */
    private fun countOrArray(root: JSONObject, key: String): Int {
        val v = root.opt(key)
        return when {
            v is JSONArray -> v.length()
            v is Number -> v.toInt()
            else -> 0
        }
    }

    /**
     * Register an SVG in the engine. Returns an id, or null if the symbol is
     * absent (an old .so) or the parse failed. `-1` (a parse failure) is also
     * mapped to null: what matters to the caller is "draws or not".
     */
    fun svgRegister(bytes: ByteArray): Int? {
        if (!isLoaded() || bytes.isEmpty()) return null
        val id = try {
            nativeSvgRegister(bytes)
        } catch (_: UnsatisfiedLinkError) {
            return null
        } catch (_: Throwable) {
            return null
        }
        return id.takeIf { it >= 0 }
    }

    /** Release the SVG geometry; silently does nothing on an old .so. */
    fun svgRelease(id: Int) {
        if (!isLoaded() || id < 0) return
        try {
            nativeSvgRelease(id)
        } catch (_: UnsatisfiedLinkError) {
        } catch (_: Throwable) {
        }
    }

    /**
     * The raster ceiling by the larger side: 4096² = 16 Mi-px — exactly
     * `MAX_RASTER_PIXELS` in Rust. Asking for more is not allowed (the engine returns
     * null), while a square at that edge is still permissible, so 4096 is the top ask.
     */
    const val SVG_RASTER_MAX_SIDE = 4096

    /**
     * Apply a batch of edits to the project in one call.
     *
     * Returns the new project's JSON, or `{"ok":false,...}` on a parse
     * error, or null if the symbol is absent (an old .so). Individual operations
     * that did not apply do not drop the batch — the engine reports them in `skipped`.
     */
    fun applyEdl(projectJson: String, opsJson: String): String? {
        if (!isLoaded()) return null
        return try {
            nativeApplyEdl(projectJson, opsJson)
        } catch (_: UnsatisfiedLinkError) {
            null
        } catch (_: Throwable) {
            null
        }
    }

    /** The parsed effect catalogue; an empty list = Rust silent/failure. */
    fun effectCatalogue(customsJson: String = ""): List<EffectDescriptor> {
        if (!isLoaded()) return emptyList()
        val json = try {
            nativeEffectCatalogueEx(customsJson)
        } catch (_: UnsatisfiedLinkError) {
            // An old .so without *Ex: the built-in effects catalogue is still needed.
            try {
                nativeEffectCatalogue()
            } catch (_: Throwable) {
                return emptyList()
            }
        } catch (_: Throwable) {
            return emptyList()
        }
        if (json.isEmpty()) return emptyList()
        return try {
            val root = JSONObject(json)
            val arr = root.optJSONArray("effects") ?: JSONArray()
            val out = mutableListOf<EffectDescriptor>()
            for (i in 0 until arr.length()) {
                val o = arr.optJSONObject(i) ?: continue
                val id = o.optString("id", "")
                if (id.isEmpty()) continue
                val params = mutableListOf<EffectParam>()
                // The offset in the shared vector that the fallback below uses.
                var runningSlot = 0
                o.optJSONArray("params")?.let { ps ->
                    for (j in 0 until ps.length()) {
                        val p = ps.optJSONObject(j) ?: continue
                        val key = p.optString("key", "")
                        if (key.isEmpty()) continue
                        val kind = p.optString("kind", "Float")
                        val paramSlots = if (kind.equals("color", ignoreCase = true)) 4 else 1
                        val defaults = mutableListOf<Float>()
                        p.optJSONArray("default")?.let { ds ->
                            for (k in 0 until ds.length()) {
                                defaults += ds.optDouble(k, 0.0).toFloat()
                            }
                        }
                        val choices = mutableListOf<String>()
                        p.optJSONArray("choices")?.let { cs ->
                            for (k in 0 until cs.length()) choices += cs.optString(k, "")
                        }
                        // FALLBACK: an old .so without the "slot" key — compute
                        // the offset as a running sum of slots (the parameter order
                        // in the catalogue = the slot order, so the sum matches).
                        val slot = if (p.has("slot")) {
                            p.optInt("slot", runningSlot)
                        } else {
                            runningSlot
                        }
                        params += EffectParam(
                            key = key,
                            label = p.optString("label", key),
                            kind = kind,
                            min = p.optDouble("min", 0.0).toFloat(),
                            max = p.optDouble("max", 0.0).toFloat(),
                            default = defaults,
                            choices = choices,
                            unit = p.optString("unit", ""),
                            slot = slot,
                        )
                        runningSlot = slot + paramSlots
                    }
                }
                val descriptor = EffectDescriptor(
                    id = id,
                    label = o.optString("label", id),
                    cost = o.optString("cost", ""),
                    space = o.optString("space", ""),
                    slots = o.optInt("slots", params.sumOf { it.slots }),
                    // The key arrives from Rust as "block_len"; "blockLen" is the
                    // old variant, kept for compatibility.
                    blockLen = if (o.has("blockLen")) o.optInt("blockLen", 0) else o.optInt("block_len", 0),
                    passes = o.optInt("passes", 0),
                    params = params,
                    custom = o.optBoolean("custom", false),
                )
                // Consistency: a parameter that sticks out past slots (or has a
                // negative offset) makes the descriptor unusable.
                // We skip it silently — no logs, no crashes; the UI shows what is
                // left (in the worst case an empty catalogue).
                if (descriptor.params.any {
                        it.slot < 0 || it.slot + it.slots > descriptor.slots
                    }
                ) {
                    continue
                }
                out += descriptor
            }
            out
        } catch (_: Throwable) {
            emptyList()
        }
    }

    /** The default parameter vector for an effect kind, length exactly slots. */
    fun defaultEffectParams(kindId: String, customsJson: String = ""): List<Float>? {
        val d = effectCatalogue(customsJson).firstOrNull { it.id == kindId } ?: return null
        val slots = d.slots
        // Zero/broken slots = an unusable descriptor: an effect with no parameters
        // cannot be added (the engine expects exactly slots values).
        if (slots <= 0) return null
        // The values are laid out into their own slots rather than concatenated in a row:
        // concatenation gives exactly slots only with contiguous offsets
        // (see EffectParam.slot), and here the "size == slots" invariant is mandatory.
        val out = MutableList(slots) { 0f }
        for (p in d.params) {
            val defaults = p.defaultSlots()
            for (i in defaults.indices) {
                val idx = p.slot + i
                if (idx in 0 until slots) out[idx] = defaults[i]
            }
        }
        return out
    }

    /**
     * The "whole descriptor" for `AMediaExtractor_setDataSourceFd`, which
     * takes a **byte count**, not an end-of-file marker. `-1` is the only
     * value meaning "to the end"; `0` is an empty source.
     *
     * Declared here rather than in `companion`, because `RumboBridge` is itself an object.
     */
    private const val WHOLE_FILE = -1L

    private const val TAG = "bridge"
}
