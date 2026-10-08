// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Image
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import com.kerneldroid.rumo.ui.panels.CompactChip
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.theme.RumoSpacing

// 1:1 with rumo-render ShapeKind variants in all_shapes() order.
val ShapeNames: List<String> = listOf(
    "Circle",
    "Square",
    "Slanted",
    "Arch",
    "Fan",
    "Arrow",
    "SemiCircle",
    "Oval",
    "Pill",
    "Triangle",
    "Diamond",
    "ClamShell",
    "Pentagon",
    "Gem",
    "Sunny",
    "VerySunny",
    "Cookie4Sided",
    "Cookie6Sided",
    "Cookie7Sided",
    "Cookie9Sided",
    "Cookie12Sided",
    "Ghostish",
    "Clover4Leaf",
    "Clover8Leaf",
    "Burst",
    "SoftBurst",
    "Boom",
    "SoftBoom",
    "Flower",
    "Puffy",
    "PuffyDiamond",
    "PixelCircle",
    "PixelTriangle",
    "Bun",
    "Heart",
    // Appended, never inserted: the ordinal IS what a layer stores, so an
    // insertion would repaint every project saved before it as a different
    // shape. "Frame" is the background — it is drawn to cover the target rather
    // than placed inside it (see the special case in `preview_shape_draws`),
    // which is what lets the background be an ordinary layer and therefore take
    // effects, opacity and keyframes.
    "Frame",
)

/**
 * What to call a shape in the picker.
 *
 * The list above is engine identity — it is compared, stored in a layer's name
 * and turned into an ordinal — so it stays as it is. The one exception is the
 * frame, whose identity "Frame" says nothing to a user: they are adding a
 * background, and the chip should say so.
 */
@Composable
fun shapeDisplayName(name: String): String =
    if (name == "Frame") stringResource(R.string.editor_shape_frame) else name

/**
 * Shape layers carry their shape kind in the display name (that is what the
 * Rust `all_shapes()` order is keyed by), so the name must always resolve back
 * to an ordinal. Older projects seeded by Rumo used a human label that is not a
 * shape kind at all — keep those rendering instead of dropping them.
 */
private val LegacyShapeAliases = mapOf("rounded rectangle 1" to "Square")

/** Engine ordinal for a SHAPE layer name, or -1 when the engine has no such kind. */
fun shapeOrdinalOf(name: String): Int {
    val direct = ShapeNames.indexOf(name)
    if (direct >= 0) return direct
    val alias = LegacyShapeAliases[name.trim().lowercase()] ?: return -1
    return ShapeNames.indexOf(alias)
}

private val AddSheetTabs = listOf("Shapes", "Text", "Media")

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AddSheet(
    state: EditorState,
    onDismiss: () -> Unit,
    pickers: MediaPickers? = null,
) {
    val trisMap by state.shapeTris.collectAsState()
    var tab by remember { mutableIntStateOf(0) }
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = RumoSpacing.l, vertical = RumoSpacing.s),
            horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s, Alignment.CenterHorizontally),
        ) {
            AddSheetTabs.forEachIndexed { index, name ->
                CompactChip(
                    label = name,
                    selected = tab == index,
                    onClick = { tab = index },
                )
            }
        }
        when (tab) {
            0 -> {
                LazyVerticalGrid(
                    columns = GridCells.Fixed(3),
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(RumoSpacing.l),
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                    verticalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                ) {
                    items(ShapeNames.size) { ordinal ->
                        val name = ShapeNames[ordinal]
                        val shown = shapeDisplayName(name)
                        Card(
                            modifier = Modifier.clickable {
                                state.addShape(name)
                                onDismiss()
                            },
                        ) {
                            Column(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .padding(RumoSpacing.m),
                                horizontalAlignment = Alignment.CenterHorizontally,
                            ) {
                                Text(
                                    text = name,
                                    textAlign = TextAlign.Center,
                                    modifier = Modifier.fillMaxWidth(),
                                )
                                Text(
                                    text = if (trisMap.containsKey(ordinal)) "${trisMap[ordinal]} tris" else "…",
                                    textAlign = TextAlign.Center,
                                    modifier = Modifier.fillMaxWidth(),
                                )
                            }
                        }
                    }
                }
            }
            1 -> {
                var query by remember { mutableStateOf("") }
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(RumoSpacing.l),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(RumoSpacing.m),
                ) {
                    TextField(
                        value = query,
                        onValueChange = { if (it.length <= 140 && !it.contains('\n')) query = it },
                        label = { Text("Layer text") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Button(
                        onClick = {
                            state.addTextLayer(query)
                            onDismiss()
                        },
                        enabled = query.isNotBlank(),
                    ) {
                        Text(text = "Add")
                    }
                }
            }
            else -> {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .horizontalScroll(rememberScrollState())
                        .padding(RumoSpacing.l),
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.m, Alignment.CenterHorizontally),
                ) {
                    Button(
                        onClick = { pickers?.launchImage() },
                        enabled = pickers != null,
                    ) {
                        Icon(Icons.Rounded.Image, contentDescription = null)
                        Text(text = "Photo", modifier = Modifier.padding(start = RumoSpacing.s))
                    }
                    Button(
                        onClick = { pickers?.launchAudio() },
                        enabled = pickers != null,
                    ) {
                        Icon(Icons.Rounded.MusicNote, contentDescription = null)
                        Text(text = "Audio", modifier = Modifier.padding(start = RumoSpacing.s))
                    }
                    Button(
                        onClick = { pickers?.launchVideo() },
                        enabled = pickers != null,
                    ) {
                        Icon(Icons.Rounded.PlayArrow, contentDescription = null)
                        Text(text = "Video", modifier = Modifier.padding(start = RumoSpacing.s))
                    }
                }
            }
        }
    }
}
