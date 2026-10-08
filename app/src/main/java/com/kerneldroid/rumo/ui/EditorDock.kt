// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.annotation.StringRes
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Text
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.AccountTree
import androidx.compose.material.icons.rounded.AutoFixHigh
import androidx.compose.material.icons.rounded.GraphicEq
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.Layers
import androidx.compose.material.icons.rounded.PhotoLibrary
import androidx.compose.material.icons.rounded.TextFields
import androidx.compose.material.icons.rounded.Tune
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.foundation.BorderStroke
import androidx.compose.material3.VerticalDivider
import androidx.compose.ui.graphics.vector.ImageVector
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.theme.EditorType
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.editor
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.ui.panels.AudioPanel
import com.kerneldroid.rumo.ui.panels.FontsPanel
import com.kerneldroid.rumo.ui.panels.InspectorPanel
import com.kerneldroid.rumo.ui.panels.LayersPanel
import com.kerneldroid.rumo.ui.panels.MediaPanel
import com.kerneldroid.rumo.ui.theme.DockTokens
import com.kerneldroid.rumo.ui.theme.hapticToggle
import kotlin.math.abs

/**
 * Pages of the bottom dock.
 *
 * Five surfaces, and one name for each. It used to be six equal slots, three of
 * which described the *same* object — the selected layer — under three different
 * names (`Props`, `Effects`, `Nodes`), so the user had to remember where a
 * control lived instead of editing. `Properties` is now the one surface for the
 * selected layer: transform, appearance, the effect chain and the parameters of
 * the selected effect. The pipeline graph is no longer a page: it is an overlay
 * opened from that page's header, because it shows the same chain plus stages
 * nothing can edit (docs/10 §7.3).
 *
 * Four slots on a 360dp screen are ~90dp each, which is what finally fits a
 * visible label; six slots could only carry an icon.
 */
enum class DockPage(@StringRes val labelRes: Int, val icon: ImageVector) {
    LAYERS(R.string.panel_layers, Icons.Rounded.Layers),
    ADJUST(R.string.panel_properties, Icons.Rounded.Tune),
    MEDIA(R.string.panel_media, Icons.Rounded.PhotoLibrary),
    AUDIO(R.string.panel_audio, Icons.Rounded.GraphicEq),
    FONTS(R.string.panel_fonts, Icons.Rounded.TextFields),
}

/**
 * Bottom dock: a rail of six equal slots and a horizontal pager underneath.
 *
 * The dock is the *remainder* of the screen: the caller gives it a weight, the
 * preview above it keeps its intrinsic 16:9 height and the panel content scrolls
 * inside whatever is left. There is no height drag and no detent: sizing the dock
 * from a fraction of `screenH` is what used to leave a dead band between the
 * preview and the timeline, and every screen-height the dock did not use was
 * unusable black.
 *
 * Gesture contract:
 *  - the pager is the *only* owner of the horizontal swipe, including a swipe
 *    that starts on the rail. The rail used to run its own
 *    `detectHorizontalDragGestures` on top of the pager, so one gesture had two
 *    handlers that could disagree mid-drag (docs/10 §3 D6);
 *  - the rail owns the vertical collapse (the grab bar above it);
 *  - nothing inside a page scrolls sideways, so no page can steal a page swipe;
 *  - the dock never resizes the preview.
 */
@Composable
fun EditorDock(
    state: EditorState,
    layers: List<LayerUi>,
    selectedLayer: LayerUi?,
    selectedId: String?,
    playheadMs: Long,
    isPlaying: Boolean,
    locked: Boolean,
    page: DockPage,
    onPageChange: (DockPage) -> Unit,
    onSelect: (String) -> Unit,
    onImportImage: () -> Unit,
    onImportAudio: () -> Unit,
    onImportVideo: () -> Unit,
    onAddLayer: () -> Unit,
    /// Layer deletion all the way through from the screen: it shows the undo.
    onDeleteLayer: (String) -> Unit,
    onOpenNodeGraph: () -> Unit,
    onTogglePlayback: () -> Unit,
    /// Clear the selection. The rail shows the exit only when there is something to clear.
    onDeselect: () -> Unit = {},
    /// True while the user has pulled the dock down to look at the picture only.
    collapsed: Boolean = false,
    onCollapsedChange: (Boolean) -> Unit = {},
    /// Height delta in dp from the handle: up is positive. The height is owned by
    /// the screen, because only it knows how much preview is left.
    onResize: (Float) -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val haptic = LocalHapticFeedback.current
    val pages = DockPage.entries

    val pagerState = rememberPagerState(pageCount = { pages.size })
    LaunchedEffect(page) {
        if (pagerState.currentPage != page.ordinal) {
            pagerState.animateScrollToPage(page.ordinal)
        }
    }
    LaunchedEffect(pagerState.settledPage) {
        pages.getOrNull(pagerState.settledPage)?.let { if (it != page) onPageChange(it) }
    }

    // A flat panel: 8dp radius, no shadow and no tonal elevation. The value step
    // and a hairline on top separate it, not a border and not a shadow
    // (TASTE.md: shadow is not used at all).
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .fillMaxHeight(),
        shape = RoundedCornerShape(topStart = 8.dp, topEnd = 8.dp),
        color = MaterialTheme.colorScheme.surface,
        tonalElevation = 0.dp,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Row(modifier = Modifier.fillMaxSize()) {
            // The rail sits on the **left** and runs top to bottom, not as a row
            // of tabs next to the panel. This is the shape of the Alight Motion
            // toolbar: navigation is a vertical icon rail along the edge, the
            // content is on the right. The reason is not imitation: a property
            // row needs width (caption, track, value), and the vertical rail
            // gives the panel the whole height and exactly 64dp of width, whereas
            // a horizontal row of tabs ate 56dp of height from every list row.
            // A collapsed dock is only the handle. The rail cannot be drawn in
            // it: it is vertical and needs up to 300dp (1 "Done" + 4 slots of
            // 60dp), while the collapsed strip is 74dp. Formerly the rail was
            // rendered unconditionally and simply cut off: the "hide the panel,
            // look at the picture" gesture left one visible icon and unreachable
            // Properties/Media/Audio.
            if (!collapsed) {
                DockRail(
                    pages = pages,
                    selected = page,
                    onSelect = { tapped ->
                        onPageChange(tapped)
                        haptic.hapticToggle(true)
                    },
                    hasSelection = selectedId != null,
                    onDeselect = onDeselect,
                )
            }
            Column(modifier = Modifier.fillMaxSize()) {
            DockGrabBar(
                collapsed = collapsed,
                onCollapsedChange = onCollapsedChange,
                onResize = onResize,
            )

            if (!collapsed) {
            HorizontalPager(
                state = pagerState,
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f)
                    // The dock's surface stays flush with the bottom of the
                    // screen, but its *content* must stop above the system bar:
                    // without this the last row of a panel sat under the
                    // gesture bar and could not be read or touched.
                    .navigationBarsPadding(),
                beyondViewportPageCount = 1,
                verticalAlignment = Alignment.Top,
            ) { index ->
                val current = pages[index]
                Box(modifier = Modifier.fillMaxSize()) {
                    when (current) {
                        DockPage.LAYERS -> LayersPanel(
                            state = state,
                            layers = layers,
                            selectedId = selectedId,
                            onSelect = onSelect,
                            onAddLayer = onAddLayer,
                            onDeleteLayer = onDeleteLayer,
                        )
                        DockPage.ADJUST -> InspectorPanel(
                            state = state,
                            layer = selectedLayer,
                            playheadMs = playheadMs,
                            locked = locked,
                            onOpenNodeGraph = onOpenNodeGraph,
                        )
                        DockPage.MEDIA -> MediaPanel(
                            state = state,
                            layers = layers,
                            selectedId = selectedId,
                            onSelect = onSelect,
                            onImportImage = onImportImage,
                            onImportVideo = onImportVideo,
                        )
                        DockPage.AUDIO -> AudioPanel(
                            state = state,
                            layers = layers,
                            selectedId = selectedId,
                            playheadMs = playheadMs,
                            isPlaying = isPlaying,
                            onSelect = onSelect,
                            onImportAudio = onImportAudio,
                            onToggleTransport = onTogglePlayback,
                        )
                        DockPage.FONTS -> FontsPanel(
                            state = state,
                            layers = layers,
                            selectedId = selectedId,
                        )
                    }
                }
            }
            }
            }
        }
    }
}

/**
 * The surface rail: vertical, quiet, with a single accent mark.
 *
 * The shape is as in the Alight Motion toolbar: navigation runs along the left
 * edge top to bottom, the content takes the rest. A horizontal row of tabs above
 * the panel (and all the more two such rows in a row) is what reads as a "toy":
 * tabs over tabs make you remember where everything is.
 *
 * There is no fill at all: the icon and the caption are muted, the active surface
 * is marked with the **accent** — the icon colour and a vertical line at the left
 * edge. One colour, one place, no scale animation: in the editor an icon bounce
 * is a delay, not feedback (TASTE.md, "Motion").
 */
@Composable
private fun DockRail(
    pages: List<DockPage>,
    selected: DockPage,
    onSelect: (DockPage) -> Unit,
    hasSelection: Boolean,
    onDeselect: () -> Unit,
) {
    // The rail is a `LazyColumn`, not a scrollable column, for one thing: the
    // selected surface must be brought into view. Six surfaces plus "Done" is
    // 420dp, while the dock is capped from above by the working preview, and on a
    // phone in portrait it gets less. Formerly the rail was cut off (the lower
    // surfaces became unreachable), then it started scrolling, but the selected
    // one could still remain past the edge: a gesture on the panel changed the
    // surface, and the rail did not know about it.
    // Motion from the scheme, not durations of its own: the editor keeps
    // `MotionScheme.standard`, and the slot must arrive at the same speed as
    // everything else in the panel.
    val motionScheme = MaterialTheme.motionScheme
    val railState = rememberLazyListState()
    // The "Done" slot is always present — with `hasSelection = false` it is
    // collapsed to zero, but it still occupies an index in the list. So surface
    // `i` lies at index `1 + i` regardless of the selection.
    val selectedIndex = (1 + pages.indexOf(selected).coerceAtLeast(0)).coerceIn(0, pages.size)
    LaunchedEffect(selected, hasSelection) {
        // We bring it into view only if the selected one is not visible.
        // `animateScrollToItem` always puts the item at the start of the viewport,
        // and on every switch the rail would jump up, even when everything fits.
        val visible = railState.layoutInfo.visibleItemsInfo.any { it.index == selectedIndex }
        if (!visible) railState.animateScrollToItem(selectedIndex)
    }
    Row(modifier = Modifier.fillMaxHeight()) {
        LazyColumn(
            state = railState,
            modifier = Modifier
                .width(DockTokens.railWidth)
                .fillMaxHeight()
                .padding(vertical = RumoSpacing.xs),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            // The way back comes first and appears only with a selection: it is a
            // verb, not a surface, and it must be where the finger looks for an
            // exit — at the start of the rail.
            //
            // The appearance is animated, not instant: the slot takes 60dp, and
            // an instant appearance shifted all the surfaces at once — the rail
            // "grew" with a jerk exactly at the moment the user selected a layer.
            // The vertical expansion moves the other slots down in a way that
            // shows where the new row came from.
            item(key = "done") {
                AnimatedVisibility(
                    visible = hasSelection,
                    enter = expandVertically(motionScheme.defaultSpatialSpec()) +
                        fadeIn(motionScheme.fastEffectsSpec()),
                    exit = shrinkVertically(motionScheme.defaultSpatialSpec()) +
                        fadeOut(motionScheme.fastEffectsSpec()),
                ) {
                    RailSlot(
                        icon = Icons.AutoMirrored.Rounded.ArrowBack,
                        label = stringResource(R.string.editor_dock_done),
                        active = false,
                        mark = false,
                        onClick = onDeselect,
                    )
                }
            }
            items(pages, key = { it.name }) { page ->
                RailSlot(
                    icon = page.icon,
                    label = stringResource(page.labelRes),
                    active = page == selected,
                    mark = true,
                    onClick = { onSelect(page) },
                )
            }
        }
        VerticalDivider(color = MaterialTheme.colorScheme.outlineVariant, thickness = 1.dp)
    }
}

/** One rail slot: an icon, a caption and — for surfaces — an activity mark. */
@Composable
private fun RailSlot(
    icon: ImageVector,
    label: String,
    active: Boolean,
    mark: Boolean,
    onClick: () -> Unit,
) {
    val palette = MaterialTheme.editor
    val tint = if (active) palette.accent else MaterialTheme.colorScheme.onSurfaceVariant
    val textColor = if (active) {
        MaterialTheme.colorScheme.onSurface
    } else {
        MaterialTheme.colorScheme.onSurfaceVariant
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(DockTokens.railSlot)
            .clickable(onClick = onClick),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // The activity mark: a vertical line at the left edge — the same device
        // as an underline in a horizontal rail, rotated along with the rail.
        Box(
            modifier = Modifier
                .width(2.dp)
                .fillMaxHeight()
                .background(if (active) palette.accent else Color.Transparent),
        )
        Column(
            modifier = Modifier.weight(1f),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            Icon(
                imageVector = icon,
                contentDescription = null,
                tint = tint,
                modifier = Modifier.size(20.dp),
            )
            Spacer(modifier = Modifier.size(3.dp))
            Text(
                text = label,
                color = textColor,
                style = EditorType.micro,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/**
 * Drag zone above the rail.
 *
 * The handle does two things, and the second appeared because without it the dock
 * could not be raised.
 *
 * 1. **Pulls the height.** A drag up gives `onResize` a positive delta, down a
 *    negative one, and the dock height is disposed of by the screen. Formerly the
 *    handle could do only one thing: accumulate 36dp and collapse. The user
 *    pulled it up to open a cut-off surface, and nothing happened — the dock did
 *    not rise, because there was nothing to raise it with.
 * 2. **Collapses.** Pulling down past the minimum collapses; a tap goes there and
 *    back. A collapsed dock is the handle and nothing more.
 *
 * The height is held by the caller, not the handle: only the screen knows how
 * much preview is left.
 */
@Composable
private fun DockGrabBar(
    collapsed: Boolean,
    onCollapsedChange: (Boolean) -> Unit,
    onResize: (Float) -> Unit,
) {
    val haptic = LocalHapticFeedback.current
    val density = LocalDensity.current
    val threshold = with(density) { 36.dp.toPx() }
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .height(DockTokens.handleZone)
            .pointerInput(collapsed) {
                var travelled = 0f
                detectVerticalDragGestures(
                    onDragStart = { travelled = 0f },
                    onDragEnd = { travelled = 0f },
                    onDragCancel = { travelled = 0f },
                ) { change, dragAmount ->
                    change.consume()
                    travelled += dragAmount
                    if (collapsed) {
                        // From the collapsed state, the gesture unfolds first.
                        if (travelled < -threshold) {
                            haptic.hapticToggle(true)
                            onCollapsedChange(false)
                        }
                    } else if (travelled > threshold) {
                        haptic.hapticToggle(false)
                        onCollapsedChange(true)
                    } else {
                        // Not yet at the collapse: this is a height change.
                        // The sign is preserved: up is a bigger dock, down is
                        // smaller.
                        onResize(-with(density) { dragAmount.toDp().value })
                    }
                }
            }
            .pointerInput(collapsed) {
                detectTapGestures {
                    haptic.hapticToggle(!collapsed)
                    onCollapsedChange(!collapsed)
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .width(40.dp)
                .height(4.dp)
                .clip(RoundedCornerShape(percent = 50))
                .background(MaterialTheme.colorScheme.outlineVariant),
        )
    }
}
