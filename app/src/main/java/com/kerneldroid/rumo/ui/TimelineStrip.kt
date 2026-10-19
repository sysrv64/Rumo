// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.Lock
import androidx.compose.material.icons.rounded.LockOpen
import androidx.compose.material.icons.rounded.Remove
import androidx.compose.material.icons.rounded.Timeline
import androidx.compose.material.icons.rounded.UnfoldLess
import androidx.compose.material.icons.rounded.UnfoldMore
import androidx.compose.material.icons.rounded.Visibility
import androidx.compose.material.icons.rounded.VisibilityOff
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.VerticalDivider
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
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.panels.mark
import com.kerneldroid.rumo.ui.panels.well
import com.kerneldroid.rumo.ui.theme.DockTokens
import com.kerneldroid.rumo.ui.theme.EditorType
import com.kerneldroid.rumo.ui.theme.RumoKind
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.editor
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.hapticToggle
import com.kerneldroid.rumo.ui.theme.monoNumerals
import kotlin.math.abs
import kotlin.math.roundToInt

/**
 * Multi-track NLE timeline inspired by After Effects, CapCut, and Alight Motion.
 *
 * Architecture:
 * - Top compact control bar: layer count, active clip range, zoom controls (`0.5x..8.0x`),
 *   and track expansion toggle (`2..5` visible rows).
 * - Left fixed Track Header column ([DockTokens.trackHeaderWidth]): synchronized vertical
 *   scroll with the track lanes, providing 1-tap visibility and lock toggles plus layer kind
 *   colour markers.
 * - Right horizontally zoomable & scrollable viewport: shared horizontal scroll state across
 *   the time ruler and all clip lanes, with left/right clip trim handles, magnetic snapping
 *   to playhead and clip boundaries (rendered with a [RumoKind.snap] vertical guide line),
 *   transition ramp indicators, and multi-track keyframe diamonds across all 5 animated
 *   tracks (`Rotation`, `Scale`, `Pos X`, `Pos Y`, `Opacity`).
 */
@Composable
fun TimelineStrip(
    state: EditorState,
    layers: List<LayerUi>,
    selectedId: String?,
    playheadMs: Long,
    durationMs: Long,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val haptic = LocalHapticFeedback.current
    val lockedIds by state.locked.collectAsState()
    val safeDuration = durationMs.coerceAtLeast(1L)

    var zoom by rememberSaveable { mutableFloatStateOf(1f) }
    var expandedTracks by rememberSaveable { mutableStateOf(false) }
    var activeSnapMs by remember { mutableStateOf<Long?>(null) }

    val maxVisibleRows = if (expandedTracks) 4 else 3
    val laneRows = layers.size.coerceIn(1, maxVisibleRows)
    val lanesHeight = DockTokens.trackRow * laneRows

    val listState = rememberLazyListState()
    val hScrollState = rememberScrollState()

    // Magnetic snap targets: start of timeline, project end, current playhead, and other clips' edges.
    val snapTargets = remember(layers, playheadMs, safeDuration, selectedId) {
        buildList {
            add(0L)
            add(playheadMs)
            add(safeDuration)
            for (l in layers) {
                if (l.id == selectedId || EditorState.isBackground(l)) continue
                add(l.startMs)
                add(l.startMs + l.durationMs)
            }
        }
    }

    fun resolveSnap(rawMs: Long, snapToleranceMs: Long): Pair<Long, Long?> {
        val clamped = rawMs.coerceAtLeast(0L)
        var bestTarget: Long? = null
        var bestDist = snapToleranceMs + 1L
        for (target in snapTargets) {
            val d = abs(clamped - target)
            if (d <= snapToleranceMs && d < bestDist) {
                bestDist = d
                bestTarget = target
            }
        }
        if (bestTarget != null) {
            return bestTarget to bestTarget
        }
        val gridSnapped = ((clamped.toFloat() / CLIP_SNAP_MS).roundToInt() * CLIP_SNAP_MS).coerceAtLeast(0L)
        return gridSnapped to null
    }

    val selectedLayer = layers.firstOrNull { it.id == selectedId }

    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(16.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerLowest)
            .border(1.dp, MaterialTheme.colorScheme.outlineVariant, RoundedCornerShape(16.dp)),
    ) {
        // Compact timeline header bar: clip info + zoom & track height controls.
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(28.dp)
                .background(MaterialTheme.colorScheme.surfaceContainerLow)
                .padding(horizontal = RumoSpacing.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
        ) {
            Icon(
                imageVector = Icons.Rounded.Timeline,
                contentDescription = null,
                tint = MaterialTheme.editor.accent,
                modifier = Modifier.size(14.dp),
            )
            Text(
                text = pluralStringResource(R.plurals.panel_layers_subtitle, layers.size, layers.size)
                    .substringBefore(" ·"),
                style = EditorType.micro,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
            if (selectedLayer != null) {
                val clipEnd = selectedLayer.startMs + selectedLayer.durationMs
                Text(
                    text = "· ${formatTick(selectedLayer.startMs)}–${formatTick(clipEnd)}",
                    style = EditorType.micro.merge(monoNumerals),
                    color = MaterialTheme.editor.signal,
                    maxLines = 1,
                )
            }
            Spacer(modifier = Modifier.weight(1f))
            // Zoom out button
            Box(
                modifier = Modifier
                    .size(24.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .clickable(enabled = zoom > MIN_TIMELINE_ZOOM) {
                        zoom = (zoom / 1.4f).coerceIn(MIN_TIMELINE_ZOOM, MAX_TIMELINE_ZOOM)
                        haptic.hapticToggle(false)
                    },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Rounded.Remove,
                    contentDescription = stringResource(R.string.editor_timeline_zoom_out),
                    tint = if (zoom > MIN_TIMELINE_ZOOM) {
                        MaterialTheme.colorScheme.onSurface
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.4f)
                    },
                    modifier = Modifier.size(14.dp),
                )
            }
            // Current zoom badge (tap to reset to 1.0x)
            Box(
                modifier = Modifier
                    .clip(RoundedCornerShape(6.dp))
                    .background(MaterialTheme.colorScheme.surfaceContainerHighest)
                    .clickable {
                        zoom = 1f
                        haptic.hapticConfirm()
                    }
                    .padding(horizontal = 6.dp, vertical = 2.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = "${((zoom * 10f).roundToInt() / 10f)}x",
                    style = EditorType.micro.merge(monoNumerals),
                    color = MaterialTheme.colorScheme.onSurface,
                )
            }
            // Zoom in button
            Box(
                modifier = Modifier
                    .size(24.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .clickable(enabled = zoom < MAX_TIMELINE_ZOOM) {
                        zoom = (zoom * 1.4f).coerceIn(MIN_TIMELINE_ZOOM, MAX_TIMELINE_ZOOM)
                        haptic.hapticToggle(true)
                    },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Rounded.Add,
                    contentDescription = stringResource(R.string.editor_timeline_zoom_in),
                    tint = if (zoom < MAX_TIMELINE_ZOOM) {
                        MaterialTheme.colorScheme.onSurface
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.4f)
                    },
                    modifier = Modifier.size(14.dp),
                )
            }
            if (layers.size > 2) {
                Box(
                    modifier = Modifier
                        .size(24.dp)
                        .clip(RoundedCornerShape(6.dp))
                        .clickable {
                            expandedTracks = !expandedTracks
                            haptic.hapticToggle(expandedTracks)
                        },
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        imageVector = if (expandedTracks) Icons.Rounded.UnfoldLess else Icons.Rounded.UnfoldMore,
                        contentDescription = stringResource(R.string.editor_timeline_expand_tracks),
                        tint = if (expandedTracks) MaterialTheme.editor.accent else MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.size(15.dp),
                    )
                }
            }
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant, thickness = 1.dp)

        if (layers.isEmpty()) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(DockTokens.trackRow)
                    .padding(horizontal = RumoSpacing.m),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(
                    imageVector = Icons.Rounded.Timeline,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(14.dp),
                )
                Text(
                    text = stringResource(R.string.editor_timeline_empty),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        } else {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(DockTokens.rulerHeight + lanesHeight),
            ) {
                // Left column: timecode readout in the ruler corner + synchronized track headers.
                Column(
                    modifier = Modifier
                        .width(DockTokens.trackHeaderWidth)
                        .fillMaxHeight()
                        .background(MaterialTheme.colorScheme.surfaceContainerLow),
                ) {
                    Box(
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(DockTokens.rulerHeight)
                            .padding(horizontal = RumoSpacing.xs),
                        contentAlignment = Alignment.CenterStart,
                    ) {
                        Text(
                            text = formatTime(playheadMs),
                            style = EditorType.micro.merge(monoNumerals),
                            color = MaterialTheme.editor.signal,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1,
                        )
                    }
                    LazyColumn(
                        state = listState,
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(lanesHeight),
                        contentPadding = PaddingValues(bottom = 2.dp),
                    ) {
                        items(layers, key = { "hdr:${it.id}" }) { layer ->
                            val isBg = EditorState.isBackground(layer)
                            val isLocked = layer.id in lockedIds
                            val isSelected = layer.id == selectedId
                            TimelineTrackHeader(
                                layer = layer,
                                isBackground = isBg,
                                isLocked = isLocked,
                                selected = isSelected,
                                onSelect = { onSelect(layer.id) },
                                onToggleVisible = {
                                    state.toggleVisibility(layer.id)
                                    haptic.hapticToggle(!layer.visible)
                                },
                                onToggleLock = {
                                    if (!isBg) {
                                        state.toggleLocked(layer.id)
                                        haptic.hapticToggle(!isLocked)
                                    }
                                },
                            )
                        }
                    }
                }

                VerticalDivider(color = MaterialTheme.colorScheme.outlineVariant, thickness = 1.dp)

                // Right column: zoomable ruler + multi-track clip lanes + playhead + snap guide.
                BoxWithConstraints(
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxHeight(),
                ) {
                    val density = LocalDensity.current
                    val viewportWidthDp = maxWidth
                    val contentWidthDp = (viewportWidthDp * zoom).coerceAtLeast(viewportWidthDp)
                    val contentWidthPx = with(density) { contentWidthDp.toPx() }.coerceAtLeast(1f)
                    val playheadFraction = (playheadMs.toFloat() / safeDuration.toFloat()).coerceIn(0f, 1f)

                    fun seekToX(x: Float) {
                        val fraction = (x / contentWidthPx).coerceIn(0f, 1f)
                        state.seekTo((fraction * safeDuration).toLong())
                    }

                    Box(
                        modifier = Modifier
                            .fillMaxSize()
                            .horizontalScroll(hScrollState, enabled = zoom > 1.01f),
                    ) {
                        Box(
                            modifier = Modifier
                                .width(contentWidthDp)
                                .fillMaxHeight(),
                        ) {
                            Column(modifier = Modifier.fillMaxSize()) {
                                TimelineRuler(
                                    durationMs = safeDuration,
                                    playheadFraction = playheadFraction,
                                    contentWidthDp = contentWidthDp,
                                    onScrub = { seekToX(it) },
                                    onScrubEnd = { haptic.hapticConfirm() },
                                    onPinchZoom = { factor ->
                                        zoom = (zoom * factor).coerceIn(MIN_TIMELINE_ZOOM, MAX_TIMELINE_ZOOM)
                                    },
                                )
                                LazyColumn(
                                    state = listState,
                                    modifier = Modifier
                                        .fillMaxWidth()
                                        .height(lanesHeight),
                                    contentPadding = PaddingValues(bottom = 2.dp),
                                ) {
                                    items(layers, key = { it.id }) { layer ->
                                        val isBg = EditorState.isBackground(layer)
                                        val isLocked = layer.id in lockedIds
                                        val keyTimes = remember(
                                            layer.keys,
                                            layer.xKeys,
                                            layer.yKeys,
                                            layer.scaleKeys,
                                            layer.alphaKeys,
                                        ) {
                                            state.allKeyframeTimes(layer)
                                        }
                                        TimelineLane(
                                            layer = layer,
                                            keyframeTimes = keyTimes,
                                            durationMs = safeDuration,
                                            laneWidthDp = contentWidthDp,
                                            selected = layer.id == selectedId,
                                            locked = isLocked || isBg,
                                            isBackground = isBg,
                                            onSelect = { onSelect(layer.id) },
                                            onDragStartTo = { rawStartMs, snapTolMs ->
                                                val (snapped, hit) = resolveSnap(rawStartMs, snapTolMs)
                                                activeSnapMs = hit
                                                state.setStartMs(layer.id, snapped)
                                            },
                                            onTrimStartTo = { rawStartMs, snapTolMs ->
                                                val (snapped, hit) = resolveSnap(rawStartMs, snapTolMs)
                                                activeSnapMs = hit
                                                state.trimLayerStart(layer.id, snapped)
                                            },
                                            onTrimEndTo = { rawEndMs, snapTolMs ->
                                                val (snapped, hit) = resolveSnap(rawEndMs, snapTolMs)
                                                activeSnapMs = hit
                                                state.trimLayerEnd(layer.id, snapped)
                                            },
                                            onClipDragEnd = {
                                                activeSnapMs = null
                                                state.endGesture()
                                            },
                                            onKeyTap = { timeMs ->
                                                state.seekTo(timeMs)
                                                state.selectLayer(layer.id)
                                                haptic.hapticConfirm()
                                            },
                                        )
                                    }
                                }
                            }

                            // Magnetic snap guide line when a clip edge aligns with playhead or another clip.
                            activeSnapMs?.let { snapMs ->
                                val snapFraction = (snapMs.toFloat() / safeDuration.toFloat()).coerceIn(0f, 1f)
                                Box(
                                    modifier = Modifier
                                        .offset(x = contentWidthDp * snapFraction - 0.75.dp)
                                        .width(1.5.dp)
                                        .fillMaxHeight()
                                        .background(RumoKind.snap),
                                )
                            }

                            // Playhead hairline across ruler + all lanes.
                            Box(
                                modifier = Modifier
                                    .offset(x = contentWidthDp * playheadFraction - 1.dp)
                                    .width(2.dp)
                                    .fillMaxHeight()
                                    .background(MaterialTheme.editor.signal),
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun TimelineTrackHeader(
    layer: LayerUi,
    isBackground: Boolean,
    isLocked: Boolean,
    selected: Boolean,
    onSelect: () -> Unit,
    onToggleVisible: () -> Unit,
    onToggleLock: () -> Unit,
) {
    val bgColor = if (selected) {
        MaterialTheme.colorScheme.surfaceContainerHighest
    } else {
        Color.Transparent
    }
    val displayName = if (isBackground) {
        stringResource(R.string.editor_layer_background)
    } else {
        layer.name
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(DockTokens.trackRow)
            .background(bgColor)
            .clickable(onClick = onSelect)
            .padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        Box(
            modifier = Modifier
                .width(3.dp)
                .height(16.dp)
                .clip(RoundedCornerShape(2.dp))
                .background(if (layer.visible) layer.kind.mark else layer.kind.mark.copy(alpha = 0.35f)),
        )
        Text(
            text = displayName,
            style = EditorType.micro,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
            color = if (layer.visible) {
                MaterialTheme.colorScheme.onSurface
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.6f)
            },
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        Box(
            modifier = Modifier
                .size(20.dp)
                .clip(RoundedCornerShape(4.dp))
                .clickable(onClick = onToggleVisible),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = if (layer.visible) Icons.Rounded.Visibility else Icons.Rounded.VisibilityOff,
                contentDescription = stringResource(if (layer.visible) R.string.panel_hide else R.string.panel_show),
                tint = if (layer.visible) {
                    MaterialTheme.colorScheme.onSurfaceVariant
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.45f)
                },
                modifier = Modifier.size(12.dp),
            )
        }
        if (!isBackground) {
            Box(
                modifier = Modifier
                    .size(20.dp)
                    .clip(RoundedCornerShape(4.dp))
                    .clickable(onClick = onToggleLock),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = if (isLocked) Icons.Rounded.Lock else Icons.Rounded.LockOpen,
                    contentDescription = stringResource(if (isLocked) R.string.panel_unlock else R.string.panel_lock),
                    tint = if (isLocked) {
                        MaterialTheme.editor.warning
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.45f)
                    },
                    modifier = Modifier.size(12.dp),
                )
            }
        }
    }
}

@Composable
private fun TimelineRuler(
    durationMs: Long,
    playheadFraction: Float,
    contentWidthDp: Dp,
    onScrub: (Float) -> Unit,
    onScrubEnd: () -> Unit,
    onPinchZoom: (Float) -> Unit,
) {
    val density = LocalDensity.current
    val rulerGround = MaterialTheme.colorScheme.surfaceContainerLowest
    val signal = MaterialTheme.editor.signal
    val rulerLabelColor = MaterialTheme.editor.muted.toArgb()
    val textSizePx = with(density) { 10.dp.toPx() }
    // Cached native Paint outside the draw loop so tick rendering never allocates per frame.
    val tickTextPaint = remember(rulerLabelColor, textSizePx) {
        android.graphics.Paint().apply {
            isAntiAlias = true
            color = rulerLabelColor
            textSize = textSizePx
        }
    }
    val playheadTriangle = remember { Path() }

    Canvas(
        modifier = Modifier
            .width(contentWidthDp)
            .height(DockTokens.rulerHeight)
            .pointerInput(Unit) {
                awaitEachGesture {
                    awaitFirstDown(requireUnconsumed = false)
                    while (true) {
                        val event = awaitPointerEvent()
                        if (event.changes.none { it.pressed }) break
                        if (event.changes.count { it.pressed } >= 2) {
                            val z = event.calculateZoom()
                            if (z.isFinite() && z > 0f && abs(z - 1f) > 0.01f) {
                                onPinchZoom(z)
                            }
                            event.changes.forEach { it.consume() }
                        }
                    }
                }
            }
            .pointerInput(durationMs, contentWidthDp) {
                detectTapGestures(onTap = { onScrub(it.x) })
            }
            .pointerInput(durationMs, contentWidthDp) {
                detectDragGestures(
                    onDragStart = { onScrub(it.x) },
                    onDragEnd = { onScrubEnd() },
                    onDrag = { change, _ ->
                        change.consume()
                        onScrub(change.position.x)
                    },
                )
            },
    ) {
        // Ladder chosen so labels stay ~66dp apart (Concat tick ladder rule).
        val minLabelPx = with(density) { 66.dp.toPx() }
        val ladder = listOf(
            100L, 200L, 500L, 1_000L, 2_000L, 5_000L, 10_000L, 15_000L,
            30_000L, 60_000L, 120_000L, 300_000L, 600_000L,
        )
        val ideal = durationMs.toFloat() / (size.width / minLabelPx).coerceAtLeast(1f)
        val step = ladder.firstOrNull { it >= ideal } ?: ladder.last()
        val subStep = (step / 2L).coerceAtLeast(50L)
        val pxPerMs = size.width / durationMs.toFloat()

        drawRect(color = rulerGround)

        var subT = 0L
        while (subT <= durationMs) {
            val sx = subT * pxPerMs
            drawLine(
                color = RumoKind.grid.copy(alpha = 0.08f),
                start = Offset(sx, size.height * 0.72f),
                end = Offset(sx, size.height),
                strokeWidth = 1f,
            )
            subT += subStep
        }

        var t = 0L
        while (t <= durationMs) {
            val x = t * pxPerMs
            drawLine(
                color = RumoKind.grid,
                start = Offset(x, size.height * 0.45f),
                end = Offset(x, size.height),
                strokeWidth = 1f,
            )
            val label = formatTick(t)
            drawContext.canvas.nativeCanvas.drawText(
                label,
                x + 4f,
                size.height * 0.42f,
                tickTextPaint,
            )
            t += step
        }
        // Playhead marker: triangle + hairline (Concat draws it last, over the ruler).
        val px = size.width * playheadFraction
        playheadTriangle.reset()
        playheadTriangle.moveTo(px - 5.dp.toPx(), 0f)
        playheadTriangle.lineTo(px + 5.dp.toPx(), 0f)
        playheadTriangle.lineTo(px, 9.dp.toPx())
        playheadTriangle.close()
        drawPath(path = playheadTriangle, color = signal)
    }
}

private enum class ClipDragKind {
    MOVE,
    TRIM_START,
    TRIM_END,
}

@Composable
private fun TimelineLane(
    layer: LayerUi,
    keyframeTimes: List<Long>,
    durationMs: Long,
    laneWidthDp: Dp,
    selected: Boolean,
    locked: Boolean,
    isBackground: Boolean,
    onSelect: () -> Unit,
    onKeyTap: (Long) -> Unit,
    onDragStartTo: (Long, Long) -> Unit,
    onTrimStartTo: (Long, Long) -> Unit,
    onTrimEndTo: (Long, Long) -> Unit,
    onClipDragEnd: () -> Unit,
) {
    val density = LocalDensity.current
    BoxWithConstraints(
        modifier = Modifier
            .width(laneWidthDp)
            .height(DockTokens.trackRow)
            .padding(horizontal = RumoSpacing.xs, vertical = 2.dp),
    ) {
        val fraction = (layer.durationMs.toFloat() / durationMs.toFloat()).coerceIn(0.015f, 1f)
        val startFraction = (layer.startMs.toFloat() / durationMs.toFloat()).coerceIn(0f, 1f)
        val laneWidth = maxWidth
        val lanePx = with(density) { laneWidth.toPx() }.coerceAtLeast(1f)
        val trimZonePx = with(density) { DockTokens.trimHandleWidth.toPx() }
        val snapToleranceMs = ((with(density) { 10.dp.toPx() } / lanePx) * durationMs.toFloat())
            .toLong()
            .coerceIn(40L, 250L)

        Box(
            modifier = Modifier
                .fillMaxSize()
                .clip(RoundedCornerShape(7.dp))
                .background(MaterialTheme.colorScheme.surfaceContainer)
                .clickable(onClick = onSelect),
        )
        Box(
            modifier = Modifier
                .fillMaxSize()
                .clip(RoundedCornerShape(7.dp)),
        ) {
            val clipWidthDp = laneWidth * fraction
            val clipWidthPx = with(density) { clipWidthDp.toPx() }
            val showTrimHandles = selected && !locked && !isBackground && clipWidthDp >= 36.dp

            Box(
                modifier = Modifier
                    .offset(x = laneWidth * startFraction)
                    .width(clipWidthDp)
                    .fillMaxSize()
                    .clip(RoundedCornerShape(7.dp))
                    .background(if (layer.visible) layer.kind.well else layer.kind.well.copy(alpha = 0.45f))
                    .then(
                        if (selected) {
                            Modifier.border(1.5.dp, MaterialTheme.editor.signal, RoundedCornerShape(7.dp))
                        } else {
                            Modifier.border(0.5.dp, layer.kind.mark.copy(alpha = 0.4f), RoundedCornerShape(7.dp))
                        },
                    )
                    .clickable(onClick = onSelect)
                    .pointerInput(layer.id, durationMs, laneWidth, selected, locked, isBackground) {
                        var dragStartMs = layer.startMs
                        var dragEndMs = layer.startMs + layer.durationMs
                        var accumDx = 0f
                        var dragKind = ClipDragKind.MOVE
                        var dragged = false
                        detectDragGestures(
                            onDragStart = { startOffset ->
                                dragStartMs = layer.startMs
                                dragEndMs = layer.startMs + layer.durationMs
                                accumDx = 0f
                                val canTrim = selected && !locked && !isBackground && clipWidthPx >= trimZonePx * 2.4f
                                dragKind = when {
                                    canTrim && startOffset.x <= trimZonePx -> ClipDragKind.TRIM_START
                                    canTrim && startOffset.x >= clipWidthPx - trimZonePx -> ClipDragKind.TRIM_END
                                    else -> ClipDragKind.MOVE
                                }
                            },
                            onDragEnd = {
                                if (dragged) onClipDragEnd()
                                dragged = false
                            },
                            onDragCancel = {
                                if (dragged) onClipDragEnd()
                                dragged = false
                            },
                            onDrag = { change, dragAmount ->
                                if (locked || isBackground || lanePx <= 0f) return@detectDragGestures
                                if (!dragged && abs(dragAmount.y) > abs(dragAmount.x)) {
                                    return@detectDragGestures
                                }
                                change.consume()
                                dragged = true
                                accumDx += dragAmount.x
                                val msPerPx = durationMs.toFloat() / lanePx
                                val deltaMs = (accumDx * msPerPx).toLong()
                                when (dragKind) {
                                    ClipDragKind.MOVE -> onDragStartTo(dragStartMs + deltaMs, snapToleranceMs)
                                    ClipDragKind.TRIM_START -> onTrimStartTo(dragStartMs + deltaMs, snapToleranceMs)
                                    ClipDragKind.TRIM_END -> onTrimEndTo(dragEndMs + deltaMs, snapToleranceMs)
                                }
                            },
                        )
                    },
            ) {
                // Transition ramp indicator at the incoming edge of the clip.
                val tr = layer.transition
                if (tr != null && tr.enabled && layer.durationMs > 0L) {
                    val trFraction = (tr.durationMs.toFloat() / layer.durationMs.toFloat()).coerceIn(0f, 1f)
                    val rampColor = layer.kind.mark.copy(alpha = 0.35f)
                    Canvas(
                        modifier = Modifier
                            .fillMaxHeight()
                            .width(clipWidthDp * trFraction),
                    ) {
                        val p = Path().apply {
                            moveTo(0f, size.height)
                            lineTo(size.width, 0f)
                            lineTo(0f, 0f)
                            close()
                        }
                        drawPath(p, color = rampColor)
                    }
                }

                Row(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(
                            horizontal = if (showTrimHandles) DockTokens.trimHandleWidth else RumoSpacing.xs,
                        ),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                ) {
                    if (!showTrimHandles) {
                        Box(
                            modifier = Modifier
                                .width(3.dp)
                                .height(14.dp)
                                .clip(RoundedCornerShape(2.dp))
                                .background(layer.kind.mark),
                        )
                    }
                    Text(
                        text = if (isBackground) stringResource(R.string.editor_layer_background) else layer.name,
                        style = EditorType.micro,
                        color = if (layer.visible) {
                            MaterialTheme.colorScheme.onSurface
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }

                // Visual left & right CapCut-style trim handles on the selected clip.
                if (showTrimHandles) {
                    Box(
                        modifier = Modifier
                            .align(Alignment.CenterStart)
                            .width(DockTokens.trimHandleWidth - 2.dp)
                            .fillMaxHeight()
                            .background(MaterialTheme.editor.signal.copy(alpha = 0.88f)),
                        contentAlignment = Alignment.Center,
                    ) {
                        Box(
                            modifier = Modifier
                                .width(2.dp)
                                .height(10.dp)
                                .clip(RoundedCornerShape(1.dp))
                                .background(Color.Black.copy(alpha = 0.7f)),
                        )
                    }
                    Box(
                        modifier = Modifier
                            .align(Alignment.CenterEnd)
                            .width(DockTokens.trimHandleWidth - 2.dp)
                            .fillMaxHeight()
                            .background(MaterialTheme.editor.signal.copy(alpha = 0.88f)),
                        contentAlignment = Alignment.Center,
                    ) {
                        Box(
                            modifier = Modifier
                                .width(2.dp)
                                .height(10.dp)
                                .clip(RoundedCornerShape(1.dp))
                                .background(Color.Black.copy(alpha = 0.7f)),
                        )
                    }
                }
            }
        }
        // Keyframe diamonds across all animated tracks on this layer.
        for (timeMs in keyframeTimes) {
            val keyFraction = (timeMs.toFloat() / durationMs.toFloat()).coerceIn(0f, 1f)
            val centre = with(density) { (lanePx * keyFraction).toDp() }
            Box(
                modifier = Modifier
                    .offset(x = centre - KEY_HIT / 2, y = 2.dp)
                    .size(KEY_HIT)
                    .clickable { onKeyTap(timeMs) },
                contentAlignment = Alignment.Center,
            ) {
                Box(
                    modifier = Modifier
                        .size(DockTokens.keyDiamond - 2.dp)
                        .clip(RoundedCornerShape(2.dp))
                        .background(if (selected) MaterialTheme.editor.signal else layer.kind.mark)
                        .border(0.75.dp, Color.Black.copy(alpha = 0.6f), RoundedCornerShape(2.dp))
                        .graphicsLayer { rotationZ = 45f },
                )
            }
        }
    }
}

/** Invisible hit box around a keyframe diamond. */
private val KEY_HIT = 28.dp

/** The clip snap step while dragging: 100 ms, like the ruler divisions. */
private const val CLIP_SNAP_MS = 100L

private const val MIN_TIMELINE_ZOOM = 1f
private const val MAX_TIMELINE_ZOOM = 8f

/** Ruler labels: seconds under a minute, m:ss above it. */
internal fun formatTick(ms: Long): String {
    val totalSeconds = ms / 1000.0
    return if (totalSeconds < 60.0) {
        val v = (totalSeconds * 10).roundToInt() / 10.0
        if (abs(v - v.roundToInt()) < 0.001) "${v.roundToInt()}s" else "${v}s"
    } else {
        val minutes = (ms / 60_000L).toInt()
        val seconds = ((ms / 1_000L) % 60L).toInt()
        "%d:%02d".format(minutes, seconds)
    }
}
