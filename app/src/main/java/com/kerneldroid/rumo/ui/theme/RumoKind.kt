// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.theme

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp

/**
 * Kind mark + darker well pairs for layer kinds.
 *
 * Source: Concat `ui/theme/dark.slint` media-kind tokens (video/audio/image/text/
 * filter mark + well). The pair is deliberately shared between the library card
 * and the timeline clip so one object keeps reading as one object after a drag.
 * Hex values are Concat's, retuned for Rumo's surfaces.
 */
object RumoKind {
    val shapeMark = Color(0xFF6D63E8)
    val shapeWell = Color(0xFF23204A)
    val textMark = Color(0xFFCF5A99)
    val textWell = Color(0xFF321C29)
    val imageMark = Color(0xFF9F63D6)
    val imageWell = Color(0xFF2A1E34)
    val audioMark = Color(0xFF34C46F)
    val audioWell = Color(0xFF12301C)

    // Timeline / element chrome (Concat dark.slint lanes, ruler, playhead).
    val playhead = Color(0xFFCBF53F)
    val grid = Color(0x24FFFFFF)
    val snap = Color(0xFFF5C542)
    val wave = Color(0xB3A7F0C2)
}

/**
 * Android phone metrics.
 * Source: Drift `src/qml/Theme.qml` Android block (topBar 56, bottomRail 64,
 * minTouchTarget 48, trackLabels 88, playheadHandle 16, clipTrimHandle 20,
 * keyframe diamond 12) and Concat `ui/phone/controls.slint` ("forty-four pixels
 * is the smallest target a thumb lands on reliably").
 */
object DockTokens {
    /**
     * Tab strip / bottom rail height. Four slots carry an icon *and* a label, so
     * this is 56dp rather than the 44dp that six icon-only slots needed: a label
     * the user can read is worth 12dp of dock content (docs/10 §7.4).
     */
    val railHeight = 56.dp

    /**
     * Width of the vertical surface rail.
     *
     * 56dp: a 20dp icon and a label under it with 10dp of air on either side.
     * Exactly as much as a thumb and an eye need, and not a tenth of a point more —
     * every point given here is taken from the property row, which needs the width
     * of its label, track and value (EditorControls.kt, [PropertyRow]).
     */
    val railWidth = 56.dp

    /** Height of one vertical rail slot: a touch target with room to spare. */
    val railSlot = 60.dp

    /** Height of the vertical drag zone above the rail (grab bar + padding). */
    val handleZone = 14.dp

    val rulerHeight = 26.dp
    val trackRow = 26.dp
    val keyDiamond = 12.dp

    /** Whole transport block: one row of 32dp verbs + the seek slider. */
    // A 44dp button + 4dp bottom padding. It was 40dp with 32dp buttons:
    // `Modifier.size(32.dp)` pins the constraints too, so
    // `minimumInteractiveComponentSize` inside IconButton could no longer
    // grow the touch target — it stayed 32dp, half the 44dp floor declared
    // in the project (`EffectsPanel.MinTouchTarget`).
    // Play is the most-pressed button on the screen, and missing it costs more
    // than eight dp of preview.
    val transportHeight = 48.dp

    /**
     * Floor for the panel area in the preview's "Fill" mode. The dock itself is
     * sized by the layout (it is the remainder of the column), not by a fraction
     * of the screen: sizing it from screenH is what left a dead band between the
     * preview and the timeline on a tall phone.
     */
    /**
     * How much the rail needs for `slots` slots: the surfaces plus "Done"
     * when there is something to deselect, plus the vertical padding.
     *
     * This is the panel's real minimum. The [panelFloorHeight] constant
     * was its lower estimate and missed: a rail with a selection asks for
     * 308dp, not 268dp, and the bottom surface got clipped.
     */
    fun railContentHeight(slots: Int) = railSlot * slots + RumoSpacing.xs * 2

    /**
     * Estimate of the timeline's height: the ruler plus two tracks plus the padding.
     *
     * Needed only for the *default* dock height. `TimelineStrip` itself knows the
     * exact height, but it depends on the number of tracks and changes as work
     * proceeds, while the dock has to get its own before the timeline is measured.
     * An error of a couple of dp here costs one step of rail scrolling, not a lost
     * surface.
     */
    val timelineEstimate = 86.dp

    /** Panel floor: how much to keep when the space is tight and the rail scrolls. */
    val panelFloorHeight = 268.dp
}
