// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import org.json.JSONArray
import org.json.JSONObject

/**
 * A project-defined effect: a WGSL module plus the parameters its `Params`
 * struct declares.
 *
 * This is user data, not code in the binary. It travels with the project, is
 * checked by the engine before it is ever drawn with, and is deliberately
 * limited to a flat `f32` uniform block — the same shape every built-in effect
 * uses — so a bad submission cannot reach the driver as anything but a rejected
 * pipeline.
 *
 * The JSON below is the engine's contract; field names and the lowercase
 * parameter-kind names mirror `nativeEffectCatalogue()` and
 * `nativeEffectValidate()`, so one description serves both the panel and the
 * assistant.
 */
data class CustomEffectUi(
    val id: String,
    val label: String,
    /** `display` or `linear`, matching the built-in catalogue. */
    val space: String = "display",
    val passes: List<CustomPassUi>,
    val params: List<CustomParamUi>,
    val source: String,
) {
    fun toJson(): JSONObject {
        val passesJson = JSONArray()
        passes.forEach { passesJson.put(it.toJson()) }
        val paramsJson = JSONArray()
        params.forEach { paramsJson.put(it.toJson()) }
        return JSONObject()
            .put("id", id)
            .put("label", label)
            .put("space", space)
            .put("passes", passesJson)
            .put("params", paramsJson)
            .put("source", source)
    }
}

/** One render pass of a [CustomEffectUi]. */
data class CustomPassUi(
    /** `@fragment` entry point name in the module. */
    val entry: String,
    /** Downscale exponent for this pass; 0 keeps full resolution. */
    val shrink: Int = 0,
) {
    fun toJson(): JSONObject = JSONObject().put("entry", entry).put("shrink", shrink)
}

/** One parameter of a [CustomEffectUi], in `Params` declaration order. */
data class CustomParamUi(
    val key: String,
    val label: String,
    /** `float`, `angle`, `int`, `bool`, `choice` or `color`. */
    val kind: String = "float",
    val min: Float = 0f,
    val max: Float = 1f,
    /** Four values; only the first `slots` are meaningful. */
    val default: List<Float> = listOf(1f, 0f, 0f, 0f),
    val unit: String = "",
    val choices: List<String> = emptyList(),
) {
    /**
     * How many `f32` this parameter occupies. A scalar is one; `color` is four
     * *separate* struct members (`key_r/_g/_b/_a`, the shape the built-ins use);
     * the vector and matrix kinds are one member each and take as many values as
     * they have components. This mapping must match `ParamKind` in Rust — if it
     * drifts, the panel edits values at the wrong offsets.
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

    fun toJson(): JSONObject {
        val defaults = JSONArray()
        // As many defaults as the kind has components: a mat4 carries sixteen,
        // and truncating to four would hand the engine a block it cannot fill.
        for (i in 0 until slots) defaults.put(default.getOrElse(i) { 0f }.toDouble())
        val choicesJson = JSONArray()
        choices.forEach { choicesJson.put(it) }
        return JSONObject()
            .put("key", key)
            .put("label", label)
            .put("kind", kind.lowercase())
            .put("min", min.toDouble())
            .put("max", max.toDouble())
            .put("default", defaults)
            .put("unit", unit)
            .put("choices", choicesJson)
    }
}

/** Reading, writing and pre-checking project-defined effects. */
object CustomEffects {
    /** Parameter kinds the engine accepts, for the authoring hints. */
    val KINDS = listOf(
        "float", "angle", "int", "bool", "choice", "color",
        "vec2", "vec3", "vec4", "mat3", "mat4",
    )

    /** JSON array in the engine's `custom` form. */
    fun encode(list: List<CustomEffectUi>): JSONArray {
        val out = JSONArray()
        list.forEach { out.put(it.toJson()) }
        return out
    }

    fun decode(array: JSONArray?): List<CustomEffectUi> {
        if (array == null) return emptyList()
        val out = ArrayList<CustomEffectUi>(array.length())
        val seen = HashSet<String>()
        for (i in 0 until array.length()) {
            val o = array.optJSONObject(i) ?: continue
            val id = o.optString("id", "")
            if (id.isEmpty() || !seen.add(id)) continue
            val passes = ArrayList<CustomPassUi>()
            o.optJSONArray("passes")?.let { ps ->
                for (j in 0 until ps.length()) {
                    val p = ps.optJSONObject(j) ?: continue
                    val entry = p.optString("entry", "")
                    if (entry.isEmpty()) continue
                    passes += CustomPassUi(entry, p.optInt("shrink", 0))
                }
            }
            val params = ArrayList<CustomParamUi>()
            o.optJSONArray("params")?.let { ps ->
                for (j in 0 until ps.length()) {
                    val p = ps.optJSONObject(j) ?: continue
                    val key = p.optString("key", "")
                    if (key.isEmpty()) continue
                    val defaults = ArrayList<Float>(16)
                    p.optJSONArray("default")?.let { ds ->
                        for (k in 0 until minOf(16, ds.length())) {
                            defaults += ds.optDouble(k, 0.0).toFloat()
                        }
                    }
                    val choices = ArrayList<String>()
                    p.optJSONArray("choices")?.let { cs ->
                        for (k in 0 until cs.length()) choices += cs.optString(k, "")
                    }
                    params += CustomParamUi(
                        key = key,
                        label = p.optString("label", key),
                        kind = p.optString("kind", "float").lowercase(),
                        min = p.optDouble("min", 0.0).toFloat(),
                        max = p.optDouble("max", 0.0).toFloat(),
                        default = defaults,
                        unit = p.optString("unit", ""),
                        choices = choices,
                    )
                }
            }
            out += CustomEffectUi(
                id = id,
                label = o.optString("label", id),
                space = o.optString("space", "display"),
                passes = passes,
                params = params,
                source = o.optString("source", ""),
            )
        }
        return out
    }

    /**
     * The checks that can be made without a WGSL front end.
     *
     * The engine re-runs all of these and then compiles the module, so this is
     * only here to give the author a message before a round trip. Returns null
     * when everything looks well-formed.
     */
    fun shapeError(effect: CustomEffectUi): String? {
        if (!effect.id.matches(Regex("[a-z0-9_]{2,64}"))) {
            return "id must be 2-64 characters of a-z, 0-9 or _"
        }
        if (effect.label.isBlank()) return "label must not be blank"
        if (effect.source.isBlank()) return "source must not be blank"
        if (effect.passes.isEmpty()) return "declare at least one pass"
        if (effect.passes.size > 8) return "at most eight passes"
        val entries = HashSet<String>()
        effect.passes.forEach { pass ->
            if (!pass.entry.matches(Regex("[A-Za-z_][A-Za-z0-9_]*"))) {
                return "pass entry `${pass.entry}` is not a WGSL identifier"
            }
            if (!entries.add(pass.entry)) return "pass entry `${pass.entry}` is declared twice"
            // The engine's own limit is 6 (1/64); a tighter one here would have
            // refused a pass the engine would happily run.
            if (pass.shrink !in 0..6) return "pass `${pass.entry}` shrink must be 0-6"
        }
        val keys = HashSet<String>()
        effect.params.forEach { param ->
            if (!param.key.matches(Regex("[A-Za-z_][A-Za-z0-9_]*"))) {
                return "parameter `${param.key}` is not a WGSL identifier"
            }
            if (!keys.add(param.key)) return "parameter `${param.key}` is declared twice"
            if (param.kind.lowercase() !in KINDS) return "parameter `${param.key}` has kind `${param.kind}`"
            if (param.min > param.max) return "parameter `${param.key}` has min above max"
            if (param.kind.equals("choice", true) && param.choices.isEmpty()) {
                return "parameter `${param.key}` is a choice with no options"
            }
            for (i in param.slots until param.default.size) {
                if (param.default[i] != 0f) {
                    return "parameter `${param.key}` of kind `${param.kind}` carries " +
                        "${param.default.size} values, and component $i is not used"
                }
            }
        }
        if (effect.params.sumOf { it.slots } > 128) return "at most 128 parameter slots"
        return null
    }
}
