// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.net.Uri
import androidx.annotation.StringRes
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.AutoAwesome
import androidx.compose.material.icons.rounded.Home
import androidx.compose.material.icons.rounded.Movie
import androidx.compose.material.icons.rounded.Storefront
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.FloatingToolbarDefaults
import androidx.compose.material3.HorizontalFloatingToolbar
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.ToggleButton
import androidx.compose.material3.ToggleButtonDefaults
import androidx.compose.material3.ToggleButtonShapes
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.boundsInParent
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.theme.hapticToggle

object Routes {
    const val HOME = "home"
    const val RUMI = "rumi"
    const val PROJECTS = "projects"
    const val SHOP = "shop"
    const val EDITOR = "editor"
    const val SETTINGS = "settings"

    /** Settings sub-sections; see `SettingsScreen`. */
    const val SETTINGS_APPEARANCE = "settings/appearance"
    const val SETTINGS_RUMI = "settings/rumi"
    const val SETTINGS_DIAGNOSTICS = "settings/diagnostics"

    /**
     * Editor route.
     *
     * `fileName` opens a stored project; null means "start a new one". [keep]
     * means "show the project already in memory and do not touch it" — the only
     * way to reach work the assistant did, since the in-memory project is the
     * only place it exists until it is saved.
     */
    fun editorRoute(fileName: String?, keep: Boolean = false): String {
        val query = buildList {
            if (fileName != null) add("file=" + Uri.encode(fileName))
            if (keep) add("keep=1")
        }
        return if (query.isEmpty()) EDITOR else "$EDITOR?" + query.joinToString("&")
    }
}

/** One dock item. */
private data class DockItem(
    val route: String,
    @StringRes val labelRes: Int,
    val icon: ImageVector,
)

private val DockItems = listOf(
    DockItem(Routes.HOME, R.string.nav_home, Icons.Rounded.Home),
    DockItem(Routes.RUMI, R.string.nav_rumi, Icons.Rounded.AutoAwesome),
    DockItem(Routes.PROJECTS, R.string.nav_projects, Icons.Rounded.Movie),
    DockItem(Routes.SHOP, R.string.nav_shop, Icons.Rounded.Storefront),
)

/**
 * The routes on which the dock is shown.
 *
 * The dock lives at the navigation level (`MainActivity`), not inside the
 * screens: otherwise it would be rebuilt on every tab switch and could not
 * animate the selection.
 */
val DockRoutes: Set<String> = DockItems.map { it.route }.toSet()

/**
 * How much space the content has to leave at the bottom so the dock does not
 * cover it.
 *
 * The dock is an overlay, so the margin is set not by it but by the screen
 * beneath it: an 8dp inset + the panel + an 8dp inset. The navigation-bar inset
 * is not part of this — the dock applies it itself, while the screens get it
 * from their own `Scaffold`.
 */
val DockContentInset = 72.dp

/**
 * The dock's size is derived from the screen, not set as a number.
 *
 * `dp` already accounts for density, but not for device size: the same number of
 * dp on a 360dp-wide phone and on a 600dp foldable is the same share of the
 * screen only on the first. Here the shorter side of the screen is taken and the
 * dock scales from it, so on a large screen it does not look glued on, and on a
 * small one it does not eat half the height. The bounds are narrow: this is an
 * adjustment, not a second design.
 */
@Composable
private fun rememberDockScale(): Float {
    val config = LocalConfiguration.current
    val shortest = minOf(config.screenWidthDp, config.screenHeightDp).toFloat()
    return (shortest / 400f).coerceIn(0.86f, 1.12f)
}

/**
 * The tab dock: a panel from `HorizontalFloatingToolbar` and a selection pill.
 *
 * ## Why the label width is an animated `Dp`, and not an `AnimatedVisibility`
 *
 * This was the cause of the jerkiness, and it is in the design of
 * `HorizontalFloatingToolbar`, not in the animation settings. Its layout with an
 * FAB (`HorizontalFloatingToolbarWithFabLayout`) computes the container width
 * like this:
 *
 * ```
 * val maxToolbarPlaceableWidth = toolbarMeasurable.maxIntrinsicWidth(...)
 * val targetWidth = maxToolbarPlaceableWidth * expandedProgress
 * val width = maxToolbarPlaceableWidth + toolbarToFabGap + FabSizeRange.start
 * val fabX = width - fabPlaceable.width
 * ```
 *
 * That is, the container gets the **intrinsic** width of the content, and the
 * FAB's position is computed from that same width. With `AnimatedVisibility`,
 * however, the intrinsic width **is not animated**: it is immediately equal to
 * the final one. So on selecting an item the container jumped instantly to the
 * full width — "the dock snaps sharply to the right" — while the label inside
 * caught up with its own animation; on collapse it snapped just as sharply back.
 * It also explains the jitter: `maxIntrinsicWidth` on content with
 * `AnimatedVisibility` is computed afresh on every frame and costs as much as
 * fully laying out the content, and on an accelerated animation scale those same
 * frames are packed twice as densely.
 *
 * Here the label width is an ordinary animated value substituted into
 * `Modifier.width`. The intrinsic width of the content becomes the sum of these
 * values, so it **animates with the same spec** as the label, and the container
 * grows with it instead of jumping.
 *
 * ## Why the width is capped by a budget
 *
 * In the layout, `width` is taken from the unclipped intrinsic width, so content
 * wider than the screen pushes the FAB past the right edge. The label budget is
 * computed from the screen width minus everything the panel occupies itself —
 * that way the container physically cannot end up wider than the space available.
 *
 * ## What is left of Shevery
 *
 * The pill: one shape that travels and stretches between the items, drawn beneath
 * the first of them (`drawWithContent`), with the item bounds from
 * `onGloballyPositioned`. The items are `ToggleButton`s with transparent
 * containers and round shapes, whose colours are set explicitly.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun RumoBottomBar(
    currentRoute: String,
    onNavigate: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val motionScheme = MaterialTheme.motionScheme
    val scheme = MaterialTheme.colorScheme
    val colors = FloatingToolbarDefaults.vibrantFloatingToolbarColors()
    val config = LocalConfiguration.current
    val density = LocalDensity.current
    val haptic = LocalHapticFeedback.current
    val scale = rememberDockScale()

    // Resolved once here: `DockItems` is a plain top-level list and cannot read
    // resources itself, and both the measured label widths and the rendered
    // labels must agree on the same text.
    val labels = DockItems.map { stringResource(it.labelRes) }

    val dockHeight = 46.dp * scale
    val iconSize = 22.dp * scale
    val iconSpacing = ButtonDefaults.IconSpacing * scale
    val labelSize = 14f * scale
    val labelSpec = motionScheme.defaultSpatialSpec<Float>()

    val selectedIndex = DockItems.indexOfFirst { it.route == currentRoute }

    // The labels are static, so they are measured once per configuration, not per frame.
    val measurer = rememberTextMeasurer()
    val labelFull = remember(measurer, labelSize, density, labels) {
        DockItems.mapIndexed { index, _ ->
            val measured = measurer.measure(
                text = AnnotatedString(labels[index]),
                style = TextStyle(fontSize = labelSize.sp),
                maxLines = 1,
                softWrap = false,
            )
            with(density) { measured.size.width.toDp() }
        }
    }

    // The label budget: the screen width minus what the panel occupies itself.
    //
    // The point is not the precision of the numbers but that the container
    // cannot become wider than the screen: in the panel's layout `width` is
    // taken from the unclipped intrinsic width, and content wider than the
    // screen pushes the FAB past the right edge.
    val available = config.screenWidthDp.dp - 16.dp
    // What the panel occupies itself: the icons with their gaps, its own
    // padding (8dp on each side), the gap to the FAB and the FAB itself in its
    // collapsed size.
    val reserved = dockHeight * DockItems.size +
        (6.dp * scale) * (DockItems.size - 1) +
        16.dp + 16.dp + 56.dp
    val labelBudget = (available - reserved).coerceIn(0.dp, 140.dp)
    // One animated fraction per item: 0 — no label, 1 — expanded. Both the gap
    // and the text width are computed from it, so the intrinsic width of the
    // content animates with the same spec as the label, and the container grows
    // with it instead of jumping.
    val labelOpen = DockItems.indices.map { i ->
        animateFloatAsState(
            targetValue = if (i == selectedIndex) 1f else 0f,
            animationSpec = labelSpec,
            label = "dockLabelOpen$i",
        ).value
    }
    // The item bounds: needed only by the pill, so it can travel over the real
    // widths (we do not reproduce `ToggleButton`'s internal padding).
    val bounds = remember { mutableStateMapOf<Int, Rect>() }
    val firstLeft = bounds[0]?.left ?: 0f
    val target = bounds[selectedIndex]
    val pillX by animateFloatAsState(
        targetValue = (target?.left ?: 0f) - firstLeft,
        animationSpec = motionScheme.defaultSpatialSpec(),
        label = "dockPillX",
    )
    val pillWidth by animateFloatAsState(
        targetValue = target?.width ?: 0f,
        animationSpec = motionScheme.defaultSpatialSpec(),
        label = "dockPillWidth",
    )

    Box(
        modifier = modifier
            .fillMaxWidth()
            .windowInsetsPadding(WindowInsets.navigationBars)
            .padding(horizontal = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        HorizontalFloatingToolbar(
            expanded = true,
            modifier = Modifier.padding(vertical = 8.dp),
            colors = colors,
            floatingActionButton = {
                FloatingToolbarDefaults.VibrantFloatingActionButton(
                    onClick = { onNavigate(Routes.EDITOR) },
                    containerColor = colors.fabContainerColor,
                    contentColor = colors.fabContentColor,
                ) {
                    Icon(Icons.Rounded.Add, contentDescription = stringResource(R.string.nav_new_project))
                }
            },
        ) {
            DockItems.forEachIndexed { index, item ->
                val selected = index == selectedIndex
                ToggleButton(
                    checked = selected,
                    onCheckedChange = {
                        if (!selected) {
                            haptic.hapticToggle(true)
                            onNavigate(item.route)
                        }
                    },
                    colors = ToggleButtonDefaults.toggleButtonColors(
                        // Transparent, not the panel colour: an opaque container
                        // of an unselected item covered the pill as it travelled
                        // beneath it.
                        containerColor = Color.Transparent,
                        contentColor = colors.toolbarContentColor,
                        checkedContainerColor = Color.Transparent,
                        checkedContentColor = scheme.onPrimary,
                    ),
                    shapes = ToggleButtonShapes(CircleShape, CircleShape, CircleShape),
                    modifier = Modifier
                        .height(dockHeight)
                        .onGloballyPositioned { coords ->
                            bounds[index] = coords.boundsInParent()
                        }
                        .then(
                            // The pill is drawn beneath the first item: the
                            // others are drawn later and land on top of it, and
                            // their containers are transparent. That way one
                            // shape serves the whole row, and it does not have to
                            // be repeated in every item.
                            if (index == 0) {
                                Modifier.drawWithContent {
                                    if (pillWidth > 0f) {
                                        drawRoundRect(
                                            color = scheme.primary,
                                            topLeft = Offset(pillX, 0f),
                                            size = Size(pillWidth, size.height),
                                            cornerRadius = CornerRadius(size.height / 2f),
                                        )
                                    }
                                    drawContent()
                                }
                            } else {
                                Modifier
                            },
                        ),
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(
                            imageVector = item.icon,
                            contentDescription = labels[index],
                            modifier = Modifier.size(iconSize),
                        )
                        Spacer(Modifier.width(iconSpacing * labelOpen[index]))
                        // The width is set from outside and animated; the excess
                        // is clipped at the row's edge. There is no separate
                        // subcomposition that clips to a changing size here — it
                        // was exactly its non-animated intrinsic width that broke
                        // the panel's layout.
                        Text(
                            text = labels[index],
                            fontSize = labelSize.sp,
                            lineHeight = (labelSize * 1.5f).sp,
                            maxLines = 1,
                            softWrap = false,
                            overflow = TextOverflow.Clip,
                            modifier = Modifier.width(
                                minOf(labelFull[index], labelBudget) * labelOpen[index],
                            ),
                        )
                    }
                }
            }
        }
    }
}
