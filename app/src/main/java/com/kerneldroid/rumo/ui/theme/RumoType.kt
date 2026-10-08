// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.theme

import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily

// W1: tabular figures for timing (consensus of docs/07-ui-research.md §3).
// Use in screens comes in W3; here it is only the style definition.
val monoNumerals = TextStyle(
    fontFamily = FontFamily.Monospace,
    fontFeatureSettings = "tnum",
)
