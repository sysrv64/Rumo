// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * Update checking for what was installed from the shop.
 *
 * ## What happens here
 *
 * [ShopInstalls] remembers which items and which versions the user installed.
 * Once per check, the fresh release of each repository is fetched
 * ([GitHubApi.latestRelease]) and compared with the installed version: the
 * release tag is the version available. An update is shown **only** for items
 * the user actually installed — by the pair `(repo, itemId)`. Inventing an
 * update for something the user does not have would mean offering to download
 * who knows what.
 *
 * Hence two consequences baked into the design:
 *
 * * a repository is queried **once**, not once per item: the records are
 *   grouped by repository, and [GitHubApi.latestRelease] is called per group. A
 *   repository has one common release tag, so the number of requests equals the
 *   number of repositories, not the number of installed items;
 * * [check] is exactly one pass over the repositories. No wait loop, no threads
 *   of its own: the function is blocking, so it is not called on the main
 *   thread.
 *
 * ## Errors are a value, not an exception
 *
 * Neither [check] nor [checkIfDue] throws. A failure on one repository (no
 * network, repository deleted, GitHub limit) does not cancel the result for the
 * others: the updates of the repositories that answered are returned, and the
 * first reason is recorded in [Outcome.error].
 *
 * ## Throttling
 *
 * The "is it time to check" policy lives entirely in [ShopPrefs] — it is not
 * duplicated here. [check] does not know about it at all: it is handed a token
 * and honestly makes one pass. [checkIfDue] is the only place that asks
 * [ShopPrefs.isCheckDue] and marks the check via [ShopPrefs.markChecked].
 *
 * ## Cache
 *
 * The last outcome lives in `filesDir/shop/updates.json`, so the "update
 * available" badge survives a restart without a new network call. A broken cache
 * reads as "no outcome".
 */
object ShopUpdates {
    /**
     * An available update: which exact installation is out of date and up to
     * what.
     *
     * [installedVersion] is the version from the install record,
     * [availableVersion] is the version from the fresh release tag (without the
     * leading `v`).
     */
    data class Update(
        val repo: String,
        val itemId: String,
        val name: String,
        val kind: RepoRules.Kind,
        val installedVersion: String,
        val availableVersion: String,
        val releaseTag: String,
        val releaseName: String,
        val notes: String,
    )

    /** The outcome of one pass. */
    data class Outcome(
        val updates: List<Update>,
        /** How many repositories were actually queried (including those that did not answer). */
        val checkedRepos: Int,
        /** The first failure reason, or null if everything answered. */
        val error: String?,
    )

    private const val DIR = "shop"
    private const val FILE = "updates.json"

    /**
     * The exact reason from [GitHubApi.latestRelease] when the repository has no
     * stable release. This is not a check failure: there is simply nothing to
     * update to, so it does not get into [Outcome.error]. The string is repeated
     * literally — GitHubApi does not export it.
     */
    private const val NO_RELEASE = "no suitable release"

    /** A version from a tag: digits and dots. Everything else does not count as a number. */
    private val NUMERIC_VERSION = Regex("""^\d+(\.\d+)*$""")

    /** A snapshot of the last outcome in memory, so [lastOutcome] does not read the disk. */
    @Volatile
    private var memory: Outcome? = null

    private fun file(context: Context): File =
        File(File(context.applicationContext.filesDir, DIR).apply { mkdirs() }, FILE)

    /**
     * Checks every repository with installed items for a fresh release.
     *
     * Blocking: it makes network requests, so it must not be called on the main
     * thread. It does not think about whether it is time to check — that is the
     * caller's business (usually through [checkIfDue]) and [ShopPrefs].
     */
    fun check(context: Context, token: String?): Outcome {
        val records = ShopInstalls.all(context)
        if (records.isEmpty()) {
            // Nothing to check: without installations there are no updates either.
            val empty = Outcome(emptyList(), 0, null)
            cache(context, empty)
            return empty
        }

        // Grouping by repository: one release per repository, not per item. The
        // order is preserved so that the "first error" is stable.
        val grouped = LinkedHashMap<String, MutableList<ShopInstalls.Record>>()
        for (record in records) {
            grouped.getOrPut(record.repo) { ArrayList() } += record
        }

        val updates = ArrayList<Update>()
        var checked = 0
        var firstError: String? = null

        for ((repo, installed) in grouped) {
            val slash = repo.indexOf('/')
            val owner = if (slash > 0) repo.substring(0, slash) else ""
            val name = if (slash >= 0) repo.substring(slash + 1) else ""
            if (owner.isEmpty() || name.isEmpty() || name.contains('/')) {
                // A record with an unparsed repository name is a corrupted
                // index, not a network failure; we skip the item entirely.
                if (firstError == null) firstError = "unparsed repository: $repo"
                continue
            }

            checked += 1
            val found = GitHubApi.latestRelease(owner, name, token)
            val release = found.value
            if (release == null) {
                val reason = found.error
                // "No release" is not an error: there is nothing to compare
                // against, so there are no updates.
                if (reason != null && reason != NO_RELEASE && firstError == null) {
                    firstError = reason
                }
                continue
            }

            // The tag is the source of truth for the version, but only if it is
            // a version: a tag like `release-2024` has nothing to compare
            // against, and any of its differences from the installed version
            // counts as an update (see [isNewer]).
            val available = stripVersionPrefix(release.tagName)
            if (available.isEmpty()) continue
            for (item in installed) {
                val kind = RepoRules.Kind.ofName(item.kind) ?: continue
                if (!isNewer(available, item.version)) continue
                updates += Update(
                    repo = repo,
                    itemId = item.itemId,
                    name = item.name,
                    kind = kind,
                    installedVersion = item.version,
                    availableVersion = available,
                    releaseTag = release.tagName,
                    releaseName = release.name.ifEmpty { release.tagName },
                    notes = release.body,
                )
            }
        }

        val outcome = Outcome(updates, checked, firstError)
        cache(context, outcome)
        return outcome
    }

    /**
     * Checks only if it is due, and marks the check done.
     *
     * Safe to call on every launch: the decision about the timing is made by
     * [ShopPrefs.isCheckDue] (which also takes into account whether the check is
     * enabled at all), and the mark is set by [ShopPrefs.markChecked]. These two
     * calls are all that [ShopUpdates] knows about settings.
     *
     * `null` means the check did not run because it was not needed.
     */
    fun checkIfDue(context: Context, token: String?): Outcome? {
        if (!ShopPrefs.isCheckDue()) return null
        val outcome = check(context, token)
        // We mark it even on a failure: otherwise, with a dead network, the app
        // would try to reach GitHub on every launch.
        ShopPrefs.markChecked()
        return outcome
    }

    /** The last saved outcome, or null if there have been no checks yet. */
    fun lastOutcome(context: Context): Outcome? {
        memory?.let { return it }
        val read = readCache(context) ?: return null
        memory = read
        return read
    }

    /** Forgets the cache of the last outcome: the badge goes dark until the next check. */
    fun clear(context: Context) {
        memory = null
        try {
            val f = file(context)
            if (!f.delete() && f.isFile) {
                // Deletion failed — write an empty outcome so the badge goes
                // dark anyway instead of showing yesterday's updates.
                writeCache(context, Outcome(emptyList(), 0, null))
                memory = null
            }
        } catch (_: Throwable) {
            // Not cleaned up is not a reason to crash; the next check will rewrite it.
        }
    }

    /**
     * Has the installed version been updated.
     *
     * Versions cannot be compared as strings: `"1.10.0" < "1.9.0"` character by
     * character, because in the second component `'1' < '9'`, even though 10 is
     * greater than 9. So, if both versions are numbers separated by dots, they
     * are compared component by component as numbers (the shorter version is
     * padded with zeros: `1.2` == `1.2.0`). If at least one version is not
     * numeric (a tag like `nightly`, `release-2024`), there is no order — then
     * the inequality rule applies: any difference counts as an update.
     */
    internal fun isNewer(available: String, installed: String): Boolean {
        val a = stripVersionPrefix(available)
        val b = stripVersionPrefix(installed)
        if (a == b) return false
        if (!NUMERIC_VERSION.matches(a) || !NUMERIC_VERSION.matches(b)) return true
        return compareNumeric(a, b) > 0
    }

    /** Component-wise comparison of numeric versions: >0 if [a] is newer than [b]. */
    private fun compareNumeric(a: String, b: String): Int {
        val xs = a.split('.')
        val ys = b.split('.')
        val count = maxOf(xs.size, ys.size)
        for (i in 0 until count) {
            val x = xs.getOrNull(i)?.toLongOrNull() ?: 0L
            val y = ys.getOrNull(i)?.toLongOrNull() ?: 0L
            if (x != y) return if (x > y) 1 else -1
        }
        return 0
    }

    /**
     * Strips the leading `v` off a version tag (`v1.2.3`), but not off a word
     * (`versions` must not become `ersions`).
     */
    private fun stripVersionPrefix(tag: String): String {
        val t = tag.trim()
        return if (t.length > 1 && (t[0] == 'v' || t[0] == 'V') && t[1].isDigit()) {
            t.substring(1)
        } else {
            t
        }
    }

    private fun readCache(context: Context): Outcome? {
        val f = file(context)
        if (!f.isFile) return null
        return try {
            val o = JSONObject(f.readText())
            val array = o.optJSONArray("updates")
            val updates = ArrayList<Update>(array?.length() ?: 0)
            for (i in 0 until (array?.length() ?: 0)) {
                val e = array?.optJSONObject(i) ?: continue
                val repo = e.optString("repo")
                val itemId = e.optString("itemId")
                val kind = RepoRules.Kind.ofName(e.optString("kind")) ?: continue
                if (repo.isEmpty() || itemId.isEmpty()) continue
                updates += Update(
                    repo = repo,
                    itemId = itemId,
                    name = e.optString("name", itemId),
                    kind = kind,
                    installedVersion = e.optString("installedVersion", ""),
                    availableVersion = e.optString("availableVersion", ""),
                    releaseTag = e.optString("releaseTag", ""),
                    releaseName = e.optString("releaseName", ""),
                    notes = e.optString("notes", ""),
                )
            }
            Outcome(
                updates = updates,
                checkedRepos = o.optInt("checkedRepos", 0),
                error = if (o.isNull("error")) null else o.optString("error"),
            )
        } catch (_: Throwable) {
            null
        }
    }

    private fun cache(context: Context, outcome: Outcome) {
        memory = outcome
        writeCache(context, outcome)
    }

    private fun writeCache(context: Context, outcome: Outcome) {
        val array = JSONArray()
        for (u in outcome.updates) {
            array.put(
                JSONObject().apply {
                    put("repo", u.repo)
                    put("itemId", u.itemId)
                    put("name", u.name)
                    put("kind", u.kind.name)
                    put("installedVersion", u.installedVersion)
                    put("availableVersion", u.availableVersion)
                    put("releaseTag", u.releaseTag)
                    put("releaseName", u.releaseName)
                    put("notes", u.notes)
                },
            )
        }
        val o = JSONObject()
            .put("updates", array)
            .put("checkedRepos", outcome.checkedRepos)
            .put("error", outcome.error ?: JSONObject.NULL)
        try {
            file(context).writeText(o.toString())
        } catch (_: Throwable) {
            // The cache did not write — the outcome stays in memory until the
            // end of the process, and the next check simply repeats the write.
        }
    }
}
