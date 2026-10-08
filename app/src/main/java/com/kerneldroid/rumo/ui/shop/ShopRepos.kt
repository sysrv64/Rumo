// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.shop

import android.content.Context
import android.content.pm.PackageManager
import androidx.annotation.PluralsRes
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.AutoFixHigh
import androidx.compose.material.icons.rounded.CheckCircle
import androidx.compose.material.icons.rounded.Dashboard
import androidx.compose.material.icons.rounded.Download
import androidx.compose.material.icons.rounded.Key
import androidx.compose.material.icons.rounded.OpenInNew
import androidx.compose.material.icons.rounded.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedListItem
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.EffectStore
import com.kerneldroid.rumo.data.GitHubApi
import com.kerneldroid.rumo.data.RepoRules
import com.kerneldroid.rumo.data.ShopInstalls
import com.kerneldroid.rumo.data.ShopPrefs
import com.kerneldroid.rumo.data.TemplateStore
import com.kerneldroid.rumo.ui.DockContentInset
import com.kerneldroid.rumo.ui.TemplateEntries
import com.kerneldroid.rumo.ui.nav.NavEmptyState
import com.kerneldroid.rumo.ui.nav.NavSectionHeader
import com.kerneldroid.rumo.ui.nav.NavSegmentGap
import com.kerneldroid.rumo.ui.nav.navSegmentedColors
import com.kerneldroid.rumo.ui.nav.navSegmentedShapes
import com.kerneldroid.rumo.ui.openBuiltInTemplate
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.hapticReject
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Template and effect sections: repositories with the `rumo-template` /
 * `rumo-effects` topic, validation against the shop rules and installing the content.
 *
 * ## Why one loader for both sections
 *
 * The "no more than two repositories per author" limit is invisible from a single
 * repository: an author may keep one repository with templates and one with effects,
 * and that is two repositories, not one. So both topics are fetched in one go, and
 * the per-author count runs over the combined list. It is also half as many requests
 * when the user switches sections.
 *
 * ## Why validation happens up front, not on open
 *
 * The shop rule is "failed validation means it is not in the menu". Showing a
 * repository and then removing it from the list on scroll would mean rows flickering.
 * So the list appears already validated; the price is several requests per repository,
 * and it is bounded by the [VALIDATE_CONCURRENCY] parallelism.
 *
 * ## Why a token is needed
 *
 * Anonymous GitHub gives 60 requests per hour per core, while validating one
 * repository costs three. Without a token the sections show that a token is needed
 * instead of hitting the limit halfway down the list.
 */
internal class ShopRepoHub(
    private val context: Context,
    private val scope: CoroutineScope,
    val images: ImageLoader,
) {
    /** One row of the repository list. */
    data class Row(
        val ref: GitHubApi.RepoRef,
        val verdict: RepoRules.Verdict?,
        val checking: Boolean,
        val failed: Boolean,
    )

    /** A parsed repository for the inner screen. */
    data class Detail(
        val branch: String,
        val verdict: RepoRules.Verdict,
        val files: Set<String>,
    )

    var templates by mutableStateOf<List<Row>>(emptyList())
    var effects by mutableStateOf<List<Row>>(emptyList())
    var loading by mutableStateOf(false)
    var error by mutableStateOf<String?>(null)
    var needsToken by mutableStateOf(false)
    var query by mutableStateOf("")

    /** The open repository; null means the list is shown. */
    var open by mutableStateOf<GitHubApi.RepoRef?>(null)
    var openDetail by mutableStateOf<Detail?>(null)
    var openLoading by mutableStateOf(false)
    var openError by mutableStateOf<String?>(null)

    var busy by mutableStateOf<Set<String>>(emptySet())

    /** Changes after an install so the rows recompute "installed". */
    var installedTick by mutableStateOf(0)

    private var loaded = false

    fun rows(kind: RepoRules.Kind): List<Row> =
        if (kind == RepoRules.Kind.TEMPLATE) templates else effects

    fun load(force: Boolean = false) {
        if (loading) return
        if (loaded && !force) return
        val token = ShopPrefs.state.value.credential()
        if (token == null) {
            needsToken = true
            return
        }
        needsToken = false
        loading = true
        error = null
        scope.launch {
            val fetched = withContext(Dispatchers.IO) {
                val t = GitHubApi.searchByTopic(RepoRules.TOPIC_TEMPLATE, token, perPage = 30)
                val e = GitHubApi.searchByTopic(RepoRules.TOPIC_EFFECTS, token, perPage = 30)
                Triple(t, e, null)
            }
            val templateResult = fetched.first
            val effectResult = fetched.second
            if (!templateResult.ok && !effectResult.ok) {
                error = templateResult.error ?: effectResult.error
                    ?: context.getString(R.string.shop_repos_error_no_response)
                loading = false
                return@launch
            }
            val templateRefs = templateResult.value.orEmpty()
            val effectRefs = effectResult.value.orEmpty()

            // The author limit is counted across both topics at once — otherwise a
            // "one template plus one effect" repository would not be counted.
            val hidden = RepoRules.applyAuthorLimits(
                (templateRefs + effectRefs).map {
                    RepoRules.RepoKey(it.fullName, it.owner, it.updatedAt)
                },
            )

            templates = validateAll(templateRefs.filter { it.fullName !in hidden }, RepoRules.Kind.TEMPLATE)
            effects = validateAll(effectRefs.filter { it.fullName !in hidden }, RepoRules.Kind.EFFECT)
            loading = false
            loaded = true
        }
    }

    /**
     * Validates repositories in batches of [VALIDATE_CONCURRENCY].
     *
     * In batches, not all at once: thirty repositories is up to ninety requests, and
     * firing them in one volley means both hitting the limit and getting a refusal on
     * half of them.
     */
    private suspend fun validateAll(
        refs: List<GitHubApi.RepoRef>,
        kind: RepoRules.Kind,
    ): List<Row> {
        val token = ShopPrefs.state.value.credential()
        val live = refs.filter { !it.archived }
        val out = ArrayList<Row>(live.size)
        for (chunk in live.chunked(VALIDATE_CONCURRENCY)) {
            // `coroutineScope` provides the receiver that `async` requires: without it
            // this is not "run in parallel" but a type error.
            val rows = coroutineScope {
                chunk.map { ref ->
                    async(Dispatchers.IO) { validate(ref, kind, token) }
                }.awaitAll()
            }
            // A repository that failed validation does not appear in the list: the shop
            // rule is "no verification file means no repository", not "one exists, but
            // flagged".
            out += rows.filter { it.verdict?.visible == true }
        }
        return out
    }

    private fun validate(
        ref: GitHubApi.RepoRef,
        kind: RepoRules.Kind,
        token: String?,
    ): Row {
        val manifestText = GitHubApi.fileText(
            ref.owner, ref.name, RepoRules.MANIFEST_FILE, ref.defaultBranch, token,
        ).value
        val verificationText = GitHubApi.fileText(
            ref.owner, ref.name, RepoRules.VERIFICATION_FILE, ref.defaultBranch, token,
        ).value
        val files = GitHubApi.tree(ref.owner, ref.name, ref.defaultBranch, token)
            .value.orEmpty().toSet()
        val verdict = RepoRules.validate(
            topic = kind.topic,
            manifest = manifestText?.let { RepoRules.parseManifest(it) },
            verification = verificationText?.let { RepoRules.parseVerification(it) },
            files = files,
            appVersionCode = appVersionCode(context),
        )
        return Row(ref, verdict, checking = false, failed = !verdict.visible)
    }

    /** Opens a repository: lazily loads the tree and the parse if not done yet. */
    fun openRepo(ref: GitHubApi.RepoRef) {
        open = ref
        openDetail = null
        openError = null
        openLoading = true
        val token = ShopPrefs.state.value.credential()
        scope.launch {
            val detail = withContext(Dispatchers.IO) {
                val manifestText = GitHubApi.fileText(
                    ref.owner, ref.name, RepoRules.MANIFEST_FILE, ref.defaultBranch, token,
                ).value
                val verificationText = GitHubApi.fileText(
                    ref.owner, ref.name, RepoRules.VERIFICATION_FILE, ref.defaultBranch, token,
                ).value
                val files = GitHubApi.tree(ref.owner, ref.name, ref.defaultBranch, token)
                    .value.orEmpty().toSet()
                val kind = RepoRules.Kind.ofTopic(
                    ref.topics.firstOrNull { RepoRules.Kind.ofTopic(it) != null } ?: "",
                ) ?: RepoRules.Kind.TEMPLATE
                val verdict = RepoRules.validate(
                    topic = kind.topic,
                    manifest = manifestText?.let { RepoRules.parseManifest(it) },
                    verification = verificationText?.let { RepoRules.parseVerification(it) },
                    files = files,
                    appVersionCode = appVersionCode(context),
                )
                Detail(ref.defaultBranch, verdict, files)
            }
            openDetail = detail
            openLoading = false
            if (!detail.verdict.visible) {
                openError = detail.verdict.reasons.firstOrNull()
                    ?: context.getString(R.string.shop_repos_error_validation)
            }
        }
    }

    fun closeRepo() {
        open = null
        openDetail = null
        openError = null
    }

    /** The item's preview URL. */
    fun previewUrl(ref: GitHubApi.RepoRef, item: RepoRules.Item): String =
        GitHubApi.rawUrl(ref.owner, ref.name, ref.defaultBranch, item.preview)

    /** The content file URL. */
    private fun contentUrl(ref: GitHubApi.RepoRef, item: RepoRules.Item): String =
        GitHubApi.rawUrl(ref.owner, ref.name, ref.defaultBranch, item.file)

    /**
     * Installs an item: downloads the file, parses it and puts it where it should
     * appear — a template into Projects, an effect into the effects menu.
     */
    /**
     * Installs an item and reports it to the outside.
     *
     * [onInstalled] is called after a successful install: the effect lives in the
     * engine catalogue that the editor keeps in memory, and without the notification
     * it would only appear in the menu after a restart.
     */
    fun install(
        ref: GitHubApi.RepoRef,
        item: RepoRules.Item,
        onInstalled: () -> Unit = {},
    ) {
        val key = ref.fullName + "#" + item.id
        if (key in busy) return
        busy = busy + key
        val token = ShopPrefs.state.value.credential()
        scope.launch {
            val message = withContext(Dispatchers.IO) {
                val reply = GitHubApi.download(contentUrl(ref, item), token, INSTALL_LIMIT_BYTES)
                if (!reply.ok || reply.bytes.isEmpty()) {
                    context.getString(R.string.shop_install_download_failed, item.name)
                } else {
                    val text = reply.bytes.toString(Charsets.UTF_8)
                    when (item.kind) {
                        RepoRules.Kind.TEMPLATE -> {
                            val template = TemplateStore.parse(text)
                            if (template == null) {
                                context.getString(R.string.shop_install_not_template, item.name)
                            } else {
                                val saved = TemplateStore.importToProjects(
                                    context,
                                    template,
                                    item.name,
                                )
                                if (saved == null) {
                                    context.getString(R.string.shop_install_save_failed, item.name)
                                } else {
                                    ShopInstalls.record(
                                        context,
                                        ShopInstalls.Record(
                                            repo = ref.fullName,
                                            itemId = item.id,
                                            name = item.name,
                                            kind = item.kind.name,
                                            version = item.version,
                                            installedAt = System.currentTimeMillis(),
                                        ),
                                    )
                                    null
                                }
                            }
                        }
                        RepoRules.Kind.EFFECT -> {
                            val effect = EffectStore.toEffect(text)
                            if (effect == null) {
                                context.getString(R.string.shop_install_not_effect, item.name)
                            } else {
                                val stored = EffectStore.install(
                                    context,
                                    effect,
                                    item.name,
                                    item.description,
                                    ref.fullName,
                                )
                                if (stored == null) {
                                    context.getString(
                                        R.string.shop_install_engine_rejected,
                                        item.name,
                                    )
                                } else {
                                    ShopInstalls.record(
                                        context,
                                        ShopInstalls.Record(
                                            repo = ref.fullName,
                                            itemId = item.id,
                                            name = item.name,
                                            kind = item.kind.name,
                                            version = item.version,
                                            installedAt = System.currentTimeMillis(),
                                        ),
                                    )
                                    null
                                }
                            }
                        }
                    }
                }
            }
            busy = busy - key
            installedTick += 1
            if (message != null) {
                openError = message
            } else {
                onInstalled()
            }
        }
    }

    fun isInstalled(ref: GitHubApi.RepoRef, item: RepoRules.Item): Boolean =
        ShopInstalls.find(context, ref.fullName, item.id) != null

    fun isBusy(ref: GitHubApi.RepoRef, item: RepoRules.Item): Boolean =
        (ref.fullName + "#" + item.id) in busy

    companion object {
        /** How many repositories are validated at once. */
        const val VALIDATE_CONCURRENCY = 6

        /** Size limit of a template or effect file. */
        const val INSTALL_LIMIT_BYTES = 8L * 1024L * 1024L
    }
}

/**
 * This build's `versionCode`.
 *
 * Read from `PackageManager`, not from `BuildConfig`: `BuildConfig` generation is
 * off in this build, and turning it on for one number would mean changing the build
 * configuration for a value the platform hands over anyway.
 */
internal fun appVersionCode(context: Context): Int = try {
    val info = context.packageManager.getPackageInfo(context.packageName, 0)
    @Suppress("DEPRECATION")
    info.versionCode
} catch (_: PackageManager.NameNotFoundException) {
    0
}

@Composable
internal fun ShopReposSection(
    hub: ShopRepoHub,
    kind: RepoRules.Kind,
    modifier: Modifier = Modifier,
    onOpenTokenSettings: () -> Unit,
    onOpenEditor: (String) -> Unit,
    onLibraryChanged: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val haptic = LocalHapticFeedback.current
    var builtInBusy by remember { mutableStateOf(false) }
    LaunchedEffect(kind) { hub.load() }

    val all = hub.rows(kind)
    val visible = remember(all, hub.query) {
        val q = hub.query.trim().lowercase()
        if (q.isEmpty()) {
            all
        } else {
            all.filter {
                it.ref.fullName.lowercase().contains(q) ||
                    it.ref.description.lowercase().contains(q)
            }
        }
    }

    // Built-in templates are always shown until they are turned off in the shop
    // settings: they depend neither on the network nor on a token, and they are the only
    // thing in the template section that works offline.
    val showBuiltIns = ShopPrefs.state.value.showBuiltInTemplates
    val builtIns = if (kind == RepoRules.Kind.TEMPLATE && showBuiltIns) {
        remember(hub.query) {
            val q = hub.query.trim().lowercase()
            if (q.isEmpty()) TemplateEntries else TemplateEntries.filter { it.title.lowercase().contains(q) }
        }
    } else {
        emptyList()
    }

    // The repository replaces the list rather than laying over it.
    //
    // The panel used to be a sibling of the `Column` with the list, and `AnimatedContent`
    // lays siblings out in a Box — that is, they ended up on top of each other, and the
    // search and a list row showed through the panel. Here there is one caller per
    // screen: either the list or the repository.
    val openRef = hub.open
    if (openRef != null) {
        RepoDetailPane(
            hub = hub,
            ref = openRef,
            onBack = { hub.closeRepo() },
            onOpenTokenSettings = onOpenTokenSettings,
            onLibraryChanged = onLibraryChanged,
        )
        return
    }

    Column(modifier = modifier.fillMaxSize()) {
        OutlinedTextField(
            value = hub.query,
            onValueChange = { hub.query = it },
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp),
            singleLine = true,
            leadingIcon = { Icon(Icons.Rounded.Search, contentDescription = null) },
            placeholder = { Text(stringResource(R.string.shop_repos_filter)) },
        )

        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            contentPadding = PaddingValues(
                start = 16.dp,
                end = 16.dp,
                top = 8.dp,
                bottom = DockContentInset + 24.dp,
            ),
            verticalArrangement = Arrangement.spacedBy(NavSegmentGap),
        ) {
            if (builtIns.isNotEmpty()) {
                item(key = "builtin-header") {
                    NavSectionHeader(stringResource(R.string.shop_repos_builtin))
                }
                itemsIndexed(builtIns, key = { _, e -> "builtin:${e.title}" }) { index, entry ->
                    SegmentedListItem(
                        selected = false,
                        onClick = {
                            if (builtInBusy) return@SegmentedListItem
                            builtInBusy = true
                            scope.launch {
                                val saved = openBuiltInTemplate(context, entry)
                                builtInBusy = false
                                if (saved == null) {
                                    haptic.hapticReject()
                                } else {
                                    haptic.hapticConfirm()
                                    onOpenEditor(saved)
                                }
                            }
                        },
                        shapes = navSegmentedShapes(index, builtIns.size),
                        colors = navSegmentedColors(),
                        modifier = Modifier.fillMaxWidth(),
                        leadingContent = { Icon(entry.icon, contentDescription = null) },
                        content = { Text(entry.title, maxLines = 1) },
                        supportingContent = { Text(entry.subtitle, maxLines = 1) },
                    )
                }
            }

            if (hub.needsToken) {
                item(key = "token") {
                    NavEmptyState(
                        title = stringResource(R.string.shop_repos_token_title),
                        message = stringResource(R.string.shop_repos_token_message),
                        icon = Icons.Rounded.Key,
                        actionLabel = stringResource(R.string.shop_repos_token_action),
                        onAction = onOpenTokenSettings,
                    )
                }
                return@LazyColumn
            }

            if (hub.error != null) {
                item(key = "error") {
                    Text(
                        text = hub.error.orEmpty(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }

            if (hub.loading && visible.isEmpty()) {
                item(key = "loading") {
                    NavEmptyState(
                        title = stringResource(R.string.shop_repos_checking_title),
                        message = stringResource(R.string.shop_repos_checking_message),
                        icon = kindIcon(kind),
                    )
                }
            }
            if (!hub.loading && visible.isEmpty()) {
                item(key = "empty") {
                    NavEmptyState(
                        title = stringResource(R.string.shop_repos_empty_title),
                        // The topic stays an API value; only the sentence around it is translated.
                        message = stringResource(R.string.shop_repos_empty_message, kind.topic),
                        icon = kindIcon(kind),
                    )
                }
            }

            if (visible.isNotEmpty()) {
                item(key = "repos-header") {
                    NavSectionHeader(stringResource(R.string.shop_repos_from_github))
                }
            }
            itemsIndexed(visible, key = { _, row -> row.ref.fullName }) { index, row ->
                RepoRowItem(
                    index = index,
                    count = visible.size,
                    row = row,
                    kind = kind,
                    onOpen = { hub.openRepo(row.ref) },
                )
            }
            item(key = "refresh") {
                TextButton(
                    onClick = { hub.load(force = true) },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(stringResource(R.string.shop_repos_refresh))
                }
            }
        }
    }
}

@Composable
private fun RepoRowItem(
    index: Int,
    count: Int,
    row: ShopRepoHub.Row,
    kind: RepoRules.Kind,
    onOpen: () -> Unit,
) {
    val verdict = row.verdict
    val itemCount = verdict?.items?.size ?: 0
    SegmentedListItem(
        selected = false,
        onClick = onOpen,
        shapes = navSegmentedShapes(index, count),
        colors = navSegmentedColors(),
        modifier = Modifier.fillMaxWidth(),
        content = { Text(row.ref.fullName, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            val itemsLabel = pluralStringResource(itemCountPlural(kind), itemCount, itemCount)
            Text(
                text = buildString {
                    if (row.ref.description.isNotEmpty()) {
                        append(row.ref.description.take(90))
                    }
                    if (itemCount > 0) {
                        if (isNotEmpty()) append(" · ")
                        append(itemsLabel)
                    }
                    if (row.ref.stars > 0) append(" · ★ ${row.ref.stars}")
                }.ifEmpty { "—" },
                maxLines = 2,
            )
        },
        leadingContent = {
            Icon(kindIcon(kind), contentDescription = null)
        },
    )
}

@Composable
private fun RepoDetailPane(
    hub: ShopRepoHub,
    ref: GitHubApi.RepoRef,
    onBack: () -> Unit,
    onOpenTokenSettings: () -> Unit,
    onLibraryChanged: () -> Unit,
) {
    val detail = hub.openDetail
    val uriHandler = LocalUriHandler.current
    val noLicence = stringResource(R.string.shop_repos_no_licence)
    // We read tick: it changes after an install, and without reading it the row would
    // not recompute "installed" until the next state change.
    val installedTick = hub.installedTick
    Column(
        modifier = Modifier
            .fillMaxSize()
            // Its own background, not transparency: the panel is a separate screen, and
            // transparency would read as "the list under it is still alive".
            .background(MaterialTheme.colorScheme.surfaceContainer),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 4.dp, end = 16.dp, top = 4.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            IconButton(onClick = onBack) {
                Icon(
                    imageVector = Icons.AutoMirrored.Rounded.ArrowBack,
                    contentDescription = stringResource(R.string.shop_repos_back),
                )
            }
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = ref.fullName,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    text = buildString {
                        append(ref.license.ifEmpty { noLicence })
                        if (ref.stars > 0) append(" · ★ ${ref.stars}")
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
            }
        }

        if (hub.openLoading) {
            NavEmptyState(
                title = stringResource(R.string.shop_repos_reading_title),
                message = stringResource(R.string.shop_repos_reading_message),
                icon = kindIcon(
                    RepoRules.Kind.ofTopic(
                        ref.topics.firstOrNull { RepoRules.Kind.ofTopic(it) != null } ?: "",
                    ) ?: RepoRules.Kind.TEMPLATE,
                ),
            )
            return
        }

        val items = detail?.verdict?.items.orEmpty()
        val installedIds = remember(items, installedTick) {
            items.filter { hub.isInstalled(ref, it) }.map { it.id }.toSet()
        }
        hub.openError?.let { message ->
            Text(
                text = message,
                modifier = Modifier.padding(horizontal = 16.dp),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }

        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            contentPadding = PaddingValues(
                start = 16.dp,
                end = 16.dp,
                top = 4.dp,
                bottom = DockContentInset + 24.dp,
            ),
            verticalArrangement = Arrangement.spacedBy(NavSegmentGap),
        ) {
            if (ref.description.isNotEmpty()) {
                item(key = "desc") {
                    Text(
                        text = ref.description,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            item(key = "meta") {
                Text(
                    text = buildString {
                        append(ref.license.ifEmpty { noLicence })
                        if (ref.stars > 0) append(" · ★ ${ref.stars}")
                        detail?.verdict?.reasons?.firstOrNull()?.let {
                            append(" · ")
                            append(it)
                        }
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            item(key = "items-header") {
                NavSectionHeader(stringResource(R.string.shop_repos_contents))
            }
            itemsIndexed(items, key = { _, item -> item.id }) { index, item ->
                val previewUrl = hub.previewUrl(ref, item)
                val preview = hub.images.bitmap(previewUrl)
                if (preview == null && !hub.images.hasFailed(previewUrl)) {
                    LaunchedEffect(previewUrl) { hub.images.request(previewUrl) }
                }
                val installed = item.id in installedIds
                SegmentedListItem(
                    selected = false,
                    onClick = { if (!installed) hub.install(ref, item, onLibraryChanged) },
                    shapes = navSegmentedShapes(index, items.size),
                    colors = navSegmentedColors(),
                    modifier = Modifier.fillMaxWidth(),
                    leadingContent = {
                        PreviewBox(
                            image = preview,
                            loading = hub.images.isBusy(previewUrl),
                            fallbackIcon = kindIcon(item.kind),
                        )
                    },
                    content = { Text(item.name, maxLines = 1) },
                    supportingContent = {
                        Text(
                            text = buildString {
                                append(item.description.take(80))
                                if (item.version.isNotEmpty()) {
                                    if (isNotEmpty()) append(" · ")
                                    append("v")
                                    append(item.version)
                                }
                            }.ifEmpty { "—" },
                            maxLines = 2,
                        )
                    },
                    trailingContent = {
                        when {
                            hub.isBusy(ref, item) -> Text("…")
                            installed -> Icon(
                                Icons.Rounded.CheckCircle,
                                contentDescription = stringResource(R.string.shop_installed),
                                tint = MaterialTheme.colorScheme.primary,
                            )
                            else -> Icon(
                                Icons.Rounded.Download,
                                contentDescription = stringResource(R.string.shop_repos_install),
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    },
                )
            }
            if (items.isEmpty()) {
                item(key = "no-items") {
                    NavEmptyState(
                        title = stringResource(R.string.shop_repos_nothing_title),
                        message = stringResource(R.string.shop_repos_nothing_message),
                        icon = Icons.Rounded.Key,
                        actionLabel = stringResource(R.string.shop_repos_check_token),
                        onAction = onOpenTokenSettings,
                    )
                }
            }
            item(key = "open") {
                TextButton(
                    onClick = {
                        if (ref.htmlUrl.isNotEmpty()) uriHandler.openUri(ref.htmlUrl)
                    },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Icon(
                        Icons.Rounded.OpenInNew,
                        contentDescription = null,
                        modifier = Modifier.size(18.dp),
                    )
                    Text(stringResource(R.string.shop_repos_open_on_github))
                }
            }
        }
    }
}

private fun kindIcon(kind: RepoRules.Kind) = when (kind) {
    RepoRules.Kind.TEMPLATE -> Icons.Rounded.Dashboard
    RepoRules.Kind.EFFECT -> Icons.Rounded.AutoFixHigh
}

/**
 * The plural for an item count. [RepoRules.Kind.label] is English-only and the
 * data layer has no `Context`, so the label is resolved here instead.
 */
@PluralsRes
private fun itemCountPlural(kind: RepoRules.Kind): Int = when (kind) {
    RepoRules.Kind.TEMPLATE -> R.plurals.shop_repos_items_templates
    RepoRules.Kind.EFFECT -> R.plurals.shop_repos_items_effects
}
