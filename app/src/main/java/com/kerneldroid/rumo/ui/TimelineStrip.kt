// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Timeline
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.panels.mark
import com.kerneldroid.rumo.ui.panels.well
import com.kerneldroid.rumo.ui.theme.DockTokens
import com.kerneldroid.rumo.ui.theme.RumoKind
import com.kerneldroid.rumo.ui.theme.editor
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.monoNumerals
import kotlin.math.abs
import kotlin.math.roundToInt

/**
 * Persistent timeline: ruler ticks on the top strip (Concat's rule — *the ruler
 * is the scrub strip*), one lane per layer below, playhead over both. Lane taps
 * select; nothing in the lanes pans or scrubs, so there is no gesture that can
 * fight the dock's page swipe.
 *
 * A clip is dragged sideways to move its [LayerUi.startMs]; the drag stays
 * inside its own lane (a vertical finger keeps scrolling the lane list) and
 * never reaches the ruler, which is a separate strip with its own scrub. The
 * whole drag is one undo entry, not one per pointer move.
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
    val safeDuration = durationMs.coerceAtLeast(1L)
    // Lane area is bounded (max two rows on a phone, scrolls beyond that) so the
    // strip keeps one height no matter what is selected — the preview never
    // resizes itself.
    val laneRows = layers.size.coerceIn(1, 2)
    val lanesHeight = DockTokens.trackRow * laneRows

    fun seekToX(x: Float, widthPx: Float) {
        if (widthPx <= 0f) return
        val fraction = (x / widthPx).coerceIn(0f, 1f)
        state.seekTo((fraction * safeDuration).toLong())
    }

    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(20.dp))
            // The strip is darker than the panel it lies in: the timeline is a well
            // into which the clips are laid, and a strip lighter than the panel would read as
            // content (TASTE.md).
            .background(MaterialTheme.colorScheme.surfaceContainerLowest),
    ) {
        BoxWithConstraints(modifier = Modifier.fillMaxWidth()) {
            val widthPx = with(LocalDensity.current) { maxWidth.toPx() }
            val playheadFraction = (playheadMs.toFloat() / safeDuration.toFloat())
                .coerceIn(0f, 1f)

            Column {
                TimelineRuler(
                    durationMs = safeDuration,
                    playheadFraction = playheadFraction,
                    onScrub = { seekToX(it, widthPx) },
                    onScrubEnd = { haptic.hapticConfirm() },
                )
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
                    LazyColumn(
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(lanesHeight),
                        contentPadding = PaddingValues(bottom = RumoSpacing.xs),
                    ) {
                        items(layers, key = { it.id }) { layer ->
                            TimelineLane(
                                layer = layer,
                                durationMs = safeDuration,
                                selected = layer.id == selectedId,
                                onSelect = { onSelect(layer.id) },
                                // One entry per drag, not per pointer move: the
                                // setter coalesces while the gesture is open and
                                // the drag end closes it.
                                onDragStartTo = { startMs ->
                                    state.setStartMs(layer.id, startMs)
                                },
                                onClipDragEnd = { state.endGesture() },
                                // A keyframe mark is a place in time, so tapping
                                // it goes there — and selects the layer, because a
                                // keyframe only means something on its own layer.
                                onKeyTap = { timeMs ->
                                    state.seekTo(timeMs)
                                    state.selectLayer(layer.id)
                                    haptic.hapticConfirm()
                                },
                            )
                        }
                    }
                }
            }
            // Playhead: 2dp line over ruler + lanes + a small top marker.
            Box(
                modifier = Modifier
                    .offset(x = maxWidth * playheadFraction - 1.dp)
                    .width(2.dp)
                    .height(DockTokens.rulerHeight + lanesHeight)
                    .background(MaterialTheme.editor.signal),
            )
        }
    }
}

@Composable
private fun TimelineRuler(
    durationMs: Long,
    playheadFraction: Float,
    onScrub: (Float) -> Unit,
    onScrubEnd: () -> Unit,
) {
    val density = LocalDensity.current
    // Colours are read here rather than in `drawRect`/`drawPath`: the contents of `Canvas` are
    // a DrawScope, not a composable scope, and `MaterialTheme` is unavailable there.
    val rulerGround = MaterialTheme.colorScheme.surfaceContainerLowest
    val signal = MaterialTheme.editor.signal
    // Ruler labels: used to be a literal, so they did not follow the scheme.
    val rulerLabelColor = MaterialTheme.editor.muted.toArgb()
    Canvas(
        modifier = Modifier
            .fillMaxWidth()
            .height(DockTokens.rulerHeight)
            .pointerInput(durationMs) {
                detectTapGestures(onTap = { onScrub(it.x) })
            }
            .pointerInput(durationMs) {
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
        val ideal = durationMs.toFloat() / (size.width / minLabelPx)
        val step = ladder.firstOrNull { it >= ideal } ?: ladder.last()
        val pxPerMs = size.width / durationMs.toFloat()

        drawRect(color = rulerGround)
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
                android.graphics.Paint().apply {
                    isAntiAlias = true
                    // Used to be a literal: the ruler labels did not follow the scheme,
                    // while everything else on the screen does.
                    color = rulerLabelColor
                    textSize = with(density) { 10.dp.toPx() }
                },
            )
            t += step
        }
        // Playhead marker: triangle + hairline (Concat draws it last, over the ruler).
        val px = size.width * playheadFraction
        val path = androidx.compose.ui.graphics.Path().apply {
            moveTo(px - 5.dp.toPx(), 0f)
            lineTo(px + 5.dp.toPx(), 0f)
            lineTo(px, 9.dp.toPx())
            close()
        }
        drawPath(path = path, color = signal)
    }
}

@Composable
private fun TimelineLane(
    layer: LayerUi,
    durationMs: Long,
    selected: Boolean,
    onSelect: () -> Unit,
    /** Called with a keyframe's time when its diamond is tapped. */
    onKeyTap: (Long) -> Unit,
    /** New layer start, snapped to [CLIP_SNAP_MS] and clamped to >= 0. */
    onDragStartTo: (Long) -> Unit,
    /** The clip drag ended (or was cancelled): closes the coalescing gesture. */
    onClipDragEnd: () -> Unit,
) {
    val density = LocalDensity.current
    BoxWithConstraints(
        modifier = Modifier
            .fillMaxWidth()
            .height(DockTokens.trackRow)
            .padding(horizontal = RumoSpacing.s, vertical = 3.dp),
    ) {
        val fraction = (layer.durationMs.toFloat() / durationMs.toFloat()).coerceIn(0.02f, 1f)
        val startFraction = (layer.startMs.toFloat() / durationMs.toFloat()).coerceIn(0f, 1f)
        val laneWidth = maxWidth
        Box(
            modifier = Modifier
                .fillMaxSize()
                .clip(RoundedCornerShape(8.dp))
                .background(MaterialTheme.colorScheme.surfaceContainer)
                .clickable(onClick = onSelect),
        )
        // Clip holder for the clip chip. The chip is positioned by `startMs`
        // and may run past either end of the project (a layer dragged right at
        // the end, or one longer than the project) — drawing it unclipped would
        // spill over the neighbouring lane and over the ruler.
        Box(
            modifier = Modifier
                .fillMaxSize()
                .clip(RoundedCornerShape(8.dp)),
        ) {
            Box(
                modifier = Modifier
                    .offset(x = laneWidth * startFraction)
                    .width(laneWidth * fraction)
                    .fillMaxSize()
                    .clip(RoundedCornerShape(8.dp))
                    .background(if (layer.visible) layer.kind.well else layer.kind.well.copy(alpha = 0.45f))
                    .then(
                        if (selected) {
                            Modifier.border(1.5.dp, MaterialTheme.editor.signal, RoundedCornerShape(8.dp))
                        } else {
                            Modifier
                        },
                    )
                    .clickable(onClick = onSelect)
                    // Horizontal move only. The gesture is deliberately narrow:
                    // it consumes a drag only once the finger has travelled
                    // horizontally past touch slop, so a vertical drag still
                    // reaches the lanes' LazyColumn and the dock's page swipe is
                    // not stolen from the panel above.
                    .pointerInput(layer.id, durationMs) {
                        var dragStartMs = layer.startMs
                        var dragged = false
                        val lanePx = with(density) { laneWidth.toPx() }
                        detectDragGestures(
                            onDragStart = { dragStartMs = layer.startMs },
                            onDragEnd = {
                                // One undo entry for the whole drag: every move
                                // in it coalesces onto the first, and closing the
                                // gesture here lets the *next* drag push its own.
                                if (dragged) onClipDragEnd()
                                dragged = false
                            },
                            onDragCancel = {
                                if (dragged) onClipDragEnd()
                                dragged = false
                            },
                            onDrag = { change, dragAmount ->
                                if (lanePx <= 0f) return@detectDragGestures
                                if (abs(dragAmount.y) > abs(dragAmount.x)) {
                                    // Let the lane list scroll instead.
                                    return@detectDragGestures
                                }
                                change.consume()
                                dragged = true
                                val msPerPx = durationMs.toFloat() / lanePx
                                val raw = dragStartMs + (dragAmount.x * msPerPx).toLong()
                                // Snap to 100 ms: the playhead lands on tenths of
                                // a second everywhere else, and a clip whose start
                                // is 37 ms off never lines up with a marker.
                                val snapped = (raw.toFloat() / CLIP_SNAP_MS)
                                    .roundToInt() * CLIP_SNAP_MS
                                onDragStartTo(snapped.coerceAtLeast(0L))
                            },
                        )
                    },
            ) {
                Row(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(horizontal = RumoSpacing.xs),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                ) {
                    Box(
                        modifier = Modifier
                            .width(3.dp)
                            .height(16.dp)
                            .clip(RoundedCornerShape(2.dp))
                            .background(layer.kind.mark),
                    )
                    Text(
                        text = layer.name,
                        style = MaterialTheme.typography.labelSmall,
                        color = if (layer.visible) {
                            MaterialTheme.colorScheme.onSurface
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
        // Keyframe diamonds. They used to be drawn and nothing else: a mark you
        // could see and not touch. A tap moves the playhead onto the key, which is
        // the whole reason to look at one (docs/10 §7.10).
        //
        // The hit target is a 32dp box around a 9dp glyph — the mark itself is far
        // below the 44dp floor, so it gets a larger invisible area rather than a
        // bigger mark, and the diamond stays a diamond.
        val lanePx = with(LocalDensity.current) { laneWidth.toPx() }
        for (key in layer.keys) {
            val keyFraction = (key.timeMs.toFloat() / durationMs.toFloat()).coerceIn(0f, 1f)
            val centre = with(LocalDensity.current) { (lanePx * keyFraction).toDp() }
            Box(
                modifier = Modifier
                    .offset(x = centre - KEY_HIT / 2, y = 4.dp)
                    .size(KEY_HIT)
                    .clickable { onKeyTap(key.timeMs) },
                contentAlignment = Alignment.Center,
            ) {
                Box(
                    modifier = Modifier
                        .size(DockTokens.keyDiamond)
                        .clip(RoundedCornerShape(2.dp))
                        .background(layer.kind.mark)
                        .graphicsLayer { rotationZ = 45f },
                )
            }
        }
    }
}

/** Invisible hit box around a keyframe diamond. */
private val KEY_HIT = 32.dp

/** The clip snap step while dragging: 100 ms, like the ruler divisions. */
private const val CLIP_SNAP_MS = 100L

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
