// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import android.content.Context
import android.net.Uri
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * MP4 export through Rust (nativeExportBegin/WriteFrameGpu|WriteFrameArgb/End).
 *
 * Every frame is drawn and encoded with **one** call to
 * [RumoBridge.exportWriteFrameGpu]: the engine composites the scene into an
 * offscreen target, a compute pass packs it into NV12 on the GPU, and the bytes
 * go to the encoder — the frame never becomes a Bitmap, an IntArray or an RGBA
 * repack on the CPU (docs/12 §12.3). Frames are taken frame by frame at the fps,
 * fully offscreen, with no screen capture and no Compose.
 *
 * There is no fallback CPU route (docs/12 §12.4): if the GPU could not assemble
 * a frame, that is an export failure with a reason in
 * [RumoBridge.exportLastError], not a file assembled by another renderer. That
 * is more honest: two renderers are two pictures, and the second was never the
 * one the user sees.
 *
 * There is deliberately no overlay: the engine composites SHAPE+TEXT+MEDIA
 * itself, and drawing over the frame would have required exactly the copies that
 * wave 2 got rid of.
 */
object Exporter {
    const val FPS = 30f
    const val BITRATE = 4_000_000

    private const val TAG = "export"

    /**
     * The reason for the last [exportMp4] failure: a string from Rust
     * ([RumoBridge.exportLastError] — codec/muxer/config) or a local step
     * reason. "" = the last export did not fail. Not thread-safe as a
     * "history": read it right after one export finishes.
     */
    @Volatile
    var lastError: String = ""
        private set

    /** The single point where a failure is recorded: the reason goes both to [lastError] and to AppLog. */
    private fun fail(message: String) {
        lastError = message
        AppLog.error(TAG, message)
    }

    /** ": <reason from Rust>" or "" — when the native side is silent. */
    private fun nativeSuffix(): String =
        RumoBridge.exportLastError().let { if (it.isEmpty()) "" else ": $it" }

    /**
     * One audio source for [exportMp4]: a project file plus its window on the
     * timeline. `gain` is the source volume multiplier (1.0 = as is).
     */
    data class AudioSource(
        val uri: String,
        val startMs: Long,
        val durationMs: Long,
        val gain: Float = 1f,
    )

    /**
     * Hand all the audio sources to the export audio track (docs/11 §11.5).
     *
     * Called AFTER `nativeExportBegin` and BEFORE the first video frame: the
     * muxer needs all the tracks before the start, and the audio format is known
     * only after the encoder's first output. Returns how many tracks were
     * accepted.
     *
     * A source that failed does NOT bring the export down: losing the video
     * because of one broken file is worse than releasing it without that piece
     * of audio. Code `1` — "the file has no audio track" — is the usual thing
     * for a video without sound and is not an error at all, so it does not get
     * into `lastError`.
     */
    private suspend fun feedAudio(
        handle: Long,
        context: Context,
        sources: List<AudioSource>,
    ): Int {
        var added = 0
        for (source in sources) {
            val uri = Uri.parse(source.uri)
            val pfd = try {
                context.contentResolver.openFileDescriptor(uri, "r")
            } catch (t: Throwable) {
                // No permission, the file was deleted, the provider refused —
                // we skip the source, the export continues.
                AppLog.error(TAG, "audio: cannot open $source.uri: ${AppLog.describe(t)}")
                null
            }
            if (pfd == null) {
                AppLog.error(TAG, "audio: no descriptor for $source.uri")
                continue
            }
            val rc = try {
                RumoBridge.exportAudioTrack(
                    handle = handle,
                    fd = pfd.fd,
                    startMs = source.startMs.coerceAtLeast(0L),
                    durationMs = source.durationMs.coerceAtLeast(0L),
                    gain = source.gain,
                )
            } catch (t: Throwable) {
                // The symbol may be missing from this build of the .so — then
                // the video must still be saved without audio, rather than the
                // export crashing.
                AppLog.error(TAG, "audio: ${source.uri} threw: ${AppLog.describe(t)}")
                null
            } finally {
                try {
                    pfd.close()
                } catch (_: Throwable) {
                    // Closing the descriptor saves nothing and breaks nothing.
                }
            }
            when {
                rc == null -> AppLog.error(TAG, "audio: ${source.uri} skipped, no audio in this export")
                rc == 0 -> {
                    added++
                    AppLog.info(TAG, "audio: ${source.uri} @${source.startMs}ms +${source.durationMs}ms")
                }
                // "The file has no audio track" is not a failure but a property
                // of the file: a silent video without sound. It does not go into
                // lastError.
                rc == 1 -> AppLog.info(TAG, "audio: ${source.uri} has no audio track")
                else -> AppLog.error(TAG, "audio: ${source.uri} rc=$rc${nativeSuffix()}")
            }
        }
        return added
    }

    /**
     * @param outPath full path of the file (usually cacheDir; the move to
     *   Download/Rumo is done by the UI via saveMp4ToDownloads).
     * @param frameAt the full Ex frame at timeMs (the engine composites
     *   SHAPE+TEXT+MEDIA itself).
     * @param onProgress 0..1, called from an IO thread.
     * @return true — the file is finalised (nativeExportEnd returned 0).
     */
    suspend fun exportMp4(
        outPath: String,
        width: Int = 512,
        height: Int = 288,
        fps: Float = FPS,
        bitrate: Int = BITRATE,
        durationMs: Long,
        frameAt: (timeMs: Long) -> RumoBridge.FrameEx,
        onProgress: (Float) -> Unit = {},
        // null = there is no audio in the project; without it the export runs video-only.
        audioContext: Context? = null,
        audioSources: List<AudioSource> = emptyList(),
    ): Boolean = withContext(Dispatchers.IO) {
        lastError = ""
        if (width <= 0 || height <= 0 || durationMs <= 0L) {
            fail("invalid export config: ${width}x$height duration=${durationMs}ms")
            return@withContext false
        }
        val handle = RumoBridge.exportBegin(outPath, width, height, fps, bitrate)
        if (handle == null) {
            fail(
                buildString {
                    append("exportBegin failed")
                    append(nativeSuffix())
                    if (!RumoBridge.isLoaded()) append(" (librumo_bridge not loaded)")
                },
            )
            return@withContext false
        }
        val fpsLong = fps.toLong().coerceAtLeast(1L)
        val totalFrames = ((durationMs * fps / 1000f).toInt()).coerceAtLeast(1)
        AppLog.info(
            TAG,
            "begin $outPath ${width}x$height @${fpsLong}fps bitrate=$bitrate frames=$totalFrames",
        )
        var ok = false
        try {
            // Audio is fed before the first video frame and inside the same
            // try: the muxer cannot be started until all the tracks are known,
            // and a source failure must not bring the video down (docs/11 §11.5).
            if (audioContext != null && audioSources.isNotEmpty()) {
                feedAudio(handle, audioContext, audioSources)
            }
            for (i in 0 until totalFrames) {
                val t = ((i * 1000L) / fpsLong).coerceAtMost(durationMs)
                val ptsUs = i * 1_000_000L / fpsLong
                val frame = frameAt(t)
                // The frame is drawn and packed into NV12 on the GPU and goes
                // to the encoder without becoming an IntArray (docs/12 §12.3).
                // There is no fallback CPU route: if the GPU could not assemble
                // a frame, the export stops with a reason rather than writing a
                // file from another renderer's frames (docs/12 §12.4).
                val rc = try {
                    RumoBridge.exportWriteFrameGpu(handle, frame, ptsUs)
                } catch (err: Throwable) {
                    AppLog.error(TAG, "frame $i: gpu path threw: ${AppLog.describe(err)}")
                    fail("frame $i/$totalFrames: exportWriteFrameGpu threw: ${AppLog.describe(err)}")
                    return@withContext false
                }
                if (rc != 0) {
                    fail("frame $i/$totalFrames: exportWriteFrameGpu rc=$rc${nativeSuffix()}")
                    return@withContext false
                }
                onProgress((i + 1).toFloat() / totalFrames.toFloat())
            }
            ok = true
        } finally {
            // Finalise the MP4 even on a break: the handle is taken off the Rust registry.
            val rc = RumoBridge.exportEnd(handle)
            if (rc != 0) {
                ok = false
                fail("exportEnd rc=$rc${nativeSuffix()}")
            }
        }
        if (ok) AppLog.info(TAG, "ok $outPath frames=$totalFrames")
        ok
    }
}
