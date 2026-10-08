// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import android.content.Context
import com.kerneldroid.rumo.ui.ProjectStore
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * Templates: a project packed into a portable file so that it can be published
 * to the shop, downloaded and opened.
 *
 * ## A template is a project, not a second format
 *
 * The body (`project`) is exactly the JSON that [RumoBridge.projectToJson]
 * returns, and the import goes back through [RumoBridge.projectFromJson]. A
 * template has no description of layers of its own, so it cannot drift from the
 * project format: the project changes — the template changes. For the same
 * reason the body is not re-assembled through [JSONObject]: parsing and
 * re-assembly would reorder the keys and reformat the numbers, that is, a second
 * way of writing the same project would appear, and "verbatim" would stop being
 * verbatim.
 *
 * ## Untrusted input
 *
 * A template file comes from the internet, so [parse] checks the document kind,
 * the format version and the body bounds: without a ceiling on the size, a
 * hostile repository would make the app parse and copy the body without a limit.
 *
 * ## Import does not overwrite
 *
 * The import picks a free name (`Intro Bounce`, then `Intro Bounce 2`, …): a
 * template opened twice must not wipe an already edited project.
 */
object TemplateStore {
    const val EXT = ".rumotemplate"
    const val KIND = "rumo-template"

    private const val FORMAT_VERSION = 1

    /** The body ceiling in string characters: the document is already read into
     *  memory, and this limit keeps parsing and body copies from growing without
     *  a bound. */
    private const val MAX_BODY_CHARS = 8 * 1024 * 1024

    private const val DIR_NAME = "templates"

    /** One template: metadata plus the project JSON as is. */
    data class Template(
        val name: String,
        val description: String,
        val createdAt: Long,
        val projectJson: String,
    )

    /** Packs the JSON of an existing project into a template document. */
    fun wrap(
        name: String,
        description: String,
        createdBy: String,
        appVersionCode: Int,
        projectJson: String,
    ): String {
        // Concatenation, not JSONObject: the project body must remain exactly
        // the string the engine wrote. The other fields are escaped with quote.
        val sb = StringBuilder(projectJson.length + 256)
        sb.append("{\"kind\":").append(JSONObject.quote(KIND))
        sb.append(",\"formatVersion\":").append(FORMAT_VERSION)
        sb.append(",\"name\":").append(JSONObject.quote(name))
        sb.append(",\"description\":").append(JSONObject.quote(description))
        sb.append(",\"createdBy\":").append(JSONObject.quote(createdBy))
        sb.append(",\"appVersionCode\":").append(appVersionCode)
        sb.append(",\"createdAt\":").append(System.currentTimeMillis())
        sb.append(",\"project\":").append(projectJson)
        sb.append('}')
        return sb.toString()
    }

    /** Parses a template document; null if the kind/format is wrong or the body is broken. */
    fun parse(text: String): Template? {
        if (text.length > MAX_BODY_CHARS) return null
        val root = try {
            JSONObject(text)
        } catch (_: Throwable) {
            return null
        }
        if (root.optString("kind", "") != KIND) return null
        if (root.optInt("formatVersion", 0) != FORMAT_VERSION) return null
        val project = root.optJSONObject("project") ?: return null
        // An empty object is "no project": there is nothing to import from it.
        if (project.length() == 0) return null
        val body = rawTopLevelValue(text, "project") ?: return null
        if (body.length > MAX_BODY_CHARS) return null
        return Template(
            name = root.optString("name", ""),
            description = root.optString("description", ""),
            createdAt = root.optLong("createdAt", 0L),
            projectJson = body,
        )
    }

    /** Exports the project from [projectBytes] as a template file; null on failure. */
    suspend fun exportFromProject(
        context: Context,
        displayName: String,
        description: String,
        createdBy: String,
        appVersionCode: Int,
        projectBytes: ByteArray,
    ): File? = withContext(Dispatchers.IO) {
        val projectJson = RumoBridge.projectToJson(projectBytes) ?: return@withContext null
        val document = wrap(displayName, description, createdBy, appVersionCode, projectJson)
        // A free name: a repeated export must not overwrite a file that is still
        // being handed to sharing.
        val base = ProjectStore.sanitize(displayName)
        val target = uniqueFile(dir(context), base)
        try {
            target.writeText(document)
        } catch (t: Throwable) {
            AppLog.error("template", "export ${target.name} failed: ${AppLog.describe(t)}")
            return@withContext null
        }
        target
    }

    /** Imports a template into Projects. The file name it was saved under, or null. */
    suspend fun importToProjects(
        context: Context,
        template: Template,
        fileName: String,
    ): String? = withContext(Dispatchers.IO) {
        // Back through the same codec as an ordinary project: a template cannot
        // lag behind the .rumo format, because it is parsed by the same code.
        val bytes = RumoBridge.projectFromJson(template.projectJson) ?: return@withContext null
        val suggested = fileName
            .removeSuffix(EXT)
            .removeSuffix(ProjectStore.EXT)
        // 48 — with room for the " N" suffix: ProjectStore.sanitize cuts a name
        // at 64 characters, and a suffix cut off along with the tail would not
        // give uniqueness, and the name search would loop forever.
        val base = ProjectStore.sanitize(suggested).take(48)
        val taken = ProjectStore.list(context).map { it.displayName.lowercase() }.toHashSet()
        var candidate = base
        var n = 2
        while (candidate.lowercase() in taken) {
            candidate = "$base $n"
            n += 1
        }
        runCatching { ProjectStore.save(context, ProjectStore.fileNameFor(candidate), bytes) }
            .onFailure {
                AppLog.error("template", "import ${template.name} failed: ${AppLog.describe(it)}")
            }
            .getOrNull()
    }

    /** The directory of exported templates; created on first access. */
    private fun dir(context: Context): File =
        File(context.applicationContext.filesDir, DIR_NAME).apply { mkdirs() }

    /** A free file name: `Name`, then `Name-2`, … (as with fonts). */
    private fun uniqueFile(dir: File, base: String): File {
        var candidate = File(dir, base + EXT)
        var n = 2
        while (candidate.exists()) {
            candidate = File(dir, "$base-$n$EXT")
            n += 1
        }
        return candidate
    }
}

/**
 * The raw text of a top-level key's value, without parsing and re-assembly.
 *
 * [JSONObject] would re-assemble the project body and reorder the keys and
 * numbers, while a template must store exactly what the engine wrote. The scan
 * accounts for string escaping, so a `}` or a `"project"` inside a string value
 * does not cut the object short. Returns null if the document does not start
 * with an object or the parsing went wrong.
 */
private fun rawTopLevelValue(text: String, key: String): String? {
    var i = 0
    val n = text.length
    while (i < n && text[i].isWhitespace()) i++
    if (i >= n || text[i] != '{') return null
    i++
    var found: String? = null
    while (i < n) {
        while (i < n && (text[i].isWhitespace() || text[i] == ',')) i++
        if (i >= n || text[i] == '}') break
        if (text[i] != '"') return null
        val keyEnd = skipJsonValue(text, i) ?: return null
        // Keys like "project" are not escaped, so the comparison is on the raw text.
        val name = text.substring(i + 1, keyEnd - 1)
        i = keyEnd
        while (i < n && text[i].isWhitespace()) i++
        if (i >= n || text[i] != ':') return null
        i++
        while (i < n && text[i].isWhitespace()) i++
        val start = i
        val end = skipJsonValue(text, i) ?: return null
        i = end
        if (name == key) found = text.substring(start, end)
    }
    return found
}

/** The index right after the JSON value from [start], or null if the parsing went wrong. */
private fun skipJsonValue(text: String, start: Int): Int? {
    if (start >= text.length) return null
    return when (text[start]) {
        '"' -> {
            var i = start + 1
            while (i < text.length) {
                when (text[i]) {
                    // We skip an escaped character whole, otherwise `\"` would
                    // close the string and the whole further parse would go off
                    // the rails.
                    '\\' -> i += 2
                    '"' -> return i + 1
                    else -> i++
                }
            }
            null
        }
        '{', '[' -> {
            var depth = 0
            var i = start
            while (i < text.length) {
                when (text[i]) {
                    '"' -> i = skipJsonValue(text, i) ?: return null
                    '{', '[' -> {
                        depth++
                        i++
                    }
                    '}', ']' -> {
                        depth--
                        i++
                        if (depth == 0) return i
                    }
                    else -> i++
                }
            }
            null
        }
        else -> {
            var i = start
            while (i < text.length && text[i] != ',' && text[i] != '}' && text[i] != ']') i++
            if (i == start) null else i
        }
    }
}
