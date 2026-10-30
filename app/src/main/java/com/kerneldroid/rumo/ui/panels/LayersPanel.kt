// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.ArrowDownward
import androidx.compose.material.icons.rounded.ArrowUpward
import androidx.compose.material.icons.rounded.Check
import androidx.compose.material.icons.rounded.ContentCopy
import androidx.compose.material.icons.rounded.ContentCut
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.DragHandle
import androidx.compose.material.icons.rounded.Layers
import androidx.compose.material.icons.rounded.Lock
import androidx.compose.material.icons.rounded.LockOpen
import androidx.compose.material.icons.rounded.MoreVert
import androidx.compose.material.icons.rounded.Visibility
import androidx.compose.material.icons.rounded.VisibilityOff
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.zIndex
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.LayerKindUi
import com.kerneldroid.rumo.ui.LayerUi
import com.kerneldroid.rumo.ui.ShapeNames
import com.kerneldroid.rumo.ui.formatTime
import com.kerneldroid.rumo.ui.layerDisplayName
import com.kerneldroid.rumo.ui.shapeOrdinalOf
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.editor
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.hapticLongPress
import com.kerneldroid.rumo.ui.theme.hapticToggle

/**
 * Layer stack page with frontmost-first display, drag-to-reorder handle, direct
 * 1-tap visibility eye & lock toggles, and rich layer actions (Duplicate, Split
 * at Playhead, Move Up/Down, Change Shape / Rename, Delete).
 */
@Composable
fun LayersPanel(
    state: EditorState,
    layers: List<LayerUi>,
    selectedId: String?,
    onSelect: (String) -> Unit,
    onAddLayer: () -> Unit,
    onDeleteLayer: (String) -> Unit = { state.removeLayer(it) },
    modifier: Modifier = Modifier,
) {
    val locked by state.locked.collectAsState()
    val trisMap by state.shapeTris.collectAsState()
    val playheadMs by state.playheadMs.collectAsState()
    val haptic = LocalHapticFeedback.current
    val density = LocalDensity.current
    val rowStep = with(density) { (LayerRowHeight + RumoSpacing.s).toPx() }
    val liveLayers by rememberUpdatedState(layers)

    val reorder = rememberReorderState()
    var renameTarget by remember { mutableStateOf<LayerUi?>(null) }
    var shapeTarget by remember { mutableStateOf<LayerUi?>(null) }

    Column(modifier = modifier.fillMaxSize()) {
        PanelHeader(
            title = stringResource(R.string.panel_layers),
            subtitle = pluralStringResource(
                R.plurals.panel_layers_subtitle,
                layers.size,
                layers.size,
            ),
            actions = {
                IconButton(onClick = onAddLayer) {
                    Icon(Icons.Rounded.Add, contentDescription = stringResource(R.string.panel_add_layer))
                }
            },
        )
        if (layers.isEmpty()) {
            PanelEmpty(
                icon = Icons.Rounded.Layers,
                title = stringResource(R.string.panel_layers_empty),
                hint = stringResource(R.string.panel_layers_empty_hint),
            )
            return@Column
        }
        LazyColumn(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f),
            contentPadding = PaddingValues(
                start = RumoSpacing.m,
                end = RumoSpacing.m,
                bottom = RumoSpacing.m,
            ),
            verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
        ) {
            val displayed = layers.asReversed()
            itemsIndexed(displayed, key = { _, layer -> layer.id }) { _, layer ->
                val isSelected = layer.id == selectedId
                val isBg = EditorState.isBackground(layer)
                val modelIdx = layers.indexOfFirst { it.id == layer.id }
                val tris = if (layer.kind == LayerKindUi.SHAPE) {
                    trisMap[shapeOrdinalOf(layer.name)]
                } else {
                    null
                }
                val totalKeys = state.allKeyframeTimes(layer).size
                val kindLabel = stringResource(layer.kind.labelRes)
                val trisLabel = tris?.let { stringResource(R.string.panel_layers_tris, it) }
                val durationLabel =
                    if (layer.kind == LayerKindUi.MEDIA || layer.kind == LayerKindUi.AUDIO) {
                        formatTime(layer.durationMs)
                    } else {
                        null
                    }
                val keysLabel = if (totalKeys > 0) {
                    pluralStringResource(
                        R.plurals.panel_layers_keys,
                        totalKeys,
                        totalKeys,
                    )
                } else {
                    null
                }
                val hiddenLabel = if (!layer.visible) {
                    stringResource(R.string.panel_layers_hidden)
                } else {
                    null
                }
                val lockedLabel = if (layer.id in locked) {
                    stringResource(R.string.panel_layers_locked)
                } else {
                    null
                }
                val badge = buildString {
                    append(kindLabel)
                    when {
                        trisLabel != null -> append(" · $trisLabel")
                        layer.kind == LayerKindUi.MEDIA || layer.kind == LayerKindUi.AUDIO ->
                            append(" · $durationLabel")
                    }
                    if (keysLabel != null) append(" · $keysLabel")
                    if (layer.effects.isNotEmpty()) append(" · FX ${layer.effects.size}")
                    if (hiddenLabel != null) append(" · $hiddenLabel")
                    if (lockedLabel != null) append(" · $lockedLabel")
                }

                LayerRow(
                    layer = layer,
                    badge = badge,
                    count = layers.size,
                    isBackground = isBg,
                    canMoveUp = modelIdx in 0 until layers.lastIndex,
                    canMoveDown = modelIdx > 0,
                    selected = isSelected,
                    locked = layer.id in locked,
                    dragging = reorder.isDragging(layer.id),
                    dragOffset = reorder.offsetOf(layer.id),
                    onSelect = { onSelect(layer.id) },
                    onToggleVisible = { state.toggleVisibility(layer.id); haptic.hapticToggle(layer.visible) },
                    onToggleLock = { state.toggleLocked(layer.id); haptic.hapticToggle(true) },
                    onDuplicate = {
                        if (state.duplicateLayer(layer.id) != null) {
                            haptic.hapticConfirm()
                        }
                    },
                    onSplit = {
                        if (state.splitLayer(layer.id, playheadMs) != null) {
                            haptic.hapticConfirm()
                        }
                    },
                    onMoveUp = {
                        if (modelIdx in 0 until layers.lastIndex) {
                            state.moveLayerOrder(modelIdx, modelIdx + 1)
                            haptic.hapticToggle(true)
                        }
                    },
                    onMoveDown = {
                        if (modelIdx > 0) {
                            state.moveLayerOrder(modelIdx, modelIdx - 1)
                            haptic.hapticToggle(false)
                        }
                    },
                    onRename = { renameTarget = layer },
                    onChangeShape = { shapeTarget = layer },
                    onDelete = { onDeleteLayer(layer.id); haptic.hapticLongPress() },
                    dragModifier = Modifier.reorderHandle(
                        state = reorder,
                        key = layer.id,
                        rowStepPx = rowStep,
                        onReorder = { delta ->
                            val from = liveLayers.indexOfFirst { it.id == layer.id }
                            if (from >= 0) state.moveLayerOrder(from, from - delta)
                            haptic.hapticToggle(delta < 0)
                        },
                        onStart = { haptic.hapticLongPress() },
                        onEnd = { haptic.hapticConfirm() },
                    ),
                )
            }
        }
    }

    renameTarget?.let { target ->
        RenameLayerDialog(
            initial = target.name,
            onDismiss = { renameTarget = null },
            onConfirm = {
                state.renameLayer(target.id, it)
                renameTarget = null
            },
        )
    }

    shapeTarget?.let { target ->
        ChangeShapeDialog(
            current = target.name,
            onDismiss = { shapeTarget = null },
            onPick = {
                state.setShapeName(target.id, it)
                shapeTarget = null
            },
        )
    }
}

@Composable
private fun ChangeShapeDialog(
    current: String,
    onDismiss: () -> Unit,
    onPick: (String) -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.panel_layers_change_shape)) },
        text = {
            LazyColumn(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(320.dp),
            ) {
                items(ShapeNames) { name ->
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(12.dp))
                            .clickable { onPick(name) }
                            .padding(horizontal = RumoSpacing.s, vertical = RumoSpacing.s),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            text = name,
                            style = MaterialTheme.typography.bodyMedium,
                            modifier = Modifier.weight(1f),
                        )
                        if (name == current) {
                            Icon(
                                imageVector = Icons.Rounded.Check,
                                contentDescription = stringResource(R.string.panel_layers_current_shape),
                                tint = MaterialTheme.colorScheme.primary,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.panel_cancel)) }
        },
    )
}

private val LayerRowHeight = 56.dp

@Composable
private fun LayerRow(
    layer: LayerUi,
    badge: String,
    count: Int,
    isBackground: Boolean,
    canMoveUp: Boolean,
    canMoveDown: Boolean,
    selected: Boolean,
    locked: Boolean,
    dragging: Boolean,
    dragOffset: Float,
    onSelect: () -> Unit,
    onToggleVisible: () -> Unit,
    onToggleLock: () -> Unit,
    onDuplicate: () -> Unit,
    onSplit: () -> Unit,
    onMoveUp: () -> Unit,
    onMoveDown: () -> Unit,
    onRename: () -> Unit,
    onChangeShape: () -> Unit,
    onDelete: () -> Unit,
    dragModifier: Modifier,
) {
    var menuOpen by remember(layer.id) { mutableStateOf(false) }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(LayerRowHeight)
            .zIndex(if (dragging) 1f else 0f)
            .graphicsLayer {
                if (dragging) {
                    translationY = dragOffset
                    scaleX = 1.02f
                    scaleY = 1.02f
                }
            }
            .clip(RoundedCornerShape(16.dp))
            .background(
                when {
                    dragging -> MaterialTheme.colorScheme.surfaceContainerHighest
                    selected -> MaterialTheme.colorScheme.secondaryContainer
                    else -> MaterialTheme.colorScheme.surfaceContainerHigh
                },
            )
            .clickable(onClick = onSelect)
            .padding(start = RumoSpacing.xs, end = RumoSpacing.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            modifier = Modifier
                .size(MinTouchTarget)
                .then(dragModifier),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = Icons.Rounded.DragHandle,
                contentDescription = stringResource(R.string.panel_layers_reorder),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(18.dp),
            )
        }
        Box(modifier = Modifier.size(RumoSpacing.xs))
        KindMark(kind = layer.kind, size = 34.dp)
        Box(modifier = Modifier.size(RumoSpacing.s))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = layerDisplayName(layer),
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                color = if (selected) {
                    MaterialTheme.colorScheme.onSecondaryContainer
                } else {
                    MaterialTheme.colorScheme.onSurface
                },
            )
            Text(
                text = badge,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        // Direct 1-tap visibility eye toggle on the layer row.
        IconButton(
            onClick = onToggleVisible,
            modifier = Modifier.size(36.dp),
        ) {
            Icon(
                imageVector = if (layer.visible) Icons.Rounded.Visibility else Icons.Rounded.VisibilityOff,
                contentDescription = stringResource(if (layer.visible) R.string.panel_hide else R.string.panel_show),
                tint = if (layer.visible) {
                    MaterialTheme.colorScheme.onSurfaceVariant
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.45f)
                },
                modifier = Modifier.size(18.dp),
            )
        }
        if (!isBackground) {
            IconButton(
                onClick = onToggleLock,
                modifier = Modifier.size(36.dp),
            ) {
                Icon(
                    imageVector = if (locked) Icons.Rounded.Lock else Icons.Rounded.LockOpen,
                    contentDescription = stringResource(if (locked) R.string.panel_unlock else R.string.panel_lock),
                    tint = if (locked) {
                        MaterialTheme.editor.danger
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.55f)
                    },
                    modifier = Modifier.size(17.dp),
                )
            }
        }
        Box {
            IconButton(onClick = { menuOpen = true }, modifier = Modifier.size(36.dp)) {
                Icon(
                    imageVector = Icons.Rounded.MoreVert,
                    contentDescription = stringResource(R.string.panel_layers_menu),
                    modifier = Modifier.size(20.dp),
                )
            }
            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                if (!isBackground) {
                    DropdownMenuItem(
                        text = {
                            Text(
                                stringResource(
                                    if (layer.kind == LayerKindUi.SHAPE) {
                                        R.string.panel_layers_change_shape
                                    } else {
                                        R.string.panel_rename
                                    },
                                ),
                            )
                        },
                        onClick = {
                            menuOpen = false
                            if (layer.kind == LayerKindUi.SHAPE) onChangeShape() else onRename()
                        },
                    )
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.panel_layers_duplicate)) },
                        onClick = {
                            menuOpen = false
                            onDuplicate()
                        },
                        leadingIcon = {
                            Icon(Icons.Rounded.ContentCopy, contentDescription = null)
                        },
                    )
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.panel_layers_split)) },
                        onClick = {
                            menuOpen = false
                            onSplit()
                        },
                        enabled = !locked,
                        leadingIcon = {
                            Icon(Icons.Rounded.ContentCut, contentDescription = null)
                        },
                    )
                }
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.panel_layers_move_up)) },
                    onClick = {
                        menuOpen = false
                        onMoveUp()
                    },
                    enabled = canMoveUp,
                    leadingIcon = {
                        Icon(Icons.Rounded.ArrowUpward, contentDescription = null)
                    },
                )
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.panel_layers_move_down)) },
                    onClick = {
                        menuOpen = false
                        onMoveDown()
                    },
                    enabled = canMoveDown,
                    leadingIcon = {
                        Icon(Icons.Rounded.ArrowDownward, contentDescription = null)
                    },
                )
                DropdownMenuItem(
                    text = {
                        Text(
                            stringResource(
                                if (layer.visible) R.string.panel_hide else R.string.panel_show,
                            ),
                        )
                    },
                    onClick = {
                        menuOpen = false
                        onToggleVisible()
                    },
                    leadingIcon = {
                        Icon(
                            imageVector = if (layer.visible) {
                                Icons.Rounded.VisibilityOff
                            } else {
                                Icons.Rounded.Visibility
                            },
                            contentDescription = null,
                        )
                    },
                )
                if (!isBackground) {
                    DropdownMenuItem(
                        text = {
                            Text(
                                stringResource(
                                    if (locked) R.string.panel_unlock else R.string.panel_lock,
                                ),
                            )
                        },
                        onClick = {
                            menuOpen = false
                            onToggleLock()
                        },
                        leadingIcon = {
                            Icon(
                                imageVector = if (locked) {
                                    Icons.Rounded.LockOpen
                                } else {
                                    Icons.Rounded.Lock
                                },
                                contentDescription = null,
                            )
                        },
                    )
                    HorizontalDivider()
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.panel_delete)) },
                        onClick = {
                            menuOpen = false
                            onDelete()
                        },
                        enabled = count > 1,
                        leadingIcon = {
                            Icon(
                                Icons.Rounded.Delete,
                                contentDescription = null,
                                tint = MaterialTheme.colorScheme.error,
                            )
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun RenameLayerDialog(
    initial: String,
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit,
) {
    var text by remember { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.panel_rename_title)) },
        text = {
            OutlinedTextField(
                value = text,
                onValueChange = { if (it.length <= 60) text = it },
                singleLine = true,
                label = { Text(stringResource(R.string.panel_rename_name)) },
                modifier = Modifier.fillMaxWidth(),
            )
        },
        confirmButton = {
            TextButton(onClick = { onConfirm(text) }, enabled = text.isNotBlank()) {
                Text(stringResource(R.string.panel_rename))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.panel_cancel)) }
        },
    )
}
