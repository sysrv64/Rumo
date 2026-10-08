// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import com.kerneldroid.aiengines.rumi.RumiHttp
import org.json.JSONArray
import org.json.JSONObject
import java.net.URI
import java.util.Base64

/**
 * A thin GitHub REST client for the shop: searching repositories by topic, reading
 * the tree and files, releases.
 *
 * ## Why no library
 *
 * There are a dozen requests and all of them are of the same kind (GET + JSON), so the client is
 * wrappers over [RumiHttp] plus response parsing. Pulling in OkHttp/Retrofit/Ktor would mean
 * putting another network stack into the APK on top of the same HttpURLConnection, while JSON
 * is parsed by the stock `org.json`, which is already part of the platform.
 *
 * ## Blocking
 *
 * Not one call suspends: all of them block on network
 * I/O. Call it **only off the main thread** — otherwise the UI will
 * freeze for the duration of the request to GitHub.
 *
 * ## Errors are a value, not an exception
 *
 * No exception flies out of the public functions. A transport failure (no
 * network, DNS, TLS) gives `status = 0`, GitHub's own error gives its code and text;
 * both are turned into a [Result] with a human-readable reason, so the
 * UI shows "limit exhausted" rather than "failed". [download], instead of
 * [Result], returns [RumiHttp.BytesReply] and on a failure gives an empty response with
 * a zero status.
 *
 * ## Token
 *
 * `Authorization` is attached **only** to requests to `api.github.com` (see
 * [apiHeaders]), because the token is access to an account, not just a header:
 * handing it to `raw.githubusercontent.com` or to the release storage GitHub
 * redirects to for a file would leak the credentials to a third party.
 * The guarantee is structural: the map with the token is built by the single
 * function [apiHeaders], and [download] assembles headers itself and adds the token
 * only when the host is the GitHub API itself.
 */
object GitHubApi {
    private const val BASE = "https://api.github.com"
    private const val API_HOST = "api.github.com"

    /** The raw-file host: no token and no API quota. */
    private const val RAW_ROOT = "https://raw.githubusercontent.com"

    /** The response media type GitHub asks for on REST version 2022-11-28. */
    private const val ACCEPT = "application/vnd.github+json"
    private const val API_VERSION = "2022-11-28"

    /** Listing requests usually finish in seconds; 30 s is headroom for a slow network. */
    private const val TIMEOUT_MS = 30_000

    /** A repository from search or `GET /repos/{owner}/{name}`. */
    data class RepoRef(
        val owner: String,
        val name: String,
        val fullName: String,
        val description: String,
        val stars: Int,
        val topics: List<String>,
        val defaultBranch: String,
        val updatedAt: String,
        val htmlUrl: String,
        val license: String,
        val archived: Boolean,
    )

    /** A directory entry from the Contents API: a file or a subdirectory. */
    data class ContentEntry(
        val name: String,
        val path: String,
        val isDir: Boolean,
        val size: Long,
        val downloadUrl: String,
    )

    /** A release with its attached files. */
    data class Release(
        val tagName: String,
        val name: String,
        val publishedAt: String,
        val body: String,
        val prerelease: Boolean,
        val draft: Boolean,
        val assets: List<Asset>,
    ) {
        /** One file attached to a release. */
        data class Asset(val name: String, val url: String, val size: Long)
    }

    /**
     * A result with a failure reason: `value` is filled on success, `error` on
     * failure. An empty list is a success ([ok] true), not an error: "nothing was
     * found" and "the request did not get through" must be shown differently by the UI.
     */
    data class Result<T>(val value: T?, val error: String?) {
        val ok: Boolean get() = value != null
    }

    private fun <T> ok(value: T): Result<T> = Result(value, null)

    private fun <T> fail(error: String): Result<T> = Result(null, error)

    /**
     * The request headers for the API. Accept is passed as a separate parameter to
     * [RumiHttp.getJson], so what remains here is the API version and — only with a
     * non-empty token — authorisation.
     *
     * The function is deliberately one for all API calls: as long as a single place builds the
     * map with the token, adding another entry point will not smear the
     * credential across the code.
     */
    private fun apiHeaders(token: String?): Map<String, String> {
        val headers = LinkedHashMap<String, String>(2)
        headers["X-GitHub-Api-Version"] = API_VERSION
        if (!token.isNullOrEmpty()) headers["Authorization"] = "Bearer $token"
        return headers
    }

    /**
     * GET against the API. A transport failure is not thrown out: it becomes a
     * response with status 0, which [failure] turns into a comprehensible reason.
     */
    private fun get(url: String, token: String?, timeoutMs: Int = TIMEOUT_MS): RumiHttp.Reply = try {
        RumiHttp.getJson(url, apiHeaders(token), timeoutMs, accept = ACCEPT)
    } catch (_: Throwable) {
        RumiHttp.Reply(0, "")
    }

    /**
     * The reason a response failed, or null if it succeeded.
     *
     * For the limit GitHub answers both 403 (quota) and 429 (secondary limit); 403 is
     * also used for other prohibitions, so the limit is identified by the response
     * body. The client keeps no request counter of its own — the server has already said everything
     * itself, and a local quota would diverge from the real one.
     */
    private fun failure(reply: RumiHttp.Reply): String? {
        if (reply.ok) return null
        if (reply.status == 0) return "no connection to GitHub"
        if ((reply.status == 403 || reply.status == 429) &&
            reply.body.contains("rate limit", ignoreCase = true)
        ) {
            return "the GitHub request limit is exhausted — add a token"
        }
        val message = try {
            val o = JSONObject(reply.body)
            if (o.isNull("message")) "" else o.optString("message")
        } catch (_: Throwable) {
            ""
        }
        return when {
            message.isNotEmpty() -> "GitHub: $message"
            reply.status == 404 -> "not found on GitHub"
            else -> "GitHub returned ${reply.status}"
        }
    }

    /**
     * `GET /search/repositories?q=topic:<topic>` — repositories with the `topic`
     * topic, freshest first (`sort=updated&order=desc`).
     *
     * The `q` value is percent-encoded: `topic:name` contains a colon, and without
     * encoding the request would go out with a character illegal in a URL.
     *
     * The list is returned whole, without trimming: if a caller needs less,
     * that is its decision, not a silent shortening here.
     */
    fun searchByTopic(topic: String, token: String?, page: Int = 1, perPage: Int = 30): Result<List<RepoRef>> {
        val query = encode("topic:${topic.trim()}")
        // 100 is GitHub's server-side maximum; more is rejected as 422. This is the
        // boundary of request correctness, not a trimming of the response.
        val per = perPage.coerceIn(1, 100)
        val p = if (page < 1) 1 else page
        val url = "$BASE/search/repositories?q=$query&sort=updated&order=desc&page=$p&per_page=$per"
        val reply = get(url, token)
        val error = failure(reply)
        if (error != null) return fail(error)
        return try {
            val items = JSONObject(reply.body).optJSONArray("items")
            ok((0 until (items?.length() ?: 0)).mapNotNull { parseRepo(items?.optJSONObject(it)) })
        } catch (_: Throwable) {
            fail("could not parse the GitHub response")
        }
    }

    /** `GET /repos/{owner}/{name}` — one repository. */
    fun repo(owner: String, name: String, token: String?): Result<RepoRef> {
        val reply = get(repoBase(owner, name), token)
        val error = failure(reply)
        if (error != null) return fail(error)
        return try {
            parseRepo(JSONObject(reply.body))?.let { ok(it) } ?: fail("could not parse the GitHub response")
        } catch (_: Throwable) {
            fail("could not parse the GitHub response")
        }
    }

    /** The default branch. Read from the same response as [repo]. */
    fun defaultBranch(owner: String, name: String, token: String?): Result<String> {
        val found = repo(owner, name, token)
        val value = found.value ?: return fail(found.error ?: "GitHub did not answer")
        return ok(value.defaultBranch)
    }

    /**
     * `GET /repos/{owner}/{name}/git/trees/{ref}?recursive=1` — all the repository's
     * paths in one request.
     *
     * The storefront needs to make sure the content file and preview declared in the
     * manifest really lie in the repository. The Contents API serves one
     * directory per request, and for a repository with four templates that is eight
     * requests instead of one; the tree serves the whole path list at once.
     *
     * Only `blob` entries are returned — that is, files. It is the existence of
     * files that has to be checked, and a directory in the list would only interfere with
     * name matches.
     */
    fun tree(owner: String, name: String, ref: String, token: String?): Result<List<String>> {
        val url = repoBase(owner, name) + "/git/trees/${encode(ref)}?recursive=1"
        val reply = get(url, token)
        val error = failure(reply)
        if (error != null) return fail(error)
        return try {
            val array = JSONObject(reply.body).optJSONArray("tree")
            val paths = ArrayList<String>(array?.length() ?: 0)
            for (i in 0 until (array?.length() ?: 0)) {
                val entry = array?.optJSONObject(i) ?: continue
                if (str(entry, "type") != "blob") continue
                val path = str(entry, "path")
                if (path.isNotEmpty()) paths += path
            }
            ok(paths)
        } catch (_: Throwable) {
            fail("could not parse the GitHub response")
        }
    }

    /**
     * A direct link to a repository file.
     *
     * Read without a token and without the API: the storefront downloads previews and content
     * files from here, so as not to spend GitHub quota on every showing of the
     * list and not to attach `Authorization` to someone else's host.
     */
    fun rawUrl(owner: String, name: String, ref: String, path: String): String =
        "$RAW_ROOT/${encode(owner)}/${encode(name)}/${encode(ref)}/${encodePath(path)}"

    /**
     * `GET /repos/{owner}/{name}/contents/{path}` — a directory's contents or
     * a single file.
     *
     * The Contents API returns an array for a directory and an object for a file, so
     * both are parsed: the caller may not know the path in
     * advance.
     */
    fun contents(
        owner: String,
        name: String,
        path: String,
        ref: String?,
        token: String?,
    ): Result<List<ContentEntry>> {
        val reply = get(contentsUrl(owner, name, path, ref), token)
        val error = failure(reply)
        if (error != null) return fail(error)
        val body = reply.body.trim()
        return try {
            val entries = when {
                body.startsWith("[") ->
                    JSONArray(body).let { arr ->
                        (0 until arr.length()).mapNotNull { parseEntry(arr.optJSONObject(it)) }
                    }
                body.startsWith("{") -> listOfNotNull(parseEntry(JSONObject(body)))
                else -> emptyList()
            }
            ok(entries)
        } catch (_: Throwable) {
            fail("could not parse the GitHub response")
        }
    }

    /**
     * Reads a text file through the Contents API and decodes the base64.
     *
     * The payload is cleaned of whitespace before decoding: the
     * Contents API wraps base64 across lines (about 60 characters each), and a
     * strict decoder does not accept line breaks as data.
     *
     * Files larger than the API limit arrive with `encoding: "none"` and an empty `content`
     * — that is a separate reason, and it is the one we show.
     */
    fun fileText(
        owner: String,
        name: String,
        path: String,
        ref: String?,
        token: String?,
    ): Result<String> {
        val reply = get(contentsUrl(owner, name, path, ref), token)
        val error = failure(reply)
        if (error != null) return fail(error)
        val body = try {
            JSONObject(reply.body)
        } catch (_: Throwable) {
            return fail("the path is not a file")
        }
        val encoding = str(body, "encoding")
        val content = str(body, "content")
        if (encoding != "base64" || content.isEmpty()) {
            return fail("the file is unavailable through the Contents API (probably too large)")
        }
        return try {
            val cleaned = content.filterNot { it.isWhitespace() }
            ok(String(Base64.getDecoder().decode(cleaned), Charsets.UTF_8))
        } catch (_: Throwable) {
            fail("could not decode the base64 from the GitHub response")
        }
    }

    /** `GET /repos/{owner}/{name}/releases` — releases as GitHub serves them. */
    fun releases(owner: String, name: String, token: String?): Result<List<Release>> {
        val reply = get(repoBase(owner, name) + "/releases", token)
        val error = failure(reply)
        if (error != null) return fail(error)
        return try {
            val arr = JSONArray(reply.body)
            ok((0 until arr.length()).mapNotNull { parseRelease(arr.optJSONObject(it)) })
        } catch (_: Throwable) {
            fail("could not parse the GitHub response")
        }
    }

    /**
     * The latest stable release: not a draft and not a pre-release, the maximum by
     * `published_at`.
     *
     * By publication date specifically, not by position in the array: the API response
     * order is not guaranteed. GitHub serves the time in UTC, so a lexicographic
     * string comparison coincides with the chronological one.
     */
    fun latestRelease(owner: String, name: String, token: String?): Result<Release> {
        val all = releases(owner, name, token)
        val list = all.value ?: return fail(all.error ?: "GitHub did not answer")
        val release = list.filterNot { it.draft || it.prerelease }.maxByOrNull { it.publishedAt }
            ?: return fail("no suitable release")
        return ok(release)
    }

    /**
     * Downloads a release file or any other URL with a byte ceiling (0 — no
     * ceiling). Blocking, call it off the main thread.
     *
     * The token goes only to `api.github.com`: the other addresses (direct release
     * links, `raw.githubusercontent.com`) never see it. A redirect inside
     * `api.github.com` to someone else's host the client does not control — [RumiHttp]
     * follows redirects itself — so the only protection here is
     * not to put the token into a request to a non-GitHub host in the first place.
     *
     * For an API file address, bytes are requested (`application/octet-stream`):
     * without that GitHub would return a JSON description, not the content.
     */
    fun download(url: String, token: String?, limitBytes: Long = 0L): RumiHttp.BytesReply {
        val host = try {
            URI(url).host.orEmpty()
        } catch (_: Throwable) {
            return RumiHttp.BytesReply(0, ByteArray(0))
        }
        val apiHost = host.equals(API_HOST, ignoreCase = true)
        val headers = if (apiHost) apiHeaders(token) else emptyMap()
        val accept = if (apiHost) "application/octet-stream" else "*/*"
        return try {
            RumiHttp.getBytes(url, headers, limitBytes, TIMEOUT_MS, accept = accept)
        } catch (_: Throwable) {
            RumiHttp.BytesReply(0, ByteArray(0))
        }
    }

    /** The URL of one repository with encoded segments. */
    private fun repoBase(owner: String, name: String): String =
        "$BASE/repos/${encode(owner)}/${encode(name)}"

    /** The `/contents` URL for a directory (without a tail) or a file. */
    private fun contentsUrl(owner: String, name: String, path: String, ref: String?): String {
        val suffix = encodePath(path)
        val tail = if (suffix.isEmpty()) "" else "/$suffix"
        return repoBase(owner, name) + "/contents" + tail + refQuery(ref)
    }

    /** `?ref=...` or an empty string if the ref is not set. */
    private fun refQuery(ref: String?): String =
        if (ref.isNullOrEmpty()) "" else "?ref=${encode(ref)}"

    /**
     * Percent-encoding of a single segment. The shared encoder from
     * [GoogleFonts] is taken: it is already tested on paths and serves requests just as well,
     * and there is no point duplicating it for one call.
     */
    private fun encode(segment: String): String = GoogleFonts.encodePathSegment(segment)

    /** A path from segments: `a/b c` → `a/b%20c`; an empty string is the root. */
    private fun encodePath(path: String): String =
        path.trim('/')
            .split('/')
            .filter { it.isNotEmpty() }
            .joinToString("/") { encode(it) }

    private fun parseRepo(o: JSONObject?): RepoRef? {
        o ?: return null
        val name = str(o, "name")
        val full = str(o, "full_name")
        if (name.isEmpty() || full.isEmpty()) return null
        val login = o.optJSONObject("owner")?.let { str(it, "login") }
        return RepoRef(
            owner = login ?: full.substringBefore('/'),
            name = name,
            fullName = full,
            description = str(o, "description"),
            stars = o.optInt("stargazers_count", 0),
            topics = stringList(o.optJSONArray("topics")),
            defaultBranch = str(o, "default_branch"),
            updatedAt = str(o, "updated_at"),
            htmlUrl = str(o, "html_url"),
            license = licenseOf(o),
            archived = o.optBoolean("archived", false),
        )
    }

    private fun parseEntry(o: JSONObject?): ContentEntry? {
        o ?: return null
        val name = str(o, "name")
        val path = str(o, "path")
        if (name.isEmpty() && path.isEmpty()) return null
        return ContentEntry(
            name = name.ifEmpty { path.substringAfterLast('/') },
            path = path.ifEmpty { name },
            isDir = str(o, "type") == "dir",
            size = o.optLong("size", 0L),
            downloadUrl = str(o, "download_url"),
        )
    }

    private fun parseRelease(o: JSONObject?): Release? {
        o ?: return null
        val tag = str(o, "tag_name")
        if (tag.isEmpty()) return null
        val assets = ArrayList<Release.Asset>(4)
        val array = o.optJSONArray("assets")
        for (i in 0 until (array?.length() ?: 0)) {
            val a = array?.optJSONObject(i) ?: continue
            val assetName = str(a, "name")
            if (assetName.isEmpty()) continue
            // A direct link is preferable to the API `url`: it works without a token and
            // without Accept: application/octet-stream, while `url` is the fallback.
            val link = str(a, "browser_download_url").ifEmpty { str(a, "url") }
            assets += Release.Asset(assetName, link, a.optLong("size", 0L))
        }
        return Release(
            tagName = tag,
            name = str(o, "name").ifEmpty { tag },
            publishedAt = str(o, "published_at"),
            body = str(o, "body"),
            prerelease = o.optBoolean("prerelease", false),
            draft = o.optBoolean("draft", false),
            assets = assets,
        )
    }

    private fun licenseOf(o: JSONObject): String {
        val license = o.optJSONObject("license") ?: return ""
        val spdx = str(license, "spdx_id")
        // NOASSERTION is the marker for "licence not identified"; a human name
        // is more useful in the UI than a technical label.
        return if (spdx.isNotEmpty() && spdx != "NOASSERTION") spdx else str(license, "name")
    }

    private fun stringList(array: JSONArray?): List<String> {
        array ?: return emptyList()
        val out = ArrayList<String>(array.length())
        for (i in 0 until array.length()) {
            if (array.isNull(i)) continue
            val s = array.optString(i)
            if (s.isNotEmpty()) out += s
        }
        return out
    }

    /**
     * A string field of an object.
     *
     * The [JSONObject.isNull] check is mandatory: for a JSON null `optString`
     * returns the string `"null"`, and a repository without a description would
     * show "null" in the list instead of an empty string.
     */
    private fun str(o: JSONObject, key: String): String =
        if (o.isNull(key)) "" else o.optString(key)
}
