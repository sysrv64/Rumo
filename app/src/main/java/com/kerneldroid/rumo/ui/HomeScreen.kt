// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowForward
import androidx.compose.material.icons.rounded.AutoAwesome
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.Folder
import androidx.compose.material.icons.rounded.Movie
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.nav.NavEmptyState
import com.kerneldroid.rumo.ui.nav.NavGroupSpacer
import com.kerneldroid.rumo.ui.nav.NavSegment
import com.kerneldroid.rumo.ui.DockContentInset
import com.kerneldroid.rumo.ui.nav.NavSegmentGap
import com.kerneldroid.rumo.ui.nav.NavSectionHeader
import com.kerneldroid.rumo.ui.theme.hapticLongPress
import kotlinx.coroutines.launch

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(onNavigate: (String) -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var projects by remember { mutableStateOf<List<ProjectEntry>>(emptyList()) }
    var refreshTick by remember { mutableStateOf(0) }
    LaunchedEffect(refreshTick) {
        projects = ProjectStore.list(context)
    }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.app_name), fontWeight = FontWeight.Bold) },
                actions = {
                    IconButton(onClick = { onNavigate(Routes.SETTINGS) }) {
                        Icon(
                            Icons.Rounded.Settings,
                            contentDescription = stringResource(R.string.settings_title),
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
        // The reserve at the bottom is for the tab dock and the FAB hanging above it.
        LazyColumn(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 16.dp, bottom = DockContentInset + 24.dp),
            verticalArrangement = Arrangement.spacedBy(NavSegmentGap),
        ) {
            item(key = "banner") {
                StartBanner(onClick = { onNavigate(Routes.RUMI) })
            }

            item(key = "gap-recent") { NavGroupSpacer() }

            // "New" was removed from the header: the primary action is the FAB in the dock,
            // and a second "New" button on the screen just competed with it.
            item(key = "recent-header") {
                NavSectionHeader(
                    text = stringResource(R.string.home_recent),
                    trailing = {
                        TextButton(onClick = { onNavigate(Routes.PROJECTS) }) {
                            Text(stringResource(R.string.home_all))
                        }
                    },
                )
            }

            if (projects.isEmpty()) {
                item(key = "empty") {
                    NavEmptyState(
                        title = stringResource(R.string.home_empty_title),
                        message = stringResource(R.string.home_empty_message),
                        icon = Icons.Rounded.Movie,
                        actionLabel = stringResource(R.string.home_start_one),
                        onAction = { onNavigate(Routes.EDITOR) },
                    )
                }
            }
            itemsIndexed(projects, key = { _, p -> p.fileName }) { index, project ->
                RecentProjectRow(
                    project = project,
                    index = index,
                    count = projects.size,
                    onClick = { onNavigate(Routes.editorRoute(project.fileName)) },
                    onDelete = {
                        scope.launch {
                            ProjectStore.delete(context, project.fileName)
                            refreshTick += 1
                        }
                    },
                )
            }
        }
    }
}

/**
 * Invitation card to Rumi.
 *
 * It used to be a `Card` with an `outlineVariant` border and a 1dp shadow — that
 * is, a border plus a shadow plus a colour of its own. Filling it with the accent
 * makes all of that redundant: the container already reads by colour, and a border
 * on top of the fill only adds noise at the seam between two tones. What is left is
 * what works: the accent fill, a round icon and an arrow.
 */
@Composable
private fun StartBanner(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        onClick = onClick,
        modifier = modifier.fillMaxWidth(),
        shape = MaterialTheme.shapes.extraLarge,
        color = MaterialTheme.colorScheme.primaryContainer,
        contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 20.dp, vertical = 20.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                modifier = Modifier.size(48.dp),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Rounded.AutoAwesome,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            }
            Spacer(modifier = Modifier.size(16.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = stringResource(R.string.home_ask_rumi),
                    style = MaterialTheme.typography.titleMedium,
                )
                Text(
                    text = stringResource(R.string.home_ask_rumi_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onPrimaryContainer.copy(alpha = 0.8f),
                )
            }
            Icon(
                imageVector = Icons.AutoMirrored.Rounded.ArrowForward,
                contentDescription = null,
            )
        }
    }
}

/**
 * A recent project row.
 *
 * It used to be a `Card` with a border and a shadow, with a `ListItem` inside:
 * two containers nested in each other, each with its own background. Now the row
 * is itself a segment — one tonal transition is enough for a group of projects to
 * read as a single capsule.
 *
 * The icon in the leading slot alternates by index, as before, but its colour comes
 * from the theme rather than the editor palette: a navigation screen should not carry
 * layer-kind markers, that is the timeline's language.
 */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
private fun RecentProjectRow(
    project: ProjectEntry,
    index: Int,
    count: Int,
    onClick: () -> Unit,
    onDelete: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val haptic = LocalHapticFeedback.current
    NavSegment(
        index = index,
        count = count,
        headline = project.displayName,
        supporting = projectSubtitle(project),
        modifier = modifier,
        leadingIcon = if (index % 2 == 0) Icons.Rounded.Movie else Icons.Rounded.Folder,
        onClick = onClick,
        trailing = {
            IconButton(onClick = {
                haptic.hapticLongPress()
                onDelete()
            }) {
                Icon(
                    imageVector = Icons.Rounded.Delete,
                    contentDescription = stringResource(R.string.home_delete_project),
                )
            }
        },
    )
}
