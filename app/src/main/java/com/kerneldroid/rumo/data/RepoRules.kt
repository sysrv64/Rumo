// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import org.json.JSONObject

/**
 * The format of a shop repository and the rules by which it gets into the storefront.
 *
 * ## What lives in a repository
 *
 * At the root are two mandatory files:
 *
 * * `rumo-verification.json` — the result of checking **inside Rumo**: that the
 *   content parses at all, that the template's project opens, that the effect's WGSL
 *   compiles. Without it the repository does not get into the storefront, because
 *   there is no other way to check "does this work": the storefront cannot trust a
 *   description coming from the repository itself.
 * * `rumo-manifest.json` — which Rumo version it is made for, what it is called, where
 *   the content file is and where the preview is.
 *
 * The preview is **webp or avif**, not png: it is an image for a list, and the difference in
 * weight between webp and png on a small image shows on every opening of the
 * storefront.
 *
 * ## Rules
 *
 * * no more than [MAX_PER_REPO] templates and [MAX_PER_REPO] effects per repository
 *   (a mixed repository is allowed: 2+2 is also four);
 * * one author holds no more than [MAX_REPOS_PER_AUTHOR] repositories;
 * * the repository topic is `rumo-template` or `rumo-effects`, and it must
 *   match what is declared in the manifest.
 *
 * The checks inside a repository are done by [validate]. The limit on an author's number of
 * repositories is not visible from one repository — it is counted by [applyAuthorLimits] over
 * the whole list the storefront loads anyway.
 *
 * ## What is not here
 *
 * There is no network access here: the rules are a pure function over already downloaded
 * files. That way they can be checked without a repository and without the internet.
 */
object RepoRules {
    /** The topic of a repository with templates. */
    const val TOPIC_TEMPLATE = "rumo-template"

    /** The topic of a repository with effects. */
    const val TOPIC_EFFECTS = "rumo-effects"

    /** The verification file at the repository root. */
    const val VERIFICATION_FILE = "rumo-verification.json"

    /** The manifest at the repository root. */
    const val MANIFEST_FILE = "rumo-manifest.json"

    /** The version of both formats that this build understands. */
    const val FORMAT_VERSION = 1

    /** How many templates and how many effects are allowed in one repository. */
    const val MAX_PER_REPO = 4

    /** How many shop repositories one author may have. */
    const val MAX_REPOS_PER_AUTHOR = 2

    /** The preview extensions the storefront accepts. */
    val PREVIEW_EXTENSIONS = listOf("webp", "avif")

    /** The kind of content: a repository with templates or with effects. */
    /**
     * What a repository holds.
     *
     * No display label here any more: it used to be an English word glued to a
     * count, which produced "3 templateов" once the interface gained languages.
     * The shop builds the count from a plural resource instead, so the label
     * would now be a second, unlocalizable name for the same thing.
     */
    enum class Kind(val topic: String) {
        TEMPLATE(TOPIC_TEMPLATE),
        EFFECT(TOPIC_EFFECTS),
        ;

        companion object {
            fun ofTopic(topic: String): Kind? = entries.firstOrNull { it.topic == topic }

            fun ofName(name: String): Kind? =
                entries.firstOrNull { it.name.equals(name, ignoreCase = true) }
        }
    }

    /** One manifest item. */
    data class Item(
        val id: String,
        val kind: Kind,
        val name: String,
        val description: String,
        val version: String,
        /** Which Rumo version it is made for (versionCode). */
        val targetAppVersionCode: Int,
        /** The minimum Rumo version that will understand this. */
        val minAppVersionCode: Int,
        /** The path to the content file inside the repository. */
        val file: String,
        /** The path to the preview (webp/avif) inside the repository. */
        val preview: String,
    )

    /** The parsed `rumo-manifest.json`. */
    data class Manifest(
        val author: String,
        val items: List<Item>,
    ) {
        /** The kinds declared in the manifest, without duplicates. */
        val kinds: Set<Kind> get() = items.map { it.kind }.toSet()
    }

    /** The parsed `rumo-verification.json`. */
    data class Verification(
        val appVersionCode: Int,
        val checkedAt: String,
        /** The ids of the items that passed the check. */
        val passed: Set<String>,
    )

    /**
     * The result of checking a repository.
     *
     * [reasons] is also non-empty when the repository is visible: a warning
     * (for example, an outdated target version) should not hide the content, but it
     * must not be lost either.
     */
    data class Verdict(
        val visible: Boolean,
        val kind: Kind?,
        val items: List<Item>,
        val reasons: List<String>,
    )

    /** The minimum about a repository needed to count the author limit. */
    data class RepoKey(val fullName: String, val owner: String, val updatedAt: String)

    /** Parsing the manifest; null means the file is of the wrong kind or broken. */
    fun parseManifest(text: String): Manifest? = try {
        val o = JSONObject(text)
        if (o.optString("kind") != "rumo-manifest") return null
        if (o.optInt("formatVersion", 0) != FORMAT_VERSION) return null
        val array = o.optJSONArray("items") ?: return null
        val items = ArrayList<Item>(array.length())
        for (i in 0 until array.length()) {
            val e = array.optJSONObject(i) ?: continue
            val kind = Kind.ofName(e.optString("kind")) ?: continue
            val id = e.optString("id")
            val file = e.optString("file")
            if (id.isEmpty() || file.isEmpty()) continue
            items += Item(
                id = id,
                kind = kind,
                name = e.optString("name", id),
                description = e.optString("description", ""),
                version = e.optString("version", ""),
                targetAppVersionCode = e.optInt("targetAppVersionCode", 0),
                minAppVersionCode = e.optInt("minAppVersionCode", 0),
                file = file,
                preview = e.optString("preview", ""),
            )
        }
        Manifest(author = o.optString("author", ""), items = items)
    } catch (_: Throwable) {
        null
    }

    /** Parsing the verification file; null means the file is of the wrong kind or broken. */
    fun parseVerification(text: String): Verification? = try {
        val o = JSONObject(text)
        if (o.optString("kind") != "rumo-verification") return null
        if (o.optInt("formatVersion", 0) != FORMAT_VERSION) return null
        val passed = HashSet<String>()
        o.optJSONArray("items")?.let { array ->
            for (i in 0 until array.length()) {
                val e = array.optJSONObject(i) ?: continue
                // Only an explicit `ok: true` counts as passing: a missing
                // field means "not checked", not "all good".
                if (e.optBoolean("ok", false)) passed += e.optString("id")
            }
        }
        Verification(
            appVersionCode = o.optInt("appVersionCode", 0),
            checkedAt = o.optString("checkedAt", ""),
            passed = passed,
        )
    } catch (_: Throwable) {
        null
    }

    /**
     * Checks a repository against already downloaded files.
     *
     * [files] is the set of paths in the repository (as the tree/contents API returns them),
     * to check that the declared files and previews really exist.
     * [appVersionCode] is this build's version: it decides whether the
     * template is outdated, but not whether it is visible.
     */
    fun validate(
        topic: String,
        manifest: Manifest?,
        verification: Verification?,
        files: Set<String>,
        appVersionCode: Int,
    ): Verdict {
        val reasons = ArrayList<String>(4)
        val topicKind = Kind.ofTopic(topic)
        if (topicKind == null) {
            return Verdict(false, null, emptyList(), listOf("unknown repository topic"))
        }
        if (verification == null) {
            // Without the verification file the content cannot be shown: there is nothing
            // else to check that it works.
            return Verdict(false, topicKind, emptyList(), listOf("no $VERIFICATION_FILE"))
        }
        if (manifest == null) {
            return Verdict(false, topicKind, emptyList(), listOf("no $MANIFEST_FILE"))
        }
        if (manifest.items.isEmpty()) {
            return Verdict(false, topicKind, emptyList(), listOf("the manifest is empty"))
        }
        if (manifest.kinds.size > 1) {
            reasons += "templates and effects are mixed in one repository"
        }
        val declared = manifest.kinds.firstOrNull()
        if (declared != null && declared != topicKind) {
            reasons += "the topic is ${topicKind.topic} but the manifest declares ${declared.topic}"
        }
        val templates = manifest.items.count { it.kind == Kind.TEMPLATE }
        val effects = manifest.items.count { it.kind == Kind.EFFECT }
        if (templates > MAX_PER_REPO || effects > MAX_PER_REPO) {
            reasons += "more than $MAX_PER_REPO items of one kind in the repository"
        }

        val accepted = ArrayList<Item>(manifest.items.size)
        for (item in manifest.items) {
            if (item.id !in verification.passed) {
                reasons += "'${item.name}' did not pass verification"
                continue
            }
            if (item.file !in files) {
                reasons += "'${item.name}': file ${item.file} not found"
                continue
            }
            if (item.preview.isEmpty() || item.preview !in files) {
                reasons += "'${item.name}': preview not found"
                continue
            }
            val ext = item.preview.substringAfterLast('.', "").lowercase()
            if (ext !in PREVIEW_EXTENSIONS) {
                // A requirement on the preview format, not nitpicking: a png in the storefront
                // list weighs many times more at the same size.
                reasons += "'${item.name}': the preview must be webp or avif"
                continue
            }
            if (item.targetAppVersionCode <= 0) {
                reasons += "'${item.name}': the Rumo version is not specified"
                continue
            }
            if (item.minAppVersionCode > appVersionCode) {
                reasons += "'${item.name}': a Rumo version newer than this one is required"
                continue
            }
            if (item.targetAppVersionCode > appVersionCode) {
                // Not hidden: a newer template usually opens, and silently
                // removing it would lie about it existing.
                reasons += "'${item.name}': made for a newer Rumo version"
            }
            accepted += item
        }
        if (accepted.isEmpty()) {
            return Verdict(false, topicKind, emptyList(), reasons.ifEmpty { listOf("no usable items") })
        }
        return Verdict(true, topicKind, accepted, reasons)
    }

    /**
     * The full names of repositories that must be hidden because of the author limit.
     *
     * Counted over the whole list the storefront loads anyway: from one
     * repository the number of that author's repositories is not visible. The author keeps
     * the freshest [MAX_REPOS_PER_AUTHOR]; the rest are hidden, because
     * the rule is either enforced or the repository is not in the menu.
     */
    fun applyAuthorLimits(repos: List<RepoKey>): Set<String> {
        val hidden = HashSet<String>()
        for ((_, group) in repos.groupBy { it.owner.lowercase() }) {
            if (group.size <= MAX_REPOS_PER_AUTHOR) continue
            val keep = group.sortedByDescending { it.updatedAt }.take(MAX_REPOS_PER_AUTHOR)
            val keepNames = keep.map { it.fullName }.toSet()
            for (repo in group) {
                if (repo.fullName !in keepNames) hidden += repo.fullName
            }
        }
        return hidden
    }
}
