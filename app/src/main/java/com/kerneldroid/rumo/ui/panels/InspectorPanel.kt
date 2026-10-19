// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import androidx.annotation.StringRes
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.RotateRight
import androidx.compose.material.icons.rounded.AccountTree
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.AutoGraph
import androidx.compose.material.icons.rounded.BorderOuter
import androidx.compose.material.icons.rounded.CenterFocusStrong
import androidx.compose.material.icons.rounded.Check
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.DeleteSweep
import androidx.compose.material.icons.rounded.FormatBold
import androidx.compose.material.icons.rounded.Key
import androidx.compose.material.icons.rounded.Opacity
import androidx.compose.material.icons.rounded.RestartAlt
import androidx.compose.material.icons.rounded.Schedule
import androidx.compose.material.icons.rounded.Speed
import androidx.compose.material.icons.rounded.SwapHoriz
import androidx.compose.material.icons.rounded.SwapVert
import androidx.compose.material.icons.rounded.Tune
import androidx.compose.material.icons.rounded.ZoomOutMap
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.InputChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.FontStore
import com.kerneldroid.rumo.ui.EaseUi
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.KeyframeUi
import com.kerneldroid.rumo.ui.LayerKindUi
import com.kerneldroid.rumo.ui.LayerUi
import com.kerneldroid.rumo.ui.TransitionUi
import com.kerneldroid.rumo.ui.formatTime
import com.kerneldroid.rumo.ui.layerDisplayName
import com.kerneldroid.rumo.ui.theme.EditorType
import com.kerneldroid.rumo.ui.theme.RumoKind
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.editor
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.hapticLongPress
import com.kerneldroid.rumo.ui.theme.hapticToggle
import com.kerneldroid.rumo.ui.theme.monoNumerals
import kotlin.math.abs
import kotlin.math.roundToInt

/**
 * The named weights, for the chips above the slider.
 */
private val TextWeightSteps = listOf(
    R.string.panel_weight_thin to 100,
    R.string.panel_weight_extra_light to 200,
    R.string.panel_weight_light to 300,
    R.string.panel_weight_regular to 400,
    R.string.panel_weight_medium to 500,
    R.string.panel_weight_semi_bold to 600,
    R.string.panel_weight_bold to 700,
    R.string.panel_weight_extra_bold to 800,
    R.string.panel_weight_black to 900,
)

private enum class InspectorCategory(@StringRes val labelRes: Int) {
    ALL(R.string.panel_inspector_filter_all),
    TRANSFORM(R.string.panel_inspector_filter_transform),
    STYLE(R.string.panel_inspector_filter_style),
    EFFECTS(R.string.panel_inspector_filter_effects),
    TIMING(R.string.panel_inspector_filter_timing),
}

/** The name of a weight, or its number when it sits between the named ones. */
@Composable
private fun weightLabel(weight: Int): String {
    val labelRes = TextWeightSteps.firstOrNull { it.second == weight }?.first
    return if (labelRes != null) stringResource(labelRes) else weight.toString()
}

@Composable
private fun TextFontPicker(
    family: String,
    enabled: Boolean,
    onPick: (String) -> Unit,
) {
    val context = LocalContext.current
    var open by remember { mutableStateOf(false) }
    val installed = remember(open) {
        if (open) FontStore.installed(context) else emptyList()
    }

    SectionLabel(stringResource(R.string.panel_inspector_font))
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        InputChip(
            selected = false,
            onClick = { if (enabled) open = true },
            enabled = enabled,
            label = {
                Text(
                    text = family.ifEmpty { stringResource(R.string.panel_font_builtin) },
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            },
        )
        if (family.isNotEmpty()) {
            IconButton(onClick = { if (enabled) onPick("") }, enabled = enabled) {
                Icon(
                    Icons.Rounded.Close,
                    contentDescription = stringResource(R.string.panel_inspector_back_builtin),
                    modifier = Modifier.size(16.dp),
                )
            }
        }
    }

    if (open) {
        AlertDialog(
            onDismissRequest = { open = false },
            title = { Text(stringResource(R.string.panel_inspector_font)) },
            text = {
                LazyColumn(modifier = Modifier.heightIn(max = 360.dp)) {
                    item(key = "builtin") {
                        FontChoiceRow(
                            name = stringResource(R.string.panel_font_builtin),
                            subtitle = stringResource(R.string.panel_fonts_always_available),
                            selected = family.isEmpty(),
                            onClick = {
                                open = false
                                onPick("")
                            },
                        )
                    }
                    if (installed.isEmpty()) {
                        item(key = "empty") {
                            Text(
                                text = stringResource(R.string.panel_inspector_no_fonts),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(8.dp),
                            )
                        }
                    }
                    items(installed, key = { it.family }) { font ->
                        FontChoiceRow(
                            name = font.displayName,
                            subtitle = if (font.family == font.displayName) {
                                font.license
                            } else {
                                "${font.family} · ${font.license}"
                            },
                            selected = family == font.family,
                            onClick = {
                                open = false
                                onPick(font.family)
                            },
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = { open = false }) { Text(stringResource(R.string.action_close)) }
            },
        )
    }
}

@Composable
private fun FontChoiceRow(
    name: String,
    subtitle: String,
    selected: Boolean,
    onClick: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(name, style = MaterialTheme.typography.bodyLarge, maxLines = 1)
            if (subtitle.isNotEmpty()) {
                Text(
                    text = subtitle,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
            }
        }
        if (selected) {
            Icon(
                Icons.Rounded.Check,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
                modifier = Modifier.size(18.dp),
            )
        }
    }
}

/**
 * Redesigned Material 3 Properties Inspector (After Effects / Alight Motion style):
 * - Category filter bar (`All`, `Transform`, `Style`, `Effects`, `Timing`) for zero-hunting access.
 * - Complete Transform controls (`Position X`, `Position Y`, `Scale`, `Rotation`, plus 1-tap
 *   Center X / Center Y / Reset Transform buttons) for `SHAPE`, `TEXT`, and `MEDIA` layers.
 * - Multi-Track Keyframe Editor (`Rotation`, `Scale`, `Position X`, `Position Y`, `Opacity`)
 *   with an interactive Cubic Bézier Curve Graph Editor (`BezierCurveEditor`) and preset chips.
 * - Inline Transition (`TransitionUi`) & Timing (`startMs`, `durationMs`) controls.
 * - Full Effect chain and parameter editor.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun InspectorPanel(
    state: EditorState,
    layer: LayerUi?,
    playheadMs: Long,
    locked: Boolean,
    onOpenNodeGraph: () -> Unit,
    modifier: Modifier = Modifier,
) {
    if (layer == null) {
        Column(modifier = modifier.fillMaxSize()) {
            PanelHeader(title = stringResource(R.string.panel_properties))
            PanelEmpty(
                icon = Icons.Rounded.Tune,
                title = stringResource(R.string.panel_inspector_empty),
                hint = stringResource(R.string.panel_inspector_empty_hint),
            )
        }
        return
    }

    val haptic = LocalHapticFeedback.current
    val catalogue by state.effectCatalogueFlow.collectAsState()
    val chain = layer.effects
    val isBg = EditorState.isBackground(layer)
    val supportsColour = layer.kind == LayerKindUi.SHAPE || layer.kind == LayerKindUi.TEXT
    val showsColour = supportsColour && !state.isSvgLayer(layer)
    val visual = layer.kind != LayerKindUi.AUDIO

    var category by rememberSaveable { mutableStateOf(InspectorCategory.ALL) }
    var activeTrack by rememberSaveable(layer.id) {
        mutableStateOf(
            if (layer.kind == LayerKindUi.MEDIA) {
                EditorState.KeyTrack.SCALE
            } else {
                EditorState.KeyTrack.ROTATION
            },
        )
    }
    var pickerOpen by remember(layer.id) { mutableStateOf(false) }
    var selectedEffectId by rememberSaveable(layer.id) { mutableStateOf<String?>(null) }
    val selectedEffect = chain.firstOrNull { it.id == selectedEffectId } ?: chain.firstOrNull()

    Column(modifier = modifier.fillMaxSize()) {
        PanelHeader(
            title = stringResource(R.string.panel_properties),
            subtitle = if (locked) {
                stringResource(R.string.panel_inspector_locked, layerDisplayName(layer))
            } else {
                layerDisplayName(layer)
            },
            actions = {
                IconButton(onClick = onOpenNodeGraph, modifier = Modifier.size(28.dp)) {
                    Icon(
                        Icons.Rounded.AccountTree,
                        contentDescription = stringResource(R.string.panel_inspector_show_pipeline),
                        modifier = Modifier.size(18.dp),
                    )
                }
            },
        )

        if (visual) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = RumoSpacing.m, vertical = 2.dp),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                InspectorCategory.entries.forEach { cat ->
                    FilterChip(
                        selected = category == cat,
                        onClick = {
                            category = cat
                            haptic.hapticToggle(true)
                        },
                        label = {
                            Text(
                                text = stringResource(cat.labelRes),
                                style = EditorType.micro,
                            )
                        },
                        modifier = Modifier.height(28.dp),
                    )
                }
            }
        }

        Column(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = RumoSpacing.m),
        ) {
            val showStyle = category == InspectorCategory.ALL || category == InspectorCategory.STYLE
            val showTransform = category == InspectorCategory.ALL || category == InspectorCategory.TRANSFORM
            val showTiming = category == InspectorCategory.ALL || category == InspectorCategory.TIMING
            val showEffects = category == InspectorCategory.ALL || category == InspectorCategory.EFFECTS

            // 1. Text Typography & Outline (Style)
            if (showStyle && layer.kind == LayerKindUi.TEXT) {
                EditorSection(stringResource(R.string.panel_inspector_section_text)) {
                    Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
                        OutlinedTextField(
                            value = layer.text,
                            onValueChange = { state.setText(layer.id, it) },
                            enabled = !locked,
                            label = { Text(stringResource(R.string.panel_inspector_layer_text)) },
                            modifier = Modifier.fillMaxWidth(),
                        )

                        TextFontPicker(
                            family = layer.textFamily,
                            enabled = !locked,
                            onPick = {
                                haptic.hapticConfirm()
                                state.setTextFamily(layer.id, it)
                            },
                        )

                        SectionLabel(stringResource(R.string.panel_inspector_weight))
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .horizontalScroll(rememberScrollState()),
                            horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            TextWeightSteps.forEach { (labelRes, weight) ->
                                FilterChip(
                                    selected = layer.textWeight == weight,
                                    onClick = {
                                        haptic.hapticConfirm()
                                        state.setTextWeight(layer.id, weight)
                                    },
                                    enabled = !locked,
                                    label = { Text(stringResource(labelRes)) },
                                )
                            }
                        }

                        val weightName = weightLabel(layer.textWeight)
                        CompactSliderRow(
                            label = stringResource(R.string.panel_inspector_axis),
                            icon = Icons.Rounded.FormatBold,
                            value = layer.textWeight
                                .coerceIn(EditorState.MIN_TEXT_WEIGHT, EditorState.MAX_TEXT_WEIGHT)
                                .toFloat(),
                            valueRange = EditorState.MIN_TEXT_WEIGHT.toFloat()..
                                EditorState.MAX_TEXT_WEIGHT.toFloat(),
                            valueText = "$weightName · ${layer.textWeight}",
                            enabled = !locked,
                            onValueChange = { state.setTextWeight(layer.id, it.roundToInt()) },
                            onValueChangeFinished = { state.endGesture() },
                            onReset = { state.setTextWeight(layer.id, 400) },
                        )

                        SectionLabel(stringResource(R.string.panel_inspector_section_outline))
                        CompactSliderRow(
                            label = stringResource(R.string.panel_inspector_thickness),
                            icon = Icons.Rounded.BorderOuter,
                            value = layer.strokePx.coerceIn(0f, EditorState.MAX_STROKE_PX),
                            valueRange = 0f..EditorState.MAX_STROKE_PX,
                            valueText = if (layer.strokePx <= 0f) {
                                stringResource(R.string.panel_off)
                            } else {
                                "${layer.strokePx.roundToInt()} px"
                            },
                            enabled = !locked,
                            onValueChange = { state.setStrokePx(layer.id, it) },
                            onValueChangeFinished = { state.endGesture() },
                        )
                        var strokePickerOpen by remember(layer.id) { mutableStateOf(false) }
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .heightIn(min = MinTouchTarget),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                        ) {
                            Text(
                                text = stringResource(R.string.panel_inspector_outline_colour),
                                style = MaterialTheme.typography.labelLarge,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.weight(1f),
                            )
                            Text(
                                text = "#%06X".format(layer.strokeArgb and 0xFFFFFFL),
                                style = MaterialTheme.typography.labelMedium.merge(monoNumerals),
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            ColourSwatch(
                                colour = Color(layer.strokeArgb.toInt()),
                                enabled = !locked,
                                description = stringResource(R.string.panel_inspector_choose_outline_colour),
                                onClick = { strokePickerOpen = true },
                            )
                        }
                        if (strokePickerOpen) {
                            ColorPickerDialog(
                                title = stringResource(R.string.panel_inspector_outline_colour),
                                initial = Color(layer.strokeArgb.toInt()),
                                onColor = {
                                    state.setStrokeArgb(layer.id, it.toArgb().toLong() and 0xFFFFFFFFL)
                                },
                                onDismiss = { strokePickerOpen = false },
                            )
                        }
                    }
                }
            }

            // 2. Transform + Multi-Track Keyframes & Cubic Bézier Curve Graph (for all visual layers)
            if (showTransform && visual && !isBg) {
                EditorSection(stringResource(R.string.panel_inspector_section_transform)) {
                    Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
                        // Quick alignment & transform reset bar
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .horizontalScroll(rememberScrollState()),
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            InputChip(
                                selected = false,
                                onClick = {
                                    state.centerLayer(layer.id, horizontal = true, vertical = false)
                                    haptic.hapticConfirm()
                                },
                                enabled = !locked,
                                label = { Text(stringResource(R.string.panel_inspector_center_x), style = EditorType.micro) },
                                leadingIcon = {
                                    Icon(Icons.Rounded.SwapHoriz, contentDescription = null, modifier = Modifier.size(14.dp))
                                },
                                modifier = Modifier.height(28.dp),
                            )
                            InputChip(
                                selected = false,
                                onClick = {
                                    state.centerLayer(layer.id, horizontal = false, vertical = true)
                                    haptic.hapticConfirm()
                                },
                                enabled = !locked,
                                label = { Text(stringResource(R.string.panel_inspector_center_y), style = EditorType.micro) },
                                leadingIcon = {
                                    Icon(Icons.Rounded.SwapVert, contentDescription = null, modifier = Modifier.size(14.dp))
                                },
                                modifier = Modifier.height(28.dp),
                            )
                            InputChip(
                                selected = false,
                                onClick = {
                                    state.resetLayerTransform(layer.id)
                                    haptic.hapticConfirm()
                                },
                                enabled = !locked,
                                label = { Text(stringResource(R.string.panel_inspector_reset_transform), style = EditorType.micro) },
                                leadingIcon = {
                                    Icon(Icons.Rounded.RestartAlt, contentDescription = null, modifier = Modifier.size(14.dp))
                                },
                                modifier = Modifier.height(28.dp),
                            )
                        }

                        val curX = state.xAt(layer, playheadMs)
                        val curY = state.yAt(layer, playheadMs)
                        val curScale = state.scaleAt(layer, playheadMs)

                        TransformRow(
                            label = stringResource(R.string.panel_inspector_offset_x),
                            icon = Icons.Rounded.SwapHoriz,
                            value = curX.coerceIn(-600f, 600f),
                            valueRange = -600f..600f,
                            valueText = "${curX.roundToInt()} px",
                            enabled = !locked,
                            onValueChange = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.POSITION_X,
                                    it,
                                    playheadMs,
                                )
                            },
                            onValueChangeFinished = { state.endGesture() },
                            onReset = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.POSITION_X,
                                    0f,
                                    playheadMs,
                                )
                            },
                            onCommit = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.POSITION_X,
                                    it,
                                    playheadMs,
                                )
                            },
                        )
                        TransformRow(
                            label = stringResource(R.string.panel_inspector_offset_y),
                            icon = Icons.Rounded.SwapVert,
                            value = curY.coerceIn(-600f, 600f),
                            valueRange = -600f..600f,
                            valueText = "${curY.roundToInt()} px",
                            enabled = !locked,
                            onValueChange = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.POSITION_Y,
                                    it,
                                    playheadMs,
                                )
                            },
                            onValueChangeFinished = { state.endGesture() },
                            onReset = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.POSITION_Y,
                                    0f,
                                    playheadMs,
                                )
                            },
                            onCommit = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.POSITION_Y,
                                    it,
                                    playheadMs,
                                )
                            },
                        )
                        TransformRow(
                            label = stringResource(R.string.panel_inspector_scale),
                            icon = Icons.Rounded.ZoomOutMap,
                            value = curScale.coerceIn(0.05f, 4f),
                            valueRange = 0.05f..4f,
                            valueText = "${(curScale * 100f).roundToInt()}%",
                            enabled = !locked,
                            onValueChange = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.SCALE,
                                    it,
                                    playheadMs,
                                )
                            },
                            onValueChangeFinished = { state.endGesture() },
                            onReset = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.SCALE,
                                    1f,
                                    playheadMs,
                                )
                            },
                            onCommit = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.SCALE,
                                    it / 100f,
                                    playheadMs,
                                )
                            },
                        )

                        if (supportsColour) {
                            val rotation = state.rotationAt(layer, playheadMs)
                            TransformRow(
                                label = stringResource(R.string.panel_inspector_rotation),
                                icon = Icons.AutoMirrored.Rounded.RotateRight,
                                value = rotation.coerceIn(-180f, 180f),
                                valueRange = -180f..180f,
                                valueText = "${rotation.roundToInt()}°",
                                enabled = !locked,
                                onValueChange = {
                                    state.setPropertyOrKeyframe(
                                        layer.id,
                                        EditorState.KeyTrack.ROTATION,
                                        it,
                                        playheadMs,
                                    )
                                },
                                onValueChangeFinished = { state.endGesture() },
                                onReset = {
                                    state.setPropertyOrKeyframe(
                                        layer.id,
                                        EditorState.KeyTrack.ROTATION,
                                        0f,
                                        playheadMs,
                                    )
                                },
                                onCommit = {
                                    state.setPropertyOrKeyframe(
                                        layer.id,
                                        EditorState.KeyTrack.ROTATION,
                                        it,
                                        playheadMs,
                                    )
                                },
                            )
                        }

                        // Multi-Track Keyframes & Cubic Bézier Curve Editor
                        MultiTrackKeyframeSection(
                            state = state,
                            layer = layer,
                            activeTrack = activeTrack,
                            onSelectTrack = { activeTrack = it },
                            playheadMs = playheadMs,
                            enabled = !locked,
                        )
                    }
                }
            }

            if (!visual) {
                EditorSection(stringResource(R.string.panel_audio)) {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(vertical = 4.dp),
                        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                    ) {
                        MetaPill(stringResource(R.string.panel_inspector_no_gain))
                        MetaPill(
                            if (layer.uri != null) {
                                stringResource(R.string.panel_inspector_file_linked)
                            } else {
                                stringResource(R.string.panel_inspector_no_file)
                            },
                        )
                    }
                }
            }

            // 3. Appearance (Opacity & Colour)
            if (showStyle && visual) {
                EditorSection(stringResource(R.string.panel_inspector_section_appearance)) {
                    Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
                        val curAlpha = state.alphaBaseAt(layer, playheadMs)
                        CompactSliderRow(
                            label = stringResource(R.string.panel_inspector_opacity),
                            icon = Icons.Rounded.Opacity,
                            value = curAlpha.coerceIn(0.02f, 1f),
                            valueRange = 0.02f..1f,
                            valueText = "${(curAlpha * 100).roundToInt()}%",
                            enabled = !locked,
                            onValueChange = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.ALPHA,
                                    it,
                                    playheadMs,
                                )
                            },
                            onValueChangeFinished = { state.endGesture() },
                            onReset = {
                                state.setPropertyOrKeyframe(
                                    layer.id,
                                    EditorState.KeyTrack.ALPHA,
                                    1f,
                                    playheadMs,
                                )
                            },
                        )
                        if (showsColour) {
                            var colourPickerOpen by remember(layer.id) { mutableStateOf(false) }
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .heightIn(min = MinTouchTarget),
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                            ) {
                                Text(
                                    text = stringResource(R.string.panel_inspector_colour),
                                    style = MaterialTheme.typography.labelLarge,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                    modifier = Modifier.weight(1f),
                                )
                                Text(
                                    text = "#%06X".format(layer.argb and 0xFFFFFFL),
                                    style = MaterialTheme.typography.labelMedium.merge(monoNumerals),
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                                ColourSwatch(
                                    colour = Color(layer.argb.toInt()),
                                    enabled = !locked,
                                    description = stringResource(R.string.panel_inspector_choose_colour),
                                    onClick = { colourPickerOpen = true },
                                )
                            }
                            if (colourPickerOpen) {
                                ColorPickerDialog(
                                    title = stringResource(R.string.panel_inspector_layer_colour),
                                    initial = Color(layer.argb.toInt()),
                                    onColor = {
                                        val argb = it.toArgb().toLong() and 0xFFFFFFFFL
                                        if (isBg) {
                                            state.setBackground(argb)
                                        } else {
                                            state.setColor(layer.id, argb)
                                        }
                                    },
                                    onDismiss = { colourPickerOpen = false },
                                )
                            }
                        }
                    }
                }
            }

            // 4. Timing & Incoming Transition
            if (showTiming) {
                EditorSection(stringResource(R.string.panel_inspector_section_timing)) {
                    Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
                        if (!isBg) {
                            CompactSliderRow(
                                label = stringResource(R.string.panel_inspector_start_time),
                                icon = Icons.Rounded.Schedule,
                                value = layer.startMs.toFloat().coerceIn(0f, 60_000f),
                                valueRange = 0f..60_000f,
                                valueText = formatTime(layer.startMs),
                                enabled = !locked,
                                onValueChange = { state.setStartMs(layer.id, it.toLong()) },
                                onValueChangeFinished = { state.endGesture() },
                                onReset = { state.setStartMs(layer.id, 0L) },
                            )
                        }
                        CompactSliderRow(
                            label = stringResource(R.string.panel_inspector_clip_length),
                            icon = Icons.Rounded.Speed,
                            value = layer.durationMs.toFloat().coerceIn(200f, 60_000f),
                            valueRange = 200f..60_000f,
                            valueText = formatTime(layer.durationMs),
                            enabled = !locked && !isBg,
                            onValueChange = { state.setDuration(layer.id, it.toLong()) },
                            onValueChangeFinished = { state.endGesture() },
                        )
                        Text(
                            text = stringResource(
                                R.string.panel_inspector_drives_length,
                                formatTime(playheadMs),
                            ),
                            style = MaterialTheme.typography.labelSmall.merge(monoNumerals),
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )

                        if (visual && !isBg) {
                            Spacer(modifier = Modifier.height(4.dp))
                            SectionLabel(stringResource(R.string.panel_inspector_section_transition))
                            TransitionInlineEditor(
                                state = state,
                                layer = layer,
                                enabled = !locked,
                            )
                        }
                    }
                }
            }

            // 5. Effect Chain & Parameters
            if (showEffects && visual) {
                EditorSection(stringResource(R.string.panel_effects)) {
                    EffectSection(
                        state = state,
                        layer = layer,
                        selectedId = selectedEffect?.id,
                        onSelect = { selectedEffectId = it },
                        onRequestAdd = { pickerOpen = true },
                        enabled = !locked,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        }
    }

    if (pickerOpen) {
        AddEffectSheet(
            state = state,
            layer = layer,
            catalogue = catalogue,
            onPick = { kindId ->
                val addedId = state.addEffect(layer.id, kindId)
                if (addedId != null) selectedEffectId = addedId
                haptic.hapticConfirm()
                pickerOpen = false
            },
            onDismiss = { pickerOpen = false },
        )
    }
}

/**
 * Multi-track keyframe selector (`Rotation`, `Scale`, `Pos X`, `Pos Y`, `Opacity`),
 * keyframe chip bar, and interactive Cubic Bézier curve editor.
 */
@Composable
private fun MultiTrackKeyframeSection(
    state: EditorState,
    layer: LayerUi,
    activeTrack: EditorState.KeyTrack,
    onSelectTrack: (EditorState.KeyTrack) -> Unit,
    playheadMs: Long,
    enabled: Boolean,
) {
    val haptic = LocalHapticFeedback.current
    val tracks = remember(layer.kind) {
        if (layer.kind == LayerKindUi.MEDIA) {
            listOf(
                EditorState.KeyTrack.SCALE,
                EditorState.KeyTrack.POSITION_X,
                EditorState.KeyTrack.POSITION_Y,
                EditorState.KeyTrack.ALPHA,
            )
        } else {
            EditorState.KeyTrack.entries
        }
    }
    val resolvedTrack = if (activeTrack in tracks) activeTrack else tracks.first()
    val keys = state.keysOf(layer, resolvedTrack)
    val activeKey = state.activeKeyframeForCurve(layer, resolvedTrack, playheadMs)

    SectionLabel(stringResource(R.string.panel_inspector_section_keyframes))

    // Track selector chips
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        tracks.forEach { track ->
            val trackKeys = state.keysOf(layer, track)
            val animated = state.isTrackAnimated(layer, track)
            val labelText = trackLabel(track)
            FilterChip(
                selected = resolvedTrack == track,
                onClick = {
                    onSelectTrack(track)
                    haptic.hapticToggle(true)
                },
                label = {
                    Text(
                        text = if (animated) "$labelText (${trackKeys.size})" else labelText,
                        style = EditorType.micro,
                    )
                },
                leadingIcon = if (animated) {
                    {
                        Icon(
                            imageVector = Icons.Rounded.Key,
                            contentDescription = null,
                            tint = MaterialTheme.editor.signal,
                            modifier = Modifier.size(12.dp),
                        )
                    }
                } else {
                    null
                },
                modifier = Modifier.height(28.dp),
            )
        }
    }

    // Keyframe chips on the selected track
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(KeyChipHeight)
            .horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        for (key in keys) {
            InputChip(
                selected = abs(key.timeMs - playheadMs) <= 15L,
                onClick = { state.seekTo(key.timeMs) },
                modifier = Modifier.height(KeyChipHeight),
                shape = RoundedCornerShape(8.dp),
                enabled = enabled,
                label = {
                    Text(
                        text = "${formatTime(key.timeMs)} ${formatTrackValue(resolvedTrack, key.value)}",
                        style = MaterialTheme.typography.labelSmall.merge(monoNumerals),
                    )
                },
                trailingIcon = {
                    IconButton(
                        onClick = {
                            if (resolvedTrack == EditorState.KeyTrack.ROTATION && keys.size <= 1) {
                                state.addKeyframeAt(layer.id, 0L, 0f)
                            } else {
                                state.removePropertyKeyframe(layer.id, key.timeMs, resolvedTrack)
                            }
                            haptic.hapticLongPress()
                        },
                        enabled = enabled,
                        modifier = Modifier.size(24.dp),
                    ) {
                        Icon(
                            imageVector = Icons.Rounded.Close,
                            contentDescription = stringResource(R.string.panel_inspector_remove_key),
                            modifier = Modifier.size(14.dp),
                        )
                    }
                },
            )
        }
        InputChip(
            selected = false,
            onClick = {
                val v = state.valueAt(layer, resolvedTrack, playheadMs)
                if (resolvedTrack == EditorState.KeyTrack.ROTATION) {
                    state.addKeyframeAt(layer.id, playheadMs, v)
                } else {
                    state.addPropertyKeyframe(layer.id, playheadMs, resolvedTrack, v)
                }
                haptic.hapticConfirm()
            },
            modifier = Modifier.height(KeyChipHeight),
            shape = RoundedCornerShape(8.dp),
            enabled = enabled,
            label = {
                Text(
                    stringResource(R.string.panel_inspector_add_key),
                    style = MaterialTheme.typography.labelSmall,
                )
            },
            leadingIcon = {
                Icon(
                    imageVector = Icons.Rounded.Add,
                    contentDescription = null,
                    modifier = Modifier.size(16.dp),
                )
            },
        )
        if (keys.isNotEmpty() && (resolvedTrack != EditorState.KeyTrack.ROTATION || keys.size > 1)) {
            InputChip(
                selected = false,
                onClick = {
                    if (resolvedTrack == EditorState.KeyTrack.ROTATION) {
                        val cur = state.rotationAt(layer, playheadMs)
                        state.clearPropertyKeys(layer.id, resolvedTrack)
                        state.addKeyframeAt(layer.id, 0L, cur)
                    } else {
                        state.clearPropertyKeys(layer.id, resolvedTrack)
                    }
                    haptic.hapticConfirm()
                },
                modifier = Modifier.height(KeyChipHeight),
                shape = RoundedCornerShape(8.dp),
                enabled = enabled,
                label = {
                    Text(
                        stringResource(R.string.panel_inspector_clear_keys),
                        style = MaterialTheme.typography.labelSmall,
                    )
                },
                leadingIcon = {
                    Icon(
                        imageVector = Icons.Rounded.DeleteSweep,
                        contentDescription = null,
                        modifier = Modifier.size(15.dp),
                    )
                },
            )
        }
    }

    // Interactive Cubic Bézier Curve Editor when there are 2+ keyframes on the active track
    if (keys.size >= 2 && activeKey != null) {
        BezierCurveEditor(
            ease = activeKey.ease,
            keyTimeMs = activeKey.timeMs,
            enabled = enabled,
            onEaseChange = { nextEase ->
                state.setKeyEase(layer.id, resolvedTrack, activeKey.timeMs, nextEase)
            },
            onGestureEnd = { state.endGesture() },
        )
    }
}

@Composable
private fun trackLabel(track: EditorState.KeyTrack): String = when (track) {
    EditorState.KeyTrack.ROTATION -> stringResource(R.string.panel_inspector_track_rotation)
    EditorState.KeyTrack.SCALE -> stringResource(R.string.panel_inspector_track_scale)
    EditorState.KeyTrack.POSITION_X -> stringResource(R.string.panel_inspector_track_pos_x)
    EditorState.KeyTrack.POSITION_Y -> stringResource(R.string.panel_inspector_track_pos_y)
    EditorState.KeyTrack.ALPHA -> stringResource(R.string.panel_inspector_track_opacity)
}

private fun formatTrackValue(track: EditorState.KeyTrack, value: Float): String = when (track) {
    EditorState.KeyTrack.ROTATION -> "${value.roundToInt()}°"
    EditorState.KeyTrack.SCALE -> "${(value * 100f).roundToInt()}%"
    EditorState.KeyTrack.POSITION_X, EditorState.KeyTrack.POSITION_Y -> "${value.roundToInt()}px"
    EditorState.KeyTrack.ALPHA -> "${(value * 100f).roundToInt()}%"
}

/**
 * Interactive Alight Motion / After Effects style Cubic Bézier Curve Editor.
 * Allows selecting from [EaseUi.PRESETS] or dragging control points `P1(x1, y1)` and
 * `P2(x2, y2)` directly on the curve graph.
 */
@Composable
private fun BezierCurveEditor(
    ease: EaseUi,
    keyTimeMs: Long,
    enabled: Boolean,
    onEaseChange: (EaseUi) -> Unit,
    onGestureEnd: () -> Unit,
) {
    val haptic = LocalHapticFeedback.current
    val accent = MaterialTheme.editor.accent
    val signal = MaterialTheme.editor.signal
    val surfaceLow = MaterialTheme.colorScheme.surfaceContainerLowest
    val outlineVar = MaterialTheme.colorScheme.outlineVariant

    var p1x by remember(keyTimeMs, ease) {
        mutableFloatStateOf(if (ease.kind == EaseUi.Kind.CUBIC) ease.x1 else 0.33f)
    }
    var p1y by remember(keyTimeMs, ease) {
        mutableFloatStateOf(if (ease.kind == EaseUi.Kind.CUBIC) ease.y1 else 0.33f)
    }
    var p2x by remember(keyTimeMs, ease) {
        mutableFloatStateOf(if (ease.kind == EaseUi.Kind.CUBIC) ease.x2 else 0.67f)
    }
    var p2y by remember(keyTimeMs, ease) {
        mutableFloatStateOf(if (ease.kind == EaseUi.Kind.CUBIC) ease.y2 else 0.67f)
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerLow)
            .border(1.dp, outlineVar, RoundedCornerShape(12.dp))
            .padding(RumoSpacing.s),
        verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Icon(
                imageVector = Icons.Rounded.AutoGraph,
                contentDescription = null,
                tint = accent,
                modifier = Modifier.size(16.dp),
            )
            Text(
                text = stringResource(R.string.panel_inspector_curve_hint, formatTime(keyTimeMs)),
                style = EditorType.micro,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            if (ease.kind == EaseUi.Kind.CUBIC) {
                Text(
                    text = "(%.2f, %.2f · %.2f, %.2f)".format(ease.x1, ease.y1, ease.x2, ease.y2),
                    style = EditorType.micro.merge(monoNumerals),
                    color = signal,
                )
            }
        }

        // Preset curve chips
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            EaseUi.PRESETS.forEach { (name, preset) ->
                FilterChip(
                    selected = ease == preset,
                    onClick = {
                        onEaseChange(preset)
                        haptic.hapticConfirm()
                    },
                    enabled = enabled,
                    label = {
                        Text(
                            text = name.replace('_', ' ').uppercase(),
                            style = EditorType.micro,
                        )
                    },
                    modifier = Modifier.height(26.dp),
                )
            }
        }

        // Interactive 2D Bézier Curve Canvas
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(128.dp)
                .clip(RoundedCornerShape(8.dp))
                .background(surfaceLow)
                .border(1.dp, outlineVar, RoundedCornerShape(8.dp))
                .pointerInput(keyTimeMs, enabled) {
                    if (!enabled) return@pointerInput
                    var draggingHandle = 0 // 1 = P1, 2 = P2
                    detectDragGestures(
                        onDragStart = { pos ->
                            val w = size.width.toFloat().coerceAtLeast(1f)
                            val h = size.height.toFloat().coerceAtLeast(1f)
                            val padX = w * 0.1f
                            val padY = h * 0.18f
                            val gw = (w - padX * 2f).coerceAtLeast(1f)
                            val gh = (h - padY * 2f).coerceAtLeast(1f)
                            val h1 = Offset(padX + p1x * gw, padY + (1f - p1y) * gh)
                            val h2 = Offset(padX + p2x * gw, padY + (1f - p2y) * gh)
                            val d1 = (pos - h1).getDistanceSquared()
                            val d2 = (pos - h2).getDistanceSquared()
                            draggingHandle = if (d1 <= d2) 1 else 2
                            haptic.hapticToggle(true)
                        },
                        onDragEnd = {
                            draggingHandle = 0
                            onGestureEnd()
                            haptic.hapticConfirm()
                        },
                        onDragCancel = {
                            draggingHandle = 0
                            onGestureEnd()
                        },
                        onDrag = { change, _ ->
                            change.consume()
                            val w = size.width.toFloat().coerceAtLeast(1f)
                            val h = size.height.toFloat().coerceAtLeast(1f)
                            val padX = w * 0.1f
                            val padY = h * 0.18f
                            val gw = (w - padX * 2f).coerceAtLeast(1f)
                            val gh = (h - padY * 2f).coerceAtLeast(1f)
                            val nx = ((change.position.x - padX) / gw).coerceIn(0f, 1f)
                            val ny = (1f - (change.position.y - padY) / gh).coerceIn(-0.45f, 1.45f)
                            if (draggingHandle == 1) {
                                p1x = nx
                                p1y = ny
                            } else if (draggingHandle == 2) {
                                p2x = nx
                                p2y = ny
                            }
                            onEaseChange(EaseUi(EaseUi.Kind.CUBIC, p1x, p1y, p2x, p2y))
                        },
                    )
                },
        ) {
            val curvePath = remember { Path() }
            Canvas(modifier = Modifier.fillMaxSize()) {
                val w = size.width
                val h = size.height
                val padX = w * 0.1f
                val padY = h * 0.18f
                val gw = (w - padX * 2f).coerceAtLeast(1f)
                val gh = (h - padY * 2f).coerceAtLeast(1f)

                fun toScreen(nx: Float, ny: Float): Offset =
                    Offset(padX + nx * gw, padY + (1f - ny) * gh)

                // Grid box and quarter divisions
                val p0 = toScreen(0f, 0f)
                val p3 = toScreen(1f, 1f)
                for (i in 0..4) {
                    val f = i / 4f
                    val gx = padX + f * gw
                    val gy = padY + f * gh
                    drawLine(RumoKind.grid, Offset(gx, padY), Offset(gx, padY + gh), strokeWidth = 1f)
                    drawLine(RumoKind.grid, Offset(padX, gy), Offset(padX + gw, gy), strokeWidth = 1f)
                }
                // Diagonal linear reference line
                drawLine(
                    color = RumoKind.grid,
                    start = p0,
                    end = p3,
                    strokeWidth = 1.2f,
                    pathEffect = PathEffect.dashPathEffect(floatArrayOf(6f, 6f), 0f),
                )

                // Sampled curve path
                curvePath.reset()
                curvePath.moveTo(p0.x, p0.y)
                val steps = 40
                for (s in 1..steps) {
                    val t = s.toFloat() / steps.toFloat()
                    val y = ease.at(t)
                    val pt = toScreen(t, y)
                    curvePath.lineTo(pt.x, pt.y)
                }
                drawPath(
                    path = curvePath,
                    color = accent,
                    style = Stroke(width = 2.5.dp.toPx(), cap = StrokeCap.Round),
                )

                // Control stems and handles
                val c1 = toScreen(p1x, p1y)
                val c2 = toScreen(p2x, p2y)
                drawLine(signal.copy(alpha = 0.7f), p0, c1, strokeWidth = 1.5.dp.toPx())
                drawLine(signal.copy(alpha = 0.7f), p3, c2, strokeWidth = 1.5.dp.toPx())

                drawCircle(color = Color.White, radius = 4.dp.toPx(), center = p0)
                drawCircle(color = Color.White, radius = 4.dp.toPx(), center = p3)
                drawCircle(color = signal, radius = 6.5.dp.toPx(), center = c1)
                drawCircle(color = Color.Black, radius = 6.5.dp.toPx(), center = c1, style = Stroke(1.2.dp.toPx()))
                drawCircle(color = accent, radius = 6.5.dp.toPx(), center = c2)
                drawCircle(color = Color.Black, radius = 6.5.dp.toPx(), center = c2, style = Stroke(1.2.dp.toPx()))
            }
        }
    }
}

/**
 * Inline incoming transition editor (`TransitionUi`) so the user can configure
 * cross-dissolves, fade-ins, duration, and easing curves directly inside Properties
 * without opening the fullscreen Node Graph.
 */
@Composable
private fun TransitionInlineEditor(
    state: EditorState,
    layer: LayerUi,
    enabled: Boolean,
) {
    val haptic = LocalHapticFeedback.current
    val tr = layer.transition

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        FilterChip(
            selected = tr != null && tr.enabled,
            onClick = {
                if (tr == null) {
                    state.setTransition(
                        layer.id,
                        TransitionUi(
                            startMs = layer.startMs,
                            durationMs = 500L.coerceAtMost(layer.durationMs),
                            withPrevious = true,
                            enabled = true,
                        ),
                    )
                } else {
                    state.setTransition(layer.id, tr.copy(enabled = !tr.enabled))
                }
                haptic.hapticToggle(true)
            },
            enabled = enabled,
            label = {
                Text(
                    text = stringResource(
                        if (tr != null && tr.enabled) {
                            R.string.panel_nodes_enabled
                        } else {
                            R.string.panel_nodes_set_cross_fade
                        },
                    ),
                    style = EditorType.micro,
                )
            },
            modifier = Modifier.height(28.dp),
        )
        if (tr != null) {
            FilterChip(
                selected = tr.withPrevious,
                onClick = {
                    state.setTransition(layer.id, tr.copy(withPrevious = !tr.withPrevious))
                    haptic.hapticToggle(!tr.withPrevious)
                },
                enabled = enabled,
                label = {
                    Text(
                        text = stringResource(R.string.panel_nodes_with_below),
                        style = EditorType.micro,
                    )
                },
                modifier = Modifier.height(28.dp),
            )
            InputChip(
                selected = false,
                onClick = {
                    state.setTransition(layer.id, null)
                    haptic.hapticConfirm()
                },
                enabled = enabled,
                label = {
                    Text(
                        text = stringResource(R.string.panel_nodes_remove),
                        style = EditorType.micro,
                    )
                },
                modifier = Modifier.height(28.dp),
            )
        }
    }

    if (tr != null && tr.enabled) {
        CompactSliderRow(
            label = stringResource(R.string.panel_nodes_duration),
            icon = Icons.Rounded.Speed,
            value = tr.durationMs.toFloat().coerceIn(50f, 5_000f),
            valueRange = 50f..5_000f,
            valueText = "${tr.durationMs} ms",
            enabled = enabled,
            onValueChange = {
                state.setTransition(layer.id, tr.copy(durationMs = it.toLong()))
            },
            onValueChangeFinished = { state.endGesture() },
            onReset = {
                state.setTransition(layer.id, tr.copy(durationMs = 500L))
            },
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            listOf(
                "smooth" to EaseUi.SMOOTH,
                "linear" to EaseUi(),
                "ease_in" to EaseUi.EASE_IN,
                "ease_out" to EaseUi.EASE_OUT,
                "snap" to EaseUi.SNAP,
            ).forEach { (name, preset) ->
                FilterChip(
                    selected = tr.ease == preset,
                    onClick = {
                        state.setTransition(layer.id, tr.copy(ease = preset))
                        haptic.hapticConfirm()
                    },
                    enabled = enabled,
                    label = {
                        Text(
                            text = name.replace('_', ' ').uppercase(),
                            style = EditorType.micro,
                        )
                    },
                    modifier = Modifier.height(26.dp),
                )
            }
        }
    }
}

/**
 * One transform line: slider, value and reset.
 */
@Composable
private fun TransformRow(
    label: String,
    icon: ImageVector,
    value: Float,
    valueRange: ClosedFloatingPointRange<Float>,
    valueText: String,
    enabled: Boolean,
    onValueChange: (Float) -> Unit,
    onReset: () -> Unit,
    onCommit: (Float) -> Unit,
    onValueChangeFinished: () -> Unit = {},
) {
    var inputOpen by remember { mutableStateOf(false) }
    CompactSliderRow(
        label = label,
        icon = icon,
        value = value,
        valueRange = valueRange,
        valueText = valueText,
        enabled = enabled,
        onValueChange = onValueChange,
        onValueChangeFinished = onValueChangeFinished,
        onValueClick = { inputOpen = true },
        onReset = onReset,
    )
    if (inputOpen) {
        NumberInputDialog(
            title = label,
            initial = value.roundToInt().toString(),
            onCommit = onCommit,
            onDismiss = { inputOpen = false },
        )
    }
}

/** Key chip height: shorter than a property row, because there can be many of them. */
private val KeyChipHeight = 32.dp
