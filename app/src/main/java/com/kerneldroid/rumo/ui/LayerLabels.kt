// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import com.kerneldroid.rumo.R

/**
 * What to call a layer in the interface.
 *
 * A layer's `name` is not a caption: for a SHAPE it is the engine's shape kind,
 * which is compared (`shapeOrdinalOf`), stored in the project and turned into a
 * draw ordinal, and `renameLayer` refuses to change it. So the name stays as it
 * is and only its *display* is translated here — the same split the effect
 * vocabulary uses.
 *
 * Only the background differs so far, because it is the one shape a user meets
 * as a concept rather than as a shape: "Frame" is the engine's word for the
 * thing, and the row that holds the background should say what it is.
 */
@Composable
fun layerDisplayName(layer: LayerUi): String =
    if (EditorState.isBackground(layer)) {
        stringResource(R.string.editor_layer_background)
    } else {
        layer.name
    }
