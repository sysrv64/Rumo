// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import android.content.Context
import com.kerneldroid.rumo.ui.CustomEffectUi
import com.kerneldroid.rumo.ui.CustomEffects
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * Effects installed from the shop: one file per effect plus an index next to them.
 *
 * ## Why a separate store
 *
 * The built-in catalogue lives in the engine, and project effects come with the
 * project. An installed effect is a third state: it is neither in the project nor
 * in the binary, yet it has to appear in the editor's menu for every new project.
 * So it lives in its own file under `filesDir/effects` and reaches the menu from
 * [installedEffects], not from some single project's JSON.
 *
 * ## The format is the author's own
 *
 * The body (`effect`) is `CustomEffectUi.toJson()`: the WGSL module and the
 * parameters it declares. The shop has no effect description of its own, so a
 * downloaded effect and one written in the editor travel the same path. Install runs
 * [CustomEffects.shapeError] before writing: the engine would reject a malformed
 * effect anyway, and there is no point keeping it in the menu as a dead record.
 *
 * ## Index
 *
 * [installed] reads the index (`index.json`) and the body from the file, because
 * only the file carries the WGSL. A broken index is an empty list, not a crash:
 * the effect files stay on disk, and the next install rebuilds the index.
 */
object EffectStore {
    const val EXT = ".rumoeffect"
    const val KIND = "rumo-effect"

    private const val FORMAT_VERSION = 1

    /** Body ceiling in string characters; see [TemplateStore.parse]. */
    private const val MAX_BODY_CHARS = 8 * 1024 * 1024

    private const val DIR_NAME = "effects"
    private const val INDEX_NAME = "index.json"

    /** One installed effect: metadata plus the body as-is. */
    data class Effect(
        val name: String,
        val description: String,
        val createdAt: Long,
        val effectJson: String,
    )

    /** Index record: what to show in the list and which file backs it. */
    private data class Entry(
        val name: String,
        val file: String,
        val description: String,
        val source: String,
        val createdAt: Long,
    )

    /** Packs an effect into a `.rumoeffect` document. */
    fun wrap(
        name: String,
        description: String,
        createdBy: String,
        appVersionCode: Int,
        effect: CustomEffectUi,
    ): String =
        JSONObject()
            .put("kind", KIND)
            .put("formatVersion", FORMAT_VERSION)
            .put("name", name)
            .put("description", description)
            .put("createdBy", createdBy)
            .put("appVersionCode", appVersionCode)
            .put("createdAt", System.currentTimeMillis())
            .put("effect", effect.toJson())
            .toString()

    /** Parses an effect document; null if the kind/format is wrong or the body is corrupt. */
    fun parse(text: String): Effect? {
        if (text.length > MAX_BODY_CHARS) return null
        val root = try {
            JSONObject(text)
        } catch (_: Throwable) {
            return null
        }
        if (root.optString("kind", "") != KIND) return null
        if (root.optInt("formatVersion", 0) != FORMAT_VERSION) return null
        val effect = root.optJSONObject("effect") ?: return null
        // An empty object is an effect with no module and no parameters; the engine has nothing to take from it.
        if (effect.length() == 0) return null
        val encoded = effect.toString()
        if (encoded.length > MAX_BODY_CHARS) return null
        return Effect(
            name = root.optString("name", ""),
            description = root.optString("description", ""),
            createdAt = root.optLong("createdAt", 0L),
            effectJson = encoded,
        )
    }

    /** Parses back into the engine model; null if the body is corrupt or malformed. */
    fun toEffect(text: String): CustomEffectUi? {
        val parsed = parse(text) ?: return null
        return decodeBody(parsed.effectJson)
    }

    /**
     * Installed effects, newest first: the files under `filesDir/effects` that the
     * index names. A file that is missing or failed to parse is skipped — listing an
     * effect that the engine was never given would mean showing something in the menu
     * that will not render.
     */
    suspend fun installed(context: Context): List<Effect> = withContext(Dispatchers.IO) {
        val entries = readIndex(context)
        val out = ArrayList<Effect>(entries.size)
        for (entry in entries) {
            val body = try {
                val file = File(dir(context), entry.file)
                if (file.isFile) file.readText() else null
            } catch (_: Throwable) {
                null
            } ?: continue
            val parsed = parse(body) ?: continue
            out += Effect(
                name = entry.name,
                description = entry.description.ifEmpty { parsed.description },
                createdAt = entry.createdAt,
                effectJson = parsed.effectJson,
            )
        }
        out
    }

    /**
     * Installs an effect: writes the file and the index record. Returns the record,
     * or null if the effect failed the shape check or was not written.
     */
    suspend fun install(
        context: Context,
        effect: CustomEffectUi,
        name: String,
        description: String,
        source: String,
    ): Effect? = withContext(Dispatchers.IO) {
        // The same checks the editor does: installing an effect the engine would
        // reject would leave a known-dead record in the menu.
        val problem = CustomEffects.shapeError(effect)
        if (problem != null) {
            AppLog.warn("effect", "install refused `${effect.id}`: $problem")
            return@withContext null
        }
        val display = name.ifBlank { effect.label }.ifBlank { effect.id }
        // There is no createdBy/appVersionCode in the signature (the install is not
        // from an author-user) — we leave them empty, as in the format example.
        val document = wrap(
            name = display,
            description = description,
            createdBy = "",
            appVersionCode = 0,
            effect = effect,
        )
        val parsed = parse(document) ?: return@withContext null
        // Reinstalling the same name reuses the same file rather than spawning a new
        // one: otherwise the directory would accumulate abandoned copies of one effect.
        val existing = readIndex(context).firstOrNull { it.name == display }
        val target = existing?.let { File(dir(context), it.file) }
            ?: uniqueFile(context, slugOf(display).ifEmpty { slugOf(effect.id).ifEmpty { "effect" } })
        try {
            target.writeText(document)
        } catch (t: Throwable) {
            AppLog.error("effect", "install ${target.name} failed: ${AppLog.describe(t)}")
            return@withContext null
        }
        // One name — one record: otherwise the menu would show two "VHS Glitch" entries.
        val entry = Entry(display, target.name, description, source, parsed.createdAt)
        writeIndex(context, listOf(entry) + readIndex(context).filterNot { it.name == display })
        Effect(display, description, parsed.createdAt, parsed.effectJson)
    }

    /** Removes an effect: the record and the file. true if the file was deleted. */
    suspend fun remove(context: Context, name: String): Boolean = withContext(Dispatchers.IO) {
        val entries = readIndex(context)
        val entry = entries.firstOrNull { it.name == name } ?: return@withContext false
        writeIndex(context, entries.filterNot { it.name == name })
        try {
            File(dir(context), entry.file).delete()
        } catch (_: Throwable) {
            false
        }
    }

    /** All installed effects, decoded for the editor's menu. */
    suspend fun installedEffects(context: Context): List<CustomEffectUi> =
        installed(context).mapNotNull { decodeBody(it.effectJson) }

    /** The effects directory; created on first access. */
    private fun dir(context: Context): File =
        File(context.applicationContext.filesDir, DIR_NAME).apply { mkdirs() }

    private fun indexFile(context: Context): File = File(dir(context), INDEX_NAME)

    private fun readIndex(context: Context): List<Entry> {
        val file = indexFile(context)
        if (!file.isFile) return emptyList()
        return try {
            val array = JSONArray(file.readText())
            (0 until array.length()).mapNotNull { i ->
                val o = array.optJSONObject(i) ?: return@mapNotNull null
                val name = o.optString("name", "")
                val stored = o.optString("file", "")
                if (name.isEmpty() || stored.isEmpty()) return@mapNotNull null
                Entry(
                    name = name,
                    file = stored,
                    description = o.optString("description", ""),
                    source = o.optString("source", ""),
                    createdAt = o.optLong("createdAt", 0L),
                )
            }
        } catch (_: Throwable) {
            // A broken index is an empty list, not a crash: the effect files are intact,
            // and the next install rebuilds the index.
            emptyList()
        }
    }

    private fun writeIndex(context: Context, entries: List<Entry>) {
        val array = JSONArray()
        for (entry in entries) {
            array.put(
                JSONObject().apply {
                    put("name", entry.name)
                    put("file", entry.file)
                    put("description", entry.description)
                    put("source", entry.source)
                    put("createdAt", entry.createdAt)
                },
            )
        }
        try {
            indexFile(context).writeText(array.toString())
        } catch (_: Throwable) {
            // The index did not write — the effect files are already on disk, and the next
            // install attempt rebuilds the index from scratch.
        }
    }

    /** The effect body from its JSON into the engine model; null on corruption or malformed shape. */
    private fun decodeBody(effectJson: String): CustomEffectUi? {
        val effect = try {
            CustomEffects.decode(JSONArray().put(JSONObject(effectJson))).firstOrNull()
        } catch (_: Throwable) {
            null
        } ?: return null
        if (CustomEffects.shapeError(effect) != null) return null
        return effect
    }

    /** File name from the display name: letters, digits and hyphen only. */
    private fun slugOf(name: String): String =
        name.lowercase()
            .map { if (it.isLetterOrDigit()) it else '-' }
            .joinToString("")
            .trim('-')
            .replace(Regex("-+"), "-")

    private fun uniqueFile(context: Context, slug: String): File {
        var candidate = File(dir(context), "$slug$EXT")
        var n = 2
        while (candidate.exists()) {
            candidate = File(dir(context), "$slug-$n$EXT")
            n += 1
        }
        return candidate
    }
}
