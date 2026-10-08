// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.content.Context
import com.kerneldroid.rumo.data.AppLog
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

data class ProjectEntry(
    val fileName: String,
    val displayName: String,
    val lastModified: Long,
)

// filesDir/projects: `.rumo` codec bytes via RumoBridge. No new deps,
// all IO on Dispatchers.IO.
object ProjectStore {
    const val EXT = ".rumo"
    private const val DIR = "projects"
    private const val MAX_NAME = 64

    /**
     * A project name suitable both for a file and for a folder.
     *
     * The regex keeps **letters and digits of any alphabet**, not just Latin:
     * `[^A-Za-z0-9 _-]` ate non-Latin text whole, so "Σχέδιο 1" collapsed to
     * `1`, and "Η ταινία μου" — to `Untitled`. The consequences were not
     * cosmetic: the link folder name is taken from here too
     * (`ProjectAssets.folderOf`), so two non-Latin titles produced **one
     * folder** — the assistant always saw the same files — and **one file
     * name**: saving a second project silently overwrote the first.
     *
     * Path separators still do not get through, though: `/`, `\`, `:` and dots
     * are not letters or digits, so the name stays a single segment and cannot
     * lead a write outside the projects folder. `.` does not pass either — so
     * `..` is impossible, and a name of nothing but dots collapses to
     * `Untitled`.
     */
    fun sanitize(raw: String): String {
        val cleaned = raw.trim().replace(Regex("[^\\p{L}\\p{N} _-]+"), " ").trim()
            .replace(Regex("\\s+"), " ")
        return cleaned.take(MAX_NAME).ifEmpty { "Untitled" }
    }

    fun fileNameFor(displayName: String): String = sanitize(displayName) + EXT

    private fun dir(context: Context): File = File(context.filesDir, DIR).apply { mkdirs() }

    private fun file(context: Context, fileName: String): File {
        val leaf = fileName.substringAfterLast('/').substringAfterLast('\\')
            .ifEmpty { "Untitled$EXT" }
        return File(dir(context), leaf)
    }

    suspend fun list(context: Context): List<ProjectEntry> = withContext(Dispatchers.IO) {
        dir(context).listFiles { f -> f.isFile && f.name.endsWith(EXT) }
            ?.map { ProjectEntry(it.name, it.name.removeSuffix(EXT), it.lastModified()) }
            ?.sortedByDescending { it.lastModified }
            ?: emptyList()
    }

    // Returns the actual file name used (extension ensured).
    suspend fun save(context: Context, fileName: String, bytes: ByteArray): String =
        withContext(Dispatchers.IO) {
            val leaf = file(context, fileName).let {
                if (it.name.endsWith(EXT)) it else File(it.parent!!, it.name + EXT)
            }
            leaf.writeBytes(bytes)
            leaf.name
        }

    suspend fun load(context: Context, fileName: String): ByteArray? =
        withContext(Dispatchers.IO) {
            val f = file(context, fileName)
            if (f.isFile) {
                runCatching { f.readBytes() }
                    .onFailure { AppLog.error("project", "read ${f.name} failed", it) }
                    .getOrNull()
            } else {
                AppLog.warn("project", "read ${f.name}: not a file")
                null
            }
        }

    suspend fun delete(context: Context, fileName: String): Boolean =
        withContext(Dispatchers.IO) {
            runCatching { file(context, fileName).delete() }.getOrDefault(false)
        }
}
