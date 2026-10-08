// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import android.util.JsonReader
import com.kerneldroid.aiengines.rumi.RumiHttp
import java.io.StringReader

/**
 * The Google Fonts catalogue: the list of families and the download of a
 * concrete face.
 *
 * ## Where the data comes from
 *
 * The list is `fonts.google.com/metadata/fonts`, the same response the Google
 * Fonts site itself uses. It has the name, the category, the subsets and the set
 * of weights, but **no files and no licence**.
 *
 * The files live in `github.com/google/fonts`, laid out by licence directory:
 * `ofl/`, `apache/`, `ufl/`. Inside a family's directory sits `METADATA.pb` —
 * a text protobuf that gives `style`, `weight`, `filename` and `copyright` for
 * each face, and `license` at the top. It is what answers "which file to take":
 * the first `filename:` in the file is **not** the regular one (for Fira Sans it
 * is `FiraSans-Thin.ttf`, for Ubuntu `Ubuntu-Light.ttf`), so a face is chosen by
 * `weight: 400` + `style: "normal"`.
 *
 * A family's directory in the repository is the name lowercased and with spaces
 * removed (`Open Sans` → `opensans`, `M PLUS 1p` → `mplus1p`). The licence
 * directory is not visible in the metadata, so it is determined by probing the
 * three buckets; the probes go through `raw.githubusercontent.com`, which does
 * not spend GitHub API quota.
 *
 * All functions are blocking: do not call them on the main thread.
 */
object GoogleFonts {
    const val CATALOG_URL = "https://fonts.google.com/metadata/fonts"

    private const val RAW_ROOT = "https://raw.githubusercontent.com/google/fonts/main"

    /**
     * The `google/fonts` buckets in order of likelihood: `ofl` is almost the
     * whole catalogue, `apache` and `ufl` are a handful of families. The order
     * is the optimisation: for 97% of families one probe is enough.
     */
    private val BUCKETS = listOf("ofl", "apache", "ufl")

    /** The licence file name inside a family's directory, per bucket. */
    internal fun licenseFile(bucket: String): String = when (bucket) {
        "ufl" -> "UFL.txt"
        "apache" -> "LICENSE.txt"
        else -> "OFL.txt"
    }

    /** One family from the Google Fonts catalogue. */
    data class Family(
        val name: String,
        val category: String,
        val subsets: List<String>,
        val popularity: Int,
        val weights: List<Int>,
        val hasItalic: Boolean,
    ) {
        /** The family's directory in `google/fonts`. */
        val dir: String get() = name.lowercase().replace(" ", "")

        /** Whether Latin is present — the "show only Latin" filter. */
        val latin: Boolean get() = subsets.contains("latin")

    }

    /** A concrete face: the file that can be downloaded. */
    data class Face(
        val family: String,
        val bucket: String,
        val dir: String,
        val fileName: String,
        val weight: Int,
        val license: String,
        val copyright: String,
    ) {
        /**
         * A direct link to the file.
         *
         * The name is percent-encoded: variable fonts have `[`, `]` and `,` in
         * their name (`Inter[opsz,wght].ttf`), and the server rejects an
         * unencoded path as an invalid URL.
         */
        val rawUrl: String get() = "$RAW_ROOT/$bucket/$dir/${encodePathSegment(fileName)}"
    }

    /**
     * The list of families, or null if the request failed.
     *
     * The response is 2.7 MB, so it is parsed as a stream ([JsonReader]): a
     * `JSONObject` tree for 1950 families with nested variants would cost tens of
     * megabytes at peak, while only four fields are needed from it.
     */
    fun fetchCatalog(timeoutMs: Int = 60_000): List<Family>? {
        val reply = RumiHttp.getJson(CATALOG_URL, emptyMap(), timeoutMs)
        if (!reply.ok) return null
        return try {
            parseCatalog(reply.body)
        } catch (_: Throwable) {
            null
        }
    }

    /** Parsing of the catalogue response. Split out of [fetchCatalog] for testability. */
    internal fun parseCatalog(body: String): List<Family> {
        val out = ArrayList<Family>(2100)
        JsonReader(StringReader(body)).use { reader ->
            reader.beginObject()
            while (reader.hasNext()) {
                when (reader.nextName()) {
                    "familyMetadataList" -> {
                        reader.beginArray()
                        while (reader.hasNext()) {
                            readFamily(reader)?.let { out += it }
                        }
                        reader.endArray()
                    }
                    else -> reader.skipValue()
                }
            }
            reader.endObject()
        }
        return out
    }

    private fun readFamily(reader: JsonReader): Family? {
        var name = ""
        var category = ""
        var popularity = 0
        var italic = false
        val subsets = ArrayList<String>(4)
        val weights = ArrayList<Int>(4)
        reader.beginObject()
        while (reader.hasNext()) {
            when (reader.nextName()) {
                "family" -> name = reader.nextString()
                "category" -> category = reader.nextString()
                "popularity" -> popularity = reader.nextInt()
                "subsets" -> {
                    reader.beginArray()
                    while (reader.hasNext()) subsets += reader.nextString()
                    reader.endArray()
                }
                // The variant keys are a weight with an optional `i` ("400",
                // "400i"), and nothing else from this object is needed.
                "fonts" -> {
                    reader.beginObject()
                    while (reader.hasNext()) {
                        val key = reader.nextName()
                        reader.skipValue()
                        key.filter { it.isDigit() }.toIntOrNull()?.let { w ->
                            if (!weights.contains(w)) weights += w
                        }
                        if (key.endsWith("i")) italic = true
                    }
                    reader.endObject()
                }
                else -> reader.skipValue()
            }
        }
        reader.endObject()
        if (name.isEmpty()) return null
        return Family(
            name = name,
            category = category,
            subsets = subsets,
            popularity = popularity,
            weights = weights.sorted(),
            hasItalic = italic,
        )
    }

    /**
     * Finds a family's face: probing the buckets, then parsing `METADATA.pb`.
     * null — if the directory is in none of the buckets.
     */
    fun resolveFace(family: Family): Face? {
        for (bucket in BUCKETS) {
            val url = "$RAW_ROOT/$bucket/${encodePathSegment(family.dir)}/METADATA.pb"
            val reply = try {
                RumiHttp.getJson(url, emptyMap(), 20_000, accept = "*/*")
            } catch (_: Throwable) {
                continue
            }
            if (!reply.ok) continue
            return parseMetadata(reply.body, family, bucket)
        }
        return null
    }

    /**
     * Parsing `METADATA.pb`: the face with `weight: 400` and `style: "normal"`,
     * or, if there is none, the nearest by weight.
     *
     * The nearest, specifically, and not the first one to hand: for Fira Sans the
     * first block is Thin (100), for Ubuntu it is Light (300), and previewing a
     * "regular" font in a thin weight would be a lie about what the user is going
     * to download.
     */
    internal fun parseMetadata(body: String, family: Family, bucket: String): Face? {
        var license = ""
        var curStyle = ""
        var curWeight = 400
        var curFile = ""
        var curCopyright = ""
        var inFace = false
        var bestWeight = 0
        var bestFile = ""
        var bestCopyright = ""
        var bestIsRegular = false
        var bestDistance = Int.MAX_VALUE
        var bestStyle = ""

        fun consider() {
            if (curFile.isEmpty()) return
            val isRegular = curStyle == "normal" && curWeight == 400
            val distance = kotlin.math.abs(curWeight - 400)
            val take = when {
                bestFile.isEmpty() -> true
                // A direct hit on regular beats anything that is not regular.
                isRegular != bestIsRegular -> isRegular
                distance != bestDistance -> distance < bestDistance
                else -> curStyle == "normal" && bestStyle != "normal"
            }
            if (take) {
                bestFile = curFile
                bestWeight = curWeight
                bestCopyright = curCopyright
                bestIsRegular = isRegular
                bestDistance = distance
                bestStyle = curStyle
            }
        }

        for (raw in body.lineSequence()) {
            val line = raw.trim()
            if (!inFace) {
                when {
                    line == "fonts {" -> {
                        inFace = true
                        curStyle = ""
                        curWeight = 400
                        curFile = ""
                        curCopyright = ""
                    }
                    line.startsWith("license:") -> license = pbValue(line)
                }
                continue
            }
            if (line == "}") {
                consider()
                inFace = false
                continue
            }
            when {
                line.startsWith("style:") -> curStyle = pbValue(line)
                line.startsWith("weight:") -> curWeight = pbValue(line).toIntOrNull() ?: 400
                line.startsWith("filename:") -> curFile = pbValue(line)
                line.startsWith("copyright:") -> curCopyright = pbValue(line)
            }
        }
        if (bestFile.isEmpty()) return null
        return Face(
            family = family.name,
            bucket = bucket,
            dir = family.dir,
            fileName = bestFile,
            weight = bestWeight,
            license = license,
            copyright = bestCopyright,
        )
    }

    /** The licence text next to the family, or null if the file is not there. */
    fun licenseText(face: Face): String? {
        val url =
            "$RAW_ROOT/${face.bucket}/${encodePathSegment(face.dir)}/${licenseFile(face.bucket)}"
        val reply = try {
            RumiHttp.getJson(url, emptyMap(), 20_000, accept = "*/*")
        } catch (_: Throwable) {
            return null
        }
        return if (reply.ok) reply.body else null
    }

    /**
     * Downloads the face, refusing a file larger than [limitBytes] (0 — no
     * limit). `tooLarge` distinguishes "too large" from "failed to download":
     * the engine cannot parse a silently truncated font, and the preview would
     * show emptiness instead of an explanation.
     */
    fun download(face: Face, limitBytes: Long = 0L): RumiHttp.BytesReply = try {
        RumiHttp.getBytes(face.rawUrl, emptyMap(), limitBytes, 60_000, accept = "*/*")
    } catch (_: Throwable) {
        RumiHttp.BytesReply(0, ByteArray(0))
    }

    /** The value of a `key: "value"` field from `METADATA.pb`. */
    private fun pbValue(line: String): String =
        line.substringAfter(':', "").trim().trim('"')

    /** Percent-encoding of a single path segment. */
    internal fun encodePathSegment(segment: String): String {
        val hex = "0123456789ABCDEF"
        val out = StringBuilder(segment.length + 8)
        for (byte in segment.toByteArray(Charsets.UTF_8)) {
            val code = byte.toInt() and 0xFF
            val ch = code.toChar()
            val safe = (code < 128 && (ch.isLetterOrDigit() || ch in "-_.~"))
            if (safe) {
                out.append(ch)
            } else {
                out.append('%').append(hex[code ushr 4]).append(hex[code and 0x0F])
            }
        }
        return out.toString()
    }
}
