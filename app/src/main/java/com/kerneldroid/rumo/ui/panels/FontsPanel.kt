// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import android.content.Context
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.MoreVert
import androidx.compose.material.icons.rounded.TextFields
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.FontStore
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.LayerKindUi
import com.kerneldroid.rumo.ui.LayerUi
import com.kerneldroid.rumo.ui.shop.decodedToImage
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * The built-in face's family: an empty string rather than a separate flag.
 *
 * It is exactly the empty string that means "built-in" on both sides — in
 * `layoutTextFamily` and in `Family::Name` — so the page has one and the same
 * key for both the label and the write into the layer.
 */
private const val BUILT_IN_FAMILY = ""

/**
 * The fonts page: a list of device faces with a real preview and a choice for the
 * selected text layer.
 *
 * ## Why the engine draws the preview
 *
 * A row shows its own name **in the face it offers** — that is
 * the point of the page. It is drawn through `nativeFontPreview`, with the same shaper,
 * atlas and compositor as a text layer, so the preview here and
 * the shop's storefront cannot diverge. `Typeface` would have given shorter code and two
 * different pictures for one font.
 *
 * ## Why a page and not one more item in Properties
 *
 * A font is a property of the device, not of a layer: the list is one for the whole app, it
 * changes when something is installed from the shop, and keeping it inside the properties panel would
 * mean re-reading the disk on every layer selection. Here it is a neighbour of Media — the place
 * where resources are looked for.
 */
@Composable
fun FontsPanel(
    state: EditorState,
    layers: List<LayerUi>,
    selectedId: String?,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val haptic = LocalHapticFeedback.current
    val density = LocalDensity.current
    val scope = rememberCoroutineScope()

    // The size is as in the shop's storefront: a list row, not a card.
    val previewPx = remember(density) { with(density) { 22.dp.toPx() }.toInt().coerceIn(16, 96) }
    // The preview ink is the theme's text colour, not a constant. The preview is drawn on a
    // transparent background: a dark constant on a dark theme gives exactly what
    // looks like an empty row.
    val inkArgb = MaterialTheme.colorScheme.onSurface.toArgb()

    // Installed fonts are device state: the list is read from disk and
    // re-read after a delete, otherwise a deleted row would come back from
    // memory on the next recomposition.
    var installed by remember { mutableStateOf<List<FontStore.Font>>(emptyList()) }
    var revision by remember { mutableIntStateOf(0) }
    LaunchedEffect(revision) {
        installed = withContext(Dispatchers.IO) {
            // Freshest first — by install time, not by index order:
            // the write order may change, while "what did I download last"
            // remains the user's question.
            FontStore.installed(context).sortedByDescending { it.addedAt }
        }
    }

    val cache = remember { FontPreviewCache() }

    // A font goes onto the **selected** layer and only if it is text: the engine
    // keeps a face only on text, the other kinds have none.
    val selectedText = layers.firstOrNull { it.id == selectedId }?.takeIf { it.kind == LayerKindUi.TEXT }
    val currentFamily: String? = selectedText?.textFamily

    // The built-in face has no bytes on disk, so there is nothing to read its name
    // from: it is the same label the font picker in Properties shows, kept in one
    // resource so the two lists cannot disagree.
    val builtInLabel = stringResource(R.string.panel_font_builtin)

    val labelOf: (String) -> String = { family ->
        if (family.isEmpty()) {
            builtInLabel
        } else {
            installed.firstOrNull { it.family == family }?.displayName ?: family
        }
    }

    // A tap on a row is the verb "apply", and it exists only with a selected
    // text layer. No layer — no action either; the status line below has already
    // said why, so the silence here does not read as a breakage.
    val applyFont: (String) -> Unit = { family ->
        if (selectedText != null) {
            haptic.hapticConfirm()
            state.setTextFamily(selectedText.id, family)
        }
    }

    Column(modifier = modifier.fillMaxSize()) {
        PanelHeader(
            title = stringResource(R.string.panel_fonts),
            subtitle = if (installed.isEmpty()) {
                stringResource(R.string.panel_fonts_none)
            } else {
                stringResource(R.string.panel_fonts_installed, installed.size)
            },
        )
        // The page's single question is "where will the font land". The line answers
        // it explicitly, including when there is nowhere to apply it.
        Text(
            text = if (selectedText == null) {
                stringResource(R.string.panel_fonts_select_text)
            } else {
                stringResource(
                    R.string.panel_fonts_selected_uses,
                    labelOf(currentFamily ?: BUILT_IN_FAMILY),
                )
            },
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(
                start = RumoSpacing.m,
                end = RumoSpacing.m,
                bottom = RumoSpacing.s,
            ),
        )

        LazyColumn(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f),
            contentPadding = PaddingValues(
                start = RumoSpacing.m,
                end = RumoSpacing.m,
                bottom = RumoSpacing.m,
            ),
            verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
        ) {
            item(key = "builtin") {
                // The built-in face has no bytes, so there is no preview and no menu:
                // there is nothing to delete. The label shows an ordinary font — that is honest,
                // the built-in face is the monospaced Compose font.
                FontRow(
                    name = builtInLabel,
                    preview = null,
                    subtitle = stringResource(R.string.panel_fonts_always_available),
                    inUse = selectedText != null && currentFamily == BUILT_IN_FAMILY,
                    onApply = { applyFont(BUILT_IN_FAMILY) },
                    onDelete = null,
                )
            }

            if (installed.isEmpty()) {
                item(key = "empty") {
                    PanelEmpty(
                        icon = Icons.Rounded.TextFields,
                        title = stringResource(R.string.panel_fonts_empty),
                        hint = stringResource(R.string.panel_fonts_empty_hint),
                    )
                }
            }

            items(installed, key = { it.family }) { font ->
                val key = fontPreviewKey(font.family, previewPx, inkArgb)
                val preview = cache.get(key)
                // The preview is requested exactly once per (family, size, colour);
                // the render is not tied to the row's composition, so scrolling does not
                // cancel a render already started.
                if (preview == null && cache.request(key)) {
                    scope.launch {
                        val image = renderFontPreview(context, font, font.displayName, previewPx, inkArgb)
                        if (image == null) cache.markFailed(key) else cache.put(key, image)
                    }
                }
                FontRow(
                    name = font.displayName,
                    preview = preview,
                    subtitle = font.license.ifEmpty {
                        stringResource(R.string.panel_fonts_license_installed)
                    },
                    inUse = currentFamily == font.family,
                    onApply = { applyFont(font.family) },
                    onDelete = {
                        scope.launch {
                            withContext(Dispatchers.IO) { FontStore.remove(context, font.family) }
                            // The deleted face's cache has no one left to show it to — holding
                            // it means holding a picture for a row that does not exist.
                            cache.dropFamily(font.family)
                            revision++
                        }
                    },
                )
            }
        }
    }
}

/**
 * One row of the list: the name (previewed by the engine, otherwise in an ordinary font),
 * the "in use" marker and, for installed ones, a menu with delete.
 */
@Composable
private fun FontRow(
    name: String,
    preview: ImageBitmap?,
    subtitle: String,
    inUse: Boolean,
    onApply: () -> Unit,
    onDelete: (() -> Unit)?,
) {
    var menuOpen by remember { mutableStateOf(false) }

    SelectedRow(selected = inUse, onClick = onApply) {
        Column(modifier = Modifier.weight(1f)) {
            if (preview != null) {
                Image(
                    bitmap = preview,
                    contentDescription = name,
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(26.dp),
                    contentScale = ContentScale.Fit,
                    alignment = Alignment.CenterStart,
                )
            } else {
                // While there is no preview (no bytes, or the engine is still drawing) — the name
                // in an ordinary font. An empty rectangle instead of a label would read
                // as a broken row.
                Text(
                    text = name,
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Text(
                text = subtitle,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (inUse) MetaPill(stringResource(R.string.panel_fonts_in_use), tint = MaterialTheme.colorScheme.primary)
        if (onDelete != null) {
            Box {
                IconButton(
                    onClick = { menuOpen = true },
                    // A touch target: an icon without a label, so the 44dp floor applies.
                    modifier = Modifier.size(MinTouchTarget),
                ) {
                    Icon(
                        imageVector = Icons.Rounded.MoreVert,
                        contentDescription = stringResource(R.string.panel_fonts_options),
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.panel_delete)) },
                        leadingIcon = { Icon(Icons.Rounded.Delete, contentDescription = null) },
                        onClick = {
                            menuOpen = false
                            onDelete()
                        },
                    )
                }
            }
        }
    }
}

/**
 * A preview of one face: the bytes from disk and the engine render — both off the main
 * thread. null means "nothing to show", and the row stays a label in an ordinary
 * font rather than emptiness.
 */
private suspend fun renderFontPreview(
    context: Context,
    font: FontStore.Font,
    text: String,
    sizePx: Int,
    argb: Int,
): ImageBitmap? {
    val bytes = withContext(Dispatchers.IO) { FontStore.bytes(context, font.family) }
        ?: return null
    return withContext(Dispatchers.Default) {
        val decoded = RumoBridge.fontPreview(
            // The face is addressed by the engine's name, and the label is the list's name:
            // for a downloaded file they may differ.
            family = font.family,
            fontBytes = bytes,
            text = text,
            sizePx = sizePx.toFloat(),
            weight = 400,
            argb = argb,
            pad = 2,
        ) ?: return@withContext null
        decodedToImage(decoded.width, decoded.height, decoded.rgba)
    }
}

/**
 * The preview key: family, size and **colour** — everything baked into the picture.
 *
 * The colour is part of the key because it is part of the raster preview: a picture
 * drawn for a light theme is invisible on a dark one. Without the colour, a theme change
 * would leave an invisible row in the cache.
 */
private fun fontPreviewKey(family: String, sizePx: Int, argb: Int): String =
    "$family@$sizePx#${argb.toUInt().toString(16)}"

/**
 * An in-memory preview cache, bounded by the number of pictures.
 *
 * `mutableStateMapOf` rather than an ordinary map: the arrival of a raster picture must
 * redraw the row. With invisible state, `LaunchedEffect` would put the
 * preview into the cache and no one would see it on the screen.
 *
 * The bound is needed because a preview is a raster per row, and the list
 * is opened and closed any number of times.
 */
private class FontPreviewCache(private val max: Int = 64) {
    private val images = mutableStateMapOf<String, ImageBitmap>()

    /** Insertion order for evicting the oldest preview. */
    private val order = ArrayDeque<String>()

    /** Keys already requested: without it every recomposition would start a render. */
    private val requested = HashSet<String>()

    /** Keys the render rejected: there is no point repeating them. */
    private val failed = HashSet<String>()

    fun get(key: String): ImageBitmap? = images[key]

    /** true means the request is the first, the render must be started. */
    fun request(key: String): Boolean = requested.add(key)

    fun markFailed(key: String) {
        failed += key
    }

    fun put(key: String, image: ImageBitmap) {
        if (images.put(key, image) == null) order.addLast(key)
        while (order.size > max) images.remove(order.removeFirst())
    }

    /** Removes everything belonging to the deleted family. */
    fun dropFamily(family: String) {
        val prefix = "$family@"
        for (key in images.keys.filter { it.startsWith(prefix) }) {
            images.remove(key)
            order.remove(key)
        }
        requested.removeAll { it.startsWith(prefix) }
        failed.removeAll { it.startsWith(prefix) }
    }
}
