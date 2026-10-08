// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * Fonts downloaded from the shop: files on disk, metadata next to them and
 * registration in the engine.
 *
 * ## Why a separate store
 *
 * The engine knows a font only by family name, and that name is read **from the
 * file itself** (`nativeFontRegister`), not from the catalogue: an "Open Sans" file
 * may call itself something other than the entry in the Google Fonts list, and
 * substituting the catalogue name would lay out text in the wrong face.
 *
 * So a record carries two names: [Font.family] — what the font is addressed by in
 * the engine (the layer and preview key), and [Font.displayName] — what the list shows.
 *
 * ## Registration
 *
 * The engine starts empty on every process launch, so installed fonts are registered
 * once per process ([registerAll]). Registering the same file again is not harmful,
 * but it is not free either: every face occupies memory in the font database until
 * the process ends, and duplicates would pile up there.
 */
object FontStore {
    /** One installed record. */
    data class Font(
        /** Family name read by the engine from the file. Key for the layer and preview. */
        val family: String,
        /** File name inside the fonts directory. */
        val file: String,
        /** How to show it in the list (usually the catalogue name). */
        val displayName: String,
        /** Where it was downloaded from: `google-fonts` or a GitHub repository. */
        val source: String,
        /** Licence tag, e.g. `OFL-1.1`. */
        val license: String,
        /** The copyright string from the file or METADATA.pb. */
        val copyright: String,
        val addedAt: Long,
    ) {
        /** List subtitle: the catalogue name when it differs from the family. */
        val subtitle: String get() = displayName
    }

    private const val DIR_NAME = "fonts"
    private const val INDEX_NAME = "index.json"

    /** The fonts directory; created on first access. */
    fun dir(context: Context): File =
        File(context.applicationContext.filesDir, DIR_NAME).apply { mkdirs() }

    private fun indexFile(context: Context): File = File(dir(context), INDEX_NAME)

    /**
     * All installed fonts, newest first.
     *
     * A broken index reads as an empty list rather than a crash: the font files
     * stay on disk, and the next install rebuilds the index.
     */
    fun installed(context: Context): List<Font> {
        val file = indexFile(context)
        if (!file.isFile) return emptyList()
        return try {
            val array = JSONArray(file.readText())
            (0 until array.length()).mapNotNull { i ->
                val o = array.optJSONObject(i) ?: return@mapNotNull null
                val family = o.optString("family")
                val name = o.optString("file")
                if (family.isEmpty() || name.isEmpty()) return@mapNotNull null
                Font(
                    family = family,
                    file = name,
                    displayName = o.optString("displayName", family),
                    source = o.optString("source", ""),
                    license = o.optString("license", ""),
                    copyright = o.optString("copyright", ""),
                    addedAt = o.optLong("addedAt", 0L),
                )
            }
        } catch (_: Throwable) {
            emptyList()
        }
    }

    /** Whether a font with this engine family is installed. */
    fun isInstalled(context: Context, family: String): Boolean =
        installed(context).any { it.family == family }

    /** The installed font's bytes, or null if the file is gone. */
    fun bytes(context: Context, family: String): ByteArray? {
        val entry = installed(context).firstOrNull { it.family == family } ?: return null
        val file = File(dir(context), entry.file)
        return try {
            if (file.isFile) file.readBytes() else null
        } catch (_: Throwable) {
            null
        }
    }

    @Volatile
    private var registered = false

    /**
     * Registers all installed fonts in the engine. Once per process.
     *
     * Blocking: it reads files from disk, so it must not be called on the main
     * thread.
     */
    fun registerAll(context: Context) {
        if (registered) return
        registered = true
        val entries = installed(context)
        if (entries.isEmpty()) return
        val fixed = ArrayList<Font>(entries.size)
        var changed = false
        for (entry in entries) {
            val file = File(dir(context), entry.file)
            val bytes = try {
                if (file.isFile) file.readBytes() else null
            } catch (_: Throwable) {
                null
            }
            if (bytes == null) {
                // The file is gone — a record without it is meaningless, and keeping it
                // means offering a font in the editor that does not exist.
                changed = true
                continue
            }
            val resolved = RumoBridge.fontRegister(bytes)
            if (resolved == null) {
                changed = true
                continue
            }
            // First launch after install: the index may have held the catalogue name,
            // while the engine knows the real one. The discrepancy is fixed here, rather
            // than silently laying out text in another face.
            if (resolved != entry.family) changed = true
            fixed += entry.copy(family = resolved)
        }
        if (changed) writeIndex(context, fixed)
    }

    /**
     * Installs a font: writes the file, registers it in the engine and adds the
     * record to the index. Returns the record with the family name **from the engine**,
     * or null if the bytes were not parsed.
     */
    fun install(
        context: Context,
        displayName: String,
        source: String,
        license: String,
        copyright: String,
        bytes: ByteArray,
    ): Font? {
        if (bytes.isEmpty()) return null
        val family = RumoBridge.fontRegister(bytes) ?: return null
        val slug = slugOf(family).ifEmpty { "font" }
        val fileName = uniqueFileName(context, slug)
        val target = File(dir(context), fileName)
        try {
            target.writeBytes(bytes)
        } catch (_: Throwable) {
            return null
        }
        val entry = Font(
            family = family,
            file = fileName,
            displayName = displayName.ifEmpty { family },
            source = source,
            license = license,
            copyright = copyright,
            addedAt = System.currentTimeMillis(),
        )
        // One family — one record: reinstalling replaces the old one, otherwise the
        // list would show two "Inter" entries with different files.
        val rest = installed(context).filterNot { it.family == family }
        writeIndex(context, listOf(entry) + rest)
        return entry
    }

    /** Removes a font: the record and the file. true if anything was removed. */
    fun remove(context: Context, family: String): Boolean {
        val entries = installed(context)
        val entry = entries.firstOrNull { it.family == family } ?: return false
        val rest = entries.filterNot { it.family == family }
        writeIndex(context, rest)
        val gone = try {
            File(dir(context), entry.file).delete()
        } catch (_: Throwable) {
            false
        }
        // The licence text sits next to the face; an orphaned licence file belongs to
        // no font any more, and keeping it means accumulating junk.
        try {
            licenseFile(context, entry).delete()
        } catch (_: Throwable) {
            // It did not delete — no reason to think the font stayed.
        }
        return gone
    }

    /**
     * The file with the licence text next to the face.
     *
     * Google Fonts requires the licence text to be distributed with the font, and
     * the `copyright` in the index is a single line, not the licence. It is kept
     * next to the file rather than in the index because it is a long text of hundreds
     * of lines that need not be read on every render of the list.
     */
    fun licenseFile(context: Context, font: Font): File =
        File(dir(context), font.file.substringBeforeLast('.') + ".LICENSE.txt")

    /** Saves the licence text next to the font. */
    fun saveLicense(context: Context, font: Font, text: String) {
        if (text.isBlank()) return
        try {
            licenseFile(context, font).writeText(text)
        } catch (_: Throwable) {
            // It did not save — the font still works; losing the install over this
            // would be worse.
        }
    }

    /** The licence text of an installed font, or null. */
    fun licenseText(context: Context, font: Font): String? {
        val file = licenseFile(context, font)
        return try {
            if (file.isFile) file.readText() else null
        } catch (_: Throwable) {
            null
        }
    }

    private fun writeIndex(context: Context, entries: List<Font>) {
        val array = JSONArray()
        for (entry in entries) {
            array.put(
                JSONObject().apply {
                    put("family", entry.family)
                    put("file", entry.file)
                    put("displayName", entry.displayName)
                    put("source", entry.source)
                    put("license", entry.license)
                    put("copyright", entry.copyright)
                    put("addedAt", entry.addedAt)
                },
            )
        }
        try {
            indexFile(context).writeText(array.toString())
        } catch (_: Throwable) {
            // The index did not write — the font files are already on disk, and the next
            // install attempt rebuilds the index from scratch.
        }
    }

    /** File name from the family name: letters, digits and hyphen only. */
    internal fun slugOf(family: String): String =
        family.lowercase()
            .map { if (it.isLetterOrDigit()) it else '-' }
            .joinToString("")
            .trim('-')
            .replace(Regex("-+"), "-")

    private fun uniqueFileName(context: Context, slug: String): String {
        var candidate = "$slug.ttf"
        var n = 2
        while (File(dir(context), candidate).exists()) {
            candidate = "$slug-$n.ttf"
            n++
        }
        return candidate
    }
}
