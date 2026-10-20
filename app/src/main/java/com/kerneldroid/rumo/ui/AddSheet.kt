// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.Image
import androidx.compose.material.icons.rounded.Movie
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.TextFields
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.panels.ActionTile
import com.kerneldroid.rumo.ui.panels.CompactChip
import com.kerneldroid.rumo.ui.theme.RumoKind
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.editor
import com.kerneldroid.rumo.ui.theme.hapticConfirm

// 1:1 with rumo-render ShapeKind variants in all_shapes() order.
val ShapeNames: List<String> = listOf(
    "Circle",
    "Square",
    "Slanted",
    "Arch",
    "Fan",
    "Arrow",
    "SemiCircle",
    "Oval",
    "Pill",
    "Triangle",
    "Diamond",
    "ClamShell",
    "Pentagon",
    "Gem",
    "Sunny",
    "VerySunny",
    "Cookie4Sided",
    "Cookie6Sided",
    "Cookie7Sided",
    "Cookie9Sided",
    "Cookie12Sided",
    "Ghostish",
    "Clover4Leaf",
    "Clover8Leaf",
    "Burst",
    "SoftBurst",
    "Boom",
    "SoftBoom",
    "Flower",
    "Puffy",
    "PuffyDiamond",
    "PixelCircle",
    "PixelTriangle",
    "Bun",
    "Heart",
    // Appended, never inserted: the ordinal IS what a layer stores, so an
    // insertion would repaint every project saved before it as a different
    // shape. "Frame" is the background — it is drawn to cover the target rather
    // than placed inside it (see the special case in `preview_shape_draws`),
    // which is what lets the background be an ordinary layer and therefore take
    // effects, opacity and keyframes.
    "Frame",
)

/**
 * What to call a shape in the picker.
 */
@Composable
fun shapeDisplayName(name: String): String =
    if (name == "Frame") stringResource(R.string.editor_shape_frame) else name

private val LegacyShapeAliases = mapOf("rounded rectangle 1" to "Square")

/** Engine ordinal for a SHAPE layer name, or -1 when the engine has no such kind. */
fun shapeOrdinalOf(name: String): Int {
    val direct = ShapeNames.indexOf(name)
    if (direct >= 0) return direct
    val alias = LegacyShapeAliases[name.trim().lowercase()] ?: return -1
    return ShapeNames.indexOf(alias)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AddSheet(
    state: EditorState,
    onDismiss: () -> Unit,
    pickers: MediaPickers? = null,
) {
    val haptic = LocalHapticFeedback.current
    val trisMap by state.shapeTris.collectAsState()
    val meshes by state.meshes.collectAsState()
    var tab by remember { mutableIntStateOf(0) }

    LaunchedEffect(Unit) {
        state.ensureMeshes(ShapeNames)
    }

    val tabLabels = listOf(
        stringResource(R.string.editor_add_tab_shapes),
        stringResource(R.string.editor_add_tab_text),
        stringResource(R.string.editor_add_tab_media),
    )

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = RumoSpacing.l, vertical = RumoSpacing.xs),
            horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s, Alignment.CenterHorizontally),
        ) {
            tabLabels.forEachIndexed { index, label ->
                CompactChip(
                    label = label,
                    selected = tab == index,
                    onClick = { tab = index },
                )
            }
        }
        when (tab) {
            0 -> {
                LazyVerticalGrid(
                    columns = GridCells.Fixed(3),
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(max = 400.dp),
                    contentPadding = PaddingValues(RumoSpacing.m),
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                    verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                ) {
                    items(ShapeNames.size) { ordinal ->
                        val name = ShapeNames[ordinal]
                        val shown = shapeDisplayName(name)
                        val tris = trisMap[ordinal]
                        val mesh = meshes[name]
                        val placeholderColor = MaterialTheme.editor.line
                        Column(
                            modifier = Modifier
                                .clip(RoundedCornerShape(16.dp))
                                .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                                .clickable {
                                    state.addShape(name)
                                    haptic.hapticConfirm()
                                    onDismiss()
                                }
                                .padding(vertical = RumoSpacing.s, horizontal = RumoSpacing.xs),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                        ) {
                            Canvas(modifier = Modifier.size(38.dp)) {
                                val points = mesh?.points
                                if (points.isNullOrEmpty()) {
                                    drawRoundRect(
                                        color = placeholderColor,
                                        cornerRadius = CornerRadius(6f, 6f),
                                    )
                                    return@Canvas
                                }
                                var minX = Float.MAX_VALUE
                                var minY = Float.MAX_VALUE
                                var maxX = -Float.MAX_VALUE
                                var maxY = -Float.MAX_VALUE
                                for (p in points) {
                                    if (p.first < minX) minX = p.first
                                    if (p.first > maxX) maxX = p.first
                                    if (p.second < minY) minY = p.second
                                    if (p.second > maxY) maxY = p.second
                                }
                                val span = maxOf(maxX - minX, maxY - minY).coerceAtLeast(1f)
                                val scale = (size.minDimension * 0.86f) / span
                                val ox = size.width / 2f - (minX + maxX) / 2f * scale
                                val oy = size.height / 2f - (minY + maxY) / 2f * scale
                                val path = Path()
                                var i = 0
                                while (i + 2 < points.size) {
                                    val a = points[i]
                                    val b = points[i + 1]
                                    val c = points[i + 2]
                                    path.moveTo(a.first * scale + ox, a.second * scale + oy)
                                    path.lineTo(b.first * scale + ox, b.second * scale + oy)
                                    path.lineTo(c.first * scale + ox, c.second * scale + oy)
                                    path.close()
                                    i += 3
                                }
                                drawPath(path = path, color = RumoKind.shapeMark)
                            }
                            Text(
                                text = shown,
                                style = MaterialTheme.typography.labelSmall,
                                textAlign = TextAlign.Center,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.fillMaxWidth(),
                            )
                            Text(
                                text = if (tris != null) stringResource(R.string.panel_media_tris, tris) else "…",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                textAlign = TextAlign.Center,
                                maxLines = 1,
                            )
                        }
                    }
                }
            }
            1 -> {
                var query by remember { mutableStateOf("") }
                val titlePreset = stringResource(R.string.editor_add_preset_title)
                val subtitlePreset = stringResource(R.string.editor_add_preset_subtitle)
                val captionPreset = stringResource(R.string.editor_add_preset_caption)
                val badgePreset = stringResource(R.string.editor_add_preset_badge)

                fun insertPresetText(
                    defaultText: String,
                    weight: Int,
                    scale: Float,
                    strokePx: Float,
                    offsetY: Float,
                ) {
                    val content = query.ifBlank { defaultText }
                    state.beginUndoBatch()
                    try {
                        state.addTextLayer(content)
                        val newId = state.selectedId.value
                        if (newId != null) {
                            state.setTextWeight(newId, weight)
                            state.setScale(newId, scale)
                            if (strokePx > 0f) state.setStrokePx(newId, strokePx)
                            if (offsetY != 0f) state.setOffset(newId, 0f, offsetY)
                        }
                    } finally {
                        state.endUndoBatch()
                    }
                    haptic.hapticConfirm()
                    onDismiss()
                }

                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(RumoSpacing.l),
                    verticalArrangement = Arrangement.spacedBy(RumoSpacing.m),
                ) {
                    OutlinedTextField(
                        value = query,
                        onValueChange = { if (it.length <= 140 && !it.contains('\n')) query = it },
                        label = { Text(stringResource(R.string.panel_inspector_layer_text)) },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .horizontalScroll(rememberScrollState()),
                        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        FilterChip(
                            selected = false,
                            onClick = { insertPresetText(titlePreset, 800, 1.45f, 2f, 0f) },
                            label = { Text(titlePreset) },
                            leadingIcon = { Icon(Icons.Rounded.TextFields, contentDescription = null, modifier = Modifier.size(16.dp)) },
                        )
                        FilterChip(
                            selected = false,
                            onClick = { insertPresetText(subtitlePreset, 600, 1.0f, 0f, 36f) },
                            label = { Text(subtitlePreset) },
                            leadingIcon = { Icon(Icons.Rounded.TextFields, contentDescription = null, modifier = Modifier.size(16.dp)) },
                        )
                        FilterChip(
                            selected = false,
                            onClick = { insertPresetText(captionPreset, 500, 0.78f, 1.5f, 84f) },
                            label = { Text(captionPreset) },
                            leadingIcon = { Icon(Icons.Rounded.TextFields, contentDescription = null, modifier = Modifier.size(16.dp)) },
                        )
                        FilterChip(
                            selected = false,
                            onClick = { insertPresetText(badgePreset, 900, 0.9f, 3f, -84f) },
                            label = { Text(badgePreset) },
                            leadingIcon = { Icon(Icons.Rounded.TextFields, contentDescription = null, modifier = Modifier.size(16.dp)) },
                        )
                    }
                    Button(
                        onClick = {
                            state.addTextLayer(query.ifBlank { titlePreset })
                            haptic.hapticConfirm()
                            onDismiss()
                        },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Icon(Icons.Rounded.Add, contentDescription = null, modifier = Modifier.size(18.dp))
                        Text(
                            text = stringResource(R.string.panel_add),
                            modifier = Modifier.padding(start = RumoSpacing.xs),
                        )
                    }
                }
            }
            else -> {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(RumoSpacing.l),
                    verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                ) {
                    ActionTile(
                        icon = Icons.Rounded.Image,
                        label = stringResource(R.string.editor_add_photo),
                        onClick = { pickers?.launchImage() },
                    )
                    ActionTile(
                        icon = Icons.Rounded.Movie,
                        label = stringResource(R.string.editor_add_video),
                        onClick = { pickers?.launchVideo() },
                    )
                    ActionTile(
                        icon = Icons.Rounded.MusicNote,
                        label = stringResource(R.string.editor_add_audio),
                        onClick = { pickers?.launchAudio() },
                    )
                }
            }
        }
    }
}
