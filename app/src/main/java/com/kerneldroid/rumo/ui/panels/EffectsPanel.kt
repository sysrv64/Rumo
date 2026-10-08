// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.ArrowDownward
import androidx.compose.material.icons.rounded.ArrowUpward
import androidx.compose.material.icons.rounded.AutoFixHigh
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.DragHandle
import androidx.compose.material.icons.rounded.RestartAlt
import androidx.compose.material.icons.rounded.Visibility
import androidx.compose.material.icons.rounded.VisibilityOff
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material.icons.rounded.MoreVert
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconToggleButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.util.Locale
import androidx.compose.ui.zIndex
import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material.icons.rounded.Search
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.EffectStore
import com.kerneldroid.rumo.ui.defaultEffectFor
import java.nio.IntBuffer
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.Effect
import com.kerneldroid.rumo.ui.LayerUi
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.hapticLongPress
import com.kerneldroid.rumo.ui.theme.hapticToggle
import com.kerneldroid.rumo.ui.theme.monoNumerals
import kotlin.math.roundToInt

/**
 * The minimum touch target for a control that has no neighbour text label
 * (docs/10 §7.13):
 *
 *   An element with no large neighbouring label may not be smaller than 44dp.
 *
 * Google asks for 48dp; 44 is the deliberate trade the document records, and it
 * is declared once here instead of being re-decided per control. The rule
 * exempts an element that has a label: a chip carrying its own text, a slider
 * row whose value sits beside it, or a panel-header glyph on the same 28dp line
 * as the page title. An icon-only target — the chain's delete, move, toggle and
 * add, the keyframe cross, the colour swatch, a parameter reset — never goes
 * below this.
 */
internal val MinTouchTarget = 44.dp

/** Chain row height. */
private val EffectRowHeight = MinTouchTarget

/**
 * The chain's own bounded scroll window: three rows. It does not share a scroll
 * with the parameters, which is the whole point of the master-detail split —
 * when master and detail share one scroll, detail always loses (docs/10 §3 D2).
 */
private val EffectChainWindow = EffectRowHeight * 3

/** Swatches offered by the colour picker; the hue slider covers everything between. */
private val ColorPickerSwatches: List<Long> = listOf(
    0xFFFFFFFF,
    0xFF000000,
    0xFF9E9E9E,
    0xFFF44336,
    0xFFFF9800,
    0xFFFFEB3B,
    0xFF4CAF50,
    0xFF00BCD4,
    0xFF2196F3,
    0xFF9C27B0,
)

/**
 * The engine's effect names, in the interface's language.
 *
 * The engine answers with English `&'static str` for every name, parameter label
 * and choice, and those strings are also the identity: an id addresses a built-in
 * kind, a key addresses a parameter, and a choice is sent back as its index.
 * Translating them where they are declared would change what the engine sees, so
 * the mapping from identity to displayed text lives here, at the display site,
 * and the engine's own string remains the fallback — an effect, parameter or
 * choice this table does not know is still shown, in English, rather than as a
 * blank or a raw id.
 *
 * `values/` keeps every string identical to the engine's own, so English reads
 * exactly as it did before; only `values-ru/` and `values-zh/` differ.
 */
internal val EffectNameRes: Map<String, Int> = mapOf(
    "blur" to R.string.effect_name_blur,
    "color_tune" to R.string.effect_name_color_tune,
    "threshold" to R.string.effect_name_threshold,
    "chroma_key" to R.string.effect_name_chroma_key,
    "copy_background" to R.string.effect_name_copy_background,
    "pixelate" to R.string.effect_name_pixelate,
    "sphere360" to R.string.effect_name_sphere360,
    "drop_shadow" to R.string.effect_name_drop_shadow,
    "inner_shadow" to R.string.effect_name_inner_shadow,
    "glow" to R.string.effect_name_glow,
)

/**
 * The engine's parameter labels, keyed by **(effect id, parameter key)**.
 *
 * Not by key alone, and that is the point: the engine reuses a key with a
 * different label in different effects — `color` is "Colour" under
 * `copy_background` and "Tint" under `glow`, `mode` labels different sets of
 * choices, and `radius` is a pixel radius in one effect and a magnification in
 * another. A key-only table would show one effect's wording under another's
 * control. The table is a transcription of `rumo-core/src/effect.rs`, so an
 * effect added there is one line here plus three resource files.
 */
private val EffectParamRes: Map<Pair<String, String>, Int> = mapOf(
    ("blur" to "mode") to R.string.effect_param_blur_mode,
    ("blur" to "radius") to R.string.effect_param_blur_radius,
    ("blur" to "angle") to R.string.effect_param_blur_angle,
    ("blur" to "downscale") to R.string.effect_param_blur_downscale,
    ("color_tune" to "hue") to R.string.effect_param_color_tune_hue,
    ("color_tune" to "chroma") to R.string.effect_param_color_tune_chroma,
    ("color_tune" to "lightness") to R.string.effect_param_color_tune_lightness,
    ("color_tune" to "brightness") to R.string.effect_param_color_tune_brightness,
    ("color_tune" to "contrast") to R.string.effect_param_color_tune_contrast,
    ("color_tune" to "saturation") to R.string.effect_param_color_tune_saturation,
    ("threshold" to "level") to R.string.effect_param_threshold_level,
    ("threshold" to "softness") to R.string.effect_param_threshold_softness,
    ("threshold" to "mode") to R.string.effect_param_threshold_mode,
    ("chroma_key" to "key") to R.string.effect_param_chroma_key_key,
    ("chroma_key" to "similarity") to R.string.effect_param_chroma_key_similarity,
    ("chroma_key" to "softness") to R.string.effect_param_chroma_key_softness,
    ("chroma_key" to "spill") to R.string.effect_param_chroma_key_spill,
    ("copy_background" to "mode") to R.string.effect_param_copy_background_mode,
    ("copy_background" to "color") to R.string.effect_param_copy_background_color,
    ("copy_background" to "tolerance") to R.string.effect_param_copy_background_tolerance,
    ("copy_background" to "feather") to R.string.effect_param_copy_background_feather,
    ("pixelate" to "size") to R.string.effect_param_pixelate_size,
    ("pixelate" to "shape") to R.string.effect_param_pixelate_shape,
    ("sphere360" to "radius") to R.string.effect_param_sphere360_radius,
    ("sphere360" to "yaw") to R.string.effect_param_sphere360_yaw,
    ("sphere360" to "pitch") to R.string.effect_param_sphere360_pitch,
    ("sphere360" to "fov") to R.string.effect_param_sphere360_fov,
    ("drop_shadow" to "offset_x") to R.string.effect_param_drop_shadow_offset_x,
    ("drop_shadow" to "offset_y") to R.string.effect_param_drop_shadow_offset_y,
    ("drop_shadow" to "blur") to R.string.effect_param_drop_shadow_blur,
    ("drop_shadow" to "spread") to R.string.effect_param_drop_shadow_spread,
    ("drop_shadow" to "color") to R.string.effect_param_drop_shadow_color,
    ("drop_shadow" to "opacity") to R.string.effect_param_drop_shadow_opacity,
    ("inner_shadow" to "offset_x") to R.string.effect_param_inner_shadow_offset_x,
    ("inner_shadow" to "offset_y") to R.string.effect_param_inner_shadow_offset_y,
    ("inner_shadow" to "blur") to R.string.effect_param_inner_shadow_blur,
    ("inner_shadow" to "spread") to R.string.effect_param_inner_shadow_spread,
    ("inner_shadow" to "color") to R.string.effect_param_inner_shadow_color,
    ("inner_shadow" to "opacity") to R.string.effect_param_inner_shadow_opacity,
    ("glow" to "threshold") to R.string.effect_param_glow_threshold,
    ("glow" to "radius") to R.string.effect_param_glow_radius,
    ("glow" to "intensity") to R.string.effect_param_glow_intensity,
    ("glow" to "color") to R.string.effect_param_glow_color,
    ("glow" to "tint") to R.string.effect_param_glow_tint,
)

/**
 * The choice values a `Choice` parameter offers, keyed by the engine's own value
 * — "Gaussian", "Square", "Mask".
 *
 * Keyed by the value rather than by (effect, key) because a choice is a value and
 * not a slot: the same word means the same chip in any list it appears in. It is
 * also why a missed lookup is safe — the chip keeps showing the engine's string,
 * so a choice an effect grows still renders and still sends its index back.
 */
private val EffectChoiceRes: Map<String, Int> = mapOf(
    "Gaussian" to R.string.effect_choice_gaussian,
    "Box" to R.string.effect_choice_box,
    "Directional" to R.string.effect_choice_directional,
    "Mask" to R.string.effect_choice_mask,
    "Luma" to R.string.effect_choice_luma,
    "Alpha" to R.string.effect_choice_alpha,
    "Chroma" to R.string.effect_choice_chroma,
    "Black" to R.string.effect_choice_black,
    "White" to R.string.effect_choice_white,
    "Color" to R.string.effect_choice_color,
    "Square" to R.string.effect_choice_square,
    "Hex" to R.string.effect_choice_hex,
)

/**
 * The effect name to show for an id, or the engine's own label when the id has no
 * translation — a project effect, or a built-in added after this table was
 * written. `stringResource` is what makes the language switch reach it: a plain
 * `descriptor.label` would stay English under any locale.
 */
@Composable
internal fun effectDisplayName(id: String, engineLabel: String): String {
    val res = EffectNameRes[id]
    return if (res != null) stringResource(res) else engineLabel
}

/** The parameter label to show, or the engine's own label when nothing matches. */
@Composable
private fun paramDisplayLabel(effectId: String, param: RumoBridge.EffectParam): String {
    val res = EffectParamRes[effectId to param.key]
    return if (res != null) stringResource(res) else param.label
}

/** One choice chip's text, or the engine's own value when nothing matches. */
@Composable
private fun choiceDisplayLabel(choice: String): String {
    val res = EffectChoiceRes[choice]
    return if (res != null) stringResource(res) else choice
}

/**
 * One line of the chain, in application order.
 *
 * Three explicit verbs and one hidden-but-kept gesture: the toggle is a visible
 * 44dp control (docs/10 §7.7 — the double tap that used to own it is not
 * discoverable), the arrows are the visible counterpart of the long-press drag,
 * and the delete stays where it was. The row is 44dp rather than the old 56dp so
 * three of them fit the bounded window above the parameters.
 *
 * This does not reuse `SelectedRow`: that row's 8dp vertical padding would make
 * a 44dp action row 60dp tall and only two rows would fit the 132dp window.
 */
@Composable
private fun EffectChainRow(
    effect: Effect,
    descriptor: RumoBridge.EffectDescriptor?,
    selected: Boolean,
    dragging: Boolean,
    dragOffset: Float,
    onSelect: () -> Unit,
    onToggle: () -> Unit,
    onDelete: () -> Unit,
    dragModifier: Modifier,
) {
    val normal = MaterialTheme.colorScheme.onSurface
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    var menuOpen by remember { mutableStateOf(false) }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(EffectRowHeight)
            .zIndex(if (dragging) 1f else 0f)
            .graphicsLayer {
                if (dragging) {
                    translationY = dragOffset
                    scaleX = 1.01f
                    scaleY = 1.01f
                }
            }
            .clip(RoundedCornerShape(12.dp))
            .background(
                if (selected) {
                    MaterialTheme.colorScheme.secondaryContainer
                } else {
                    MaterialTheme.colorScheme.surfaceContainerHigh
                },
            )
            .then(
                if (selected) {
                    Modifier.border(1.dp, MaterialTheme.colorScheme.primary, RoundedCornerShape(12.dp))
                } else {
                    Modifier
                },
            )
            .clickable(onClick = onSelect)
            .padding(horizontal = RumoSpacing.xs),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
    ) {
        // The handle **is** the drag target, and it drags immediately.
        //
        // It used to be the other way round: the handle was just an icon, and you
        // had to drag the whole row with a long press. In the properties panel a
        // row lives inside a scrollable column, and the long press lost to the
        // scroll — the panel scrolled instead of reordering. Now the gesture is
        // attached to the handle and starts on the first movement, so there is
        // nobody to compete with, and it is also honest: the handle exists exactly
        // so that you pull it.
        Box(
            modifier = Modifier
                .size(MinTouchTarget)
                .then(dragModifier),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = Icons.Rounded.DragHandle,
                contentDescription = stringResource(R.string.panel_effects_reorder),
                tint = muted,
                modifier = Modifier.size(16.dp),
            )
        }
        Column(modifier = Modifier.weight(1f)) {
            Text(
                // Unknown kind: show its real id, never hide it.
                text = descriptor?.let { effectDisplayName(it.id, it.label) } ?: effect.kindId,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                color = if (effect.enabled) normal else muted,
                // §7.7: a disabled effect stays in the chain, struck through.
                textDecoration = if (effect.enabled) null else TextDecoration.LineThrough,
            )
            if (descriptor == null) {
                MetaPill(
                    stringResource(R.string.panel_effects_unavailable),
                    tint = MaterialTheme.colorScheme.error,
                )
            }
        }
        // One menu instead of three buttons.
        //
        // It used to be `eye · up · down · bin` — four 44dp targets in a row, two
        // of which duplicated the drag (the row was already reordered by long
        // press and drag), while the bin stood as the most reachable button in the
        // row, that is, the most frequent miss at the most destructive action. Now
        // the row has one control — the three dots — and inside it exactly two
        // verbs: show/hide and delete.
        //
        // Reordering stayed where it was: drag-to-drop.
        Box {
            IconButton(
                onClick = { menuOpen = true },
                modifier = Modifier.size(MinTouchTarget),
            ) {
                Icon(
                    imageVector = Icons.Rounded.MoreVert,
                    contentDescription = stringResource(R.string.panel_effects_actions),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(18.dp),
                )
            }
            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                DropdownMenuItem(
                    text = {
                        Text(
                            stringResource(
                                if (effect.enabled) R.string.panel_hide else R.string.panel_show,
                            ),
                        )
                    },
                    leadingIcon = {
                        Icon(
                            imageVector = if (effect.enabled) {
                                Icons.Rounded.VisibilityOff
                            } else {
                                Icons.Rounded.Visibility
                            },
                            contentDescription = null,
                            modifier = Modifier.size(18.dp),
                        )
                    },
                    onClick = {
                        menuOpen = false
                        onToggle()
                    },
                )
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.panel_delete)) },
                    leadingIcon = {
                        Icon(
                            imageVector = Icons.Rounded.Delete,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.error,
                            modifier = Modifier.size(18.dp),
                        )
                    },
                    onClick = {
                        menuOpen = false
                        onDelete()
                    },
                )
            }
        }
    }
}

/**
 * The effect chain of the selected layer, in its own bounded scroll window.
 *
 * The window is [EffectChainWindow] tall and nothing else shares its scroll: the
 * parameters below it keep their own region, which is what stops a long chain
 * from pushing the values off screen (docs/10 §3 D2, §7.5).
 *
 * The header carries the single "add effect" affordance (§7.14: there used to be
 * a header icon and a full-width tile, two buttons for one verb).
 */
@Composable
fun EffectSection(
    state: EditorState,
    layer: LayerUi,
    selectedId: String?,
    onSelect: (String) -> Unit,
    onRequestAdd: () -> Unit,
    enabled: Boolean,
    modifier: Modifier = Modifier,
) {
    // A subscription, not a getter read: otherwise an effect installed from the
    // shop would appear in the menu only after an app restart.
    val catalogue by state.effectCatalogueFlow.collectAsState()
    val chain = layer.effects
    val haptic = LocalHapticFeedback.current

    // The reorder threshold is the row step **together with the list gap**: a row
    // is 44dp plus 4dp between rows. Without the gap the threshold would fire 4dp
    // before the row reached its neighbour.
    val reorder = rememberReorderState()
    val rowStep = with(LocalDensity.current) { (EffectRowHeight + RumoSpacing.xs).toPx() }

    Column(modifier = modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(MinTouchTarget),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            SectionLabel(stringResource(R.string.panel_effects_chain), modifier = Modifier.weight(1f))
            IconButton(
                onClick = onRequestAdd,
                enabled = catalogue.isNotEmpty(),
                modifier = Modifier.size(MinTouchTarget),
            ) {
                Icon(Icons.Rounded.Add, contentDescription = stringResource(R.string.panel_effects_add))
            }
        }
        // A plain column, not a `LazyColumn`.
        //
        // This was a real breakage, not style: the panel sections live in one
        // scrollable column, and a nested scrollable list inside it is forbidden —
        // it is measured with infinite height and is not drawn at all. The symptom
        // was exactly what the user saw: you add an effect, and it is not in the
        // list. The effect chain is a few rows, laziness buys nothing here, and it
        // breaks the prohibition.
        Column(
            modifier = Modifier.fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
        ) {
            if (catalogue.isEmpty()) {
                MetaPill(stringResource(R.string.panel_effects_no_catalogue))
            }
            if (chain.isEmpty()) {
                PanelEmpty(
                    icon = Icons.Rounded.AutoFixHigh,
                    title = stringResource(R.string.panel_effects_empty),
                    hint = stringResource(R.string.panel_effects_empty_hint),
                )
            }
            for (effect in chain) {
                // `key` — and this is not decoration, it is the reason reordering
                // can work at all.
                //
                // In a `LazyColumn` the elements have a `key`, so the dragged row
                // keeps its identity when the list is reordered: the gesture
                // continues, the offset is applied to the same row. Here the
                // column is plain, and without `key` Compose reuses slots **by
                // position**: on the first reorder the composable owning the
                // gesture ends up bound to another effect, the `pointerInput` key
                // changes, and the gesture is cancelled — the row freezes exactly
                // at the moment it was moved. The symptom that was visible:
                // "slightly down — and it hung".
                key(effect.id) {
                EffectChainRow(
                    effect = effect,
                    // An unknown kindId stays an "unavailable" row — the model
                    // does not lose it, it just has no parameters.
                    descriptor = catalogue.firstOrNull { it.id == effect.kindId },
                    selected = effect.id == selectedId,
                    dragging = reorder.isDragging(effect.id),
                    dragOffset = reorder.offsetOf(effect.id),
                    onSelect = { onSelect(effect.id) },
                    onToggle = {
                        state.toggleEffect(layer.id, effect.id)
                        haptic.hapticToggle(!effect.enabled)
                    },
                    onDelete = {
                        state.removeEffect(layer.id, effect.id)
                        haptic.hapticLongPress()
                    },
                    dragModifier = Modifier.reorderHandle(
                        state = reorder,
                        key = effect.id,
                        rowStepPx = rowStep,
                        onReorder = { delta ->
                            state.moveEffect(layer.id, effect.id, delta)
                            haptic.hapticToggle(delta < 0)
                        },
                        onStart = { haptic.hapticLongPress() },
                        onEnd = { haptic.hapticConfirm() },
                    ),
                )
                if (effect.id == selectedId) {
                    EffectParamsInline(
                        state = state,
                        layer = layer,
                        effect = effect,
                        descriptor = catalogue.firstOrNull { it.id == effect.kindId },
                        enabled = enabled,
                    )
                }
                }
            }
        }
    }
}

/**
 * The parameters of one effect, right under its own row.
 *
 * Inline rather than in a region of its own. The dock is one surface, and two
 * regions sharing its height give each half less room than it needs; the row
 * above already says which effect these belong to, so there is no header here
 * either.
 */
@Composable
private fun EffectParamsInline(
    state: EditorState,
    layer: LayerUi,
    effect: Effect,
    descriptor: RumoBridge.EffectDescriptor?,
    enabled: Boolean,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = RumoSpacing.s, bottom = RumoSpacing.s),
        verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
    ) {
        when {
            descriptor == null -> MetaPill(
                text = stringResource(R.string.panel_effects_unknown),
                tint = MaterialTheme.colorScheme.error,
            )
            descriptor.params.isEmpty() -> MetaPill(stringResource(R.string.panel_effects_no_params))
            else -> for (param in descriptor.params) {
                EffectParamEditor(
                    param = param,
                    effect = effect,
                    enabled = enabled,
                    onComponent = { component, value ->
                        state.setEffectParamValue(
                            layerId = layer.id,
                            effectId = effect.id,
                            key = param.key,
                            component = component,
                            value = value,
                        )
                    },
                )
            }
        }
    }
}

/**
 * Catalogue picker: every entry, its id and its cost classification.
 *
 * Kept for the node sheet, which lists the chain too (NodesPanel).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AddEffectSheet(
    state: EditorState,
    layer: LayerUi?,
    catalogue: List<RumoBridge.EffectDescriptor>,
    onPick: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val layers by state.layers.collectAsState()
    val projectEffects by state.customEffects.collectAsState()
    val installed by state.installedEffectsFlow.collectAsState()

    var query by remember { mutableStateOf("") }
    val previews = remember(state) { EffectPreviewLoader(state, scope) }

    // The layer for the preview is the selected one if it is visible, otherwise
    // the last visible one. The preview must be a picture of what the effect does
    // to the real layer, not to an invented shape: a layer has colour, shape and
    // opacity, and the effect is read exactly on those.
    val previewLayer = remember(layers, layer?.id) {
        layers.firstOrNull { it.id == layer?.id && it.visible }
            ?: layers.lastOrNull { it.visible }
    }
    val canvasW = state.canvasWidth.collectAsState().value
    val canvasH = state.canvasHeight.collectAsState().value
    val timeMs = state.playheadMs.collectAsState().value
    // The preview width is fixed, the height follows the canvas proportion so
    // that the frame is not stretched: an effect that judges geometry lies on a
    // stretched frame.
    val previewW = 160
    val previewH = ((previewW.toLong() * canvasH) / canvasW.coerceAtLeast(1)).toInt().coerceIn(48, 240)

    val installedIds = remember(installed) { installed.map { it.id }.toSet() }
    val projectIds = remember(projectEffects) { projectEffects.map { it.id }.toSet() }

    // The names the rows actually show, resolved before the filter: `stringResource`
    // is a composable read and cannot run inside `remember`.
    val displayNames = catalogue.map { it.id to effectDisplayName(it.id, it.label) }.toMap()
    val shown = remember(catalogue, displayNames, query) {
        val q = query.trim().lowercase()
        if (q.isEmpty()) {
            catalogue
        } else {
            // Matches the name the user can see, plus the id, which the row prints
            // under it. The engine's English label is deliberately not searched: on
            // a Russian or Chinese screen it is a word nobody can read, and keeping
            // it would only help a user who typed a name that is not displayed.
            catalogue.filter {
                displayNames[it.id].orEmpty().lowercase().contains(q) ||
                    it.id.lowercase().contains(q)
            }
        }
    }

    fun deleteEffect(id: String) {
        if (id in installedIds) {
            scope.launch {
                val entries = withContext(Dispatchers.IO) { EffectStore.installed(context) }
                val entry = entries.firstOrNull { EffectStore.toEffect(it.effectJson)?.id == id }
                if (entry != null) {
                    withContext(Dispatchers.IO) { EffectStore.remove(context, entry.name) }
                }
                // Installed effects live in the device list: the editor keeps it
                // in memory and will not learn about the removal on its own.
                state.loadInstalledEffects(context)
            }
        } else {
            // A project effect is removed together with all its uses: leaving a
            // chain with a reference to a removed kind would leave a dead node
            // that the engine silently drops.
            state.removeCustomEffect(id)
        }
    }

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheet) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(bottom = RumoSpacing.l),
            verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = RumoSpacing.l),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        stringResource(R.string.panel_effects_add),
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Text(
                        text = previewLayer?.let {
                            stringResource(R.string.panel_effects_preview_on, it.name)
                        } ?: stringResource(R.string.panel_effects_no_preview_layer),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = RumoSpacing.l),
                singleLine = true,
                leadingIcon = { Icon(Icons.Rounded.Search, contentDescription = null) },
                placeholder = { Text(stringResource(R.string.panel_effects_search)) },
            )

            if (catalogue.isEmpty()) {
                Text(
                    text = stringResource(R.string.panel_effects_no_catalogue_long),
                    modifier = Modifier.padding(horizontal = RumoSpacing.l),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            LazyColumn(
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(max = 420.dp),
                contentPadding = PaddingValues(horizontal = RumoSpacing.s),
                verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
            ) {
                items(shown, key = { it.id }) { descriptor ->
                    val key = "${previewLayer?.id}|${descriptor.id}|$previewW"
                    val image = previews.ready[key]
                    if (image == null && previewLayer != null && !previews.broken.containsKey(key)) {
                        LaunchedEffect(key) {
                            previews.request(
                                key = key,
                                layerId = previewLayer.id,
                                effectId = descriptor.id,
                                timeMs = timeMs,
                                w = previewW,
                                h = previewH,
                            )
                        }
                    }
                    EffectPickRow(
                        descriptor = descriptor,
                        preview = image,
                        waiting = previewLayer != null && image == null &&
                            !previews.broken.containsKey(key),
                        deletable = descriptor.id in installedIds || descriptor.id in projectIds,
                        installed = descriptor.id in installedIds,
                        onPick = { onPick(descriptor.id) },
                        onDelete = { deleteEffect(descriptor.id) },
                    )
                }
            }
        }
    }
}

/**
 * One effect-picker row: preview, caption and the action menu.
 *
 * The preview is a frame of the **real layer** with this effect, drawn by the
 * same engine that draws the editor. A list without previews forced choosing an
 * effect by name, and effect names ("Threshold", "Glow") describe the technique,
 * not what you will get on this frame.
 */
@Composable
private fun EffectPickRow(
    descriptor: RumoBridge.EffectDescriptor,
    preview: ImageBitmap?,
    waiting: Boolean,
    deletable: Boolean,
    installed: Boolean,
    onPick: () -> Unit,
    onDelete: () -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
            .clickable(onClick = onPick)
            .padding(horizontal = RumoSpacing.s, vertical = RumoSpacing.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
    ) {
        Box(
            modifier = Modifier
                .size(width = 64.dp, height = 36.dp)
                .clip(RoundedCornerShape(8.dp))
                .background(MaterialTheme.colorScheme.surfaceContainerLowest),
            contentAlignment = Alignment.Center,
        ) {
            when {
                preview != null -> Image(
                    bitmap = preview,
                    contentDescription = null,
                    modifier = Modifier.fillMaxSize(),
                    contentScale = ContentScale.Crop,
                )
                waiting -> Text(
                    text = "…",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                else -> Icon(
                    imageVector = Icons.Rounded.AutoFixHigh,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(16.dp),
                )
            }
        }
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = effectDisplayName(descriptor.id, descriptor.label),
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs)) {
                Text(
                    text = descriptor.id,
                    style = MaterialTheme.typography.labelSmall.merge(monoNumerals),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (descriptor.cost.isNotEmpty()) MetaPill(descriptor.cost.uppercase())
                if (installed) MetaPill(stringResource(R.string.panel_effects_shop))
            }
        }
        Box {
            IconButton(
                onClick = { menuOpen = true },
                modifier = Modifier.size(MinTouchTarget),
            ) {
                Icon(
                    imageVector = Icons.Rounded.MoreVert,
                    contentDescription = stringResource(R.string.panel_effects_actions),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(18.dp),
                )
            }
            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.panel_effects_add_to_layer)) },
                    leadingIcon = {
                        Icon(
                            imageVector = Icons.Rounded.Add,
                            contentDescription = null,
                            modifier = Modifier.size(18.dp),
                        )
                    },
                    onClick = {
                        menuOpen = false
                        onPick()
                    },
                )
                if (deletable) {
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.panel_delete)) },
                        leadingIcon = {
                            Icon(
                                imageVector = Icons.Rounded.Delete,
                                contentDescription = null,
                                tint = MaterialTheme.colorScheme.error,
                                modifier = Modifier.size(18.dp),
                            )
                        },
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
 * Effect previews: a frame of the layer with one effect, rendered offscreen.
 *
 * The requests are serialised by a mutex rather than fired in a batch:
 * [EditorState.effectProbeAt] swaps the layer list for the duration of the
 * render, and two simultaneous renders would see each other's swap. The
 * restriction is one preview at a time, and there are a dozen in the list, each
 * costing single-digit milliseconds.
 */
private class EffectPreviewLoader(
    private val state: EditorState,
    private val scope: CoroutineScope,
) {
    val ready = mutableStateMapOf<String, ImageBitmap>()
    val broken = mutableStateMapOf<String, Unit>()
    private val inFlight = HashSet<String>()
    private val gate = Mutex()

    fun request(key: String, layerId: String, effectId: String, timeMs: Long, w: Int, h: Int) {
        if (ready.containsKey(key) || broken.containsKey(key)) return
        if (!inFlight.add(key)) return
        scope.launch {
            val image = gate.withLock {
                withContext(Dispatchers.Default) {
                    val effect = defaultEffectFor(effectId, state.customsJson())
                        ?: return@withContext null
                    val px = state.effectProbeAt(timeMs, w, h, layerId, listOf(effect))
                        ?: return@withContext null
                    argbToImage(px, w, h)
                }
            }
            inFlight.remove(key)
            if (image == null) broken[key] = Unit else ready[key] = image
        }
    }
}

/** An engine frame (0xAARRGGBB) into an `ImageBitmap` for Compose. */
private fun argbToImage(px: IntArray, w: Int, h: Int): ImageBitmap? = try {
    if (px.size != w * h) {
        null
    } else {
        Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888).also {
            it.copyPixelsFromBuffer(IntBuffer.wrap(px))
        }.asImageBitmap()
    }
} catch (_: Throwable) {
    null
}

/**
 * Editor for one catalogue parameter — generated from `kind`, never hardcoded
 * per effect. Slider and field writes go through `setEffectParamValue`, which
 * clamps and snaps exactly like Rust's `ParamSpec::clamp`.
 *
 * `enabled` is what makes a locked layer (D12) read as locked everywhere: a lock
 * that leaves the effect parameters live is a lock that does not hold.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun EffectParamEditor(
    param: RumoBridge.EffectParam,
    effect: Effect,
    onComponent: (component: Int, value: Float) -> Unit,
    enabled: Boolean = true,
) {
    // Values are read by slot, not by loop index: the parameters go into the
    // vector with their own offsets (a colour takes four slots).
    val values = List(param.slots) { i -> effect.params.getOrElse(param.slot + i) { 0f } }
    // Resolved once and handed to the branch, so every control of one parameter —
    // its caption, its reset's spoken label, the colour picker's title — names it
    // the same way, and each branch does not look the pair up again.
    val label = paramDisplayLabel(effect.kindId, param)

    when (param.kind.lowercase()) {
        "color" -> ColorParamEditor(param, values, enabled, label, onComponent)
        "choice" -> ChoiceParamEditor(param, values, enabled, label, onComponent)
        "bool" -> BoolParamEditor(param, values, enabled, label, onComponent)
        "float", "angle", "int" -> ScalarParamEditor(param, values, enabled, label, onComponent)
        else -> Row(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = MinTouchTarget),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = label,
                style = MaterialTheme.typography.labelLarge,
                modifier = Modifier.weight(1f),
            )
            // An unknown parameter kind: show it, but do not invent a control.
            MetaPill(param.kind.lowercase())
        }
    }
}

/**
 * Colour parameter (§7.9): one swatch row plus an alpha slider instead of four
 * sliders. The wire shape is untouched — the model stays four `f32` and the
 * picker writes components 0..2 one by one.
 */
@Composable
private fun ColorParamEditor(
    param: RumoBridge.EffectParam,
    values: List<Float>,
    enabled: Boolean,
    label: String,
    onComponent: (component: Int, value: Float) -> Unit,
) {
    val red = values.getOrElse(0) { 0f }.coerceIn(0f, 1f)
    val green = values.getOrElse(1) { 0f }.coerceIn(0f, 1f)
    val blue = values.getOrElse(2) { 0f }.coerceIn(0f, 1f)
    val alpha = values.getOrElse(3) { 1f }.coerceIn(0f, 1f)
    var pickerOpen by remember { mutableStateOf(false) }
    val colour = Color(red, green, blue)

    Column {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = MinTouchTarget),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
        ) {
            Text(
                text = label,
                style = MaterialTheme.typography.labelLarge,
                color = if (enabled) {
                    MaterialTheme.colorScheme.onSurface
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = hexOf(red, green, blue),
                style = MaterialTheme.typography.labelMedium.merge(monoNumerals),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            ColourSwatch(
                colour = colour,
                enabled = enabled,
                description = stringResource(R.string.panel_effects_choose, label),
                onClick = { pickerOpen = true },
            )
            IconButton(
                onClick = {
                    val defaults = param.defaultSlots()
                    for (component in defaults.indices) onComponent(component, defaults[component])
                },
                enabled = enabled,
                modifier = Modifier.size(MinTouchTarget),
            ) {
                Icon(
                    imageVector = Icons.Rounded.RestartAlt,
                    contentDescription = stringResource(R.string.panel_control_reset, label),
                    modifier = Modifier.size(18.dp),
                )
            }
        }
        if (param.slots >= 4) {
            // Alpha is the component people drag as a number; the picker owns hue.
            CompactSliderRow(
                label = stringResource(R.string.panel_effects_alpha),
                value = alpha,
                valueRange = 0f..1f,
                valueText = "%.2f".format(alpha),
                enabled = enabled,
                onValueChange = { onComponent(3, it) },
            )
        }
    }

    if (pickerOpen) {
        ColorPickerDialog(
            title = label,
            initial = colour,
            onColor = { picked ->
                onComponent(0, picked.red)
                onComponent(1, picked.green)
                onComponent(2, picked.blue)
            },
            onDismiss = { pickerOpen = false },
        )
    }
}

/** Choice parameter: one labelled chip per option, no fake slider. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ChoiceParamEditor(
    param: RumoBridge.EffectParam,
    values: List<Float>,
    enabled: Boolean,
    label: String,
    onComponent: (component: Int, value: Float) -> Unit,
) {
    val current = values.getOrElse(0) { 0f }.roundToInt()
    Column {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = MinTouchTarget),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = label,
                style = MaterialTheme.typography.labelLarge,
                color = if (enabled) {
                    MaterialTheme.colorScheme.onSurface
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
                modifier = Modifier.weight(1f),
            )
            IconButton(
                onClick = { onComponent(0, param.defaultSlots().getOrElse(0) { 0f }) },
                enabled = enabled,
                modifier = Modifier.size(MinTouchTarget),
            ) {
                Icon(
                    imageVector = Icons.Rounded.RestartAlt,
                    contentDescription = stringResource(R.string.panel_control_reset, label),
                    modifier = Modifier.size(18.dp),
                )
            }
        }
        if (param.choices.isEmpty()) {
            MetaPill(stringResource(R.string.panel_effects_no_choices))
        } else {
            FlowRow(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
            ) {
                for ((i, choice) in param.choices.withIndex()) {
                    // Labelled chip: it may stay compact (§7.13 — the exemption
                    // is for elements that carry their own text label).
                    CompactChipScope {
                        FilterChip(
                            selected = i == current,
                            onClick = { onComponent(0, i.toFloat()) },
                            enabled = enabled,
                            label = {
                                Text(
                                    // The engine's value is what goes back on a tap;
                                    // only the chip's caption is translated.
                                    choiceDisplayLabel(choice),
                                    style = MaterialTheme.typography.labelSmall,
                                )
                            },
                            modifier = Modifier.height(28.dp),
                            shape = RoundedCornerShape(8.dp),
                        )
                    }
                }
            }
        }
    }
}

/** Boolean parameter: a switch, disabled with the rest of a locked layer. */
@Composable
private fun BoolParamEditor(
    param: RumoBridge.EffectParam,
    values: List<Float>,
    enabled: Boolean,
    label: String,
    onComponent: (component: Int, value: Float) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelLarge,
            color = if (enabled) {
                MaterialTheme.colorScheme.onSurface
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
            modifier = Modifier.weight(1f),
        )
        Switch(
            checked = values.getOrElse(0) { 0f } >= 0.5f,
            onCheckedChange = { onComponent(0, if (it) 1f else 0f) },
            enabled = enabled,
        )
    }
}

/**
 * Scalar parameter (§7.8): the slider for coarse work, the value itself for the
 * exact number the slider cannot reach, and a reset back to the catalogue
 * default. `param.default` reaches the UI from Rust and was previously never
 * read (§4.2) — this is where it is.
 */
@Composable
private fun ScalarParamEditor(
    param: RumoBridge.EffectParam,
    values: List<Float>,
    enabled: Boolean,
    label: String,
    onComponent: (component: Int, value: Float) -> Unit,
) {
    val min = param.min
    // A broken range must not bring the Slider down (start < end is mandatory).
    val max = if (param.max > param.min) param.max else param.min + 1f
    val raw = values.getOrElse(0) { 0f }
    val clamped = raw.coerceIn(min, max)
    // Int snaps to whole numbers: between the ends there are exactly (max - min - 1) steps.
    val steps = if (param.kind.equals("int", ignoreCase = true)) {
        ((max - min).roundToInt() - 1).coerceIn(0, 1000)
    } else {
        0
    }
    var inputOpen by remember { mutableStateOf(false) }

    CompactSliderRow(
        label = label,
        value = clamped,
        valueRange = min..max,
        valueText = paramValueText(param, clamped),
        steps = steps,
        enabled = enabled,
        onValueChange = { onComponent(0, it) },
        onValueClick = { inputOpen = true },
        onReset = { onComponent(0, param.defaultSlots().getOrElse(0) { 0f }) },
    )

    if (inputOpen) {
        NumberInputDialog(
            title = label,
            // A bare number: the field's parse must not have to strip a unit.
            initial = paramNumberText(param, clamped),
            onCommit = { onComponent(0, it) },
            onDismiss = { inputOpen = false },
        )
    }
}

/**
 * Colour swatch: the visible colour plus a 44dp target with its own label for a
 * screen reader (the §7.9 row has no text beside the swatch).
 */
@Composable
internal fun ColourSwatch(
    colour: Color,
    enabled: Boolean,
    description: String,
    onClick: () -> Unit,
) {
    Box(
        modifier = Modifier
            .size(MinTouchTarget)
            .clip(CircleShape)
            .clickable(enabled = enabled, onClick = onClick)
            .semantics { contentDescription = description },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .size(24.dp)
                .clip(CircleShape)
                .background(colour)
                .border(1.dp, MaterialTheme.colorScheme.outlineVariant, CircleShape),
        )
    }
}

/**
 * One-line slider row: optional icon, label, track, monospace value.
 *
 * §7.5 budgets a single line instead of the two-line `ParamSlider` (that control
 * stays for the node sheet and the layer panels). `onValueClick` and `onReset`
 * are optional because not every parameter wants exact entry or a default: a
 * transform offset does, opacity does not.
 */
@Composable
internal fun CompactSliderRow(
    label: String,
    value: Float,
    valueRange: ClosedFloatingPointRange<Float>,
    valueText: String,
    onValueChange: (Float) -> Unit,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    steps: Int = 0,
    enabled: Boolean = true,
    onValueChangeFinished: () -> Unit = {},
    onValueClick: (() -> Unit)? = null,
    onReset: (() -> Unit)? = null,
) {
    // One row implementation for the whole editor: [PropertyRow]. Formerly there
    // was a Material Slider with a round thumb, and every panel drew its own
    // version of "caption + slider + value" — three languages in one list. Now
    // the row is one, and it is in the instrument form (EditorControls.kt).
    //
    // `steps` goes into the stock `Slider`: it draws the ticks itself and snaps
    // the value to them itself, so there is nothing to round here.
    PropertyRow(
        label = label,
        value = value,
        range = valueRange,
        valueText = valueText,
        icon = icon,
        steps = steps,
        enabled = enabled,
        onValueClick = onValueClick,
        onReset = onReset,
        onValueCommit = onValueChangeFinished,
        onValue = onValueChange,
        modifier = modifier,
    )
}

/**
 * Exact numeric entry for one parameter (§7.8). A plain text keyboard, not
 * `KeyboardType.Number`: offsets and angles are signed and the numeric IME has
 * no minus key.
 */
@Composable
internal fun NumberInputDialog(
    title: String,
    initial: String,
    onCommit: (Float) -> Unit,
    onDismiss: () -> Unit,
) {
    var text by remember { mutableStateOf(initial) }
    // A comma is accepted because a decimal keypad in a comma-locale offers one,
    // and refusing the character the keyboard just produced is not a validation
    // rule anyone can act on.
    val parsed = text.trim().replace(',', '.').toFloatOrNull()
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                singleLine = true,
                label = { Text(stringResource(R.string.panel_effects_value)) },
                isError = parsed == null,
                modifier = Modifier.fillMaxWidth(),
            )
        },
        confirmButton = {
            TextButton(
                onClick = {
                    parsed?.let(onCommit)
                    onDismiss()
                },
                enabled = parsed != null,
            ) {
                Text(stringResource(R.string.panel_effects_set))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.panel_cancel)) }
        },
    )
}

/**
 * Colour picker for the layer swatch and for `color` effect parameters.
 *
 * A fixed palette plus a hue slider: honest and small, and it writes straight
 * through, so the preview follows the finger. Swatches are declared at
 * [MinTouchTarget] and the hue slider is a normal slider, because a picker made
 * of 16dp dots is the same defect as the 16dp keyframe cross (§7.13).
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ColorPickerDialog(
    title: String,
    initial: Color,
    onColor: (Color) -> Unit,
    onDismiss: () -> Unit,
) {
    var hue by remember { mutableFloatStateOf(hueOf(initial.red, initial.green, initial.blue)) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.s)) {
                FlowRow(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                    verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                ) {
                    for (argb in ColorPickerSwatches) {
                        val swatch = Color(argb)
                        ColourSwatch(
                            colour = swatch,
                            enabled = true,
                            description = stringResource(
                                R.string.panel_effects_colour,
                                hexOf(swatch.red, swatch.green, swatch.blue),
                            ),
                            onClick = { onColor(swatch) },
                        )
                    }
                }
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 48.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = stringResource(R.string.panel_effects_hue),
                        style = MaterialTheme.typography.labelLarge,
                        modifier = Modifier.width(44.dp),
                    )
                    Slider(
                        value = hue,
                        onValueChange = {
                            hue = it
                            onColor(Color.hsv(it, 1f, 1f))
                        },
                        valueRange = 0f..360f,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.panel_effects_done)) }
        },
    )
}

/** Value text for scalars: int snaps, angle reads in degrees, unit appends. */
private fun paramValueText(param: RumoBridge.EffectParam, value: Float): String {
    val body = paramNumberText(param, value)
    return if (param.unit.isEmpty()) body else "$body${param.unit}"
}

/**
 * The bare number of a scalar parameter, for the numeric-entry field.
 *
 * `Locale.ROOT`, not the default locale, and this is a bug fix rather than a
 * style choice. The text this produces prefills an editable field that is read
 * back with `toFloatOrNull()`, which accepts only a dot — while `"%.2f".format`
 * on a phone set to Russian writes "0,50". The value shown could not be typed
 * back, so on such a phone the field was a dead end: open it, and the OK button
 * was already disabled by the number it had put there itself.
 *
 * The slider's readout uses the same text, so a parameter reads the same in the
 * label and in the field. That is deliberate: two spellings of one value is
 * worse than one spelling a reader might not have chosen.
 */
private fun paramNumberText(param: RumoBridge.EffectParam, value: Float): String = when {
    param.kind.equals("int", ignoreCase = true) -> value.roundToInt().toString()
    param.kind.equals("angle", ignoreCase = true) -> String.format(Locale.ROOT, "%.1f", value)
    else -> String.format(Locale.ROOT, "%.2f", value)
}

/** `#RRGGBB` for the swatch readout; the alpha channel lives on its own slider. */
private fun hexOf(red: Float, green: Float, blue: Float): String {
    fun channel(v: Float) = (v.coerceIn(0f, 1f) * 255f).roundToInt()
    return "#%02X%02X%02X".format(channel(red), channel(green), channel(blue))
}

/** Hue of an RGB triple in degrees, for the picker's initial slider position. */
private fun hueOf(red: Float, green: Float, blue: Float): Float {
    val max = maxOf(red, green, blue)
    val min = minOf(red, green, blue)
    val delta = max - min
    if (delta <= 0f) return 0f
    val raw = when (max) {
        red -> 60f * (((green - blue) / delta) % 6f)
        green -> 60f * (((blue - red) / delta) + 2f)
        else -> 60f * (((red - green) / delta) + 4f)
    }
    return (raw + 360f) % 360f
}
