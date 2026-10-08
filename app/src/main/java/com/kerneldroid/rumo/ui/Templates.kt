// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.content.Context
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.BurstMode
import androidx.compose.material.icons.rounded.PlayCircle
import androidx.compose.material.icons.rounded.Title
import androidx.compose.ui.graphics.vector.ImageVector
import com.kerneldroid.rumo.data.RumoBridge
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

// Template seed: project name + ready-made layers. Built directly from LayerUi
// (id — UUID), applied through EditorState.loadSeed.
/**
 * A template: the layers it starts from and the frame they are composed in.
 *
 * The canvas belongs to the template because a lower third and a vertical
 * slideshow are not the same frame, and a template that arrives in the wrong
 * aspect has to be rebuilt by hand before it can be used.
 */
data class EditorSeed(
    val name: String,
    val layers: List<LayerUi>,
    val canvasWidth: Int = EditorState.PREVIEW_W,
    val canvasHeight: Int = EditorState.PREVIEW_H,
    val backgroundArgb: Long = EditorState.PREVIEW_BG,
)

private fun uuid(): String = UUID.randomUUID().toString()

// (a) Dark background Square + yellow Sunny with rotation keys 0→180 + Title text.
fun buildIntroBounce(): EditorSeed = EditorSeed(
    name = "Intro Bounce",
    layers = listOf(
        LayerUi(
            id = uuid(),
            kind = LayerKindUi.SHAPE,
            name = "Square",
            visible = true,
            argb = 0xFF141824,
        ),
        LayerUi(
            id = uuid(),
            kind = LayerKindUi.SHAPE,
            name = "Sunny",
            visible = true,
            argb = 0xFFFFEB3B,
            keys = listOf(KeyframeUi(0L, 0f), KeyframeUi(2000L, 180f)),
        ),
        LayerUi(
            id = uuid(),
            kind = LayerKindUi.TEXT,
            name = "Title",
            visible = true,
            argb = 0xFFFFFFFF,
            text = "Rumo",
        ),
    ),
)

// (b) Pill banner at the bottom + a line of text over it.
fun buildLowerThird(): EditorSeed = EditorSeed(
    name = "Lower Third",
    // Broadcast horizontal: a lower third is composed for 16:9.
    canvasWidth = 1920,
    canvasHeight = 1080,
    layers = listOf(
        LayerUi(
            id = uuid(),
            kind = LayerKindUi.SHAPE,
            name = "Pill",
            visible = true,
            argb = 0xFF1D2026,
            offsetY = 240f,
        ),
        LayerUi(
            id = uuid(),
            kind = LayerKindUi.TEXT,
            name = "Name",
            visible = true,
            argb = 0xFFFFFFFF,
            offsetY = 240f,
            text = "Guest of the studio",
        ),
    ),
)

// (c) Three SHAPE placeholders left/centre/right + a hint text about Media.
fun buildPhotoSlideshow(): EditorSeed = EditorSeed(
    name = "Photo Slideshow",
    // Vertical, because that is what a slideshow made on a phone is for. The
    // placeholders below sit on one row, so this template is honestly a
    // horizontal one — the canvas is left at the default and the aspect is
    // shown in the list so the choice is visible rather than implied.
    canvasWidth = EditorState.PREVIEW_W,
    canvasHeight = EditorState.PREVIEW_H,
    layers = listOf(
        LayerUi(
            id = uuid(),
            kind = LayerKindUi.SHAPE,
            name = "Square",
            visible = true,
            argb = 0xFF4DD0E1,
            offsetX = -320f,
        ),
        LayerUi(
            id = uuid(),
            kind = LayerKindUi.SHAPE,
            name = "Square",
            visible = true,
            argb = 0xFFFF9800,
            offsetX = 0f,
        ),
        LayerUi(
            id = uuid(),
            kind = LayerKindUi.SHAPE,
            name = "Square",
            visible = true,
            argb = 0xFFF44336,
            offsetX = 320f,
        ),
        LayerUi(
            id = uuid(),
            kind = LayerKindUi.TEXT,
            name = "Hint",
            visible = true,
            argb = 0xFFFFFFFF,
            offsetY = -280f,
            text = "Replace with photos via Media",
        ),
    ),
)

/**
 * A built-in template: the thing that opens without a network and without a token.
 *
 * It lives in the shop rather than in a separate tab. The "Templates" tab was a catalog
 * of three entries, and the shop is that same catalog plus what others publish;
 * two tabs would split one concept and drift apart in presentation.
 */
internal data class TemplateEntry(
    val title: String,
    val subtitle: String,
    val icon: ImageVector,
    val build: () -> EditorSeed,
)

internal val TemplateEntries = listOf(
    TemplateEntry("Intro Bounce", "Dark Square + spinning Sunny + title", Icons.Rounded.PlayCircle, ::buildIntroBounce),
    TemplateEntry("Lower Third", "Pill banner + name line · 1920×1080", Icons.Rounded.Title, ::buildLowerThird),
    TemplateEntry("Photo Slideshow", "3 placeholders, swap via Media · 512×288", Icons.Rounded.BurstMode, ::buildPhotoSlideshow),
)

/**
 * Opens a built-in template in the editor: seed → JSON → bytes → project.
 *
 * The temporary [EditorState] is needed only as a bridge: the seed and the project have different
 * representations, and the only proven path between them is the same
 * `loadSeed`/`toJson` the editor uses.
 *
 * Returns the name of the saved project, or null if the engine did not accept the JSON.
 */
internal suspend fun openBuiltInTemplate(context: Context, entry: TemplateEntry): String? {
    val seed = entry.build()
    val state = EditorState()
    state.loadSeed(seed)
    val bytes = withContext(Dispatchers.IO) {
        RumoBridge.projectFromJson(state.toJson())
    } ?: return null
    val actual = ProjectStore.save(context, ProjectStore.fileNameFor(seed.name), bytes)
    state.markSaved()
    return actual
}
