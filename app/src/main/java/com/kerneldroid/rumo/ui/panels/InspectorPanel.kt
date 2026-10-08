// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.AccountTree
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.BorderOuter
import androidx.compose.material.icons.rounded.Check
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Opacity
import androidx.compose.material.icons.automirrored.rounded.RotateRight
import androidx.compose.material.icons.rounded.Speed
import androidx.compose.material.icons.rounded.FormatBold
import androidx.compose.material.icons.rounded.SwapHoriz
import androidx.compose.material.icons.rounded.SwapVert
import androidx.compose.material.icons.rounded.Tune
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.InputChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.FontStore
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.layerDisplayName
import com.kerneldroid.rumo.ui.KeyframeUi
import com.kerneldroid.rumo.ui.LayerKindUi
import com.kerneldroid.rumo.ui.LayerUi
import com.kerneldroid.rumo.ui.formatTime
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.hapticLongPress
import com.kerneldroid.rumo.ui.theme.monoNumerals
import kotlin.math.roundToInt

/**
 * The named weights, for the chips above the slider.
 *
 * The engine asks the font database for the weight and thickens the glyph itself
 * when the family has no such face, so every one of these is a real choice even
 * in a monospace family that ships only Regular — which is the common case on
 * Android.
 *
 * The slider below them covers 1..1000 rather than these nine: a variable font
 * declares its own range (Google Sans Flex runs 1..1000), and offering only the
 * named steps would make most of that axis unreachable.
 */
private val TextWeightSteps = listOf(
    R.string.panel_weight_thin to 100,
    R.string.panel_weight_extra_light to 200,
    R.string.panel_weight_light to 300,
    R.string.panel_weight_regular to 400,
    R.string.panel_weight_medium to 500,
    R.string.panel_weight_semi_bold to 600,
    R.string.panel_weight_bold to 700,
    R.string.panel_weight_extra_bold to 800,
    R.string.panel_weight_black to 900,
)

/** The name of a weight, or its number when it sits between the named ones. */
@Composable
private fun weightLabel(weight: Int): String {
    val labelRes = TextWeightSteps.firstOrNull { it.second == weight }?.first
    return if (labelRes != null) stringResource(labelRes) else weight.toString()
}

/**
 * Font selection for a text layer: the built-in face plus everything downloaded
 * in the shop.
 *
 * Through a dialog, not a row of chips: there can be any number of installed
 * fonts, and a horizontal strip of chips on a phone-width panel would turn into
 * scrolling with no landmark. A dialog gives a list with the names in full.
 *
 * The list is read from disk on open rather than held in the panel's state: it
 * changes only when something is installed in the shop, while the panel is
 * rebuilt on every layer selection — a cache here would be a cache that has to
 * be invalidated.
 */
@Composable
private fun TextFontPicker(
    family: String,
    enabled: Boolean,
    onPick: (String) -> Unit,
) {
    val context = LocalContext.current
    var open by remember { mutableStateOf(false) }
    val installed = remember(open) {
        if (open) FontStore.installed(context) else emptyList()
    }

    SectionLabel(stringResource(R.string.panel_inspector_font))
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        InputChip(
            selected = false,
            onClick = { if (enabled) open = true },
            enabled = enabled,
            label = {
                Text(
                    text = family.ifEmpty { stringResource(R.string.panel_font_builtin) },
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            },
        )
        if (family.isNotEmpty()) {
            IconButton(onClick = { if (enabled) onPick("") }, enabled = enabled) {
                Icon(
                    Icons.Rounded.Close,
                    contentDescription = stringResource(R.string.panel_inspector_back_builtin),
                    modifier = Modifier.size(16.dp),
                )
            }
        }
    }

    if (open) {
        AlertDialog(
            onDismissRequest = { open = false },
            title = { Text(stringResource(R.string.panel_inspector_font)) },
            text = {
                LazyColumn(modifier = Modifier.heightIn(max = 360.dp)) {
                    item(key = "builtin") {
                        FontChoiceRow(
                            name = stringResource(R.string.panel_font_builtin),
                            subtitle = stringResource(R.string.panel_fonts_always_available),
                            selected = family.isEmpty(),
                            onClick = {
                                open = false
                                onPick("")
                            },
                        )
                    }
                    if (installed.isEmpty()) {
                        item(key = "empty") {
                            Text(
                                text = stringResource(R.string.panel_inspector_no_fonts),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(8.dp),
                            )
                        }
                    }
                    items(installed, key = { it.family }) { font ->
                        FontChoiceRow(
                            name = font.displayName,
                            subtitle = if (font.family == font.displayName) {
                                font.license
                            } else {
                                "${font.family} · ${font.license}"
                            },
                            selected = family == font.family,
                            onClick = {
                                open = false
                                // What goes into the layer is the engine's name,
                                // not the list's label: it is what the font
                                // database is addressed by.
                                onPick(font.family)
                            },
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = { open = false }) { Text(stringResource(R.string.action_close)) }
            },
        )
    }
}

@Composable
private fun FontChoiceRow(
    name: String,
    subtitle: String,
    selected: Boolean,
    onClick: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(name, style = MaterialTheme.typography.bodyLarge, maxLines = 1)
            if (subtitle.isNotEmpty()) {
                Text(
                    text = subtitle,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
            }
        }
        if (selected) {
            Icon(
                Icons.Rounded.Check,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
                modifier = Modifier.size(18.dp),
            )
        }
    }
}

/**
 * The Properties page: everything about the selected layer on one surface.
 *
 * Layout follows docs/10 §7.5, with one correction the document's own arithmetic
 * forces. The page is master + detail, and the two halves that hold the working
 * loop have their own scroll regions:
 *
 *  - a compact transform/appearance block, bounded and scrollable only when its
 *    content does not fit (the §7.5 figure draws 40dp transform rows because it
 *    assumes offsets are dragged on the preview; the preview has no offset drag
 *    yet, so the rows keep a slider and are one 48dp line instead);
 *  - the effect chain in its own 132dp window, three 44dp rows;
 *  - the parameters of the selected effect, which take what is left and are
 *    never the last item of the chain's list (§3 D2).
 *
 * The master block and the parameters share the leftover height with a weight
 * each: §7.6 shows that transform, appearance, chain and parameters cannot all
 * be unbounded at once, so neither half may claim the other's space. The
 * master's weight is `fill = false`, so a short master (a MEDIA or AUDIO layer)
 * does not push the parameters around.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun InspectorPanel(
    state: EditorState,
    layer: LayerUi?,
    playheadMs: Long,
    locked: Boolean,
    onOpenNodeGraph: () -> Unit,
    modifier: Modifier = Modifier,
) {
    if (layer == null) {
        Column(modifier = modifier.fillMaxSize()) {
            PanelHeader(title = stringResource(R.string.panel_properties))
            PanelEmpty(
                icon = Icons.Rounded.Tune,
                title = stringResource(R.string.panel_inspector_empty),
                hint = stringResource(R.string.panel_inspector_empty_hint),
            )
        }
        return
    }

    val haptic = LocalHapticFeedback.current
    val catalogue by state.effectCatalogueFlow.collectAsState()
    val chain = layer.effects
    val supportsColour = layer.kind == LayerKindUi.SHAPE || layer.kind == LayerKindUi.TEXT
    // An SVG layer is a SHAPE, but colour does not belong to it: the engine draws
    // the document with its own paints and ignores the layer's `argb`
    // (`ShapeSpec.svg_id`). The colour swatch changed nothing here — so it has to
    // be removed rather than left as a control that lies (the `argb` field stays
    // in the model).
    //
    // The transform (offset, rotation, keys) stays for SVG: it has geometry, and
    // `supportsColour` above opens that up too, so we switch off only the swatch,
    // not the whole section.
    val showsColour = supportsColour && !state.isSvgLayer(layer)
    // The panel shows what the layer has, and nothing beyond that.
    //
    // Previously every section was drawn for every layer: a shape had a "Text"
    // section with a text field, an audio layer had "Transform" with offset and
    // scale. That is exactly the "extra": half the panel taken up by controls the
    // selected object does not have. `visual` is the only boundary needed here:
    // audio has no geometry, no colour and no engine effects.
    val visual = layer.kind != LayerKindUi.AUDIO

    var pickerOpen by remember(layer.id) { mutableStateOf(false) }
    // The chain selects the effect whose parameters the detail region shows. The
    // id is remembered across configuration changes; a stale id (the effect was
    // deleted) falls back to the first row rather than showing nothing.
    var selectedEffectId by rememberSaveable(layer.id) { mutableStateOf<String?>(null) }
    val selectedEffect = chain.firstOrNull { it.id == selectedEffectId } ?: chain.firstOrNull()

    Column(modifier = modifier.fillMaxSize()) {
        PanelHeader(
            title = stringResource(R.string.panel_properties),
            subtitle = if (locked) {
                stringResource(R.string.panel_inspector_locked, layerDisplayName(layer))
            } else {
                layerDisplayName(layer)
            },
            actions = {
                IconButton(onClick = onOpenNodeGraph, modifier = Modifier.size(28.dp)) {
                    Icon(
                        Icons.Rounded.AccountTree,
                        contentDescription = stringResource(R.string.panel_inspector_show_pipeline),
                        modifier = Modifier.size(18.dp),
                    )
                }
            },
        )

        // Sections of one column, not tabs inside a tab.
        //
        // There used to be a row of chips `Transform · Text · Appearance ·
        // Effects` under the panel's header — a second level of navigation under
        // the first. Material says this plainly: "the navigation rail should be
        // the only visible navigation element". The level stayed single (the
        // surface rail), and everything below it is sections of one scrolling
        // column: you can see what else the layer has, and you do not have to
        // remember which tab the thing you need is behind.
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = RumoSpacing.m),
        ) {

            if (layer.kind == LayerKindUi.TEXT) {
                EditorSection(stringResource(R.string.panel_inspector_section_text)) {
    Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
                // The section header has already said "Text": the inner label
                // repeated it word for word and spent a line in a panel that is
                // cramped as it is.
                OutlinedTextField(
                    value = layer.text,
                    onValueChange = { state.setText(layer.id, it) },
                    label = { Text(stringResource(R.string.panel_inspector_layer_text)) },
                    modifier = Modifier.fillMaxWidth(),
                )

                // Weight is a face the family may or may not have. Three steps,
                // because that is what the faces on a phone are; a layer holding
                // some other weight — the assistant can ask for 900 — shows the
                // number instead of pretending to be one of these three.
                TextFontPicker(
                    family = layer.textFamily,
                    enabled = !locked,
                    onPick = {
                        haptic.hapticConfirm()
                        state.setTextFamily(layer.id, it)
                    },
                )

                SectionLabel(stringResource(R.string.panel_inspector_weight))
                // The named steps go in a scrolling row, not a wrap: nine chips
                // wrapped give two or three rows of different lengths, and the
                // panel jerks on selection. The same device as the shop's filters.
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    TextWeightSteps.forEach { (labelRes, weight) ->
                        FilterChip(
                            selected = layer.textWeight == weight,
                            onClick = {
                                haptic.hapticConfirm()
                                state.setTextWeight(layer.id, weight)
                            },
                            enabled = !locked,
                            label = { Text(stringResource(labelRes)) },
                        )
                    }
                }
                // And the full range beneath them: a variable font has its own
                // weight axis (Google Sans Flex is 1..1000), and the steps do not
                // cover it. The engine rounds to a whole number, hence a step of 1.
                // Read before the row, not inside the template: the value carries
                // the weight's name and the name is a resource.
                val weightName = weightLabel(layer.textWeight)
                CompactSliderRow(
                    label = stringResource(R.string.panel_inspector_axis),
                    icon = Icons.Rounded.FormatBold,
                    value = layer.textWeight
                        .coerceIn(EditorState.MIN_TEXT_WEIGHT, EditorState.MAX_TEXT_WEIGHT)
                        .toFloat(),
                    valueRange = EditorState.MIN_TEXT_WEIGHT.toFloat()..
                        EditorState.MAX_TEXT_WEIGHT.toFloat(),
                    valueText = "$weightName · ${layer.textWeight}",
                    enabled = !locked,
                    onValueChange = { state.setTextWeight(layer.id, it.roundToInt()) },
                    onValueChangeFinished = { state.endGesture() },
                    onReset = { state.setTextWeight(layer.id, 400) },
                )

                // The outline is drawn behind the glyphs, so thickness is the
                // whole control: at zero there is no contour and the colour row
                // is not worth showing.
                SectionLabel(stringResource(R.string.panel_inspector_section_outline))
                CompactSliderRow(
                    label = stringResource(R.string.panel_inspector_thickness),
                    icon = Icons.Rounded.BorderOuter,
                    value = layer.strokePx.coerceIn(0f, EditorState.MAX_STROKE_PX),
                    valueRange = 0f..EditorState.MAX_STROKE_PX,
                    valueText = if (layer.strokePx <= 0f) {
                        stringResource(R.string.panel_off)
                    } else {
                        "${layer.strokePx.roundToInt()} px"
                    },
                    enabled = !locked,
                    onValueChange = { state.setStrokePx(layer.id, it) },
                    onValueChangeFinished = { state.endGesture() },
                )
                var strokePickerOpen by remember(layer.id) { mutableStateOf(false) }
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = MinTouchTarget),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                ) {
                    Text(
                        text = stringResource(R.string.panel_inspector_outline_colour),
                        style = MaterialTheme.typography.labelLarge,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    Text(
                        text = "#%06X".format(layer.strokeArgb and 0xFFFFFFL),
                        style = MaterialTheme.typography.labelMedium.merge(monoNumerals),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    ColourSwatch(
                        colour = Color(layer.strokeArgb.toInt()),
                        enabled = !locked,
                        description = stringResource(R.string.panel_inspector_choose_outline_colour),
                        onClick = { strokePickerOpen = true },
                    )
                }
                if (strokePickerOpen) {
                    ColorPickerDialog(
                        title = stringResource(R.string.panel_inspector_outline_colour),
                        initial = Color(layer.strokeArgb.toInt()),
                        onColor = {
                            state.setStrokeArgb(layer.id, it.toArgb().toLong() and 0xFFFFFFFFL)
                        },
                        onDismiss = { strokePickerOpen = false },
                    )
                }
            }
                }
            }
            if (visual) {
                EditorSection(stringResource(R.string.panel_inspector_section_transform)) {
    Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
            if (supportsColour) {
                TransformRow(
                    label = stringResource(R.string.panel_inspector_offset_x),
                    icon = Icons.Rounded.SwapHoriz,
                    value = layer.offsetX.coerceIn(-400f, 400f),
                    valueRange = -400f..400f,
                    valueText = "${layer.offsetX.toInt()} px",
                    enabled = !locked,
                    onValueChange = { state.setOffset(layer.id, it, layer.offsetY) },
                    onValueChangeFinished = { state.endGesture() },
                    onReset = { state.setOffset(layer.id, 0f, layer.offsetY) },
                    onCommit = { state.setOffset(layer.id, it, layer.offsetY) },
                )
                TransformRow(
                    label = stringResource(R.string.panel_inspector_offset_y),
                    icon = Icons.Rounded.SwapVert,
                    value = layer.offsetY.coerceIn(-400f, 400f),
                    valueRange = -400f..400f,
                    valueText = "${layer.offsetY.toInt()} px",
                    enabled = !locked,
                    onValueChange = { state.setOffset(layer.id, layer.offsetX, it) },
                    onValueChangeFinished = { state.endGesture() },
                    onReset = { state.setOffset(layer.id, layer.offsetX, 0f) },
                    onCommit = { state.setOffset(layer.id, layer.offsetX, it) },
                )

                // The engine has exactly one keyframe track, and it is rotation
                // (docs/10 §4.4). The track's editor lives here, once: the
                // slider writes a key at the playhead on release, and the chips
                // below name the keys that exist.
                val rotation = state.rotationAt(layer, playheadMs)
                var keyDrag by remember(layer.id, playheadMs) { mutableStateOf<Float?>(null) }
                val shown = (keyDrag ?: rotation).coerceIn(-180f, 180f)
                TransformRow(
                    // The moment the key lands on is the playhead; naming it in
                    // the label would cost more width than the row has.
                    label = stringResource(R.string.panel_inspector_rotation),
                    icon = Icons.AutoMirrored.Rounded.RotateRight,
                    value = shown,
                    valueRange = -180f..180f,
                    valueText = "${shown.toInt()}°",
                    enabled = !locked,
                    onValueChange = { keyDrag = it },
                    onValueChangeFinished = {
                        keyDrag?.let {
                            state.addKeyframe(layer.id, it)
                            haptic.hapticConfirm()
                        }
                        keyDrag = null
                    },
                    onReset = { state.addKeyframe(layer.id, 0f) },
                    onCommit = { state.addKeyframe(layer.id, it) },
                )
                KeyframeRow(
                    keys = layer.keys,
                    playheadMs = playheadMs,
                    enabled = !locked,
                    onSeek = { state.seekTo(it) },
                    onRemove = { state.removeKeyframe(layer.id, it) },
                    onAdd = { state.addKeyframe(layer.id) },
                )
            }

            }
                }
            }
            if (!visual) {
                // Audio has no opacity, no colour, no stroke — and a control that
                // changes nothing must not be here. The engine has no gain, so it
                // is more honest to say that than to show a slider.
                EditorSection(stringResource(R.string.panel_audio)) {
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                    ) {
                        MetaPill(stringResource(R.string.panel_inspector_no_gain))
                        MetaPill(
                            if (layer.uri != null) {
                                stringResource(R.string.panel_inspector_file_linked)
                            } else {
                                stringResource(R.string.panel_inspector_no_file)
                            },
                        )
                    }
                }
            }
            if (visual) {
                EditorSection(stringResource(R.string.panel_inspector_section_appearance)) {
    Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
            // See above: the section header and the label duplicated each other.
            if (layer.kind == LayerKindUi.AUDIO) {
                // No gain stage in the engine: say so instead of offering a
                // slider that would change nothing.
                Row(horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
                    MetaPill(stringResource(R.string.panel_inspector_no_gain))
                    MetaPill(
                        if (layer.uri != null) {
                            stringResource(R.string.panel_inspector_file_linked)
                        } else {
                            stringResource(R.string.panel_inspector_no_file)
                        },
                    )
                }
            } else {
                // Alpha is consumed by the engine for SHAPE/TEXT/MEDIA quads.
                CompactSliderRow(
                    label = stringResource(R.string.panel_inspector_opacity),
                    icon = Icons.Rounded.Opacity,
                    value = layer.alpha.coerceIn(0.02f, 1f),
                    valueRange = 0.02f..1f,
                    valueText = "${(layer.alpha * 100).toInt()}%",
                    enabled = !locked,
                    onValueChange = { state.setAlpha(layer.id, it) },
                    onValueChangeFinished = { state.endGesture() },
                )
            }
            if (showsColour) {
                // §7.9: one swatch row, not four sliders. The engine still
                // stores the colour as an ARGB integer for a layer.
                var colourPickerOpen by remember(layer.id) { mutableStateOf(false) }
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = MinTouchTarget),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                ) {
                    Text(
                        text = stringResource(R.string.panel_inspector_colour),
                        style = MaterialTheme.typography.labelLarge,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    Text(
                        text = "#%06X".format(layer.argb and 0xFFFFFFL),
                        style = MaterialTheme.typography.labelMedium.merge(monoNumerals),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    ColourSwatch(
                        colour = Color(layer.argb.toInt()),
                        enabled = !locked,
                        description = stringResource(R.string.panel_inspector_choose_colour),
                        onClick = { colourPickerOpen = true },
                    )
                }
                if (colourPickerOpen) {
                    ColorPickerDialog(
                        title = stringResource(R.string.panel_inspector_layer_colour),
                        initial = Color(layer.argb.toInt()),
                        onColor = {
                            state.setColor(layer.id, it.toArgb().toLong() and 0xFFFFFFFFL)
                        },
                        onDismiss = { colourPickerOpen = false },
                    )
                }
            }

            SectionLabel(stringResource(R.string.panel_inspector_section_timing))
            CompactSliderRow(
                label = stringResource(R.string.panel_inspector_clip_length),
                icon = Icons.Rounded.Speed,
                value = layer.durationMs.toFloat().coerceIn(200f, 60_000f),
                valueRange = 200f..60_000f,
                valueText = formatTime(layer.durationMs),
                enabled = !locked,
                onValueChange = { state.setDuration(layer.id, it.toLong()) },
                onValueChangeFinished = { state.endGesture() },
            )
            Text(
                text = stringResource(
                    R.string.panel_inspector_drives_length,
                    formatTime(playheadMs),
                ),
                style = MaterialTheme.typography.labelSmall.merge(monoNumerals),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
                }
            }
            if (visual) {
                EditorSection(stringResource(R.string.panel_effects)) {
    EffectSection(
                    state = state,
                    layer = layer,
                    selectedId = selectedEffect?.id,
                    onSelect = { selectedEffectId = it },
                    onRequestAdd = { pickerOpen = true },
                    enabled = !locked,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            }
            }

    }

    if (pickerOpen) {
        AddEffectSheet(
            state = state,
            layer = layer,
            catalogue = catalogue,
            onPick = { kindId ->
                // D1: the id comes back from the model. Re-deriving it by
                // diffing the chain is exactly how the panel used to end up
                // editing the first effect instead of the added one.
                val addedId = state.addEffect(layer.id, kindId)
                if (addedId != null) selectedEffectId = addedId
                haptic.hapticConfirm()
                pickerOpen = false
            },
            onDismiss = { pickerOpen = false },
        )
    }
}

/**
 * One transform line: slider, value and reset.
 *
 * The value is also a target for exact entry — a slider cannot land on `12` in a
 * −400..400 range (§7.8). The reset ruler icon is an icon-only control, so the
 * row hands it the 44dp target declared in [MinTouchTarget].
 */
@Composable
private fun TransformRow(
    label: String,
    icon: ImageVector,
    value: Float,
    valueRange: ClosedFloatingPointRange<Float>,
    valueText: String,
    enabled: Boolean,
    onValueChange: (Float) -> Unit,
    onReset: () -> Unit,
    onCommit: (Float) -> Unit,
    onValueChangeFinished: () -> Unit = {},
) {
    var inputOpen by remember { mutableStateOf(false) }
    CompactSliderRow(
        label = label,
        icon = icon,
        value = value,
        valueRange = valueRange,
        valueText = valueText,
        enabled = enabled,
        onValueChange = onValueChange,
        onValueChangeFinished = onValueChangeFinished,
        onValueClick = { inputOpen = true },
        onReset = onReset,
    )
    if (inputOpen) {
        NumberInputDialog(
            title = label,
            initial = value.roundToInt().toString(),
            onCommit = onCommit,
            onDismiss = { inputOpen = false },
        )
    }
}

/**
 * The rotation keyframe track (docs/10 §7.10).
 *
 * One keyframe track exists in the engine, on rotation only, and keys are held
 * on the timeline — so the editor names the keys instead of placing a diamond
 * next to every parameter slider. Every chip's cross is a 44dp target: at 16dp
 * it was the smallest offender of §7.13.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun KeyframeRow(
    keys: List<KeyframeUi>,
    playheadMs: Long,
    enabled: Boolean,
    onSeek: (Long) -> Unit,
    onRemove: (Long) -> Unit,
    onAdd: () -> Unit,
) {
    val haptic = LocalHapticFeedback.current
    // The chips go in one scrolling row, not wrapped across lines.
    //
    // The wrap was deliberate ("a horizontal scroller in the dock would fight the
    // page swipe"), but it has a cost: three keys took two rows of different
    // lengths, "Add key" drifted onto its own, and every new pair of keys added a
    // row to an already cramped panel. A row of fixed height keeps the panel's
    // size constant.
    //
    // The argument with the page swipe remains, but only when the row really is
    // wider than the screen: the chips are squeezed to 32dp, and three keys with
    // "Add key" fit on a phone in portrait — then there is nothing left for the
    // drag to consume, and the page-switch gesture works as before. A finger on
    // the row itself will scroll it in the overflowing case — which is what was
    // asked for.
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(KeyChipHeight)
            .horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        for (key in keys) {
            InputChip(
                selected = key.timeMs == playheadMs,
                onClick = { onSeek(key.timeMs) },
                modifier = Modifier.height(KeyChipHeight),
                shape = RoundedCornerShape(8.dp),
                enabled = enabled,
                label = {
                    Text(
                        text = "${formatTime(key.timeMs)} ${key.value.toInt()}°",
                        style = MaterialTheme.typography.labelSmall,
                    )
                },
                trailingIcon = {
                    // The icon is 16dp, but the finger target stays 44dp: M3's
                    // `IconButton` adds `minimumInteractiveComponentSize` itself,
                    // and the touch area extends beyond the visible bounds. The
                    // picture shrinks, not the target — §7.13 is about the target.
                    IconButton(
                        onClick = {
                            onRemove(key.timeMs)
                            haptic.hapticLongPress()
                        },
                        enabled = enabled,
                        modifier = Modifier.size(24.dp),
                    ) {
                        Icon(
                            imageVector = Icons.Rounded.Close,
                            contentDescription = stringResource(R.string.panel_inspector_remove_key),
                            modifier = Modifier.size(14.dp),
                        )
                    }
                },
            )
        }
        InputChip(
            selected = false,
            onClick = {
                onAdd()
                haptic.hapticConfirm()
            },
            modifier = Modifier.height(KeyChipHeight),
            shape = RoundedCornerShape(8.dp),
            enabled = enabled,
            label = {
                Text(
                    stringResource(R.string.panel_inspector_add_key),
                    style = MaterialTheme.typography.labelSmall,
                )
            },
            leadingIcon = {
                Icon(
                    imageVector = Icons.Rounded.Add,
                    contentDescription = null,
                    modifier = Modifier.size(16.dp),
                )
            },
        )
    }
}

/** Key chip height: shorter than a property row, because there can be many of them. */
private val KeyChipHeight = 32.dp
