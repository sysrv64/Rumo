// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.content.ContentUris
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.MediaStore
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.AppLog
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * The reference folder of one project: `Download/Rumo/<project>/`.
 *
 * A project's material lives next to the projects rather than inside the app, so
 * the references can be dropped there from a file manager and survive the app
 * being reinstalled. That is the point of the folder — it is an input the user
 * controls, and the assistant reads it.
 *
 * Reading it is not free: files a *user* put there belong to the shared storage,
 * not to this app, so the media collections can only be queried once the app
 * holds the read-media permission. Where the permission is missing the listing
 * says so instead of pretending the folder is empty.
 */
object ProjectAssets {
    /** Root of everything the app writes to the shared storage. */
    const val ROOT = "Rumo"

    /** Where a project's references go, relative to `Download/`. */
    fun folderOf(projectName: String): String = "$ROOT/${ProjectStore.sanitize(projectName)}"

    /**
     * SVG is a separate kind, not a picture: it stays a vector in the engine too,
     * so its layer is a SHAPE with `uri`, not a MEDIA with a texture
     * (see `EditorState.svgUriOf`). Without its own branch, `.svg` landed in IMAGE by
     * MIME `image/…` and in OTHER by name, and neither path drew it.
     */
    enum class Kind { IMAGE, VIDEO, AUDIO, SVG, OTHER }

    data class Asset(
        val name: String,
        val uri: Uri,
        val kind: Kind,
        val sizeBytes: Long,
        val addedAt: Long,
    )

    /** True when the app may read other apps' media, which the folder needs. */
    fun readable(context: Context): Boolean {
        val needed = if (Build.VERSION.SDK_INT >= 33) {
            arrayOf(
                "android.permission.READ_MEDIA_IMAGES",
                "android.permission.READ_MEDIA_VIDEO",
                "android.permission.READ_MEDIA_AUDIO",
            )
        } else {
            arrayOf("android.permission.READ_EXTERNAL_STORAGE")
        }
        // Any one of the three is enough to see *something*; the caller decides
        // whether that is enough for what it was asked to do.
        return needed.any {
            context.checkSelfPermission(it) == android.content.pm.PackageManager.PERMISSION_GRANTED
        }
    }

    /** Which read-media permissions are still missing, for a message. */
    fun missingPermissions(context: Context): List<String> {
        val needed = if (Build.VERSION.SDK_INT >= 33) {
            listOf(
                "android.permission.READ_MEDIA_IMAGES",
                "android.permission.READ_MEDIA_VIDEO",
                "android.permission.READ_MEDIA_AUDIO",
            )
        } else {
            listOf("android.permission.READ_EXTERNAL_STORAGE")
        }
        return needed.filter {
            context.checkSelfPermission(it) != android.content.pm.PackageManager.PERMISSION_GRANTED
        }
    }

    /**
     * Everything in the project's folder, the app's own note excepted.
     *
     * On API 29+ the media collections are queried by relative path, which is the
     * only way to see files this app did not create; below that the folder is read
     * straight off the filesystem. Both paths return the same shape, so callers
     * never branch on the platform.
     */
    suspend fun list(context: Context, projectName: String): List<Asset> =
        listAll(context, projectName).filterNot { it.name == README_NAME }

    /** The folder as it is on disk, [README_NAME] included. */
    private suspend fun listAll(context: Context, projectName: String): List<Asset> =
        withContext(Dispatchers.IO) {
            val folder = folderOf(projectName)
            val fromMediaStore = runCatching { queryMediaStore(context, folder) }.getOrElse {
                AppLog.warn(TAG, "media query for $folder failed: ${it.message}")
                emptyList()
            }
            if (fromMediaStore.isNotEmpty()) return@withContext fromMediaStore
            runCatching { listFromDisk(folder) }.getOrElse {
                AppLog.warn(TAG, "folder $folder unreadable: ${it.message}")
                emptyList()
            }
        }

    /** One asset by file name, or null. */
    suspend fun find(context: Context, projectName: String, name: String): Asset? {
        val wanted = name.substringAfterLast('/').substringAfterLast('\\').trim()
        if (wanted.isEmpty()) return null
        return list(context, projectName).firstOrNull { it.name.equals(wanted, ignoreCase = true) }
    }

    private fun queryMediaStore(context: Context, folder: String): List<Asset> {
        val collection = MediaStore.Files.getContentUri("external")
        val projection = arrayOf(
            MediaStore.MediaColumns._ID,
            MediaStore.MediaColumns.DISPLAY_NAME,
            MediaStore.MediaColumns.SIZE,
            MediaStore.MediaColumns.DATE_ADDED,
            MediaStore.MediaColumns.MIME_TYPE,
        )
        // The relative path of a file in the project folder is
        // `Download/Rumo/<project>/`, so the selection has to carry the leading
        // `Download/` as well: without it nothing ever matched and every listing
        // fell back to the filesystem, which cannot see a non-media file at all
        // (the folder's own note, or a `.zip` the user dropped in). `LIKE` with
        // the trailing separator keeps `Rumo/Project` from matching
        // `Rumo/Project 2`.
        val selection = "${MediaStore.MediaColumns.RELATIVE_PATH} LIKE ?"
        val args = arrayOf("${Environment.DIRECTORY_DOWNLOADS}/$folder/%")
        val out = ArrayList<Asset>()
        context.contentResolver.query(collection, projection, selection, args, null)?.use { cursor ->
            val idColumn = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns._ID)
            val nameColumn = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns.DISPLAY_NAME)
            val sizeColumn = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns.SIZE)
            val dateColumn = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns.DATE_ADDED)
            val mimeColumn = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns.MIME_TYPE)
            while (cursor.moveToNext()) {
                val name = cursor.getString(nameColumn) ?: continue
                val mime = cursor.getString(mimeColumn).orEmpty()
                out += Asset(
                    name = name,
                    uri = ContentUris.withAppendedId(collection, cursor.getLong(idColumn)),
                    kind = kindOf(name, mime),
                    sizeBytes = cursor.getLong(sizeColumn),
                    addedAt = cursor.getLong(dateColumn) * 1_000L,
                )
            }
        }
        return out.sortedByDescending { it.addedAt }
    }

    private fun listFromDisk(folder: String): List<Asset> {
        @Suppress("DEPRECATION")
        val root = Environment.getExternalStorageDirectory()
        val dir = File(File(root, "Download"), folder)
        if (!dir.isDirectory) return emptyList()
        return dir.listFiles { f -> f.isFile && !f.name.startsWith(".") }
            ?.map { file ->
                Asset(
                    name = file.name,
                    uri = Uri.fromFile(file),
                    kind = kindOf(file.name, null),
                    sizeBytes = file.length(),
                    addedAt = file.lastModified(),
                )
            }
            ?.sortedByDescending { it.addedAt }
            ?: emptyList()
    }

    /** Classify by extension first: a `.png` in Downloads often has no MIME row. */
    private fun kindOf(name: String, mime: String?): Kind {
        val lower = name.lowercase()
        return when {
            // SVG is checked before the general `image/`: its MIME `image/svg+xml`
            // starts with `image/`, and without this branch it would become a picture —
            // a different kind, for which it is overkill to have a separate vector path.
            mime?.startsWith("image/svg") == true || SVG_EXT.any { lower.endsWith(it) } -> Kind.SVG
            mime?.startsWith("image") == true || IMAGE_EXT.any { lower.endsWith(it) } -> Kind.IMAGE
            mime?.startsWith("video") == true || VIDEO_EXT.any { lower.endsWith(it) } -> Kind.VIDEO
            mime?.startsWith("audio") == true || AUDIO_EXT.any { lower.endsWith(it) } -> Kind.AUDIO
            else -> Kind.OTHER
        }
    }

    /**
     * Copy a reference the user picked into the project's folder.
     *
     * The folder is meant to be the one place a project's material lives, and a
     * reference that only exists as a picker grant is invisible to a file manager
     * and to the assistant's listing. Small files are copied for that reason;
     * a large one is left where it is, because duplicating a gigabyte of video to
     * make a listing prettier is not a trade worth making. The layer keeps
     * pointing at where the file came from either way — the copy is for the
     * folder, not a move.
     */
    suspend fun copyIn(
        context: Context,
        projectName: String,
        uri: Uri,
        mime: String?,
        maxBytes: Long = COPY_LIMIT_BYTES,
    ): String? = withContext(Dispatchers.IO) {
        val bytes = runCatching { readUriBytes(context, uri, maxBytes.toInt()) }.getOrNull()
            ?: return@withContext null
        if (bytes.isEmpty() || bytes.size > maxBytes) {
            AppLog.info(TAG, "reference ${bytes.size}B left in place (copy limit $maxBytes)")
            return@withContext null
        }
        val name = queryDisplayName(context, uri, "reference")
        val kind = when {
            mime?.startsWith("video") == true -> DownloadKind.VIDEO
            mime?.startsWith("audio") == true -> DownloadKind.AUDIO
            else -> DownloadKind.IMAGE
        }
        when (val result = saveBytesToFolder(context, kind, bytes, name, folderOf(projectName), mime)) {
            is SaveResult.Ok -> result.path
            is SaveResult.Failed -> {
                AppLog.warn(TAG, "reference copy failed: ${result.reason}")
                null
            }
        }
    }

    /**
     * Write a ready-made SVG into the project folder.
     *
     * Separate from [copyIn], although the bytes are the same: the name must stay
     * `.svg`. The layer identifies an SVG by name/path, and a file without the extension would
     * stop being an SVG both for [kindOf] and for the renderer. The returned record carries a
     * permanent uri — that is what the layer will read the geometry from.
     */
    suspend fun writeSvg(
        context: Context,
        projectName: String,
        name: String,
        bytes: ByteArray,
    ): Asset? = withContext(Dispatchers.IO) {
        val clean = name.trim().ifEmpty { "drawing" }
        val fileName = if (clean.lowercase().endsWith(".svg")) clean else "$clean.svg"
        when (
            val result = saveBytesToFolder(
                context = context,
                kind = DownloadKind.IMAGE,
                bytes = bytes,
                name = fileName,
                folder = folderOf(projectName),
                mime = SVG_MIME,
            )
        ) {
            is SaveResult.Ok -> Asset(
                name = fileName,
                uri = result.uri,
                kind = Kind.SVG,
                sizeBytes = bytes.size.toLong(),
                addedAt = System.currentTimeMillis(),
            )
            is SaveResult.Failed -> {
                AppLog.warn(TAG, "svg write failed: ${result.reason}")
                null
            }
        }
    }

    /**
     * Write a raster picture into the project folder.
     *
     * Needed by the SVG legacy fallback: a rasterised document stops being a
     * vector and becomes an ordinary picture, so the file must be
     * `.png` too — that is how [kindOf] and the engine recognise it as a picture and not as the
     * SVG it no longer is. The name is reduced to `<stem>.png`.
     */
    suspend fun writePicture(
        context: Context,
        projectName: String,
        name: String,
        bytes: ByteArray,
    ): Asset? = withContext(Dispatchers.IO) {
        val clean = name.trim().ifEmpty { "picture" }
        // Trim the previous extension (`logo.svg` -> `logo`), but leave a name without a dot
        // as it is: `substringBeforeLast` with a missing value.
        val stem = clean.substringBeforeLast('.', clean).ifEmpty { "picture" }
        val fileName = if (stem.endsWith(".png", true)) stem else "$stem.png"
        when (
            val result = saveBytesToFolder(
                context = context,
                kind = DownloadKind.IMAGE,
                bytes = bytes,
                name = fileName,
                folder = folderOf(projectName),
                mime = "image/png",
            )
        ) {
            is SaveResult.Ok -> Asset(
                name = fileName,
                uri = result.uri,
                kind = Kind.IMAGE,
                sizeBytes = bytes.size.toLong(),
                addedAt = System.currentTimeMillis(),
            )
            is SaveResult.Failed -> {
                AppLog.warn(TAG, "picture write failed: ${result.reason}")
                null
            }
        }
    }

    /**
     * Write generated bytes (sound, picture, video) into the project folder.
     *
     * Separate from [copyIn], because there is no source uri: the bytes came from a
     * service response, not from a picker, and there is nothing to "copy".
     * The name must keep its extension — that is how the engine and [kindOf] recognise
     * the kind — so for a name without a dot the extension is added from the MIME rather than
     * left as a name with no kind.
     */
    suspend fun writeMedia(
        context: Context,
        projectName: String,
        name: String,
        bytes: ByteArray,
        mime: String,
    ): Asset? = withContext(Dispatchers.IO) {
        val clean = name.trim().ifEmpty { "generated" }
        val fileName = if (clean.substringAfterLast('/').contains('.')) {
            clean
        } else {
            "$clean.${extensionFor(mime)}"
        }
        val kind = when {
            mime.startsWith("video") -> DownloadKind.VIDEO
            mime.startsWith("audio") -> DownloadKind.AUDIO
            else -> DownloadKind.IMAGE
        }
        when (val result = saveBytesToFolder(context, kind, bytes, fileName, folderOf(projectName), mime)) {
            is SaveResult.Ok -> Asset(
                name = fileName,
                uri = result.uri,
                kind = kindOf(fileName, mime),
                sizeBytes = bytes.size.toLong(),
                addedAt = System.currentTimeMillis(),
            )
            is SaveResult.Failed -> {
                AppLog.warn(TAG, "generated media write failed: ${result.reason}")
                null
            }
        }
    }

    /** Extension by MIME — only for a name that arrived without its own. */
    private fun extensionFor(mime: String): String = when {
        mime.contains("wav") -> "wav"
        mime.contains("mpeg") -> "mp3"
        mime.contains("mp4") -> "mp4"
        mime.contains("webp") -> "webp"
        mime.contains("jpeg") || mime.contains("jpg") -> "jpg"
        else -> "bin"
    }

    /**
     * Accept a user-picked SVG into the project folder.
     *
     * The same copy is needed as for the other references: the layer points at a file
     * in the project folder, not at a temporary picker grant, otherwise the SVG
     * would disappear after a restart. The bytes are read whole — an SVG is textual and
     * small; the ceiling is the same as [copyIn]'s.
     */
    suspend fun copySvgIn(
        context: Context,
        projectName: String,
        uri: Uri,
        maxBytes: Long = COPY_LIMIT_BYTES,
    ): Asset? = withContext(Dispatchers.IO) {
        val bytes = runCatching { readUriBytes(context, uri, maxBytes.toInt()) }.getOrNull()
            ?: return@withContext null
        if (bytes.isEmpty() || bytes.size > maxBytes) {
            AppLog.info(TAG, "svg ${bytes.size}B left in place (copy limit $maxBytes)")
            return@withContext null
        }
        val raw = queryDisplayName(context, uri, "drawing.svg")
        writeSvg(context, projectName, raw, bytes)
    }

    /** What the app writes into a project folder itself, e.g. a snapshot. */
    fun relativePathFor(projectName: String): String = folderOf(projectName)

    /**
     * Make the project's folder exist, so the user has somewhere to put material.
     *
     * A project folder used to appear only once something was copied into it, so
     * the answer to "where do I drop the references" was "nowhere yet". It is
     * created here instead, when the project is created, opened or saved.
     *
     * The note is what creates it: MediaStore has no call that makes a directory,
     * only a file whose relative path it creates on the way there, and an empty
     * directory would be invisible to the user anyway. The note is the app's own
     * marker rather than a reference, so [list] hides it.
     */
    suspend fun ensureFolder(context: Context, projectName: String): SaveResult {
        val folder = folderOf(projectName)
        val existing = listAll(context, projectName).firstOrNull { it.name == README_NAME }
        if (existing != null) return SaveResult.Ok(existing.uri, "Download/$folder")
        return saveBytesToFolder(
            context = context,
            kind = DownloadKind.TEXT,
            bytes = readmeFor(context, projectName).toByteArray(Charsets.UTF_8),
            name = README_NAME,
            folder = folder,
            mime = "text/plain",
        )
    }

    /**
     * Show the project's folder in whatever file manager will take it.
     *
     * The folder is the one place a project's material lives, and after dropping
     * a file there the user has to be able to find it without guessing a path.
     * DocumentsUI addresses a directory as a document id `primary:<path>`, which
     * is what this asks for; a device with nothing that handles it says false and
     * the caller shows the path instead of pretending a window opened.
     */
    fun openFolder(context: Context, projectName: String): Boolean {
        val id = "primary:Download/${folderOf(projectName)}"
        val uri = DocumentsContract.buildDocumentUri(EXTERNAL_STORAGE_AUTHORITY, id)
        val intent = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, DocumentsContract.Document.MIME_TYPE_DIR)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        return runCatching { context.startActivity(intent) }.isSuccess
    }

    /** The app's own note in a project folder, so the folder is never bare. */
    const val README_NAME = "README.txt"

    /**
     * The note's body: the folder it belongs to, then what to put there.
     *
     * The text is a resource, but the note is written once, when the folder is
     * first created, and is never rewritten: a folder made under one language
     * keeps that language's note afterwards. Rewriting it on a language change
     * would mean touching the user's storage on every switch for a note nobody
     * re-reads, and the file is a marker as much as a message.
     */
    private fun readmeFor(context: Context, projectName: String): String =
        context.getString(R.string.editor_assets_readme, ProjectStore.sanitize(projectName))

    private const val EXTERNAL_STORAGE_AUTHORITY = "com.android.externalstorage.documents"

    private const val TAG = "assets"

    /** Above this a reference is not copied into the project folder. */
    private const val COPY_LIMIT_BYTES = 32L * 1024L * 1024L

    /** The MIME under which SVG is written and recognised in MediaStore. */
    const val SVG_MIME = "image/svg+xml"

    private val SVG_EXT = listOf(".svg")
    private val IMAGE_EXT = listOf(".png", ".jpg", ".jpeg", ".webp", ".bmp", ".gif", ".heic")
    private val VIDEO_EXT = listOf(".mp4", ".mkv", ".webm", ".mov", ".m4v", ".3gp", ".avi")
    private val AUDIO_EXT = listOf(".mp3", ".m4a", ".aac", ".wav", ".ogg", ".opus", ".flac")
}
