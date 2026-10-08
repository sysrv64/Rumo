// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Image
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.ShapeLine
import androidx.compose.material.icons.rounded.TextFields
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalMinimumInteractiveComponentSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.LayerKindUi
import com.kerneldroid.rumo.ui.theme.EditorType
import com.kerneldroid.rumo.ui.theme.RumoKind
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.monoNumerals

/** Kind mark: the saturated hue that identifies a layer kind everywhere. */
val LayerKindUi.mark: Color
    get() = when (this) {
        LayerKindUi.SHAPE -> RumoKind.shapeMark
        LayerKindUi.TEXT -> RumoKind.textMark
        LayerKindUi.MEDIA -> RumoKind.imageMark
        LayerKindUi.AUDIO -> RumoKind.audioMark
    }

/** Kind well: the dark ground the mark sits on (Concat's mark+well pair). */
val LayerKindUi.well: Color
    get() = when (this) {
        LayerKindUi.SHAPE -> RumoKind.shapeWell
        LayerKindUi.TEXT -> RumoKind.textWell
        LayerKindUi.MEDIA -> RumoKind.imageWell
        LayerKindUi.AUDIO -> RumoKind.audioWell
    }

val LayerKindUi.icon: ImageVector
    get() = when (this) {
        LayerKindUi.SHAPE -> Icons.Rounded.ShapeLine
        LayerKindUi.TEXT -> Icons.Rounded.TextFields
        LayerKindUi.MEDIA -> Icons.Rounded.Image
        LayerKindUi.AUDIO -> Icons.Rounded.MusicNote
    }

/**
 * The kind's name as a word, not as an identifier.
 *
 * `LayerKindUi.name` is the enum constant (`SHAPE`, `AUDIO`, …): it is what the
 * code compares and what a screen reader would otherwise spell out. Anything
 * shown or announced goes through here instead.
 */
val LayerKindUi.labelRes: Int
    get() = when (this) {
        LayerKindUi.SHAPE -> R.string.panel_kind_shape
        LayerKindUi.TEXT -> R.string.panel_kind_text
        LayerKindUi.MEDIA -> R.string.panel_kind_media
        LayerKindUi.AUDIO -> R.string.panel_kind_audio
    }

/** Tinted square that carries a kind mark — the unit of visual language. */
@Composable
fun KindMark(
    kind: LayerKindUi,
    modifier: Modifier = Modifier,
    size: Dp = 34.dp,
    iconSize: Dp = 18.dp,
) {
    Box(
        modifier = modifier
            .size(size)
            .clip(RoundedCornerShape(size / 3.4f))
            .background(kind.well)
            .border(1.dp, kind.mark.copy(alpha = 0.35f), RoundedCornerShape(size / 3.4f)),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            imageVector = kind.icon,
            contentDescription = stringResource(kind.labelRes),
            tint = kind.mark,
            modifier = Modifier.size(iconSize),
        )
    }
}

/**
 * Sticky panel title row with a trailing action slot.
 *
 * One 28dp line: `titleSmall` instead of `titleMedium`, no second row. The
 * optional subtitle rides on the same line so existing call sites keep their
 * information without paying for a stacked header.
 */
@Composable
fun PanelHeader(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    actions: @Composable RowScope.() -> Unit = {},
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .height(32.dp)
            .padding(start = RumoSpacing.m, end = RumoSpacing.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = title,
            style = EditorType.title,
            color = MaterialTheme.colorScheme.onSurface,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        if (subtitle != null) {
            Text(
                text = " · $subtitle",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
        } else {
            Spacer(modifier = Modifier.weight(1f))
        }
        actions()
    }
}

/**
 * Group title inside a panel.
 *
 * It used to be **all-caps and in the accent colour**. An all-caps accent
 * caption above every group is exactly the "MVP" look: it shouts louder than the
 * values the panel was opened for, and multiplies the accent across the whole
 * screen. The accent is used in exactly four places in the editor, and the group
 * caption is not one of them (TASTE.md). Here the caption is quiet and in normal
 * case: the value beneath it must be the loudest thing on the line.
 */
@Composable
fun SectionLabel(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = EditorType.label,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = modifier.padding(top = RumoSpacing.s, bottom = RumoSpacing.xs),
    )
}

/** Card-free empty state: glyph, title, one-line hint. */
@Composable
fun PanelEmpty(
    icon: ImageVector,
    title: String,
    hint: String,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(RumoSpacing.m),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(28.dp),
        )
        Text(text = title, style = MaterialTheme.typography.titleSmall)
        Text(
            text = hint,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/**
 * One parameter line: icon + label + live value above a slider
 * (ImageToolbox EnhancedSliderItem shape, M3 typography).
 */
@Composable
fun ParamSlider(
    label: String,
    value: Float,
    valueRange: ClosedFloatingPointRange<Float>,
    valueText: String,
    onValueChange: (Float) -> Unit,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    steps: Int = 0,
    enabled: Boolean = true,
    onValueChangeFinished: () -> Unit = {},
) {
    val labelColor = if (enabled) {
        MaterialTheme.colorScheme.onSurface
    } else {
        MaterialTheme.colorScheme.onSurfaceVariant
    }
    Column(modifier = modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            if (icon != null) {
                Icon(
                    imageVector = icon,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(16.dp),
                )
                Box(modifier = Modifier.size(RumoSpacing.s))
            }
            Text(
                text = label,
                style = MaterialTheme.typography.labelLarge,
                color = labelColor,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = valueText,
                style = MaterialTheme.typography.labelMedium.merge(monoNumerals),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Slider(
            value = value,
            onValueChange = onValueChange,
            valueRange = valueRange,
            steps = steps,
            enabled = enabled,
            onValueChangeFinished = onValueChangeFinished,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/** Tonal square button used for rail-level actions in a panel. */
@Composable
fun ActionTile(
    icon: ImageVector,
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    val container by animateColorAsState(
        targetValue = if (enabled) {
            MaterialTheme.colorScheme.secondaryContainer
        } else {
            MaterialTheme.colorScheme.surfaceContainerHighest
        },
    )
    Column(
        modifier = modifier
            .clip(RoundedCornerShape(16.dp))
            .background(container)
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = RumoSpacing.m, vertical = RumoSpacing.s),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = if (enabled) {
                MaterialTheme.colorScheme.onSecondaryContainer
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
            modifier = Modifier.size(20.dp),
        )
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            color = if (enabled) {
                MaterialTheme.colorScheme.onSecondaryContainer
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
        )
    }
}


/** Small pill used for metadata and inline verbs. */
@Composable
fun MetaPill(
    text: String,
    modifier: Modifier = Modifier,
    tint: Color = MaterialTheme.colorScheme.onSurfaceVariant,
) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelSmall,
        color = tint,
        // One step, not two: the mark is a caption for the object, not a control,
        // and it must not be the highest-contrast spot on the line.
        modifier = modifier
            .clip(RoundedCornerShape(4.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
            .padding(horizontal = RumoSpacing.s, vertical = 2.dp),
    )
}

/**
 * Chip metrics for rows of category chips inside the dock.
 *
 * M3 chips enforce a 48 dp minimum interactive target, which is right for an
 * isolated action but wrong for a row of tabs sharing the dock's height budget
 * with the preview: four of them cost ~200 dp of chrome. This provides 0 dp for
 * just the chip subtree — the 48 dp rule is untouched everywhere else in the app
 * (buttons, list rows, icon buttons).
 */
@Composable
fun CompactChipScope(content: @Composable () -> Unit) {
    CompositionLocalProvider(LocalMinimumInteractiveComponentSize provides 0.dp) { content() }
}

/** Category chip: [CompactChipScope] metrics plus a fixed 28 dp row height. */
@Composable
fun CompactChip(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CompactChipScope {
        FilterChip(
            selected = selected,
            onClick = onClick,
            label = {
                Text(
                    text = label,
                    style = MaterialTheme.typography.labelSmall,
                    maxLines = 1,
                )
            },
            modifier = modifier.height(28.dp),
            shape = RoundedCornerShape(8.dp),
        )
    }
}
