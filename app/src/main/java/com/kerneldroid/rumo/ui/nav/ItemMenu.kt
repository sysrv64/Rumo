// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.nav

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Code
import androidx.compose.material.icons.rounded.ContentCopy
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.Edit
import androidx.compose.material.icons.rounded.FileDownload
import androidx.compose.material3.DropdownMenuGroup
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.DropdownMenuPopup
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.FilledTonalIconToggleButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MenuDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalViewConfiguration
import androidx.compose.ui.platform.ViewConfiguration

/**
 * One action in a list-row menu.
 *
 * The icon is required, not optional: the menu opens over the list, and a row
 * without an icon reads more slowly than one with it — the eye finds "rename" by
 * the pencil before it finds it by the word.
 */
data class NavMenuAction(
    val label: String,
    val icon: ImageVector,
    val onClick: () -> Unit,
)

/**
 * Action menu for a list row.
 *
 * ## Why a button, and not the list straight away
 *
 * Each row used to carry a delete icon — one action out of four, shown at all
 * times. That made deletion the most accessible action in the list, and it is
 * the only irreversible one. A menu button shows all the actions at once and
 * singles out none of them.
 *
 * ## Groups
 *
 * The actions come in groups separated by a gap, and the groups are rounded at
 * the edges (`groupShape`) — that way the menu reads as several sets, not as one
 * list. The irreversible action sits in a group of its own: the gap says "this
 * is something else" before the user has read the word.
 *
 * ## Where it opens
 *
 * `DropdownMenuPopup` anchors to the parent `Box`, so the button and the menu
 * live in one `Box`: without it the menu would sit along the edge of the row
 * instead of along the button.
 */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun NavItemMenu(
    contentDescription: String,
    icon: ImageVector,
    groups: List<List<NavMenuAction>>,
) {
    var expanded by remember { mutableStateOf(false) }
    val filled = groups.flatten()
    if (filled.isEmpty()) return
    Box {
        FilledTonalIconToggleButton(
            checked = expanded,
            onCheckedChange = { expanded = it },
            // The shape changes on press and when open: a button that does not
            // respond reads as a picture, not as a button.
            shapes = IconButtonDefaults.toggleableShapes(),
        ) {
            Icon(imageVector = icon, contentDescription = contentDescription)
        }
        DropdownMenuPopup(expanded = expanded, onDismissRequest = { expanded = false }) {
            groups.forEachIndexed { groupIndex, actions ->
                if (actions.isEmpty()) return@forEachIndexed
                if (groupIndex > 0) Spacer(Modifier.height(MenuDefaults.GroupSpacing))
                DropdownMenuGroup(shapes = MenuDefaults.groupShape(groupIndex, groups.size)) {
                    actions.forEachIndexed { index, action ->
                        DropdownMenuItem(
                            onClick = {
                                // Collapses before the action: the action may
                                // lead to another screen, and a menu left
                                // hanging over it is a menu someone forgot.
                                expanded = false
                                action.onClick()
                            },
                            text = { Text(action.label) },
                            leadingIcon = { Icon(action.icon, contentDescription = null) },
                            // `.shape`, not `shapes`: a tapped item has one
                            // shape, while `MenuItemShapes` is a pair of "normal
                            // and selected", and only selectable items need it.
                            shape = MenuDefaults.itemShape(index, actions.size).shape,
                        )
                    }
                }
            }
        }
    }
}

/**
 * A hold lasting [holdMs] instead of the system one.
 *
 * The system hold is about half a second, and for a list that is little: a touch
 * during a scroll lasts just as long, so a menu opening on a hold would trigger
 * on an ordinary finger movement. Three seconds is a deliberate action, and it
 * cannot be missed.
 *
 * `ViewConfiguration` is Compose's own extension point: `combinedClickable` and
 * `detectTapGestures` read the hold duration from there rather than from a
 * constant, so there is no need to override the gesture behaviour — naming the
 * duration is enough.
 */
private class HoldConfiguration(
    private val base: ViewConfiguration,
    private val holdMs: Long,
) : ViewConfiguration by base {
    override val longPressTimeoutMillis: Long get() = holdMs
}

/** Hold duration for the list-row menu. */
const val ListHoldMillis = 3_000L

/** Wrap a list so that a hold inside it lasts [holdMs]. */
@Composable
fun ProvideHoldDuration(holdMs: Long = ListHoldMillis, content: @Composable () -> Unit) {
    val base = LocalViewConfiguration.current
    val configuration = remember(base, holdMs) { HoldConfiguration(base, holdMs) }
    CompositionLocalProvider(LocalViewConfiguration provides configuration, content = content)
}

/** Icons for the menus, so that screens do not each pick their own. */
object NavMenuIcons {
    val Edit = Icons.Rounded.Edit
    val Copy = Icons.Rounded.ContentCopy
    val Export = Icons.Rounded.FileDownload
    val Delete = Icons.Rounded.Delete
    val Code = Icons.Rounded.Code
}
