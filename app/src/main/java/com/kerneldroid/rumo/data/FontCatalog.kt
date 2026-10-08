// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * Cache of the Google Fonts catalog.
 *
 * The catalog weighs 2.7 MB and changes once every few weeks, so it is kept on
 * disk in a trimmed form (only the fields the list shows — about 150 KB)
 * and reloaded when it expires. Without the cache, every opening of the fonts tab
 * would pull almost three megabytes.
 *
 * The expiry is deliberately long: catalog freshness is not worth the traffic, and a new
 * family will be seen after the cache refreshes anyway.
 */
object FontCatalog {
    private const val DIR = "shop"
    private const val FILE = "fonts-catalog.json"

    /** After how many milliseconds the catalog is considered stale. */
    const val TTL_MS = 30L * 24 * 60 * 60 * 1000

    /** A catalog snapshot together with the moment it was loaded. */
    data class Snapshot(val families: List<GoogleFonts.Family>, val fetchedAt: Long)

    @Volatile
    private var memory: Snapshot? = null

    private fun file(context: Context): File =
        File(File(context.applicationContext.filesDir, DIR).apply { mkdirs() }, FILE)

    /** The in-memory snapshot, if it has already been loaded in this process. */
    fun cached(): Snapshot? = memory

    /**
     * The catalog: from memory, from disk, and when stale — from the network.
     *
     * `null` means there is no catalog at all (the network is unavailable and there was no cache).
     * A stale cache is returned when the network is unavailable: showing a
     * month-old list is better than an empty screen.
     */
    suspend fun load(context: Context, force: Boolean = false): Snapshot? {
        val now = System.currentTimeMillis()
        val inMemory = memory
        if (!force && inMemory != null && now - inMemory.fetchedAt < TTL_MS) return inMemory
        return withContext(Dispatchers.IO) {
            val disk = readDisk(context)
            if (!force && disk != null && now - disk.fetchedAt < TTL_MS) {
                memory = disk
                return@withContext disk
            }
            val fetched = GoogleFonts.fetchCatalog()
            if (fetched == null || fetched.isEmpty()) {
                val fallback = disk ?: inMemory
                if (fallback != null) memory = fallback
                return@withContext fallback
            }
            val snapshot = Snapshot(fetched, now)
            writeDisk(context, snapshot)
            memory = snapshot
            snapshot
        }
    }

    private fun readDisk(context: Context): Snapshot? {
        val f = file(context)
        if (!f.isFile) return null
        return try {
            val o = JSONObject(f.readText())
            val array = o.optJSONArray("families") ?: return null
            val families = ArrayList<GoogleFonts.Family>(array.length())
            for (i in 0 until array.length()) {
                val e = array.optJSONObject(i) ?: continue
                val name = e.optString("n")
                if (name.isEmpty()) continue
                families += GoogleFonts.Family(
                    name = name,
                    category = e.optString("c"),
                    subsets = e.optJSONArray("s").toStringList(),
                    popularity = e.optInt("p"),
                    weights = e.optJSONArray("w").toIntList(),
                    hasItalic = e.optBoolean("i", false),
                )
            }
            if (families.isEmpty()) null else Snapshot(families, o.optLong("fetchedAt", 0L))
        } catch (_: Throwable) {
            null
        }
    }

    private fun writeDisk(context: Context, snapshot: Snapshot) {
        val array = JSONArray()
        for (f in snapshot.families) {
            array.put(
                JSONObject()
                    .put("n", f.name)
                    .put("c", f.category)
                    .put("s", JSONArray(f.subsets))
                    .put("p", f.popularity)
                    .put("w", JSONArray(f.weights))
                    .put("i", f.hasItalic),
            )
        }
        val o = JSONObject().put("fetchedAt", snapshot.fetchedAt).put("families", array)
        try {
            file(context).writeText(o.toString())
        } catch (_: Throwable) {
            // The cache was not written — the catalog stays in memory until the end of the process,
            // and the next load will simply repeat the request.
        }
    }

    private fun JSONArray?.toStringList(): List<String> {
        if (this == null) return emptyList()
        val out = ArrayList<String>(length())
        for (i in 0 until length()) out += optString(i, "")
        return out
    }

    private fun JSONArray?.toIntList(): List<Int> {
        if (this == null) return emptyList()
        val out = ArrayList<Int>(length())
        for (i in 0 until length()) out += optInt(i, 0)
        return out
    }
}
