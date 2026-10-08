// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * What has been installed from the shop: the index `filesDir/shop/installs.json`.
 *
 * ## Why a separate index
 *
 * [FontStore], [EffectStore] and [TemplateStore] know the **content**, but do not
 * remember where it came from: a font has a family name, an effect has a name, and from
 * them you cannot tell which repository the item was downloaded from and which version
 * it was. And for checking updates you need exactly that pair — `(repo, itemId)` — and
 * the version at install time. So the provenance is stored separately, and this
 * is the only place that owns it.
 *
 * ## One item — one record
 *
 * The key is `(repo, itemId)`, not the name: reinstalling the same item replaces
 * the record ([record]), otherwise duplicates of one install would pile up in the index.
 * Deleting the content (font, effect, template) does not by itself touch the record —
 * the index only answers the question "what was installed and when".
 *
 * ## Flow
 *
 * Reading ([all]) is a small file, so the function is simple and blocking, like
 * `installed` on [FontStore]. [find] and [ofKind] are thin wrappers over it and do no
 * disk work of their own. The mutating functions ([record], [forget],
 * [forgetRepo]) write to disk and therefore suspend on [Dispatchers.IO].
 * Mutations are serialised: read-modify-write is not an atomic operation,
 * and without a lock two simultaneous installs would lose one record.
 *
 * A corrupt or missing index reads as an empty list rather than a crash:
 * the content on disk is intact, and the next install will rebuild the index.
 */
object ShopInstalls {
    /**
     * One item installed from a shop repository.
     *
     * [version] is the version from the manifest **at the moment of install**, not the current
     * one: it is exactly what the tag of a fresh release is later compared against to see whether
     * there is an update.
     */
    data class Record(
        /** The repository as `owner/name`. */
        val repo: String,
        /** The item id from the manifest. */
        val itemId: String,
        val name: String,
        /** The item kind: `RepoRules.Kind.name`. */
        val kind: String,
        val version: String,
        val installedAt: Long,
    )

    private const val DIR = "shop"
    private const val INDEX = "installs.json"

    /** The lock for read-modify-write of the index. */
    private val lock = Any()

    /**
     * An in-memory snapshot of the index. The mutating functions update it, so
     * repeated [all] calls do not re-read the disk. `@Volatile` — so the write is
     * visible to other threads without extra locks on reading.
     */
    @Volatile
    private var cache: List<Record>? = null

    /** The shop directory; created on first access. */
    private fun dir(context: Context): File =
        File(context.applicationContext.filesDir, DIR).apply { mkdirs() }

    private fun indexFile(context: Context): File = File(dir(context), INDEX)

    /**
     * All install records, freshest first.
     *
     * Reads a small file synchronously (like [FontStore.installed]); a corrupt index
     * gives an empty list rather than an exception.
     */
    fun all(context: Context): List<Record> {
        cache?.let { return it }
        val read = readIndex(context)
        cache = read
        return read
    }

    /**
     * Adds or replaces the record for `(repo, itemId)`: one item — one
     * record.
     */
    suspend fun record(context: Context, record: Record) {
        withContext(Dispatchers.IO) {
            synchronized(lock) {
                val current = readIndex(context)
                val updated = listOf(record) +
                    current.filterNot { it.repo == record.repo && it.itemId == record.itemId }
                writeIndex(context, updated)
                cache = updated
            }
        }
    }

    /** Forgets an item. `true` if the record existed and was removed. */
    suspend fun forget(context: Context, repo: String, itemId: String): Boolean =
        withContext(Dispatchers.IO) {
            synchronized(lock) {
                val current = readIndex(context)
                val updated = current.filterNot { it.repo == repo && it.itemId == itemId }
                if (updated.size == current.size) {
                    false
                } else {
                    writeIndex(context, updated)
                    cache = updated
                    true
                }
            }
        }

    /** Forgets all items of a repository. `true` if anything was removed. */
    suspend fun forgetRepo(context: Context, repo: String): Boolean =
        withContext(Dispatchers.IO) {
            synchronized(lock) {
                val current = readIndex(context)
                val updated = current.filterNot { it.repo == repo }
                if (updated.size == current.size) {
                    false
                } else {
                    writeIndex(context, updated)
                    cache = updated
                    true
                }
            }
        }

    /** The record of an item, or null if it was never installed. */
    fun find(context: Context, repo: String, itemId: String): Record? =
        all(context).firstOrNull { it.repo == repo && it.itemId == itemId }

    /** Installed records of the given kind. */
    fun ofKind(context: Context, kind: RepoRules.Kind): List<Record> =
        all(context).filter { RepoRules.Kind.ofName(it.kind) == kind }

    private fun readIndex(context: Context): List<Record> {
        val file = indexFile(context)
        if (!file.isFile) return emptyList()
        return try {
            val array = JSONArray(file.readText())
            (0 until array.length()).mapNotNull { i ->
                val o = array.optJSONObject(i) ?: return@mapNotNull null
                val repo = o.optString("repo")
                val itemId = o.optString("itemId")
                // A record without a key is useless: you can neither find the item by it
                // nor match it to an update.
                if (repo.isEmpty() || itemId.isEmpty()) return@mapNotNull null
                Record(
                    repo = repo,
                    itemId = itemId,
                    name = o.optString("name", itemId),
                    kind = o.optString("kind", ""),
                    version = o.optString("version", ""),
                    installedAt = o.optLong("installedAt", 0L),
                )
            }
        } catch (_: Throwable) {
            emptyList()
        }
    }

    private fun writeIndex(context: Context, entries: List<Record>) {
        val array = JSONArray()
        for (entry in entries) {
            array.put(
                JSONObject().apply {
                    put("repo", entry.repo)
                    put("itemId", entry.itemId)
                    put("name", entry.name)
                    put("kind", entry.kind)
                    put("version", entry.version)
                    put("installedAt", entry.installedAt)
                },
            )
        }
        try {
            indexFile(context).writeText(array.toString())
        } catch (_: Throwable) {
            // The index was not written — the shop content is already on disk, and
            // the next install will rebuild the index; failing the
            // install over this would be worse.
        }
    }
}
