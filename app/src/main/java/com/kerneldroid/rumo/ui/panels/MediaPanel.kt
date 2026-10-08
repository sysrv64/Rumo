// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import android.graphics.Bitmap
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.StringRes
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.AddPhotoAlternate
import androidx.compose.material.icons.rounded.Collections
import androidx.compose.material.icons.rounded.Movie
import androidx.compose.material.icons.rounded.GraphicEq
import androidx.compose.material.icons.rounded.Image
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.ShapeLine
import androidx.compose.material.icons.rounded.TextFields
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.AppLog
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.LayerKindUi
import com.kerneldroid.rumo.ui.LayerUi
import com.kerneldroid.rumo.ui.MeshUi
import com.kerneldroid.rumo.ui.ProjectAssets
import com.kerneldroid.rumo.ui.ShapeNames
import com.kerneldroid.rumo.ui.formatTime
import com.kerneldroid.rumo.ui.queryDisplayName
import com.kerneldroid.rumo.ui.readUriBytes
import com.kerneldroid.rumo.ui.theme.editor
import com.kerneldroid.rumo.ui.theme.RumoKind
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.hapticToggle
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private enum class LibraryTab(@StringRes val labelRes: Int) {
    SHAPES(R.string.panel_media_tab_shapes),
    PHOTOS(R.string.panel_media_tab_photos),
    SVG(R.string.panel_media_tab_svg),
    TEXT(R.string.panel_media_tab_text),
}

/**
 * Library page: the engine's own shape set, imported photos, text presets and
 * audio. Every tile writes through a real EditorState mutator — no dead chips.
 */
@Composable
fun MediaPanel(
    state: EditorState,
    layers: List<LayerUi>,
    selectedId: String?,
    onSelect: (String) -> Unit,
    onImportImage: () -> Unit,
    /// Video lived only in the "+" sheet, so "add video" was not to be found on
    /// the Media page — the feature existed, there was no place for it (UX audit, T7).
    onImportVideo: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    var tab by remember { mutableIntStateOf(0) }
    val haptic = LocalHapticFeedback.current
    val trisMap by state.shapeTris.collectAsState()
    val meshes by state.meshes.collectAsState()

    val shapeLayers = layers.filter { it.kind == LayerKindUi.SHAPE }
    val photoLayers = layers.filter { it.kind == LayerKindUi.MEDIA }
    val textLayers = layers.filter { it.kind == LayerKindUi.TEXT }
    // An SVG layer is a SHAPE with a uri to a .svg (the engine knows no separate
    // kind), so "shapes" and "SVG" are two views of one list, not two lists.
    val svgLayers = shapeLayers.filter { state.isSvgLayer(it) }

    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val project = state.projectName.collectAsState().value
    // An import awaiting the user's decision: the file is read, and the vector
    // either loses elements or does not parse at all. A dialog is the form of
    // question the app already uses (`AlertDialog`), so the choice is offered
    // through one rather than through a new surface.
    var pendingSvg by remember { mutableStateOf<PendingSvgChoice?>(null) }
    // An import failure the user has to see, not only the log.
    var svgImportError by remember { mutableStateOf<String?>(null) }
    // One path for both: show it in a dialog and leave a line in the log. A
    // silent failure here would mean "pressed — and nothing", which reads as a
    // breakdown.
    fun reportSvgError(message: String) {
        svgImportError = message
        AppLog.warn("svg", message)
    }

    /**
     * Write the vector: a copy into the project folder, registration, and only
     * then the layer.
     *
     * Registration is checked **before** [EditorState.addSvgLayer]: a failure
     * would otherwise leave a layer that silently does not draw. Returns the
     * layer's uri, or null with a reason — we show the reason rather than hide it.
     */
    suspend fun importVector(uri: Uri): Pair<String?, String> {
        val asset = ProjectAssets.copySvgIn(context, project, uri)
            ?: return null to "the file was not copied into the project folder"
        val svgUri = asset.uri.toString()
        val id = withContext(Dispatchers.IO) { state.ensureSvgRegistered(svgUri) }
        if (id == null) {
            return null to (state.svgFailure(svgUri) ?: "the engine could not parse the SVG")
        }
        state.addSvgLayer(asset.name, svgUri)
        return svgUri to ""
    }

    /**
     * Write the raster: the engine draws the whole document
     * (`nativeSvgRasterize`), and the result becomes an ordinary picture — the
     * same MEDIA layer as any imported PNG. This is the difference the user has
     * to see: a picture does not scale without blurring.
     */
    suspend fun importRaster(name: String, bytes: ByteArray, vectorPossible: Boolean, reason: String) {
        val side = maxOf(state.canvasWidth.value, state.canvasHeight.value)
            .coerceIn(1, RumoBridge.SVG_RASTER_MAX_SIDE)
        val decoded = withContext(Dispatchers.IO) { RumoBridge.svgRasterize(bytes, side) }
        if (decoded == null) {
            // Both paths failed — we name both, not only the second.
            reportSvgError(
                if (vectorPossible) {
                    "«$name»: the engine's rasteriser returned empty. Nothing was written — the vector " +
                        "path works for this file; on import choose «Draw as vector»."
                } else {
                    "«$name»: the vector did not parse" +
                        (if (reason.isBlank()) "" else " ($reason)") +
                        ", and the engine's rasteriser returned empty. Nothing was written."
                },
            )
            return
        }
        val png = rasterPng(decoded)
        if (png == null) {
            reportSvgError("«$name»: the raster could not be encoded to PNG. Nothing was written.")
            return
        }
        val asset = ProjectAssets.writePicture(context, project, name, png)
        if (asset == null) {
            reportSvgError("«$name»: the raster could not be written to the project folder. Nothing was written.")
            return
        }
        val uri = asset.uri.toString()
        state.addMediaLayer(asset.name, LayerKindUi.MEDIA, EditorState.DEFAULT_MIN_DURATION_MS, uri)
        // We load the texture at once: without it the MEDIA layer will not make it into the frame.
        state.stageTexture(uri, decoded)
        haptic.hapticConfirm()
    }

    // The picker here is its own, not the shared `pickers` from EditorScreen: SVG
    // has its own MIME, and adding it to that same list would mean threading a
    // callback through EditorScreen for one button. We copy into the project
    // folder at once — the picker's grant lives only until a restart, and the
    // file has to stay.
    val pickSvg = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            val bytes = withContext(Dispatchers.IO) { readUriBytes(context, uri) }
            if (bytes == null || bytes.isEmpty()) {
                reportSvgError("Cannot read the file: $uri")
                return@launch
            }
            val name = queryDisplayName(context, uri, "drawing.svg")
            val validation = RumoBridge.svgValidate(bytes)
            when {
                // The vector is clean — nothing is lost, there is nothing to ask about.
                validation != null && validation.ok && validation.skipped == 0 -> {
                    val (svgUri, reason) = importVector(uri)
                    if (svgUri != null) haptic.hapticConfirm()
                    else reportSvgError("«$name»: $reason")
                }
                // The vector parses, but the engine will skip some of the
                // elements: that is a real difference (something will not be
                // drawn), so the choice belongs to the user, not to a silent
                // substitution.
                validation != null && validation.ok -> {
                    pendingSvg = PendingSvgChoice(
                        name = name,
                        uri = uri,
                        bytes = bytes,
                        reason = "",
                        skipped = validation.skipped,
                        vectorPossible = true,
                    )
                }
                // Parsing rejected the document: the vector is impossible, so we offer the raster.
                validation != null -> {
                    pendingSvg = PendingSvgChoice(
                        name = name,
                        uri = uri,
                        bytes = bytes,
                        reason = validation.error,
                        skipped = 0,
                        vectorPossible = false,
                    )
                }
                // There is no validation symbol (an old .so): we judge the
                // vector's fitness by registration, not by the absence of an answer.
                else -> {
                    val (svgUri, reason) = importVector(uri)
                    if (svgUri != null) {
                        haptic.hapticConfirm()
                    } else {
                        pendingSvg = PendingSvgChoice(
                            name = name,
                            uri = uri,
                            bytes = bytes,
                            reason = reason,
                            skipped = 0,
                            vectorPossible = false,
                        )
                    }
                }
            }
        }
    }

    // Previews of every shape are engine meshes (nativeShapeMesh) — load them
    // once so a tile shows the real geometry the renderer will use.
    LaunchedEffect(Unit) { state.ensureMeshes(ShapeNames) }

    Column(modifier = modifier.fillMaxSize()) {
        PanelHeader(
            // One name with the rail: the rail says "Media", the header used to
            // say "Library", and both were visible at once — the place could not
            // be learned (docs/10, "one name per page").
            title = stringResource(R.string.panel_media),
            // Four counted nouns in one line: each one is its own plural resource
            // so Russian can inflect them, and the parts are joined exactly as
            // before.
            subtitle = listOf(
                pluralStringResource(R.plurals.panel_media_shapes, shapeLayers.size, shapeLayers.size),
                pluralStringResource(R.plurals.panel_media_photos, photoLayers.size, photoLayers.size),
                pluralStringResource(R.plurals.panel_media_svg, svgLayers.size, svgLayers.size),
                pluralStringResource(
                    R.plurals.panel_media_audio,
                    layers.count { it.kind == LayerKindUi.AUDIO },
                    layers.count { it.kind == LayerKindUi.AUDIO },
                ),
            ).joinToString(" · "),
        )
        PanelTabs(
            labels = LibraryTab.entries.map { stringResource(it.labelRes) },
            selected = tab,
            onSelect = { tab = it },
            modifier = Modifier.padding(
                start = RumoSpacing.m,
                end = RumoSpacing.m,
                bottom = RumoSpacing.s,
            ),
        )
        when (LibraryTab.entries[tab]) {
            LibraryTab.SHAPES -> LazyVerticalGrid(
                columns = GridCells.Fixed(3),
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f),
                contentPadding = PaddingValues(
                    start = RumoSpacing.m,
                    end = RumoSpacing.m,
                    bottom = RumoSpacing.m,
                ),
                horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
            ) {
                items(ShapeNames.size) { ordinal ->
                    val name = ShapeNames[ordinal]
                    ShapeTile(
                        name = name,
                        tris = trisMap[ordinal],
                        mesh = meshes[name],
                        onClick = {
                            state.addShape(name)
                            haptic.hapticConfirm()
                        },
                    )
                }
            }

            LibraryTab.PHOTOS -> LazyColumn(
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
                item {
                    ActionTile(
                        icon = Icons.Rounded.AddPhotoAlternate,
                        label = stringResource(R.string.panel_media_import_photo),
                        onClick = onImportImage,
                    )
                }
                item {
                    ActionTile(
                        icon = Icons.Rounded.Movie,
                        label = stringResource(R.string.panel_media_import_video),
                        onClick = onImportVideo,
                    )
                }
                if (photoLayers.isEmpty()) {
                    item {
                        PanelEmpty(
                            icon = Icons.Rounded.Collections,
                            title = stringResource(R.string.panel_media_photos_empty),
                            hint = stringResource(R.string.panel_media_photos_empty_hint),
                        )
                    }
                }
                items(photoLayers, key = { it.id }) { layer ->
                    SelectedRow(
                        selected = layer.id == selectedId,
                        onClick = { onSelect(layer.id) },
                    ) {
                        PhotoThumb(uri = layer.uri, modifier = Modifier.size(44.dp))
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                text = layer.name,
                                style = MaterialTheme.typography.bodyMedium,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Text(
                                text = stringResource(
                                    R.string.panel_media_photo_meta,
                                    formatTime(layer.durationMs),
                                ),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }

            LibraryTab.SVG -> LazyColumn(
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
                item {
                    ActionTile(
                        icon = Icons.Rounded.ShapeLine,
                        label = stringResource(R.string.panel_media_import_svg),
                        onClick = { pickSvg.launch(arrayOf("image/svg+xml")) },
                    )
                }
                if (svgLayers.isEmpty()) {
                    item {
                        PanelEmpty(
                            icon = Icons.Rounded.ShapeLine,
                            title = stringResource(R.string.panel_media_svg_empty),
                            hint = stringResource(R.string.panel_media_svg_empty_hint),
                        )
                    }
                }
                items(svgLayers, key = { it.id }) { layer ->
                    SelectedRow(
                        selected = layer.id == selectedId,
                        onClick = { onSelect(layer.id) },
                    ) {
                        KindMark(kind = LayerKindUi.SHAPE, size = 34.dp)
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                text = layer.name,
                                style = MaterialTheme.typography.bodyMedium,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Text(
                                text = stringResource(R.string.panel_media_svg_meta),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }

            LibraryTab.TEXT -> Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f)
                    .padding(horizontal = RumoSpacing.m),
                verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
            ) {
                TextComposer(onAdd = { state.addTextLayer(it); haptic.hapticConfirm() })
                LazyColumn(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f),
                    verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                ) {
                    items(textLayers, key = { it.id }) { layer ->
                        SelectedRow(
                            selected = layer.id == selectedId,
                            onClick = { onSelect(layer.id) },
                        ) {
                            KindMark(kind = LayerKindUi.TEXT, size = 34.dp)
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    text = layer.name,
                                    style = MaterialTheme.typography.bodyMedium,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                Text(
                                    text = stringResource(R.string.panel_media_text_meta),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                }
            }

        }
    }

    // Choosing a path: we ask where the app already asks — in a dialog. The
    // raster is not a silent substitution for the vector: it does not scale, and
    // that is said here plainly.
    pendingSvg?.let { pending ->
        SvgPathDialog(
            pending = pending,
            onVector = {
                pendingSvg = null
                scope.launch {
                    val (svgUri, reason) = importVector(pending.uri)
                    if (svgUri != null) haptic.hapticConfirm()
                    else reportSvgError("«${pending.name}»: $reason")
                }
            },
            onRaster = {
                pendingSvg = null
                scope.launch {
                    importRaster(pending.name, pending.bytes, pending.vectorPossible, pending.reason)
                }
            },
            onDismiss = { pendingSvg = null },
        )
    }

    // A failure of both paths (or of the write) — with a reason, not with silence.
    svgImportError?.let { message ->
        AlertDialog(
            onDismissRequest = { svgImportError = null },
            title = { Text(stringResource(R.string.panel_media_svg_failed)) },
            text = { Text(message) },
            confirmButton = {
                TextButton(onClick = { svgImportError = null }) { Text(stringResource(R.string.panel_ok)) }
            },
        )
    }
}

/** An SVG import awaiting a path choice; the bytes are already read. */
private data class PendingSvgChoice(
    val name: String,
    val uri: Uri,
    val bytes: ByteArray,
    /** The vector's rejection reason, if it turned the document down. */
    val reason: String,
    /** How many elements the vector will skip (0 when the vector is impossible). */
    val skipped: Int,
    /** Whether it can still be drawn as a vector (with losses). */
    val vectorPossible: Boolean,
)

/**
 * The choice dialog: vector or raster.
 *
 * It shows exactly what distinguishes the paths — how many elements the vector
 * will lose and that the raster does not scale — so that the user decides rather
 * than receiving a silent substitution.
 */
@Composable
private fun SvgPathDialog(
    pending: PendingSvgChoice,
    onVector: () -> Unit,
    onRaster: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = {
            Text(
                if (pending.vectorPossible) {
                    stringResource(R.string.panel_media_svg_lossy_title)
                } else {
                    stringResource(R.string.panel_media_svg_refused_title)
                },
            )
        },
        text = {
            Text(
                if (pending.vectorPossible) {
                    stringResource(
                        R.string.panel_media_svg_lossy_message,
                        pending.name,
                        pending.skipped,
                    )
                } else if (pending.reason.isBlank()) {
                    stringResource(R.string.panel_media_svg_refused_message, pending.name)
                } else {
                    stringResource(
                        R.string.panel_media_svg_refused_message_reason,
                        pending.name,
                        pending.reason,
                    )
                },
            )
        },
        confirmButton = {
            TextButton(onClick = onRaster) {
                Text(stringResource(R.string.panel_media_svg_raster))
            }
        },
        dismissButton = {
            Row {
                if (pending.vectorPossible) {
                    TextButton(onClick = onVector) {
                        Text(stringResource(R.string.panel_media_svg_vector))
                    }
                }
                TextButton(onClick = onDismiss) { Text(stringResource(R.string.panel_cancel)) }
            }
        },
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun PanelTabs(
    labels: List<String>,
    selected: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    FlowRow(
        modifier = modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
    ) {
        labels.forEachIndexed { index, label ->
            CompactChip(
                label = label,
                selected = selected == index,
                onClick = { onSelect(index) },
            )
        }
    }
}

/** Selection ground shared by every list row in the dock. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun SelectedRow(
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    onDoubleClick: (() -> Unit)? = null,
    content: @Composable RowScope.() -> Unit,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(
                if (selected) {
                    MaterialTheme.colorScheme.secondaryContainer
                } else {
                    MaterialTheme.colorScheme.surfaceContainerHigh
                },
            )
            .then(
                if (selected) {
                    Modifier.border(
                        width = 1.dp,
                        color = MaterialTheme.colorScheme.primary,
                        shape = RoundedCornerShape(14.dp),
                    )
                } else {
                    Modifier
                },
            )
            .then(
                if (onDoubleClick == null) {
                    Modifier.clickable(onClick = onClick)
                } else {
                    // Second verb on the row itself instead of one more icon:
                    // a double tap is what turns the row on and off.
                    Modifier.combinedClickable(
                        onClick = onClick,
                        onDoubleClick = onDoubleClick,
                    )
                },
            )
            .padding(horizontal = RumoSpacing.s, vertical = RumoSpacing.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
        content = content,
    )
}

/**
 * Shape tile. The silhouette is drawn from the engine mesh
 * (`nativeShapeMesh`, one triangle per 3 points) so the tile shows exactly the
 * geometry the compositor will rasterise — including the triangle count.
 */
@Composable
private fun ShapeTile(
    name: String,
    tris: Int?,
    mesh: MeshUi?,
    onClick: () -> Unit,
) {
    Column(
        modifier = Modifier
            .clip(RoundedCornerShape(16.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
            .clickable(onClick = onClick)
            .padding(vertical = RumoSpacing.s, horizontal = RumoSpacing.xs),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
    ) {
        val placeholderColor = MaterialTheme.editor.line
        Canvas(modifier = Modifier.size(40.dp)) {
            val points = mesh?.points
            if (points.isNullOrEmpty()) {
                // Engine mesh not loaded yet: neutral placeholder, not a fake shape.
                drawRoundRect(
                    // The placeholder is chrome, not data: it takes the scheme's
                    // line, not a white literal that did not change with the seed.
                    color = placeholderColor,
                    cornerRadius = androidx.compose.ui.geometry.CornerRadius(6f, 6f),
                )
                return@Canvas
            }
            var minX = Float.MAX_VALUE
            var minY = Float.MAX_VALUE
            var maxX = -Float.MAX_VALUE
            var maxY = -Float.MAX_VALUE
            for (p in points) {
                if (p.first < minX) minX = p.first
                if (p.first > maxX) maxX = p.first
                if (p.second < minY) minY = p.second
                if (p.second > maxY) maxY = p.second
            }
            val span = maxOf(maxX - minX, maxY - minY).coerceAtLeast(1f)
            val scale = (size.minDimension * 0.86f) / span
            val ox = size.width / 2f - (minX + maxX) / 2f * scale
            val oy = size.height / 2f - (minY + maxY) / 2f * scale
            val path = Path()
            var i = 0
            while (i + 2 < points.size) {
                val a = points[i]
                val b = points[i + 1]
                val c = points[i + 2]
                path.moveTo(a.first * scale + ox, a.second * scale + oy)
                path.lineTo(b.first * scale + ox, b.second * scale + oy)
                path.lineTo(c.first * scale + ox, c.second * scale + oy)
                path.close()
                i += 3
            }
            drawPath(path = path, color = RumoKind.shapeMark)
        }
        Text(
            text = name,
            style = MaterialTheme.typography.labelSmall,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            textAlign = TextAlign.Center,
            modifier = Modifier.fillMaxWidth(),
        )
        Text(
            text = if (tris != null) stringResource(R.string.panel_media_tris, tris) else "…",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
        )
    }
}

/** Live preview of an imported photo layer (decoded by the Rust pipeline). */
@Composable
private fun PhotoThumb(uri: String?, modifier: Modifier = Modifier) {
    val bitmap = rememberUriThumb(uri)
    Box(
        modifier = modifier
            .clip(RoundedCornerShape(10.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerHighest),
        contentAlignment = Alignment.Center,
    ) {
        if (bitmap != null) {
            androidx.compose.foundation.Image(
                bitmap = bitmap,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )
        } else {
            Icon(
                imageVector = Icons.Rounded.Image,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(18.dp),
            )
        }
    }
}

@Composable
private fun TextComposer(onAdd: (String) -> Unit) {
    var query by remember { mutableStateOf("") }
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
    ) {
        OutlinedTextField(
            value = query,
            onValueChange = { if (it.length <= 140 && !it.contains('\n')) query = it },
            label = { Text(stringResource(R.string.panel_media_new_text)) },
            singleLine = true,
            modifier = Modifier.weight(1f),
        )
        Button(
            onClick = {
                onAdd(query)
                query = ""
            },
            enabled = query.isNotBlank(),
        ) {
            Text(stringResource(R.string.panel_add))
        }
    }
}

/** Photo bytes -> RGBA (Rust) -> ImageBitmap; null while loading or on failure. */
@Composable
private fun rememberUriThumb(uri: String?, maxSide: Int = 192): ImageBitmap? {
    val context = LocalContext.current
    var bitmap by remember(uri) { mutableStateOf<ImageBitmap?>(null) }
    LaunchedEffect(uri) {
        bitmap = null
        if (uri == null) return@LaunchedEffect
        bitmap = withContext(Dispatchers.IO) {
            try {
                val bytes = readUriBytes(context, Uri.parse(uri)) ?: return@withContext null
                val decoded = RumoBridge.decodeImage(bytes, maxSide) ?: return@withContext null
                val bmp = Bitmap.createBitmap(
                    decoded.width,
                    decoded.height,
                    Bitmap.Config.ARGB_8888,
                )
                bmp.copyPixelsFromBuffer(ByteBuffer.wrap(decoded.rgba))
                bmp.asImageBitmap()
            } catch (_: Exception) {
                null
            }
        }
    }
    return bitmap
}

/** The engine's raster (RGBA8) -> PNG bytes for the picture layer; null when the codec fails. */
private fun rasterPng(decoded: RumoBridge.DecodedImage): ByteArray? {
    val px = rgbaToArgb(decoded.rgba, decoded.width, decoded.height) ?: return null
    return try {
        val bitmap = Bitmap.createBitmap(px, decoded.width, decoded.height, Bitmap.Config.ARGB_8888)
        ByteArrayOutputStream().use { out ->
            if (bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)) out.toByteArray() else null
        }
    } catch (_: Throwable) {
        null
    }
}

/**
 * RGBA8 (the engine's order, straight alpha) -> one int per pixel (the Bitmap
 * order 0xAARRGGBB).
 *
 * Explicit assembly, not `copyPixelsFromBuffer`: the latter puts the bytes into
 * memory as they are, and the RGBA stream would land in an ARGB buffer with R
 * and B swapped. The PNG channels have to match what the engine has already
 * drawn.
 */
private fun rgbaToArgb(rgba: ByteArray, width: Int, height: Int): IntArray? {
    if (width <= 0 || height <= 0 || rgba.size != width * height * 4) return null
    val px = IntArray(width * height)
    var src = 0
    for (i in px.indices) {
        val r = rgba[src].toInt() and 0xFF
        val g = rgba[src + 1].toInt() and 0xFF
        val b = rgba[src + 2].toInt() and 0xFF
        val a = rgba[src + 3].toInt() and 0xFF
        px[i] = (a shl 24) or (r shl 16) or (g shl 8) or b
        src += 4
    }
    return px
}
