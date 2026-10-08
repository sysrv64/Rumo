// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.shop

import androidx.activity.compose.BackHandler
import androidx.annotation.StringRes
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.AutoFixHigh
import androidx.compose.material.icons.rounded.Dashboard
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material.icons.rounded.TextFields
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Switch
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.FontStore
import com.kerneldroid.rumo.data.RepoRules
import com.kerneldroid.rumo.data.ShopPrefs
import com.kerneldroid.rumo.data.ShopUpdates
import com.kerneldroid.rumo.ui.Routes
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** The shop's three sections. */
internal enum class ShopSection(@StringRes val labelRes: Int, val icon: ImageVector) {
    FONTS(R.string.shop_section_fonts, Icons.Rounded.TextFields),
    TEMPLATES(R.string.shop_section_templates, Icons.Rounded.Dashboard),
    EFFECTS(R.string.shop_section_effects, Icons.Rounded.AutoFixHigh),
}

/**
 * The shop: fonts, templates and effects.
 *
 * ## Why the sections are a switcher, and not a dock
 *
 * The dock is taken up by the app's top-level sections, and three more items in
 * it would turn choosing "where do I go" into choosing "where am I". Inside the
 * shop the sections switch in place: the title stays, the content changes, and
 * the move reads as a change of view rather than as leaving the screen.
 *
 * ## What is done once per screen here, and what is done per app
 *
 * Registering the installed fonts with the engine is per app: a text layer has
 * to find its font even when the shop has never been opened. That is why it is
 * called from `MainActivity`, and here the list is only read.
 *
 * The update check is here: the update banner lives in the shop, and there is no
 * point checking releases while the shop is not open. Throttling ("once a day"
 * and the interval setting) is in [ShopPrefs], so the schedule survives a
 * restart.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ShopScreen(
    onNavigate: (String) -> Unit,
    /** Called after an install from the shop: the device's library has changed. */
    onLibraryChanged: () -> Unit = {},
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val haptic = LocalHapticFeedback.current

    val images = remember { ImageLoader(scope) }
    val fonts = remember { ShopFontsState(context, scope, images) }
    val hub = remember { ShopRepoHub(context, scope, images) }

    var section by remember { mutableStateOf(ShopSection.FONTS) }
    var settingsOpen by remember { mutableStateOf(false) }
    var updates by remember { mutableStateOf<ShopUpdates.Outcome?>(null) }
    var prefsTick by remember { mutableStateOf(0) }

    LaunchedEffect(Unit) {
        // I/O goes on IO, not on the main thread.
        //
        // `LaunchedEffect` runs on the composition dispatcher, that is, on the
        // main thread, while `refreshInstalled` and `lastOutcome` read files and
        // parse JSON right inside themselves. This used to happen in the first
        // frames of the transition to the tab — that is, within the animation
        // frame budget itself — and the Shop tab opened more slowly than any
        // other. On top of that, the index was read twice per entry: here and
        // once more from `ShopFontsSection`.
        ShopPrefs.init(context)
        val loaded = withContext(Dispatchers.IO) {
            FontStore.installed(context) to ShopUpdates.lastOutcome(context)
        }
        fonts.adoptInstalled(loaded.first)
        updates = loaded.second
        fonts.load(force = false)
    }

    // The release check runs on its own schedule; the result is only displayed.
    LaunchedEffect(prefsTick) {
        val outcome = withContext(Dispatchers.IO) {
            ShopUpdates.checkIfDue(context, ShopPrefs.state.value.credential())
        }
        if (outcome != null) updates = outcome
    }

    val token = ShopPrefs.state.value.token
    val openRepo = hub.open
    BackHandler(enabled = openRepo != null) { hub.closeRepo() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(stringResource(R.string.shop_title))
                        Text(
                            text = subtitleFor(section, fonts, hub, token),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
                actions = {
                    if (updates?.updates?.isNotEmpty() == true) {
                        val updateCount = updates?.updates?.size ?: 0
                        Text(
                            text = pluralStringResource(
                                R.plurals.shop_updates_count,
                                updateCount,
                                updateCount,
                            ),
                            style = MaterialTheme.typography.labelLarge,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.padding(end = 4.dp),
                        )
                    }
                    IconButton(onClick = { settingsOpen = true }) {
                        Icon(
                            Icons.Rounded.Settings,
                            contentDescription = stringResource(R.string.shop_settings),
                        )
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.surfaceContainer,
                ),
            )
        },
        containerColor = MaterialTheme.colorScheme.surfaceContainer,
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
        ) {
            SectionSwitcher(
                selected = section,
                onSelect = { section = it },
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            )

            val available = updates?.updates.orEmpty()
            if (available.isNotEmpty()) {
                UpdateBanner(
                    updates = available,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }

            // Material's pull-to-refresh: `PullToRefreshBox` holds the gesture,
            // the threshold and the indicator itself, and it is the same
            // component as the one in ExtraLabs. We do not override the
            // indicator — the component has its own.
            //
            // `isRefreshing` is the section's loading state, not a separate
            // "pulled" flag: a separate flag would have to be cleared on
            // completion, and it would stick if the request never started (for
            // instance, a load was already running). A loading state cannot
            // stick.
            val pullState = rememberPullToRefreshState()
            val busy = if (section == ShopSection.FONTS) fonts.loading else hub.loading
            PullToRefreshBox(
                isRefreshing = busy,
                onRefresh = {
                    haptic.hapticConfirm()
                    when (section) {
                        ShopSection.FONTS -> fonts.load(force = true)
                        ShopSection.TEMPLATES, ShopSection.EFFECTS -> hub.load(force = true)
                    }
                },
                state = pullState,
                modifier = Modifier.fillMaxSize(),
            ) {
            AnimatedContent(
                targetState = section,
                transitionSpec = { fadeIn() togetherWith fadeOut() },
                label = "shop-section",
            ) { current ->
                when (current) {
                    ShopSection.FONTS -> ShopFontsSection(fonts, Modifier.fillMaxSize())
                    ShopSection.TEMPLATES -> ShopReposSection(
                        hub = hub,
                        kind = RepoRules.Kind.TEMPLATE,
                        modifier = Modifier.fillMaxSize(),
                        onOpenTokenSettings = { settingsOpen = true },
                        onOpenEditor = { fileName -> onNavigate(Routes.editorRoute(fileName)) },
                        onLibraryChanged = onLibraryChanged,
                    )
                    ShopSection.EFFECTS -> ShopReposSection(
                        hub = hub,
                        kind = RepoRules.Kind.EFFECT,
                        modifier = Modifier.fillMaxSize(),
                        onOpenTokenSettings = { settingsOpen = true },
                        onOpenEditor = { fileName -> onNavigate(Routes.editorRoute(fileName)) },
                        onLibraryChanged = onLibraryChanged,
                    )
                }
            }
            }
        }
    }

    if (settingsOpen) {
        ShopSettingsDialog(
            onDismiss = {
                settingsOpen = false
                prefsTick += 1
            },
        )
    }
}

@Composable
private fun SectionSwitcher(
    selected: ShopSection,
    onSelect: (ShopSection) -> Unit,
    modifier: Modifier = Modifier,
) {
    // Material's stock segmented row: each segment's shape is its role
    // (first/middle/last), and not a single radius is written by hand here.
    SingleChoiceSegmentedButtonRow(modifier = modifier) {
        ShopSection.entries.forEachIndexed { index, entry ->
            SegmentedButton(
                selected = selected == entry,
                onClick = { onSelect(entry) },
                shape = SegmentedButtonDefaults.itemShape(
                    index = index,
                    count = ShopSection.entries.size,
                ),
                icon = {},
                label = { Text(stringResource(entry.labelRes)) },
            )
        }
    }
}

@Composable
private fun UpdateBanner(
    updates: List<ShopUpdates.Update>,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(
                Icons.Rounded.Dashboard,
                contentDescription = null,
                modifier = Modifier.size(16.dp),
                tint = MaterialTheme.colorScheme.primary,
            )
            Text(
                text = stringResource(R.string.shop_updates_available),
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
            )
        }
        updates.take(3).forEach { update ->
            Text(
                text = stringResource(
                    R.string.shop_update_line,
                    update.name,
                    update.installedVersion,
                    update.availableVersion,
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun ShopSettingsDialog(onDismiss: () -> Unit) {
    val context = LocalContext.current
    val state = ShopPrefs.state.value
    var token by remember { mutableStateOf(state.token) }
    var checkUpdates by remember { mutableStateOf(state.checkUpdates) }
    var previewInList by remember { mutableStateOf(state.previewInList) }
    var showBuiltIns by remember { mutableStateOf(state.showBuiltInTemplates) }
    var interval by remember { mutableStateOf(state.intervalHours.toString()) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.shop_settings)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(
                    text = stringResource(R.string.shop_settings_token_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedTextField(
                    value = token,
                    onValueChange = { token = it },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                    label = { Text(stringResource(R.string.shop_settings_token_label)) },
                    visualTransformation = PasswordVisualTransformation(),
                )
                SettingSwitch(
                    label = stringResource(R.string.shop_settings_check_updates),
                    checked = checkUpdates,
                    onChange = { checkUpdates = it },
                )
                SettingSwitch(
                    label = stringResource(R.string.shop_settings_preview_fonts),
                    checked = previewInList,
                    onChange = { previewInList = it },
                )
                SettingSwitch(
                    label = stringResource(R.string.shop_settings_show_builtins),
                    checked = showBuiltIns,
                    onChange = { showBuiltIns = it },
                )
                OutlinedTextField(
                    value = interval,
                    onValueChange = { interval = it.filter { ch -> ch.isDigit() }.take(4) },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                    label = { Text(stringResource(R.string.shop_settings_interval)) },
                )
                val installed = remember { FontStore.installed(context) }
                if (installed.isNotEmpty()) {
                    Text(
                        text = stringResource(
                            R.string.shop_settings_installed_fonts,
                            installed.joinToString(", ") { it.displayName },
                        ),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    ShopPrefs.setToken(token)
                    ShopPrefs.setCheckUpdates(checkUpdates)
                    ShopPrefs.setPreviewInList(previewInList)
                    ShopPrefs.setShowBuiltInTemplates(showBuiltIns)
                    ShopPrefs.setIntervalHours(interval.toIntOrNull() ?: ShopPrefs.DEFAULT_INTERVAL_HOURS)
                    onDismiss()
                },
            ) {
                Text(stringResource(R.string.action_save))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.shop_cancel)) }
        },
    )
}

@Composable
private fun SettingSwitch(
    label: String,
    checked: Boolean,
    onChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(label, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = onChange)
    }
}

@Composable
private fun subtitleFor(
    section: ShopSection,
    fonts: ShopFontsState,
    hub: ShopRepoHub,
    token: String,
): String = when (section) {
    ShopSection.FONTS -> when {
        fonts.loading -> stringResource(R.string.shop_fonts_subtitle_loading)
        fonts.installed.isEmpty() -> pluralStringResource(
            R.plurals.shop_fonts_subtitle_none,
            fonts.catalog.size,
            fonts.catalog.size,
        )
        else -> pluralStringResource(
            R.plurals.shop_fonts_subtitle_installed,
            fonts.catalog.size,
            fonts.catalog.size,
            fonts.installed.size,
        )
    }
    ShopSection.TEMPLATES -> repoSubtitle(hub, RepoRules.Kind.TEMPLATE, token)
    ShopSection.EFFECTS -> repoSubtitle(hub, RepoRules.Kind.EFFECT, token)
}

@Composable
private fun repoSubtitle(hub: ShopRepoHub, kind: RepoRules.Kind, token: String): String = when {
    token.isEmpty() -> stringResource(R.string.shop_repos_subtitle_no_token)
    hub.loading -> stringResource(R.string.shop_repos_subtitle_loading)
    // The topic is an API value (`rumo-template`), never translated — it is an argument.
    hub.rows(kind).isEmpty() -> stringResource(R.string.shop_repos_subtitle_none, kind.topic)
    else -> pluralStringResource(
        R.plurals.shop_repos_subtitle_count,
        hub.rows(kind).size,
        hub.rows(kind).size,
    )
}
