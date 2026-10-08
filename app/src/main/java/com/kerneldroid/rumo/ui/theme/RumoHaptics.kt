// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.theme

import androidx.compose.ui.hapticfeedback.HapticFeedback
import androidx.compose.ui.hapticfeedback.HapticFeedbackType

// W1: haptics helpers modelled on Tomato ui/Haptics.kt (compose
// LocalHapticFeedback — no new dependencies, no view.performHapticFeedback,
// safe on minSdk 26: everything is carried out by the compose runtime).
// NOTE: the spec asked for ui/Haptics.kt, but W1 allows touching ONLY ui/theme/**,
// so the file lives here (com.kerneldroid.rumo.ui.theme).
fun HapticFeedback.hapticToggle(checked: Boolean) =
    performHapticFeedback(
        if (checked) HapticFeedbackType.ToggleOn else HapticFeedbackType.ToggleOff,
    )

fun HapticFeedback.hapticConfirm() = performHapticFeedback(HapticFeedbackType.Confirm)

fun HapticFeedback.hapticReject() = performHapticFeedback(HapticFeedbackType.Reject)

fun HapticFeedback.hapticLongPress() = performHapticFeedback(HapticFeedbackType.LongPress)
