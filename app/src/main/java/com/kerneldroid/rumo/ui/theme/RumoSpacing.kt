// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.theme

import androidx.compose.ui.unit.dp

// W1: a single spacing scale (consensus of docs/07-ui-research.md §4:
// ImageToolbox + RvSM: 8/12/16/20, cards 24, lists spacedBy(16)).
// Durations are deliberately ABSENT here: every spec comes from
// MaterialTheme.motionScheme (threaded through RumoTheme), applied in W3.
object RumoSpacing {
    val xs = 4.dp
    val s = 8.dp
    val m = 12.dp
    val l = 16.dp
    val xl = 20.dp
    val card = 24.dp
    val maxPane = 600.dp
}
