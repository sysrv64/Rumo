// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

// Storage for the manual node layout of the fullscreen graph, with no new deps:
// SharedPreferences "rumo_node_layout", a single String — a JSON object
// key ("$layerId/$stage") -> [x, y]. The key is globally unique, since layerId is a UUID.
object NodeLayoutRepo {
    private const val PREFS = "rumo_node_layout"
    private const val KEY_LAYOUT = "layout"

    @Volatile
    private var inited = false
    private var prefs: SharedPreferences? = null

    fun init(context: Context) {
        if (inited) return
        inited = true
        prefs = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    }

    /** Saved layout. Empty if the storage is not initialized, nothing has been
     *  written, or the JSON is corrupt — we do not propagate exceptions. */
    fun read(): Map<String, Pair<Float, Float>> {
        val raw = prefs?.getString(KEY_LAYOUT, null) ?: return emptyMap()
        return try {
            val obj = JSONObject(raw)
            val out = LinkedHashMap<String, Pair<Float, Float>>()
            for (key in obj.keys()) {
                val arr = obj.optJSONArray(key) ?: continue
                if (arr.length() < 2) continue
                val x = arr.optDouble(0, Double.NaN)
                val y = arr.optDouble(1, Double.NaN)
                if (x.isNaN() || y.isNaN()) continue
                out[key] = x.toFloat() to y.toFloat()
            }
            out
        } catch (_: JSONException) {
            emptyMap()
        }
    }

    fun write(layout: Map<String, Pair<Float, Float>>) {
        val p = prefs ?: return
        val obj = JSONObject()
        for ((key, pos) in layout) {
            obj.put(key, JSONArray().put(pos.first.toDouble()).put(pos.second.toDouble()))
        }
        p.edit().putString(KEY_LAYOUT, obj.toString()).apply()
    }
}
