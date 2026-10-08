// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import androidx.annotation.RequiresApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import java.io.File
import java.io.FileOutputStream
import java.io.OutputStream
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.AppLog
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.ui.theme.RumoSpacing

// MP4 file name: rumo_YYYYMMDD_HHMMSS.mp4.
fun exportMp4FileName(now: Date = Date()): String {
    val fmt = SimpleDateFormat("yyyyMMdd_HHmmss", Locale.US)
    return "rumo_${fmt.format(now)}.mp4"
}

/** Export settings chosen by the user (W×H already accounts for orientation). */
data class ExportSettings(
    val width: Int,
    val height: Int,
    val fps: Int,
)

/**
 * The "export settings" step before writing the MP4: resolution + frame rate.
 * The presets come from [RumoBridge.resolutionPresets]; if Rust hands back only
 * landscape variants while the project's aspects know about portrait, the
 * portrait variants are added by swapping width/height. The resulting size is
 * shown as a line of text.
 */
@Composable
fun ExportSettingsDialog(
    presets: List<RumoBridge.ResolutionPreset>,
    fpsOptions: List<Int>,
    portraitAvailable: Boolean,
    onDismiss: () -> Unit,
    onConfirm: (ExportSettings) -> Unit,
) {
    // Read outside `remember`: `stringResource` is a composable call, and the
    // suffix has to be part of the remembered options again when the language changes.
    val portraitSuffix = stringResource(R.string.editor_export_portrait_suffix)
    val options = remember(presets, portraitAvailable, portraitSuffix) {
        if (portraitAvailable && presets.none { it.height > it.width }) {
            presets + presets.map {
                it.copy(
                    id = "${it.id}_portrait",
                    label = it.label + portraitSuffix,
                    width = it.height,
                    height = it.width,
                )
            }
        } else {
            presets
        }
    }
    var selectedId by remember(options) {
        mutableStateOf(
            options.firstOrNull { it.width == 1920 && it.height == 1080 }?.id
                ?: options.firstOrNull()?.id,
        )
    }
    var fps by remember(fpsOptions) {
        mutableStateOf(fpsOptions.firstOrNull { it == 30 } ?: fpsOptions.firstOrNull() ?: 30)
    }
    val selected = options.firstOrNull { it.id == selectedId }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.editor_export_mp4)) },
        text = {
            Column(modifier = Modifier.fillMaxWidth()) {
                Text(
                    text = stringResource(R.string.editor_export_resolution),
                    style = MaterialTheme.typography.labelLarge,
                )
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(max = 260.dp)
                        .verticalScroll(rememberScrollState()),
                ) {
                    for (p in options) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { selectedId = p.id },
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(
                                selected = p.id == selectedId,
                                onClick = { selectedId = p.id },
                            )
                            Text(
                                text = "${p.label} · ${p.width}×${p.height}",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        }
                    }
                }
                Text(
                    text = stringResource(R.string.editor_export_frame_rate),
                    style = MaterialTheme.typography.labelLarge,
                    modifier = Modifier.padding(top = RumoSpacing.s),
                )
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                ) {
                    for (f in fpsOptions) {
                        TextButton(onClick = { fps = f }) {
                            Text(
                                text = "$f",
                                color = if (f == fps) {
                                    MaterialTheme.colorScheme.primary
                                } else {
                                    MaterialTheme.colorScheme.onSurfaceVariant
                                },
                            )
                        }
                    }
                }
                Text(
                    text = selected?.let {
                        stringResource(R.string.editor_export_summary, it.width, it.height, fps)
                    } ?: "—",
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(top = RumoSpacing.s),
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    val p = selected ?: return@TextButton
                    onConfirm(ExportSettings(p.width, p.height, fps))
                },
                enabled = selected != null,
            ) {
                Text(stringResource(R.string.editor_export_action))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.editor_cancel))
            }
        },
    )
}

// --- Writing to Download/Rumo ---
// One path for the MP4 and the text logs: API 29+ — MediaStore (RELATIVE_PATH +
// IS_PENDING), API 26-28 — the public Download through File I/O (needs
// WRITE_EXTERNAL_STORAGE). Any failure returns a reason, not a faceless null.
//
// API 29+: we always insert into the Downloads collection, not into Video. The
// Video collection does not accept the primary Download directory — the platform
// throws IllegalArgumentException on insert — and it is Downloads that preserves
// the Download/Rumo location promised to the user.

const val RUMO_DOWNLOAD_DIR = "Rumo"

/** Where to put the file: MP4, logs, snapshots and copied references — all in
 * Downloads (the difference is the MIME). */
enum class DownloadKind { VIDEO, TEXT, IMAGE, AUDIO }

/** Result of writing to Download/Rumo; [Ok.path] is what we show the user. */
sealed interface SaveResult {
    data class Ok(val uri: Uri, val path: String) : SaveResult

    data class Failed(val reason: String) : SaveResult
}

private const val SAVE_TAG = "storage"

private fun mimeFor(kind: DownloadKind): String = when (kind) {
    DownloadKind.VIDEO -> "video/mp4"
    DownloadKind.IMAGE -> "image/png"
    DownloadKind.AUDIO -> "audio/mpeg"
    DownloadKind.TEXT -> "text/plain"
}

/** Write failure: class + message + an errno hint (EACCES/EPERM/ENOSPC). */
private fun ioReason(step: String, t: Throwable): String {
    val msg = t.message.orEmpty()
    val hint = when {
        msg.contains("EACCES") || msg.contains("EPERM") ->
            " (storage permission denied; API 26-28 needs WRITE_EXTERNAL_STORAGE)"
        msg.contains("ENOSPC") -> " (no space left)"
        else -> ""
    }
    return "$step failed: ${t.javaClass.simpleName}: " +
        "${if (msg.isEmpty()) "no message" else msg}$hint"
}

// Move the finished MP4 from cacheDir to Download/Rumo.
suspend fun saveMp4ToDownloads(context: Context, src: File, name: String): SaveResult =
    writeToDownloads(context, DownloadKind.VIDEO, name) { out ->
        src.inputStream().use { it.copyTo(out) }
    }

/** Text report (AppLog + render diagnostics) into Download/Rumo. */
suspend fun saveTextToDownloads(context: Context, text: String, name: String): SaveResult =
    writeToDownloads(context, DownloadKind.TEXT, name) { out ->
        out.write(text.toByteArray(Charsets.UTF_8))
    }

/**
 * Bytes into a sub-directory of `Download/` — the project's references folder or
 * the Rumo root.
 *
 * A separate entry point, not a parameter on [saveBytesToDownloads]: a copied
 * reference has its own MIME (JPEG stays JPEG), and its own directory too, and
 * there is no point dragging that through every snapshot-saving call.
 */
suspend fun saveBytesToFolder(
    context: Context,
    kind: DownloadKind,
    bytes: ByteArray,
    name: String,
    folder: String,
    mime: String? = null,
): SaveResult = writeToDownloads(context, kind, name, folder, mime) { out ->
    out.write(bytes)
}

/**
 * Ready bytes (a PNG frame snapshot) into Download/Rumo — so that the
 * assistant's snapshot is visible in a file manager, not only inside the
 * conversation.
 */
suspend fun saveBytesToDownloads(
    context: Context,
    kind: DownloadKind,
    bytes: ByteArray,
    name: String,
): SaveResult = writeToDownloads(context, kind, name) { out ->
    out.write(bytes)
}

/**
 * The single implementation of "put a file into Download/Rumo": the MP4 and the
 * text log share one failure handler (insert / openOutputStream / copy /
 * finalize). The I/O runs on Dispatchers.IO; the failure reason is always logged
 * to AppLog.
 */
private suspend fun writeToDownloads(
    context: Context,
    kind: DownloadKind,
    displayName: String,
    /** Sub-folder under `Download/`; defaults to the app's own root. */
    folder: String = RUMO_DOWNLOAD_DIR,
    /** MIME to record; defaults to the kind's. A copied reference keeps its own. */
    mime: String? = null,
    write: (OutputStream) -> Unit,
): SaveResult = withContext(Dispatchers.IO) {
    try {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            writeViaMediaStore(context, kind, displayName, folder, mime ?: mimeFor(kind), write)
        } else {
            writeViaLegacyDownloads(displayName, folder, write)
        }
    } catch (t: Throwable) {
        AppLog.error(SAVE_TAG, "save $displayName threw: ${AppLog.describe(t)}")
        SaveResult.Failed(AppLog.describe(t))
    }
}

@RequiresApi(Build.VERSION_CODES.Q)
private fun writeViaMediaStore(
    context: Context,
    kind: DownloadKind,
    displayName: String,
    folder: String,
    mime: String,
    write: (OutputStream) -> Unit,
): SaveResult {
    // Downloads is the only collection that accepts the primary Download
    // directory; the Video collection rejects it on insert.
    val collection = MediaStore.Downloads.EXTERNAL_CONTENT_URI
    val relative = Environment.DIRECTORY_DOWNLOADS + "/" + folder
    val values = ContentValues().apply {
        put(MediaStore.MediaColumns.DISPLAY_NAME, displayName)
        put(MediaStore.MediaColumns.MIME_TYPE, mime)
        put(MediaStore.MediaColumns.RELATIVE_PATH, relative)
        put(MediaStore.MediaColumns.IS_PENDING, 1)
    }
    val resolver = context.contentResolver
    val uri = try {
        resolver.insert(collection, values)
    } catch (t: Throwable) {
        AppLog.error(SAVE_TAG, "MediaStore.insert($displayName) threw", t)
        return SaveResult.Failed(ioReason("MediaStore.insert", t))
    }
    if (uri == null) {
        AppLog.error(SAVE_TAG, "MediaStore.insert($displayName) returned null (collection=$collection)")
        return SaveResult.Failed("MediaStore.insert returned null for $displayName")
    }
    val out = try {
        resolver.openOutputStream(uri)
    } catch (t: Throwable) {
        runCatching { resolver.delete(uri, null, null) }
        AppLog.error(SAVE_TAG, "openOutputStream($displayName) threw", t)
        return SaveResult.Failed(ioReason("openOutputStream", t))
    }
    if (out == null) {
        runCatching { resolver.delete(uri, null, null) }
        AppLog.error(SAVE_TAG, "openOutputStream($displayName) returned null")
        return SaveResult.Failed("openOutputStream returned null for $displayName")
    }
    try {
        out.use { write(it) }
    } catch (t: Throwable) {
        runCatching { resolver.delete(uri, null, null) }
        AppLog.error(SAVE_TAG, "write $displayName threw", t)
        return SaveResult.Failed(ioReason("write", t))
    }
    val rows = try {
        val done = ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }
        resolver.update(uri, done, null, null)
    } catch (t: Throwable) {
        AppLog.error(SAVE_TAG, "IS_PENDING finalize($displayName) threw", t)
        return SaveResult.Failed(ioReason("IS_PENDING finalize", t))
    }
    if (rows <= 0) {
        AppLog.error(SAVE_TAG, "IS_PENDING finalize($displayName) updated $rows rows; item stays pending")
        return SaveResult.Failed("finalize failed: IS_PENDING update affected $rows rows")
    }
    val path = "Download/$folder/$displayName"
    AppLog.info(SAVE_TAG, "saved $displayName -> $path (uri=$uri)")
    return SaveResult.Ok(uri, path)
}

/** API 26-28: the public Download/Rumo through File I/O (no RELATIVE_PATH/IS_PENDING). */
private fun writeViaLegacyDownloads(
    folder: String,
    displayName: String,
    write: (OutputStream) -> Unit,
): SaveResult {
    val state = Environment.getExternalStorageState()
    if (state != Environment.MEDIA_MOUNTED) {
        AppLog.error(SAVE_TAG, "external storage is $state, not mounted")
        return SaveResult.Failed("external storage is $state")
    }
    @Suppress("DEPRECATION")
    val base = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)
    val dir = File(base, folder)
    if (!dir.isDirectory && !dir.mkdirs()) {
        AppLog.error(SAVE_TAG, "mkdirs failed: ${dir.absolutePath}")
        return SaveResult.Failed("cannot create ${dir.absolutePath}")
    }
    val file = File(dir, displayName)
    try {
        FileOutputStream(file).use { write(it) }
    } catch (t: Throwable) {
        AppLog.error(SAVE_TAG, "legacy write to ${file.absolutePath} threw", t)
        return SaveResult.Failed(ioReason("write to ${file.absolutePath}", t))
    }
    val path = "Download/$folder/$displayName"
    AppLog.info(
        SAVE_TAG,
        "saved $displayName -> $path (${file.length()} bytes, legacy API ${Build.VERSION.SDK_INT})",
    )
    return SaveResult.Ok(Uri.fromFile(file), path)
}

// The "Export MP4" progress dialog: progress 0..1 (null = starting),
// error — an error message instead of the indicator.
@Composable
fun Mp4ExportDialog(
    progress: Float?,
    error: String?,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.editor_export_mp4)) },
        text = {
            Column(modifier = Modifier.fillMaxWidth()) {
                if (error != null) {
                    Text(
                        text = error,
                        color = MaterialTheme.colorScheme.error,
                    )
                } else {
                    // An indeterminate indicator + the percentage as text:
                    // without binding to the LinearProgressIndicator(progress)
                    // overloads.
                    LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                    val pct = ((progress ?: 0f) * 100f).toInt().coerceIn(0, 100)
                    Text(
                        text = "$pct%",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.padding(top = RumoSpacing.s),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) {
                Text(
                    stringResource(
                        if (error != null) R.string.action_close else R.string.editor_export_hide,
                    ),
                )
            }
        },
    )
}
