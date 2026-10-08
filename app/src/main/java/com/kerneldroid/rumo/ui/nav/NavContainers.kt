// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.nav

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.AutoAwesome
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItemColors
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedListItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp

/**
 * Navigation screen containers: sections glued into one capsule, plus the empty
 * state.
 *
 * ## Why this is a separate file
 *
 * Home, Projects, Template and Settings are assembled from the same bricks, and
 * earlier every screen drew them by hand — four copies of the same thing. Worse,
 * the copies drifted apart: Home picked radii with the constants `20.dp`/`14.dp`,
 * while Projects took its shape from `ListItemDefaults`, and the sections looked
 * different.
 *
 * ## What is stock here and what is not
 *
 * The gluing itself is stock MD3 Expressive: `SegmentedListItem` plus
 * `ListItemDefaults.segmentedShapes(index, count)`. Material decides for itself
 * what corner the first and last element have and what gap sits between
 * neighbours, so no shape number is written by hand here.
 *
 * Hand-made is only what Material does not have: the section header (without it
 * the phone shows "just a list of rows") and the empty state with an action.
 */

/**
 * The gap between the elements of one group.
 *
 * In material3 1.5.0-alpha26 the gap is set by the constant
 * `ListItemDefaults.segmentedGap`, but it is not exposed in the public API —
 * available only as a generated getter in the bytecode, unreachable from Kotlin.
 * So the value is written here rather than taken from Material: 2dp is exactly
 * Material's stock gap between segments, and the feel of "one capsule made of
 * several rows" rests on it.
 *
 * A separate constant is needed because `LazyColumn(spacedBy(...))` cannot do a
 * 2dp gap between segments and 12dp between groups: those are two different
 * intervals in one list. So within a group the elements sit edge to edge, and
 * the groups are separated by an explicit [NavGroupSpacer] element.
 */
val NavSegmentGap = 2.dp

/** The margin between a group header and its capsule. */
val NavHeaderGap = 4.dp

/** The margin between the capsules of different groups. */
val NavGroupGap = 12.dp

/**
 * Segment shapes: Material's defaults, but with our shape scheme instead of the
 * stock one.
 *
 * Extracted because `segmentedShapes` takes a `ListItemShapes` as input, and
 * every screen would have had to assemble it itself — otherwise a section on one
 * screen silently got stock corners and did not match its neighbour.
 */
@Composable
fun navSegmentedShapes(index: Int, count: Int) =
    ListItemDefaults.segmentedShapes(index, count, ListItemDefaults.shapes())

/**
 * Segment colours: `surfaceBright` on the screen background `surfaceContainer`.
 *
 * Tonal contrast instead of a border — this is Material's way of saying "this is
 * a capsule" without a single line. The values are stock: with our own numbers we
 * would be signing up for colours that Material should decide.
 */
@Composable
fun navSegmentedColors(): ListItemColors = ListItemDefaults.segmentedColors()

/**
 * Group header.
 *
 * Semantic, not decorative: it answers the question "what is this block", which
 * otherwise has to be guessed from the row contents. The colour is muted so it
 * does not fight the group's content.
 */
@Composable
fun NavSectionHeader(
    text: String,
    modifier: Modifier = Modifier,
    trailing: (@Composable () -> Unit)? = null,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .padding(top = 8.dp, bottom = NavHeaderGap),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = text,
            modifier = Modifier.weight(1f),
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        trailing?.invoke()
    }
}

/**
 * A group row, already ready for gluing: [index] and [count] are its place in the
 * group.
 *
 * It exists so the calling code cannot forget the index and get a group where
 * every element thinks it is the last one and draws all four outer corners.
 *
 * [onClick] is mandatory: the `SegmentedListItem` overload that has `selected`
 * requires a handler, and without it the row is not clickable. So a row with no
 * action is not a `NavSegment` but a separate container group.
 */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun NavSegment(
    index: Int,
    count: Int,
    headline: String,
    modifier: Modifier = Modifier,
    supporting: String? = null,
    leadingIcon: ImageVector? = null,
    leading: (@Composable () -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
    onClick: () -> Unit,
    /**
     * Row hold.
     *
     * Separate from the click, because they are different actions: the click
     * opens, the hold offers what can be done to the row. The hold duration is
     * set by the caller — see `ProvideHoldDuration`.
     */
    onLongClick: (() -> Unit)? = null,
    selected: Boolean = false,
    enabled: Boolean = true,
) {
    SegmentedListItem(
        selected = selected,
        onClick = onClick,
        onLongClick = onLongClick,
        shapes = navSegmentedShapes(index, count),
        colors = navSegmentedColors(),
        enabled = enabled,
        modifier = modifier.fillMaxWidth(),
        leadingContent = leading ?: leadingIcon?.let { icon ->
            { Icon(icon, contentDescription = null) }
        },
        // The header slot here is called `content`, not `headlineContent`:
        // on a segment it is the headline, and the subheading is
        // `supportingContent`.
        content = { Text(headline, maxLines = 1) },
        supportingContent = supporting?.let { { Text(it, maxLines = 1) } },
        trailingContent = trailing,
    )
}

/**
 * Empty state: a container, an explanation and an action.
 *
 * Earlier Projects and Home showed a single line of text. An empty list that
 * offers nothing reads as a breakage: it is unclear whether it is "empty for now"
 * or "something failed to load". The container and the button answer both
 * questions at once.
 */
@Composable
fun NavEmptyState(
    title: String,
    message: String,
    modifier: Modifier = Modifier,
    icon: ImageVector = Icons.Rounded.AutoAwesome,
    actionLabel: String? = null,
    onAction: (() -> Unit)? = null,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 8.dp, vertical = 16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            modifier = Modifier.size(56.dp),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = title,
            style = MaterialTheme.typography.titleMedium,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Text(
            text = message,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (actionLabel != null && onAction != null) {
            FilledTonalButton(onClick = onAction) { Text(actionLabel) }
        }
    }
}

/**
 * The gap between groups — as a separate list element.
 *
 * Not a `Spacer` with a fixed margin and not `Arrangement.spacedBy`: the gap
 * within a group (a couple of dp) and the gap between groups (12dp) are different
 * intervals in one list, and a single `spacedBy` cannot express them. Here it is
 * exactly the remainder that is added to the gap between segments.
 */
@Composable
fun NavGroupSpacer(height: androidx.compose.ui.unit.Dp = NavGroupGap) {
    Spacer(modifier = Modifier.size(width = 0.dp, height = height - NavSegmentGap))
}