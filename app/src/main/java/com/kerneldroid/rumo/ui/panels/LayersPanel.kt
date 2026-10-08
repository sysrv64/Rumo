// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
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
import androidx.compose.material.icons.rounded.Check
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
import androidx.compose.ui.input.pointer.pointerInput
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
import com.kerneldroid.rumo.ui.layerDisplayName
import com.kerneldroid.rumo.ui.LayerKindUi
import com.kerneldroid.rumo.ui.LayerUi
import com.kerneldroid.rumo.ui.ShapeNames
import com.kerneldroid.rumo.ui.shapeOrdinalOf
import com.kerneldroid.rumo.ui.formatTime
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.hapticLongPress
import com.kerneldroid.rumo.ui.theme.hapticToggle

/**
 * Layer stack page. Row anatomy follows the reference editors
 * (Concat `timeline/track-header.slint`, Drift `TrackHeaderColumn.qml`):
 * grip · kind mark · name+badge · spacer · lock · overflow. Hide and Delete
 * live in the overflow: an always-visible eye was one tap-target too many, and
 * the badge says `HIDDEN` so the state stays legible.
 *
 * Drag a row by its handle to reorder: the drag steps one slot each time it crosses
 * half a row, so the order follows the finger without a floating ghost.
 */
@Composable
fun LayersPanel(
    state: EditorState,
    layers: List<LayerUi>,
    selectedId: String?,
    onSelect: (String) -> Unit,
    onAddLayer: () -> Unit,
    /// Layer deletion comes in from outside, so the screen can offer an undo.
    /// The panel used to delete it itself, and the only undo lived in the top bar's menu.
    onDeleteLayer: (String) -> Unit = { state.removeLayer(it) },
    modifier: Modifier = Modifier,
) {
    val locked by state.locked.collectAsState()
    val trisMap by state.shapeTris.collectAsState()
    val haptic = LocalHapticFeedback.current
    val density = LocalDensity.current
    val rowStep = with(density) { (LayerRowHeight + RumoSpacing.s).toPx() }
    // The drag coroutine is keyed on the layer id alone so a reorder cannot
    // cancel a gesture in flight, which means the list it captured at
    // composition time goes stale the moment the first step lands. Reading the
    // live list through `rememberUpdatedState` is what keeps the next step
    // counting from the *current* position instead of the original one (the
    // stale snapshot made a downward drag oscillate around the start index).
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
            // The list is shown **frontmost first**.
            //
            // The order in the model is the drawing order: index 0 is drawn
            // first, that is, it lands lowest of all. The panel, though, drew the
            // list as it was, so the top row was the **rearmost** layer, and
            // dragging down brought a layer forward. The user moved a photo up
            // expecting to see it on top — and it went backwards.
            //
            // The flip is here, not in the model: the document order stays what
            // is written to the file, and already saved projects do not change
            // their look because of the fix. The indices for the gesture are
            // recomputed below.
            val displayed = layers.asReversed()
            itemsIndexed(displayed, key = { _, layer -> layer.id }) { _, layer ->
                val isSelected = layer.id == selectedId
                val tris = if (layer.kind == LayerKindUi.SHAPE) {
                    trisMap[shapeOrdinalOf(layer.name)]
                } else {
                    null
                }
                // `buildString` is a plain lambda, not a composable scope, so the
                // resources are read before entering it.
                val kindLabel = stringResource(layer.kind.labelRes)
                val trisLabel = tris?.let { stringResource(R.string.panel_layers_tris, it) }
                val durationLabel =
                    if (layer.kind == LayerKindUi.MEDIA || layer.kind == LayerKindUi.AUDIO) {
                        formatTime(layer.durationMs)
                    } else {
                        null
                    }
                val keysLabel = if (layer.keys.size > 1) {
                    pluralStringResource(
                        R.plurals.panel_layers_keys,
                        layer.keys.size,
                        layer.keys.size,
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
                    // The row no longer carries an eye icon, so the badge is the
                    // only place that can say a layer is off the frame.
                    if (hiddenLabel != null) append(" · $hiddenLabel")
                    if (lockedLabel != null) append(" · $lockedLabel")
                }

                LayerRow(
                    layer = layer,
                    badge = badge,
                    count = layers.size,
                    selected = isSelected,
                    locked = layer.id in locked,
                    dragging = reorder.isDragging(layer.id),
                    dragOffset = reorder.offsetOf(layer.id),
                    onSelect = { onSelect(layer.id) },
                    onToggleVisible = { state.toggleVisibility(layer.id); haptic.hapticToggle(layer.visible) },
                    onToggleLock = { state.toggleLocked(layer.id); haptic.hapticToggle(true) },
                    onRename = { renameTarget = layer },
                    onChangeShape = { shapeTarget = layer },
                    onDelete = { onDeleteLayer(layer.id); haptic.hapticLongPress() },
                    // The same shared gesture as in the effects chain
                    // (EditorControls.reorderHandle): bound to the handle and
                    // starting immediately. One implementation for two panels —
                    // so the behaviour does not drift apart, the way it once did.
                    dragModifier = Modifier.reorderHandle(
                        state = reorder,
                        key = layer.id,
                        rowStepPx = rowStep,
                        onReorder = { delta ->
                            // The screen is flipped relative to the model, so the
                            // step is flipped too: dragging up (delta < 0) must
                            // bring the layer forward, that is, increase the
                            // index in the model.
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
    selected: Boolean,
    locked: Boolean,
    dragging: Boolean,
    dragOffset: Float,
    onSelect: () -> Unit,
    onToggleVisible: () -> Unit,
    onToggleLock: () -> Unit,
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
        // The gesture lives on the handle, not on the row. An immediate drag
        // hung on the whole row would swallow the list's scrolling: a vertical
        // swipe on a row would travel with the row instead of scrolling the
        // list. The handle is a finger-sized target, and that is what it is drawn
        // for.
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
        // The lock was removed from the row: it duplicated the "Lock" item in the
        // menu below, that is, it was a second control for one verb — and on top
        // of that it showed a state an unlocked layer does not have. One entry
        // point remains, in the menu, and a locked layer is still visible: the
        // row's badge says "LOCKED", and that is a state, not a button.
        Box {
            IconButton(onClick = { menuOpen = true }) {
                Icon(
                    imageVector = Icons.Rounded.MoreVert,
                    contentDescription = stringResource(R.string.panel_layers_menu),
                    modifier = Modifier.size(20.dp),
                )
            }
            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                DropdownMenuItem(
                    // A SHAPE layer's name *is* the shape kind the engine draws
                    // (see shapeOrdinalOf), so it gets "Change shape" instead of
                    // a free-text rename that would erase it from the frame.
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
