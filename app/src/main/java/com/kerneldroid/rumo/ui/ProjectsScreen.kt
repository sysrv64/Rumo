// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.Movie
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
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
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.nav.NavEmptyState
import com.kerneldroid.rumo.ui.nav.NavSegment
import com.kerneldroid.rumo.ui.DockContentInset
import com.kerneldroid.rumo.ui.nav.NavSegmentGap
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import androidx.compose.material3.OutlinedTextField
import androidx.compose.ui.text.style.TextOverflow
import com.kerneldroid.rumo.ui.nav.NavItemMenu
import com.kerneldroid.rumo.ui.nav.NavMenuAction
import com.kerneldroid.rumo.ui.nav.NavMenuIcons
import com.kerneldroid.rumo.ui.SaveResult
import com.kerneldroid.rumo.ui.saveBytesToFolder
import com.kerneldroid.rumo.ui.DownloadKind
import com.kerneldroid.rumo.ui.theme.hapticReject
import kotlinx.coroutines.launch

/**
 * Saved projects, from the real store.
 *
 * This screen used to be a static list of two invented names with a `TODO`, which
 * made the tab worse than useless: it looked like a library and answered
 * nothing. A project saved from the editor — or by the assistant through its
 * `project` tool — now appears here, opens on tap, and can be deleted.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProjectsScreen(onNavigate: (String) -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var projects by remember { mutableStateOf<List<ProjectEntry>>(emptyList()) }
    var loaded by remember { mutableStateOf(false) }
    var pendingDelete by remember { mutableStateOf<ProjectEntry?>(null) }
    var pendingRename by remember { mutableStateOf<ProjectEntry?>(null) }
    var renameText by remember { mutableStateOf("") }
    var notice by remember { mutableStateOf<String?>(null) }
    val haptic = LocalHapticFeedback.current
    var refreshTick by remember { mutableStateOf(0) }
    // Whether the user is pulling the list right now. Separate from `loaded`: that one
    // means "the first read has happened", while this means "a gesture-triggered re-fetch is
    // in progress", and the indicator must appear for the gesture, not for opening the tab.
    var refreshing by remember { mutableStateOf(false) }

    /**
     * A copy of the project under a free name.
     *
     * The name is picked rather than taken as "Name copy": `ProjectStore.save` writes by
     * file name and overwrites silently, so a copy over a previous copy
     * would be a loss, not a copy.
     */
    suspend fun clone(project: ProjectEntry) {
        val bytes = ProjectStore.load(context, project.fileName)
        if (bytes == null) {
            notice = context.getString(R.string.projects_read_failed, project.displayName)
            return
        }
        // The copy's name is a project name and a file name, so it is not translated:
        // a localised "copy" would change the stored file name with the app language.
        val taken = projects.map { it.displayName }.toSet()
        var candidate = "${project.displayName} copy"
        var n = 2
        while (candidate in taken) {
            candidate = "${project.displayName} copy $n"
            n += 1
        }
        val saved = ProjectStore.save(context, ProjectStore.fileNameFor(candidate), bytes)
        haptic.hapticConfirm()
        notice = context.getString(
            R.string.projects_cloned,
            saved.removeSuffix(ProjectStore.EXT),
        )
        refreshTick += 1
    }

    /**
     * The project out, to Download/Rumo.
     *
     * By the same path as snapshots and clips: the file is needed so it can be
     * shared or saved, not only opened inside the app.
     */
    suspend fun exportProject(project: ProjectEntry) {
        val bytes = ProjectStore.load(context, project.fileName)
        if (bytes == null) {
            notice = context.getString(R.string.projects_read_failed, project.displayName)
            return
        }
        val result = saveBytesToFolder(
            context = context,
            kind = DownloadKind.TEXT,
            bytes = bytes,
            name = project.displayName + ProjectStore.EXT,
            folder = "Rumo",
            // A custom MIME, not `text/plain`: this is not text, and a file manager
            // should offer to open it with an app, not an editor.
            mime = "application/octet-stream",
        )
        when (result) {
            is SaveResult.Ok -> {
                haptic.hapticConfirm()
                notice = context.getString(R.string.projects_exported, result.path)
            }
            is SaveResult.Failed -> {
                haptic.hapticReject()
                notice = context.getString(R.string.projects_export_failed, result.reason)
            }
        }
    }

    // Runs again on every return to the tab, so a project saved in the editor or
    // by Rumi shows up without a manual refresh.
    LaunchedEffect(refreshTick) {
        projects = ProjectStore.list(context)
        loaded = true
        refreshing = false
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(stringResource(R.string.projects_title))
                        Text(
                            text = when {
                                !loaded -> stringResource(R.string.projects_reading)
                                projects.isEmpty() -> stringResource(R.string.projects_none)
                                else -> pluralStringResource(
                                    R.plurals.projects_saved_count,
                                    projects.size,
                                    projects.size,
                                )
                            },
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
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
        // Material's pull-to-refresh, not a homemade one: `PullToRefreshBox`
        // holds the gesture, the threshold and the indicator itself, and it is also the one used in
        // ExtraLabs. The indicator is not overridden: the component has its own, and it
        // is designed to morph from a circle into a container.
        val pullState = rememberPullToRefreshState()
        PullToRefreshBox(
            isRefreshing = refreshing,
            onRefresh = {
                haptic.hapticConfirm()
                refreshing = true
                refreshTick += 1
            },
            state = pullState,
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
        ) {
        LazyColumn(
            modifier = Modifier
                .fillMaxSize(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 16.dp, bottom = DockContentInset + 24.dp),
            // Within a group — Material's 2dp between segments. It used to be
            // 16dp here, and the join was visible only by colour: the rows
            // drifted apart and the group stopped reading as a capsule.
            verticalArrangement = Arrangement.spacedBy(NavSegmentGap),
        ) {
            if (loaded && projects.isEmpty()) {
                item(key = "empty") {
                    NavEmptyState(
                        title = stringResource(R.string.projects_empty_title),
                        message = stringResource(R.string.projects_empty_message),
                        icon = Icons.Rounded.Movie,
                        actionLabel = stringResource(R.string.projects_start_one),
                        onAction = { onNavigate(Routes.EDITOR) },
                    )
                }
            }
            itemsIndexed(projects, key = { _, p -> p.fileName }) { index, project ->
                NavSegment(
                    index = index,
                    count = projects.size,
                    headline = project.displayName,
                    supporting = projectSubtitle(project),
                    leadingIcon = Icons.Rounded.Movie,
                    onClick = { onNavigate(Routes.editorRoute(project.fileName)) },
                    // A menu instead of a delete icon.
                    //
                    // One icon showed one action out of four — and it was
                    // the only irreversible one. A menu button shows all of them
                    // and highlights none; delete sits in a separate group,
                    // so that the spacing says "this is different" before the word does.
                    trailing = {
                        NavItemMenu(
                            contentDescription = stringResource(
                                R.string.projects_actions,
                                project.displayName,
                            ),
                            icon = NavMenuIcons.Code,
                            groups = listOf(
                                listOf(
                                    NavMenuAction(
                                        stringResource(R.string.projects_rename),
                                        NavMenuIcons.Edit,
                                    ) {
                                        pendingRename = project
                                        renameText = project.displayName
                                    },
                                    NavMenuAction(
                                        stringResource(R.string.projects_clone),
                                        NavMenuIcons.Copy,
                                    ) {
                                        scope.launch { clone(project) }
                                    },
                                    NavMenuAction(
                                        stringResource(R.string.projects_export),
                                        NavMenuIcons.Export,
                                    ) {
                                        scope.launch { exportProject(project) }
                                    },
                                ),
                                listOf(
                                    NavMenuAction(
                                        stringResource(R.string.projects_delete),
                                        NavMenuIcons.Delete,
                                    ) {
                                        pendingDelete = project
                                    },
                                ),
                            ),
                        )
                    },
                )
            }
        }
        }
    }

    pendingRename?.let { project ->
        AlertDialog(
            onDismissRequest = { pendingRename = null },
            title = { Text(stringResource(R.string.projects_rename_title)) },
            text = {
                OutlinedTextField(
                    value = renameText,
                    onValueChange = { renameText = it },
                    label = { Text(stringResource(R.string.projects_rename_name)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        val target = pendingRename
                        pendingRename = null
                        if (target == null) return@TextButton
                        val clean = renameText.trim()
                        if (clean.isEmpty() || clean == target.displayName) return@TextButton
                        scope.launch {
                            val bytes = ProjectStore.load(context, target.fileName)
                            if (bytes == null) {
                                notice = context.getString(
                                    R.string.projects_read_failed,
                                    target.displayName,
                                )
                                return@launch
                            }
                            // A copy under the new name and deletion of the old one is not
                            // renaming a file but moving the content:
                            // that way the project name stays one and the same place
                            // of truth, rather than diverging between the file and what is
                            // inside.
                            ProjectStore.save(context, ProjectStore.fileNameFor(clean), bytes)
                            ProjectStore.delete(context, target.fileName)
                            haptic.hapticConfirm()
                            refreshTick += 1
                        }
                    },
                ) {
                    Text(stringResource(R.string.projects_rename))
                }
            },
            dismissButton = {
                TextButton(onClick = { pendingRename = null }) {
                    Text(stringResource(R.string.projects_cancel))
                }
            },
        )
    }

    notice?.let { message ->
        AlertDialog(
            onDismissRequest = { notice = null },
            title = { Text(stringResource(R.string.projects_title)) },
            text = { Text(message) },
            confirmButton = {
                TextButton(onClick = { notice = null }) {
                    Text(stringResource(R.string.projects_ok))
                }
            },
        )
    }

    pendingDelete?.let { project ->
        AlertDialog(
            onDismissRequest = { pendingDelete = null },
            title = { Text(stringResource(R.string.projects_delete_title)) },
            text = {
                Text(stringResource(R.string.projects_delete_message, project.displayName))
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingDelete = null
                        scope.launch {
                            ProjectStore.delete(context, project.fileName)
                            refreshTick += 1
                        }
                    },
                ) {
                    Text(stringResource(R.string.projects_delete))
                }
            },
            dismissButton = {
                TextButton(onClick = { pendingDelete = null }) {
                    Text(stringResource(R.string.projects_cancel))
                }
            },
        )
    }
}
