// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.content.Context
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import androidx.activity.compose.ManagedActivityResultLauncher
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalContext
import com.kerneldroid.rumo.data.AppLog
import com.kerneldroid.rumo.data.RumoBridge
import java.io.ByteArrayOutputStream
import java.io.File

// Three launchers with no permissions in the manifest:
// - PickVisualMedia (photos, ImageOnly) — the system photo picker;
// - GetContent("audio/*") — the system file picker;
// - PickVisualMedia (video, VideoOnly) — the system photo picker.
// None of them require permissions.
data class MediaPickers(
    val image: ManagedActivityResultLauncher<PickVisualMediaRequest, Uri?>,
    val audio: ManagedActivityResultLauncher<String, Uri?>,
    val video: ManagedActivityResultLauncher<PickVisualMediaRequest, Uri?>,
) {
    fun launchImage() {
        image.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
    }

    fun launchAudio() {
        audio.launch("audio/*")
    }

    fun launchVideo() {
        video.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.VideoOnly))
    }
}

@Composable
fun rememberMediaPicker(onPicked: (uri: Uri, mime: String?) -> Unit): MediaPickers {
    val context = LocalContext.current
    val imageLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri: Uri? ->
        if (uri != null) {
            onPicked(uri, context.contentResolver.getType(uri))
        }
    }
    val audioLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent(),
    ) { uri: Uri? ->
        if (uri != null) {
            onPicked(uri, context.contentResolver.getType(uri))
        }
    }
    val videoLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri: Uri? ->
        if (uri != null) {
            onPicked(uri, context.contentResolver.getType(uri))
        }
    }
    return MediaPickers(image = imageLauncher, audio = audioLauncher, video = videoLauncher)
}

// File name from MediaStore/provider; error → fallback.
fun queryDisplayName(context: Context, uri: Uri, fallback: String): String {
    return try {
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                cursor.getString(0)?.takeIf { it.isNotBlank() } ?: fallback
            } else {
                fallback
            }
        } ?: fallback
    } catch (_: Exception) {
        fallback
    }
}

// Audio duration through Rust (symphonia probe by fd); error/unknown → 5000L.
fun audioDurationMs(context: Context, uri: Uri): Long {
    // pfd.fd is passed into the native call while pfd is kept open via use{}
    // (no detachFd): Rust reads /proc/self/fd/<fd>, ownership stays with
    // ParcelFileDescriptor and it is closed right here.
    return try {
        context.contentResolver.openFileDescriptor(uri, "r")?.use { pfd ->
            RumoBridge.audioDurationMs(pfd.fd)
        } ?: EditorState.DEFAULT_MIN_DURATION_MS
    } catch (_: Exception) {
        EditorState.DEFAULT_MIN_DURATION_MS
    }
}

// Video probe through Rust (width/height/durationMs by fd); error/unknown → null.
// pfd lives only for the duration of the probe (use{} → close), as in audioDurationMs.
fun videoInfo(context: Context, uri: Uri, localPath: String? = null): RumoBridge.VideoInfo? {
    // A real file the app itself wrote is preferred, and this is not a
    // micro-optimisation: `AMediaExtractor_setDataSourceFd` takes a seekable
    // descriptor and still answers `AMEDIA_ERROR_UNKNOWN` for the one a
    // `content://` provider hands out, while a descriptor opened from a path is
    // the canonical case. The content descriptor stays the fallback, so a file
    // too large to copy is still probed.
    videoInfoFromFile(localPath)?.let { return it }
    return try {
        context.contentResolver.openFileDescriptor(uri, "r")?.use { pfd ->
            // Only the offset is passed: the length defaults to WHOLE_FILE, and
            // `0L` here is an *empty file*, not "the whole thing". Naming the
            // default explicitly is the point — see RumoBridge.WHOLE_FILE.
            RumoBridge.videoInfoFor(pfd.fd, 0L)
        }
    } catch (t: Exception) {
        AppLog.warn(MEDIA_TAG, "video probe failed for $uri: ${t.message}")
        null
    }
}

/** The same probe on a real file, or null when there is no such file. */
fun videoInfoFromFile(absolutePath: String?): RumoBridge.VideoInfo? {
    if (absolutePath.isNullOrEmpty()) return null
    val file = File(absolutePath)
    if (!file.isFile) return null
    return try {
        ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY).use { pfd ->
            RumoBridge.videoInfoFor(pfd.fd, 0L)
        }
    } catch (t: Exception) {
        AppLog.warn(MEDIA_TAG, "video probe failed for $absolutePath: ${t.message}")
        null
    }
}

/**
 * `/storage/emulated/0/` + a path of the form `Download/Rumo/<project>/<file>`,
 * which is how [ProjectAssets] records what it wrote.
 *
 * `Environment.getExternalStorageDirectory` rather than a MediaStore query: on
 * modern Android the `_data` column is unreadable, and this file is one **this
 * app** wrote, so it knows where it is.
 */
fun externalStorageFile(relativePath: String): File? {
    if (relativePath.isEmpty()) return null
    if (relativePath.startsWith("/")) return File(relativePath).takeIf { it.isFile }
    val root = android.os.Environment.getExternalStorageDirectory() ?: return null
    return File(root, relativePath).takeIf { it.isFile }
}

private const val MEDIA_TAG = "media"

// uri bytes with a limit; exceeding the limit or an error → null.
fun readUriBytes(context: Context, uri: Uri, maxBytes: Int = 32 * 1024 * 1024): ByteArray? {
    return try {
        context.contentResolver.openInputStream(uri)?.use { stream ->
            val out = ByteArrayOutputStream()
            val buf = ByteArray(8192)
            var total = 0
            while (true) {
                val n = stream.read(buf)
                if (n < 0) break
                total += n
                if (total > maxBytes) return null
                out.write(buf, 0, n)
            }
            out.toByteArray()
        }
    } catch (_: Exception) {
        null
    }
}
