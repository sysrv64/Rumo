// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import androidx.annotation.StringRes
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.AccountTree
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.Animation
import androidx.compose.material.icons.rounded.AutoFixHigh
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.CompareArrows
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.Key
import androidx.compose.material.icons.rounded.Opacity
import androidx.compose.material.icons.rounded.OpenInFull
import androidx.compose.material.icons.rounded.Output
import androidx.compose.material.icons.rounded.RestartAlt
import androidx.compose.material.icons.rounded.RotateRight
import androidx.compose.material.icons.rounded.Schedule
import androidx.compose.material.icons.rounded.Speed
import androidx.compose.material.icons.rounded.SwapHoriz
import androidx.compose.material.icons.rounded.SwapVert
import androidx.compose.material.icons.rounded.Tune
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.Effect
import com.kerneldroid.rumo.ui.LayerKindUi
import com.kerneldroid.rumo.ui.LayerUi
import com.kerneldroid.rumo.ui.TransitionUi
import com.kerneldroid.rumo.ui.formatTime
import com.kerneldroid.rumo.ui.theme.editor
import com.kerneldroid.rumo.ui.theme.RumoKind
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.hapticToggle
import com.kerneldroid.rumo.ui.theme.monoNumerals
import kotlin.math.roundToInt

/** Stages of the real compositor pipeline that a layer kind passes through. */
private enum class NodeStage(@StringRes val titleRes: Int) {
    SOURCE(R.string.panel_node_source),
    TRANSFORM(R.string.panel_node_transform),
    APPEARANCE(R.string.panel_node_appearance),
    TRANSITION(R.string.panel_node_transition),
    TIME(R.string.panel_node_timing),
    OUTPUT(R.string.panel_node_output),
}

private fun stagesFor(kind: LayerKindUi): List<NodeStage> = when (kind) {
    // The engine composites MEDIA as a centred quad (no per-layer offset), and
    // AUDIO never enters the frame — those kinds only have a source and a sink.
    // The cross-fade needs the frame, so AUDIO does not get that node.
    LayerKindUi.SHAPE -> listOf(
        NodeStage.SOURCE,
        NodeStage.TRANSFORM,
        NodeStage.APPEARANCE,
        NodeStage.TRANSITION,
        NodeStage.TIME,
        NodeStage.OUTPUT,
    )
    LayerKindUi.TEXT -> listOf(
        NodeStage.SOURCE,
        NodeStage.TRANSFORM,
        NodeStage.APPEARANCE,
        NodeStage.TRANSITION,
        NodeStage.TIME,
        NodeStage.OUTPUT,
    )
    LayerKindUi.MEDIA -> listOf(
        NodeStage.SOURCE,
        NodeStage.TRANSITION,
        NodeStage.OUTPUT,
    )
    LayerKindUi.AUDIO -> listOf(NodeStage.SOURCE, NodeStage.OUTPUT)
}

/**
 * One node of the selected layer's graph: a fixed pipeline stage, or a real
 * effect from the layer's chain.
 *
 * The effect nodes are the ones that make the graph usable — the chain is
 * executed by the engine, so a node can actually be added to it from here,
 * unlike the fixed stage list, which mirrors a pipeline the engine owns.
 */
private sealed interface GraphNode {
    val key: String

    /** The node's name as a word: a stage's title is a resource, an effect's is the engine's. */
    @Composable
    fun title(): String

    data class Stage(val stage: NodeStage) : GraphNode {
        override val key: String get() = stage.name

        @Composable
        override fun title(): String = stringResource(stage.titleRes)
    }

    data class Fx(
        val effect: Effect,
        val descriptor: RumoBridge.EffectDescriptor?,
    ) : GraphNode {
        override val key: String get() = "effect/${effect.id}"

        @Composable
        // The same resolver the effects panel uses, so a node and its row in the
        // chain cannot name the same effect differently.
        override fun title(): String =
            effectDisplayName(effect.kindId, descriptor?.label ?: effect.kindId)
    }
}

/**
 * A node's icon. A composable lookup rather than a property of the interface,
 * because a stage's icon is one.
 */
@Composable
private fun GraphNode.icon(): ImageVector = when (this) {
    is GraphNode.Stage -> stage.icon()
    is GraphNode.Fx -> Icons.Rounded.AutoFixHigh
}

/**
 * The stage order with the layer's effect chain spliced onto the pixel path:
 * the chain runs on the layer's composited image, so it sits between
 * Appearance and Timing.
 */
private fun graphNodes(
    layer: LayerUi,
    catalogue: List<RumoBridge.EffectDescriptor>,
): List<GraphNode> {
    val stages = stagesFor(layer.kind)
    val (pixelPath, tail) = stages.partition {
        it != NodeStage.TIME && it != NodeStage.OUTPUT
    }
    val fx = layer.effects.map { effect ->
        GraphNode.Fx(effect, catalogue.firstOrNull { it.id == effect.kindId })
    }
    return pixelPath.map { GraphNode.Stage(it) } + fx + tail.map { GraphNode.Stage(it) }
}

@Composable
private fun GraphNode.summary(layer: LayerUi, renderPath: String): String = when (this) {
    is GraphNode.Stage -> stageSummary(stage, layer, renderPath)
    is GraphNode.Fx -> {
        // Not `buildString`: its lambda is a plain one, and the resources have to be
        // read in a composable scope.
        val body = if (descriptor == null) {
            stringResource(R.string.panel_effects_unavailable)
        } else {
            pluralStringResource(
                R.plurals.panel_nodes_params,
                descriptor.params.size,
                descriptor.params.size,
            )
        }
        if (effect.enabled) body else body + " · " + stringResource(R.string.panel_off)
    }
}

/** Compact graph node: 130×52dp, so the box shrinks to this. */
private val NodeW = 130.dp
private val NodeH = 52.dp

@Composable
private fun NodeStage.icon(): ImageVector = when (this) {
    NodeStage.SOURCE -> Icons.Rounded.Tune
    NodeStage.TRANSFORM -> Icons.Rounded.SwapHoriz
    NodeStage.APPEARANCE -> Icons.Rounded.Opacity
    NodeStage.TRANSITION -> Icons.Rounded.CompareArrows
    NodeStage.TIME -> Icons.Rounded.Speed
    NodeStage.OUTPUT -> Icons.Rounded.Output
}

/**
 * One node's controls in a sheet: the stage module, or an effect's own
 * parameters. Shared by the panel and the full-screen graph so both entry
 * points behave identically.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun NodeSheet(
    node: GraphNode,
    layer: LayerUi,
    state: EditorState,
    playheadMs: Long,
    renderPath: String,
    engineActive: Boolean,
    locked: Boolean,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = RumoSpacing.m, end = RumoSpacing.m, bottom = RumoSpacing.l),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
            ) {
                Box(
                    modifier = Modifier
                        .size(24.dp)
                        .clip(RoundedCornerShape(8.dp))
                        .background(layer.kind.mark.copy(alpha = 0.18f)),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        imageVector = node.icon(),
                        contentDescription = null,
                        tint = layer.kind.mark,
                        modifier = Modifier.size(14.dp),
                    )
                }
                Text(
                    text = node.title(),
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    text = node.summary(layer, renderPath),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Spacer(modifier = Modifier.size(RumoSpacing.s))
            when (node) {
                is GraphNode.Stage -> NodeBody(
                    stage = node.stage,
                    layer = layer,
                    state = state,
                    playheadMs = playheadMs,
                    renderPath = renderPath,
                    engineActive = engineActive,
                    locked = locked,
                )

                is GraphNode.Fx -> FxNodeBody(
                    effect = node.effect,
                    descriptor = node.descriptor,
                    state = state,
                    layerId = layer.id,
                )
            }
        }
    }
}

/**
 * An effect node's parameters, edited where the node is. Enable/Remove live here
 * because the node is the thing being configured — the chain list stays the
 * place for ordering.
 */
@Composable
private fun FxNodeBody(
    effect: Effect,
    descriptor: RumoBridge.EffectDescriptor?,
    state: EditorState,
    layerId: String,
) {
    val haptic = LocalHapticFeedback.current
    Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(
                onClick = {
                    state.toggleEffect(layerId, effect.id)
                    haptic.hapticToggle(!effect.enabled)
                },
            ) {
                Text(stringResource(if (effect.enabled) R.string.panel_nodes_disable else R.string.panel_nodes_enable))
            }
            Spacer(modifier = Modifier.weight(1f))
            TextButton(
                onClick = {
                    state.removeEffect(layerId, effect.id)
                    haptic.hapticConfirm()
                },
            ) {
                Icon(
                    imageVector = Icons.Rounded.Delete,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.error,
                    modifier = Modifier.size(16.dp),
                )
                Spacer(modifier = Modifier.size(RumoSpacing.xs))
                Text(stringResource(R.string.panel_nodes_remove), color = MaterialTheme.colorScheme.error)
            }
        }
        if (descriptor == null) {
            MetaPill(
                text = stringResource(R.string.panel_effects_unknown),
                tint = MaterialTheme.colorScheme.error,
            )
        } else if (descriptor.params.isEmpty()) {
            MetaPill(stringResource(R.string.panel_effects_no_params))
        } else {
            for (param in descriptor.params) {
                EffectParamEditor(
                    param = param,
                    effect = effect,
                    onComponent = { component, value ->
                        state.setEffectParamValue(
                            layerId = layerId,
                            effectId = effect.id,
                            key = param.key,
                            component = component,
                            value = value,
                        )
                    },
                )
            }
        }
    }
}

/** One overview row of the chain: module, its live summary, tap for parameters. */
@Composable
private fun GraphNodeRow(
    node: GraphNode,
    summary: String,
    accent: Color,
    hasInput: Boolean,
    hasOutput: Boolean,
    onClick: () -> Unit,
) {
    val haptic = LocalHapticFeedback.current
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
            .clickable {
                haptic.hapticToggle(true)
                onClick()
            }
            .padding(horizontal = RumoSpacing.s, vertical = RumoSpacing.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
    ) {
        PortDot(visible = hasInput, accent = accent)
        Box(
            modifier = Modifier
                .size(24.dp)
                .clip(RoundedCornerShape(8.dp))
                .background(accent.copy(alpha = 0.18f)),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = node.icon(),
                contentDescription = null,
                tint = accent,
                modifier = Modifier.size(14.dp),
            )
        }
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = node.title(),
                style = MaterialTheme.typography.labelLarge,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
            )
            Text(
                text = summary,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        PortDot(visible = hasOutput, accent = accent)
    }
}

/** Vertical bezier + socket dots between two cards. */
@Composable
private fun NodeWire(modifier: Modifier = Modifier) {
    // The wire is a graph line, not data: it must follow the scheme like
    // any other line. A literal used to sit here, and it left the graph
    // gray under every seed.
    val wireColor = MaterialTheme.editor.lineStrong
    Canvas(modifier = modifier.width(24.dp)) {
        val cx = size.width / 2f
        val path = Path().apply {
            moveTo(cx, 0f)
            cubicTo(cx, size.height, cx, 0f, cx, size.height)
        }
        drawPath(
            path = path,
            color = wireColor,
            style = Stroke(width = 2.dp.toPx()),
        )
        drawCircle(color = wireColor, radius = 3.dp.toPx(), center = Offset(cx, size.height))
    }
}

@Composable
private fun PortDot(visible: Boolean, accent: Color) {
    Box(modifier = Modifier.width(10.dp)) {
        if (visible) {
            Box(
                modifier = Modifier
                    .size(8.dp)
                    .clip(CircleShape)
                    .background(accent),
            )
        }
    }
}

@Composable
private fun stageSummary(stage: NodeStage, layer: LayerUi, renderPath: String): String =
    when (stage) {
        // The kind is a word here too — `kind.name` is the enum constant.
        NodeStage.SOURCE -> {
            val kindLabel = stringResource(layer.kind.labelRes)
            "$kindLabel · ${layer.name}"
        }
        NodeStage.TRANSFORM ->
            "x ${layer.offsetX.toInt()} · y ${layer.offsetY.toInt()}"
        NodeStage.APPEARANCE ->
            "#${layer.argb.toString(16).uppercase()} · ${(layer.alpha * 100).toInt()}%"
        NodeStage.TRANSITION -> layer.transition?.let { tr ->
            val kind = if (tr.withPrevious) {
                stringResource(R.string.panel_nodes_cross_fade)
            } else {
                stringResource(R.string.panel_nodes_fade_in)
            }
            if (!tr.enabled) {
                kind + " · " + stringResource(R.string.panel_off)
            } else {
                "$kind · ${formatTime(tr.durationMs)}"
            }
        } ?: stringResource(R.string.panel_nodes_not_set)
        NodeStage.TIME ->
            formatTime(layer.durationMs) + " · " +
                pluralStringResource(
                    R.plurals.panel_nodes_keys,
                    layer.keys.size,
                    layer.keys.size,
                )
        NodeStage.OUTPUT -> stringResource(R.string.panel_nodes_output, renderPath.uppercase())
    }

/**
 * The layer's cross-fade against the one under it.
 *
 * The node is always in the chain — this is where a transition gets a window, not
 * a switch that only exists while on. Nothing here is faked: the window becomes
 * the per-frame alphas the compositor blends with, so the dissolve is two real
 * layers meeting on the GPU. That is also why it needs the layer below: with
 * `withPrevious` off, the layer just fades up from transparent.
 */
@Composable
private fun TransitionBody(layer: LayerUi, state: EditorState) {
    val haptic = LocalHapticFeedback.current
    val tr = layer.transition
    if (tr == null) {
        MetaPill(stringResource(R.string.panel_nodes_no_cross_fade))
        ActionTile(
            icon = Icons.Rounded.Add,
            label = stringResource(R.string.panel_nodes_set_cross_fade),
            onClick = {
                state.setTransition(layer.id, TransitionUi())
                haptic.hapticConfirm()
            },
        )
        Text(
            text = stringResource(R.string.panel_nodes_cross_fade_hint),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        return
    }

    val minMs = TransitionUi.MIN_DURATION_MS.toFloat()
    val maxMs = TransitionUi.MAX_DURATION_MS.toFloat()
    var dragStart by remember(layer.id) { mutableStateOf<Float?>(null) }
    var dragDuration by remember(layer.id) { mutableStateOf<Float?>(null) }
    ParamSlider(
        label = stringResource(R.string.panel_nodes_start),
        icon = Icons.Rounded.Schedule,
        value = (dragStart ?: tr.startMs.toFloat()).coerceIn(0f, maxMs),
        valueRange = 0f..maxMs,
        valueText = formatTime((dragStart ?: tr.startMs.toFloat()).toLong()),
        onValueChange = {
            dragStart = it
            state.setTransition(layer.id, tr.copy(startMs = it.toLong()))
        },
        onValueChangeFinished = { dragStart = null },
    )
    ParamSlider(
        label = stringResource(R.string.panel_nodes_duration),
        icon = Icons.Rounded.Animation,
        value = (dragDuration ?: tr.durationMs.toFloat()).coerceIn(minMs, maxMs),
        valueRange = minMs..maxMs,
        valueText = formatTime((dragDuration ?: tr.durationMs.toFloat()).toLong()),
        onValueChange = {
            dragDuration = it
            state.setTransition(layer.id, tr.copy(durationMs = it.toLong()))
        },
        onValueChangeFinished = { dragDuration = null },
    )
    SwitchRow(
        label = stringResource(R.string.panel_nodes_with_below),
        checked = tr.withPrevious,
        onCheckedChange = { state.setTransition(layer.id, tr.copy(withPrevious = it)) },
    )
    SwitchRow(
        label = stringResource(R.string.panel_nodes_enabled),
        checked = tr.enabled,
        onCheckedChange = { state.setTransition(layer.id, tr.copy(enabled = it)) },
    )
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.End,
    ) {
        TextButton(
            onClick = {
                state.setTransition(layer.id, null)
                haptic.hapticConfirm()
            },
        ) {
            Icon(
                imageVector = Icons.Rounded.Delete,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.error,
                modifier = Modifier.size(16.dp),
            )
            Spacer(modifier = Modifier.size(RumoSpacing.xs))
            Text(stringResource(R.string.panel_nodes_remove), color = MaterialTheme.colorScheme.error)
        }
    }
}

/** Two-word boolean control: a label and the switch. */
@Composable
private fun SwitchRow(
    label: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelLarge,
            modifier = Modifier.weight(1f),
        )
        Switch(checked = checked, onCheckedChange = onCheckedChange)
    }
}

@Composable
private fun NodeBody(
    stage: NodeStage,
    layer: LayerUi,
    state: EditorState,
    playheadMs: Long,
    renderPath: String,
    engineActive: Boolean,
    locked: Boolean,
) {
    val haptic = LocalHapticFeedback.current
    when (stage) {
        NodeStage.SOURCE -> Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
            ) {
                KindMark(kind = layer.kind, size = 30.dp, iconSize = 16.dp)
                Text(
                    text = layer.uri ?: layer.name,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }

        NodeStage.TRANSFORM -> Column {
            var dragX by remember(layer.id) { mutableStateOf<Float?>(null) }
            var dragY by remember(layer.id) { mutableStateOf<Float?>(null) }
            ParamSlider(
                label = stringResource(R.string.panel_inspector_offset_x),
                icon = Icons.Rounded.SwapVert,
                enabled = !locked,
                value = (dragX ?: layer.offsetX).coerceIn(-400f, 400f),
                valueRange = -400f..400f,
                valueText = "${(dragX ?: layer.offsetX).toInt()} px",
                onValueChange = {
                    dragX = it
                    state.setOffset(layer.id, it, dragY ?: layer.offsetY)
                },
                onValueChangeFinished = { dragX = null },
            )
            ParamSlider(
                label = stringResource(R.string.panel_inspector_offset_y),
                icon = Icons.Rounded.SwapHoriz,
                enabled = !locked,
                value = (dragY ?: layer.offsetY).coerceIn(-400f, 400f),
                valueRange = -400f..400f,
                valueText = "${(dragY ?: layer.offsetY).toInt()} px",
                onValueChange = {
                    dragY = it
                    state.setOffset(layer.id, dragX ?: layer.offsetX, it)
                },
                onValueChangeFinished = { dragY = null },
            )
            if (locked) {
                Text(
                    text = stringResource(R.string.panel_nodes_locked),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        NodeStage.APPEARANCE -> Column {
            var dragAlpha by remember(layer.id) { mutableStateOf<Float?>(null) }
            ParamSlider(
                label = stringResource(R.string.panel_inspector_opacity),
                icon = Icons.Rounded.Opacity,
                value = (dragAlpha ?: layer.alpha).coerceIn(0.02f, 1f),
                valueRange = 0.02f..1f,
                valueText = "${((dragAlpha ?: layer.alpha) * 100).toInt()}%",
                onValueChange = {
                    dragAlpha = it
                    state.setAlpha(layer.id, it)
                },
                onValueChangeFinished = { dragAlpha = null },
            )
        }

        NodeStage.TRANSITION -> TransitionBody(layer = layer, state = state)

        NodeStage.TIME -> Column {
            var dragDuration by remember(layer.id) { mutableStateOf<Float?>(null) }
            ParamSlider(
                label = stringResource(R.string.panel_nodes_duration),
                icon = Icons.Rounded.Animation,
                value = (dragDuration ?: layer.durationMs.toFloat()).coerceIn(200f, 60_000f),
                valueRange = 200f..60_000f,
                valueText = formatTime((dragDuration ?: layer.durationMs.toFloat()).toLong()),
                onValueChange = {
                    dragDuration = it
                    state.setDuration(layer.id, it.toLong())
                },
                onValueChangeFinished = { dragDuration = null },
            )
            // The rotation editor used to be duplicated here and in the
            // properties page — two widgets for one scalar track, with different
            // shapes and different affordances. There is one copy now, in the
            // page that owns the layer's transform (docs/10 §7.10), and this
            // pipeline view only *shows* the stage.
            Text(
                text = pluralStringResource(
                    R.plurals.panel_nodes_rotation_keys,
                    layer.keys.size,
                    layer.keys.size,
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        NodeStage.OUTPUT -> Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
            Row(horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
                MetaPill(stringResource(layer.kind.labelRes))
                MetaPill(stringResource(R.string.panel_nodes_scene))
                MetaPill(
                    stringResource(
                        if (engineActive) R.string.panel_nodes_surface else R.string.panel_nodes_bitmap,
                    ),
                )
                MetaPill(renderPath.uppercase())
            }
        }
    }
}

/** Full-screen graph canvas: draggable nodes, bezier wires, pan + zoom. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NodeGraphOverlay(
    state: EditorState,
    layer: LayerUi?,
    playheadMs: Long,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val renderPath by state.renderPath.collectAsState()
    val engineActive by state.engineActive.collectAsState()
    val saved by state.nodePos.collectAsState()
    val density = LocalDensity.current
    val haptic = LocalHapticFeedback.current

    val gapY = 40.dp
    val nodeWPx = with(density) { NodeW.toPx() }
    val nodeHPx = with(density) { NodeH.toPx() }
    val gapPx = with(density) { gapY.toPx() }
    val startX = with(density) { 28.dp.toPx() }
    val startY = with(density) { 28.dp.toPx() }
    val marginPx = with(density) { 16.dp.toPx() }

    // Subscribing to the catalogue instead of reading a getter: an effect
    // installed in the shop must appear in the graph without restarting the app.
    val catalogue by state.effectCatalogueFlow.collectAsState()
    val nodes = layer?.let { graphNodes(it, catalogue) } ?: emptyList()
    var scale by remember { mutableStateOf(1f) }
    var panX by remember { mutableStateOf(0f) }
    var panY by remember { mutableStateOf(0f) }
    var fitRequest by remember { mutableIntStateOf(0) }
    var pickerOpen by remember(layer?.id) { mutableStateOf(false) }
    var sheetKey by remember(layer?.id) { mutableStateOf<String?>(null) }

    fun positionOf(index: Int, key: String): Pair<Float, Float> =
        saved[key] ?: (startX to (startY + index * (nodeHPx + gapPx)))

    Surface(
        modifier = modifier.fillMaxSize(),
        color = MaterialTheme.colorScheme.surfaceContainerLowest,
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .windowInsetsPadding(WindowInsets.statusBars),
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = RumoSpacing.m, vertical = RumoSpacing.s),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(
                    imageVector = Icons.Rounded.AccountTree,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.size(20.dp),
                )
                Spacer(modifier = Modifier.size(RumoSpacing.s))
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = stringResource(R.string.editor_node_graph),
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        text = stringResource(
                            R.string.panel_nodes_subtitle,
                            layer?.name ?: stringResource(R.string.panel_nodes_no_layer),
                        ),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                IconButton(
                    onClick = { pickerOpen = true },
                    enabled = catalogue.isNotEmpty() &&
                        layer?.kind != LayerKindUi.AUDIO,
                ) {
                    Icon(
                        Icons.Rounded.Add,
                        contentDescription = stringResource(R.string.panel_nodes_add_effect),
                    )
                }
                TextButton(
                    onClick = {
                        for (node in nodes) state.clearNodePos(nodeKey(layer?.id, node))
                        scale = 1f
                        panX = 0f
                        panY = 0f
                        fitRequest++
                        haptic.hapticConfirm()
                    },
                ) {
                    Icon(
                        imageVector = Icons.Rounded.RestartAlt,
                        contentDescription = null,
                        modifier = Modifier.size(16.dp),
                    )
                    Spacer(modifier = Modifier.size(RumoSpacing.xs))
                    Text(stringResource(R.string.panel_nodes_reset))
                }
                IconButton(onClick = onDismiss) {
                    Icon(
                        Icons.Rounded.Close,
                        contentDescription = stringResource(R.string.panel_nodes_close),
                    )
                }
            }
            BoxWithConstraints(
                modifier = Modifier
                    .fillMaxSize()
                    .clip(RoundedCornerShape(20.dp))
                    .background(MaterialTheme.colorScheme.surfaceContainerLow)
                    .pointerInput(Unit) {
                        detectTransformGestures { _, pan, zoom, _ ->
                            scale = (scale * zoom).coerceIn(0.5f, 2.2f)
                            panX += pan.x
                            panY += pan.y
                        }
                    },
            ) {
                val viewW = with(density) { maxWidth.toPx() }
                val viewH = with(density) { maxHeight.toPx() }

                // Initial view: fit the whole chain (bounding box of the current
                // positions, saved or default) instead of starting at 1:1 and
                // making the user pinch-scroll to find the nodes.
                LaunchedEffect(layer?.id, viewW, viewH, fitRequest) {
                    if (nodes.isEmpty() || viewW <= 0f || viewH <= 0f) return@LaunchedEffect
                    var minX = Float.MAX_VALUE
                    var minY = Float.MAX_VALUE
                    var maxX = -Float.MAX_VALUE
                    var maxY = -Float.MAX_VALUE
                    nodes.forEachIndexed { index, node ->
                        val (px, py) = positionOf(index, nodeKey(layer?.id, node))
                        minX = minOf(minX, px)
                        minY = minOf(minY, py)
                        maxX = maxOf(maxX, px + nodeWPx)
                        maxY = maxOf(maxY, py + nodeHPx)
                    }
                    val boxW = (maxX - minX).coerceAtLeast(1f)
                    val boxH = (maxY - minY).coerceAtLeast(1f)
                    val fitted = minOf(
                        (viewW - marginPx * 2f) / boxW,
                        (viewH - marginPx * 2f) / boxH,
                    ).coerceIn(0.3f, 1f)
                    scale = fitted
                    panX = (viewW - boxW * fitted) / 2f - minX * fitted
                    panY = (viewH - boxH * fitted) / 2f - minY * fitted
                }

                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .graphicsLayer {
                            scaleX = scale
                            scaleY = scale
                            translationX = panX
                            translationY = panY
                        },
                ) {
                    // The wire is a graph line, not data: it follows the scheme.
                    val wireColor = MaterialTheme.editor.lineStrong
                    Canvas(modifier = Modifier.fillMaxSize()) {
                        // Dot grid, 24dp pitch (Concat grid #ffffff24).
                        val pitch = 24.dp.toPx()
                        var x = 0f
                        while (x < size.width) {
                            var y = 0f
                            while (y < size.height) {
                                drawCircle(
                                    color = RumoKind.grid,
                                    radius = 1f,
                                    center = Offset(x, y),
                                )
                                y += pitch
                            }
                            x += pitch
                        }
                        // Wires between consecutive node boxes.
                        for (i in 0 until nodes.size - 1) {
                            val from = positionOf(i, nodeKey(layer?.id, nodes[i]))
                            val to = positionOf(i + 1, nodeKey(layer?.id, nodes[i + 1]))
                            val x1 = from.first + nodeWPx / 2f
                            val y1 = from.second + nodeHPx
                            val x2 = to.first + nodeWPx / 2f
                            val y2 = to.second
                            val path = Path().apply {
                                moveTo(x1, y1)
                                cubicTo(x1, y1 + (y2 - y1) * 0.5f, x2, y2 - (y2 - y1) * 0.5f, x2, y2)
                            }
                            drawPath(
                                path = path,
                                color = wireColor,
                                style = Stroke(width = 2.dp.toPx()),
                            )
                            drawCircle(color = wireColor, radius = 3.5f.dp.toPx(), center = Offset(x2, y2))
                        }
                    }
                    nodes.forEachIndexed { index, node ->
                        val key = nodeKey(layer?.id, node)
                        val pos = positionOf(index, key)
                        Box(
                            modifier = Modifier
                                .offset { IntOffset(pos.first.roundToInt(), pos.second.roundToInt()) }
                                .width(NodeW)
                                .height(NodeH)
                                .pointerInput(key) {
                                    // Tap opens the compact parameter sheet…
                                    detectTapGestures { sheetKey = node.key }
                                }
                                .pointerInput(key) {
                                    // …a drag still moves the node.
                                    detectDragGestures { change, amount ->
                                        change.consume()
                                        // Read fresh from the flow: the lambda
                                        // captured at composition must not drive
                                        // the drag math from a stale snapshot.
                                        val cur = state.nodePos.value[key] ?: pos
                                        state.setNodePos(
                                            key,
                                            cur.first + amount.x,
                                            cur.second + amount.y,
                                        )
                                    }
                                },
                        ) {
                            NodeCard(
                                node = node,
                                accent = layer?.kind?.mark ?: MaterialTheme.editor.muted,
                                hasInput = index > 0,
                                hasOutput = index < nodes.lastIndex,
                            )
                        }
                    }
                }
            }
        }
    }

    if (pickerOpen && layer != null) {
        AddEffectSheet(
            state = state,
            layer = layer,
            catalogue = catalogue,
            onPick = { kindId ->
                state.addEffect(layer.id, kindId)
                haptic.hapticConfirm()
                pickerOpen = false
            },
            onDismiss = { pickerOpen = false },
        )
    }

    val open = layer?.let { layer -> nodes.firstOrNull { it.key == sheetKey } }
    if (open != null && layer != null) {
        NodeSheet(
            node = open,
            layer = layer,
            state = state,
            playheadMs = playheadMs,
            renderPath = renderPath,
            engineActive = engineActive,
            locked = false,
            onDismiss = { sheetKey = null },
        )
    }
}

/**
 * The graph's node box at 130×52dp: input dot, icon + node name, output dot.
 * Nothing else fits — no slider, no swatch and no prose goes in here.
 */
@Composable
private fun NodeCard(
    node: GraphNode,
    accent: Color,
    hasInput: Boolean,
    hasOutput: Boolean,
) {
    Row(
        modifier = Modifier
            .fillMaxSize()
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
            .border(1.dp, accent.copy(alpha = 0.45f), RoundedCornerShape(12.dp))
            .padding(horizontal = RumoSpacing.xs),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
    ) {
        PortDot(visible = hasInput, accent = accent)
        Box(
            modifier = Modifier
                .size(22.dp)
                .clip(RoundedCornerShape(7.dp))
                .background(accent.copy(alpha = 0.18f)),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = node.icon(),
                contentDescription = null,
                tint = accent,
                modifier = Modifier.size(14.dp),
            )
        }
        Text(
            text = node.title(),
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.SemiBold,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        PortDot(visible = hasOutput, accent = accent)
    }
}

/**
 * Layout key of one node. A stage keeps its bare name so layouts saved before
 * effect nodes existed still resolve; an effect node is keyed by its own id,
 * which is a UUID and therefore unique across projects.
 */
private fun nodeKey(layerId: String?, node: GraphNode): String = "$layerId/${node.key}"
