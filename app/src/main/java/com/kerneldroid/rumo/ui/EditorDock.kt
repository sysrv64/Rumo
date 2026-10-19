// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.annotation.StringRes
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.GraphicEq
import androidx.compose.material.icons.rounded.KeyboardArrowDown
import androidx.compose.material.icons.rounded.KeyboardArrowUp
import androidx.compose.material.icons.rounded.Layers
import androidx.compose.material.icons.rounded.PhotoLibrary
import androidx.compose.material.icons.rounded.TextFields
import androidx.compose.material.icons.rounded.Tune
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.panels.AudioPanel
import com.kerneldroid.rumo.ui.panels.FontsPanel
import com.kerneldroid.rumo.ui.panels.InspectorPanel
import com.kerneldroid.rumo.ui.panels.LayersPanel
import com.kerneldroid.rumo.ui.panels.MediaPanel
import com.kerneldroid.rumo.ui.theme.DockTokens
import com.kerneldroid.rumo.ui.theme.EditorType
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.editor
import com.kerneldroid.rumo.ui.theme.hapticToggle

/**
 * Pages of the bottom/side dock.
 *
 * Five surfaces, and one name for each. `Properties` is the unified surface for the
 * selected layer: transform, multi-track keyframes & Bézier curves, appearance,
 * transitions, timing, and the effect chain.
 */
enum class DockPage(@StringRes val labelRes: Int, val icon: ImageVector) {
    LAYERS(R.string.panel_layers, Icons.Rounded.Layers),
    ADJUST(R.string.panel_properties, Icons.Rounded.Tune),
    MEDIA(R.string.panel_media, Icons.Rounded.PhotoLibrary),
    AUDIO(R.string.panel_audio, Icons.Rounded.GraphicEq),
    FONTS(R.string.panel_fonts, Icons.Rounded.TextFields),
}

/**
 * Adaptive Material 3 editor dock with Alight Motion / CapCut style vertical navigation
 * rail, quick height detents (`Compact`, `Half`, `Expand`), and swipeable surface pager.
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
    onDeleteLayer: (String) -> Unit,
    onOpenNodeGraph: () -> Unit,
    onTogglePlayback: () -> Unit,
    onDeselect: () -> Unit = {},
    collapsed: Boolean = false,
    onCollapsedChange: (Boolean) -> Unit = {},
    onResize: (Float) -> Unit = {},
    onSnapDetent: ((Float) -> Unit)? = null,
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

    Surface(
        modifier = modifier
            .fillMaxWidth()
            .fillMaxHeight(),
        shape = RoundedCornerShape(topStart = 12.dp, topEnd = 12.dp),
        color = MaterialTheme.colorScheme.surface,
        tonalElevation = 0.dp,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Row(modifier = Modifier.fillMaxSize()) {
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
                    activePageLabel = stringResource(page.labelRes),
                    selectedLayerName = selectedLayer?.name,
                    collapsed = collapsed,
                    onCollapsedChange = onCollapsedChange,
                    onResize = onResize,
                    onSnapDetent = onSnapDetent,
                )

                if (!collapsed) {
                    HorizontalPager(
                        state = pagerState,
                        modifier = Modifier
                            .fillMaxWidth()
                            .weight(1f)
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

@Composable
private fun DockRail(
    pages: List<DockPage>,
    selected: DockPage,
    onSelect: (DockPage) -> Unit,
    hasSelection: Boolean,
    onDeselect: () -> Unit,
) {
    val motionScheme = MaterialTheme.motionScheme
    val railState = rememberLazyListState()
    val selectedIndex = (1 + pages.indexOf(selected).coerceAtLeast(0)).coerceIn(0, pages.size)
    LaunchedEffect(selected, hasSelection) {
        val visible = railState.layoutInfo.visibleItemsInfo.any { it.index == selectedIndex }
        if (!visible) railState.animateScrollToItem(selectedIndex)
    }
    Row(modifier = Modifier.fillMaxHeight()) {
        LazyColumn(
            state = railState,
            modifier = Modifier
                .width(DockTokens.railWidth)
                .fillMaxHeight()
                .background(MaterialTheme.colorScheme.surfaceContainerLow)
                .padding(vertical = RumoSpacing.xs),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
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
                        onClick = onDeselect,
                    )
                }
            }
            items(pages, key = { it.name }) { page ->
                RailSlot(
                    icon = page.icon,
                    label = stringResource(page.labelRes),
                    active = page == selected,
                    onClick = { onSelect(page) },
                )
            }
        }
        VerticalDivider(color = MaterialTheme.colorScheme.outlineVariant, thickness = 1.dp)
    }
}

@Composable
private fun RailSlot(
    icon: ImageVector,
    label: String,
    active: Boolean,
    onClick: () -> Unit,
) {
    val palette = MaterialTheme.editor
    val tint = if (active) palette.accent else MaterialTheme.colorScheme.onSurfaceVariant
    val textColor = if (active) {
        MaterialTheme.colorScheme.onSurface
    } else {
        MaterialTheme.colorScheme.onSurfaceVariant
    }
    val slotBg = if (active) {
        MaterialTheme.colorScheme.surfaceContainerHighest.copy(alpha = 0.65f)
    } else {
        Color.Transparent
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(DockTokens.railSlot)
            .background(slotBg)
            .clickable(onClick = onClick),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            modifier = Modifier
                .width(3.dp)
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
                fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/**
 * Interactive dock grab bar with drag-to-resize, tap-to-collapse, and 1-tap height
 * detent pills (`Compact`, `Half`, `Expand`).
 */
@Composable
private fun DockGrabBar(
    activePageLabel: String,
    selectedLayerName: String?,
    collapsed: Boolean,
    onCollapsedChange: (Boolean) -> Unit,
    onResize: (Float) -> Unit,
    onSnapDetent: ((Float) -> Unit)? = null,
) {
    val haptic = LocalHapticFeedback.current
    val density = LocalDensity.current
    val threshold = with(density) { 36.dp.toPx() }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(24.dp)
            .background(MaterialTheme.colorScheme.surfaceContainerLow)
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
                        if (travelled < -threshold) {
                            haptic.hapticToggle(true)
                            onCollapsedChange(false)
                        }
                    } else if (travelled > threshold) {
                        haptic.hapticToggle(false)
                        onCollapsedChange(true)
                    } else {
                        onResize(-with(density) { dragAmount.toDp().value })
                    }
                }
            }
            .pointerInput(collapsed) {
                detectTapGestures {
                    haptic.hapticToggle(!collapsed)
                    onCollapsedChange(!collapsed)
                }
            }
            .padding(horizontal = RumoSpacing.s),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = if (selectedLayerName != null) {
                "$activePageLabel · $selectedLayerName"
            } else {
                activePageLabel
            },
            style = EditorType.micro,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        Box(
            modifier = Modifier
                .width(36.dp)
                .height(4.dp)
                .clip(RoundedCornerShape(percent = 50))
                .background(MaterialTheme.colorScheme.outlineVariant),
        )
        Row(
            modifier = Modifier.weight(1f),
            horizontalArrangement = Arrangement.End,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (onSnapDetent != null && !collapsed) {
                DetentPill(
                    label = stringResource(R.string.editor_dock_half),
                    onClick = {
                        haptic.hapticToggle(true)
                        onSnapDetent(0.5f)
                    },
                )
                Spacer(modifier = Modifier.width(4.dp))
                DetentPill(
                    label = stringResource(R.string.editor_dock_full),
                    onClick = {
                        haptic.hapticToggle(true)
                        onSnapDetent(0.85f)
                    },
                )
                Spacer(modifier = Modifier.width(4.dp))
            }
            Icon(
                imageVector = if (collapsed) Icons.Rounded.KeyboardArrowUp else Icons.Rounded.KeyboardArrowDown,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(16.dp),
            )
        }
    }
}

@Composable
private fun DetentPill(
    label: String,
    onClick: () -> Unit,
) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(4.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerHighest)
            .clickable(onClick = onClick)
            .padding(horizontal = 5.dp, vertical = 1.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = label,
            style = EditorType.micro,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
        )
    }
}
