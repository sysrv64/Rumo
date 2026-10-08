// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.ui.panels.ColorPickerDialog
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.panels.ColourSwatch

/**
 * The frame the project is composed in: its size and its background.
 *
 * A project is not stuck at the size the app happens to ship. This dialog sets
 * the canvas either from a preset (the same list the exporter offers, portrait
 * variants included) or from numbers the user types, and it sets the background
 * in the same place, because those two are the same decision: what the frame is.
 *
 * Both are project data — they travel in the saved file — so a project made here
 * opens here, on another device, in the same frame.
 */
@Composable
fun CanvasDialog(
    currentWidth: Int,
    currentHeight: Int,
    currentBackground: Long,
    onApply: (width: Int, height: Int, background: Long) -> Unit,
    onDismiss: () -> Unit,
) {
    var width by remember { mutableStateOf(currentWidth.toString()) }
    var height by remember { mutableStateOf(currentHeight.toString()) }
    var background by remember { mutableStateOf(Color(currentBackground.toInt())) }
    var pickerOpen by remember { mutableStateOf(false) }

    val parsedWidth = width.trim().toIntOrNull()
    val parsedHeight = height.trim().toIntOrNull()
    val valid = parsedWidth != null && parsedHeight != null &&
        parsedWidth in EditorState.MIN_CANVAS..EditorState.MAX_CANVAS &&
        parsedHeight in EditorState.MIN_CANVAS..EditorState.MAX_CANVAS

    val presets = remember { RumoBridge.resolutionPresets() }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.editor_canvas_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.s)) {
                Text(
                    text = stringResource(R.string.editor_canvas_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )

                LazyColumn(
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(160.dp),
                    verticalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                ) {
                    items(presets, key = { it.id }) { preset ->
                        val selected = parsedWidth == preset.width && parsedHeight == preset.height
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .height(44.dp)
                                .clip(RoundedCornerShape(8.dp))
                                .background(
                                    if (selected) {
                                        MaterialTheme.colorScheme.secondaryContainer
                                    } else {
                                        MaterialTheme.colorScheme.surfaceContainerHigh
                                    },
                                )
                                .clickable {
                                    width = preset.width.toString()
                                    height = preset.height.toString()
                                }
                                .padding(horizontal = RumoSpacing.s),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                text = preset.label,
                                style = MaterialTheme.typography.bodyMedium,
                                modifier = Modifier.weight(1f),
                            )
                            Text(
                                text = "${preset.width}×${preset.height}",
                                style = MaterialTheme.typography.labelSmall.merge(
                                    androidx.compose.ui.text.TextStyle(fontFamily = FontFamily.Monospace),
                                ),
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }

                Row(horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s)) {
                    OutlinedTextField(
                        value = width,
                        onValueChange = { width = it.filter { c -> c.isDigit() }.take(4) },
                        label = { Text(stringResource(R.string.editor_canvas_width)) },
                        singleLine = true,
                        isError = parsedWidth != null &&
                            parsedWidth !in EditorState.MIN_CANVAS..EditorState.MAX_CANVAS,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                        modifier = Modifier.weight(1f),
                    )
                    OutlinedTextField(
                        value = height,
                        onValueChange = { height = it.filter { c -> c.isDigit() }.take(4) },
                        label = { Text(stringResource(R.string.editor_canvas_height)) },
                        singleLine = true,
                        isError = parsedHeight != null &&
                            parsedHeight !in EditorState.MIN_CANVAS..EditorState.MAX_CANVAS,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                        modifier = Modifier.weight(1f),
                    )
                }

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(RumoSpacing.s),
                ) {
                    Text(
                        text = stringResource(R.string.editor_canvas_background),
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.weight(1f),
                    )
                    ColourSwatch(
                        colour = background,
                        enabled = true,
                        description = stringResource(R.string.editor_canvas_background_colour),
                        onClick = { pickerOpen = true },
                    )
                    Text(
                        text = hexOf(background),
                        style = MaterialTheme.typography.labelSmall.merge(
                            androidx.compose.ui.text.TextStyle(fontFamily = FontFamily.Monospace),
                        ),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }

                if (!valid) {
                    Text(
                        text = stringResource(
                            R.string.editor_canvas_range,
                            EditorState.MIN_CANVAS,
                            EditorState.MAX_CANVAS,
                        ),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                enabled = valid,
                onClick = {
                    onApply(
                        parsedWidth ?: currentWidth,
                        parsedHeight ?: currentHeight,
                        background.toArgb().toLong() and 0xFFFFFFFFL,
                    )
                },
            ) {
                Text(stringResource(R.string.editor_canvas_apply))
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.editor_cancel)) } },
    )

    if (pickerOpen) {
        ColorPickerDialog(
            title = stringResource(R.string.editor_canvas_background),
            initial = background,
            onColor = { background = it },
            onDismiss = { pickerOpen = false },
        )
    }
}

/** `#RRGGBB` for a colour whose alpha is implied: a background is opaque. */
private fun hexOf(colour: Color): String {
    val argb = colour.toArgb()
    return "#%02X%02X%02X".format(
        (argb shr 16) and 0xFF,
        (argb shr 8) and 0xFF,
        argb and 0xFF,
    )
}

/** A small dot of the background, used where a full swatch would be noise. */
@Composable
internal fun CanvasDot(argb: Long, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier
            .size(14.dp)
            .clip(RoundedCornerShape(4.dp))
            .background(Color(argb.toInt())),
    ) {}
}
