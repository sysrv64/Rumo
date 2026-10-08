// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Refresh
import androidx.compose.material3.Icon
import androidx.compose.material3.Slider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.theme.EditorType
import com.kerneldroid.rumo.ui.theme.editor

/**
 * A property row: label, scrubber and value — three tiles in one strip.
 *
 * The shape is taken from Alight Motion's properties panel, not invented: there a row
 * is `[label tile] [notched track] [value tile]`, three parts of one strip with no
 * gaps. Why this is better than what was there: the row used to be
 * `[label] [Material Slider] [value]`, that is a label, the round M3 thumb and a
 * label on the right — three elements speaking different languages, and a column of
 * such rows reads as a form, not as an instrument.
 *
 * Here:
 * The widths are computed for a phone, not a tablet: on 360dp minus the rail (56dp)
 * and the margins about 270dp remain, of which the label, value and reset take 164dp,
 * leaving the track about 110dp — enough to hit with a finger, and that is exactly the
 * reason the labels here are short.
 *
 * * **label** — a tile at the `raised` level, on the left, [labelWidth] wide;
 * * **track** — notches across the full width and a vertical pointer at the value.
 *   You can drag anywhere along the track, not by the thumb: the thumb has to be hit,
 *   while the track is the target. Direct manipulation, no animation (TASTE.md);
 * * **value** — a tile at the `field` level, on the right, in tabular figures. Tapping
 *   it gives exact input: the scrubber cannot land on `12` over a −400..400 range;
 * * **reset** — only when the value differs from the original, otherwise it is decoration.
 *
 * The [height] is one for the whole row: label, track and value are aligned on one
 * axis, and that is what makes a list of rows an instrument panel. 44dp, not 40 —
 * that is what a stock `Slider` asks for under a finger.
 */
@Composable
fun PropertyRow(
    label: String,
    value: Float,
    range: ClosedFloatingPointRange<Float>,
    valueText: String,
    onValue: (Float) -> Unit,
    modifier: Modifier = Modifier,
    // Smaller tiles, a wider track: a 44dp row and 68/64dp tiles are 132dp out of
    // the ~300dp the panel has on a phone, and the thumb was left with less than half
    // the width. The value is legible even at 56dp ("-132 px") and the label at 60dp,
    // and both stay touch targets thanks to the row height.
    height: androidx.compose.ui.unit.Dp = 40.dp,
    labelWidth: androidx.compose.ui.unit.Dp = 60.dp,
    valueWidth: androidx.compose.ui.unit.Dp = 56.dp,
    icon: ImageVector? = null,
    /** Discrete values: the stock `Slider` draws its notches at them. */
    steps: Int = 0,
    enabled: Boolean = true,
    /** Tap on the value: exact input. `null` means input is unavailable. */
    onValueClick: (() -> Unit)? = null,
    /** The reset button; shown only when the value is not the original. */
    onReset: (() -> Unit)? = null,
    onValueCommit: () -> Unit = {},
) {
    val palette = MaterialTheme.editor
    val span = (range.endInclusive - range.start).takeIf { it > 0f } ?: 1f
    val fraction = ((value - range.start) / span).coerceIn(0f, 1f)

    Row(
        modifier = modifier
            .fillMaxWidth()
            .height(height),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // The label tile.
        Row(
            modifier = Modifier
                .width(labelWidth)
                .height(height)
                .clip(RoundedCornerShape(4.dp))
                .background(MaterialTheme.colorScheme.surfaceContainer)
                .padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            if (icon != null) {
                Icon(
                    imageVector = icon,
                    contentDescription = null,
                    tint = if (enabled) MaterialTheme.colorScheme.onSurfaceVariant else palette.dim,
                    modifier = Modifier.size(14.dp),
                )
            }
            Text(
                text = label,
                style = EditorType.label,
                color = if (enabled) MaterialTheme.colorScheme.onSurface else palette.dim,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }

        // The track is the **stock Material 3 `Slider`**, not custom drawing.
        //
        // At first there was a hand-drawn track here, and it was broken at the core:
        // the width was written into state from inside the draw pass, so the gesture
        // read the previous frame's width, and before the first draw it read one, and
        // the first finger movement jumped across the whole range. Custom drawing also
        // gave neither a pressed state nor the correct thumb shape.
        //
        // The library control solves all of this itself: the rail, the thumb, the state
        // layer under the finger, accessibility and exact touch behaviour. It takes its
        // colours from `colorScheme.primary`, that is from the editor accent — there is
        // nothing to tune.
        Slider(
            value = value,
            onValueChange = onValue,
            valueRange = range,
            steps = steps,
            enabled = enabled,
            onValueChangeFinished = onValueCommit,
            modifier = Modifier
                .weight(1f)
                .padding(horizontal = 2.dp),
        )

        // The value tile.
        Box(
            modifier = Modifier
                .width(valueWidth)
                .height(height)
                .clip(RoundedCornerShape(4.dp))
                .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                .then(
                    if (onValueClick != null && enabled) {
                        Modifier.clickable(onClick = onValueClick)
                    } else {
                        Modifier
                    },
                ),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text = valueText,
                style = EditorType.value,
                color = if (enabled) MaterialTheme.colorScheme.onSurface else palette.dim,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }

        if (onReset != null) {
            Icon(
                imageVector = Icons.Rounded.Refresh,
                contentDescription = stringResource(R.string.panel_control_reset, label),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .size(32.dp)
                    .clip(RoundedCornerShape(4.dp))
                    .clickable(enabled = enabled, onClick = onReset)
                    .padding(8.dp),
            )
        }
    }
}

/**
 * A panel section: a quiet heading and content.
 *
 * A section is the answer to "tabs inside a tab". The editor has one navigation level
 * (the surface rail), and everything below it is sections of a single scrollable
 * column. This is both the Material rule ("a navigation rail should be the only
 * visible navigation element") and how properties panels are built in the tools that
 * invented them: sections, not a second row of tabs.
 */
@Composable
fun EditorSection(
    title: String,
    modifier: Modifier = Modifier,
    trailing: (@Composable RowScope.() -> Unit)? = null,
    content: @Composable () -> Unit,
) {
    Column(modifier = modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 12.dp, end = 4.dp, top = 14.dp, bottom = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = title,
                style = EditorType.label,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.weight(1f),
            )
            trailing?.invoke(this)
        }
        content()
    }
}

/**
 * A grid of tiles to pick from: what opens a tool in the editor.
 *
 * A choice is an action, not a mode: that is why it opens as a grid of large tiles
 * (an icon and a label) rather than a third row of tabs. Each tile is a touch target
 * in full, not a 24dp icon.
 */
@Composable
fun TileGrid(
    tiles: List<TileSpec>,
    modifier: Modifier = Modifier,
    columns: Int = 3,
) {
    Column(
        modifier = modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        tiles.chunked(columns).forEach { rowTiles ->
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                rowTiles.forEach { tile ->
                    Tile(tile = tile, modifier = Modifier.weight(1f))
                }
                // The empty slots of the last row: the grid must not stretch the last
                // tile across the full width.
                repeat(columns - rowTiles.size) { Spacer(modifier = Modifier.weight(1f)) }
            }
        }
    }
}

/** One pick tile. */
data class TileSpec(
    val label: String,
    val icon: ImageVector,
    val onClick: () -> Unit,
    /** Whether the tile is marked as selected. */
    val selected: Boolean = false,
)

@Composable
private fun Tile(tile: TileSpec, modifier: Modifier = Modifier) {
    val palette = MaterialTheme.editor
    Column(
        modifier = modifier
            .height(84.dp)
            .clip(RoundedCornerShape(6.dp))
            .background(
                if (tile.selected) palette.accentWell else MaterialTheme.colorScheme.surfaceContainer,
            )
            .clickable(onClick = tile.onClick)
            .padding(6.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Icon(
            imageVector = tile.icon,
            contentDescription = null,
            tint = if (tile.selected) palette.accent else MaterialTheme.colorScheme.onSurface,
            modifier = Modifier.size(24.dp),
        )
        Spacer(modifier = Modifier.height(6.dp))
        Text(
            text = tile.label,
            style = EditorType.label,
            color = if (tile.selected) palette.onAccentWell else MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/**
 * Drag-reorder state — one per list.
 *
 * It lives separately from the row because one row is dragged, yet the offset concerns
 * all: the row being dragged moves with the finger, the others stay in place.
 */
class ReorderState internal constructor() {
    internal var dragging by mutableStateOf<Any?>(null)
    internal var offset by mutableStateOf(0f)

    /** The offset of row [key]: zero for all but the one being dragged. */
    fun offsetOf(key: Any): Float = if (dragging == key) offset else 0f

    /** Whether row [key] is being dragged. */
    fun isDragging(key: Any): Boolean = dragging == key

    internal fun reset() {
        dragging = null
        offset = 0f
    }
}

@Composable
fun rememberReorderState(): ReorderState = remember { ReorderState() }

/**
 * The reorder gesture, attached to the row's handle and starting **immediately**.
 *
 * No long press — and that is not a trifle. An effect-chain row lives inside the
 * panel's scrollable column, and a gesture that waits for a long press loses to the
 * scroll: the finger moves the panel in time, the long press is cancelled, and no
 * reorder happens at all. That is exactly why it worked in the layers panel (where the
 * list is height-bounded and competes with no one) but not in the effect chain. The
 * handle wins the contest from the first movement, and that is fairer: the handle
 * exists precisely to be pulled by.
 *
 * The threshold is half a row step: a row swaps once it has covered half the distance
 * to its neighbour, and the offset counter is reduced by a step, so the next swap needs
 * the same amount of movement again rather than a single pixel.
 */
fun Modifier.reorderHandle(
    state: ReorderState,
    key: Any,
    rowStepPx: Float,
    onReorder: (delta: Int) -> Unit,
    onStart: () -> Unit = {},
    onEnd: () -> Unit = {},
): Modifier = this.pointerInput(key) {
    detectDragGestures(
        onDragStart = {
            state.dragging = key
            state.offset = 0f
            onStart()
        },
        onDragEnd = {
            state.reset()
            onEnd()
        },
        onDragCancel = { state.reset() },
    ) { change, amount ->
        change.consume()
        state.offset += amount.y
        if (state.offset > rowStepPx / 2f) {
            onReorder(1)
            state.offset -= rowStepPx
        } else if (state.offset < -rowStepPx / 2f) {
            onReorder(-1)
            state.offset += rowStepPx
        }
    }
}
