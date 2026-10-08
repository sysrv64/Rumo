// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Terminal
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.ui.panels.MetaPill
import com.kerneldroid.rumo.ui.panels.PanelEmpty
import com.kerneldroid.rumo.ui.panels.SectionLabel
import com.kerneldroid.rumo.ui.theme.RumoKind
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.monoNumerals

/** Positive path colour (GPU): Rumo's "audio" mark reads as go-green. */
private val PathGpuColor = RumoKind.audioMark

/** Warning path colour (CPU): Rumo's snap amber. */
private val PathCpuColor = RumoKind.snap

/**
 * Why the renderer is on CPU instead of GPU. Read-only view over
 * [RumoBridge.RenderDiagnostics]: current path, adapter/backend, the Rust
 * hint, effects whose shader failed to build, and the native log (newest last).
 *
 * Degrades to an empty/absent state on a null or partial report — never throws.
 */
@Composable
fun RenderDiagnosticsDialog(
    diagnostics: RumoBridge.RenderDiagnostics?,
    /**
     * Status of the lazily drawn layers: one word per layer. Video and SVG draw
     * silently — neither path reports that a resource was missing — and "the
     * layer does not appear" without this line means guessing between the decoder,
     * a missing file, an unloaded texture and unregistered SVG geometry. The
     * section is called "Layer status", not "Video", because the lines here are
     * shared by both.
     */
    videoReport: List<String> = emptyList(),
    onRefresh: () -> Unit,
    onClear: () -> Unit,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    val path = diagnostics?.path.orEmpty()
    val adapter = diagnostics?.adapter.orEmpty()
    val backend = diagnostics?.backend.orEmpty()
    val hint = diagnostics?.hint.orEmpty()
    val rejected = diagnostics?.rejectedEffects.orEmpty()
    val entries = diagnostics?.entries.orEmpty()

    val scrollState = rememberScrollState()
    // New entries go at the end: let the layout measure its height and jump down.
    LaunchedEffect(entries.size) {
        withFrameNanos { }
        scrollState.scrollTo(scrollState.maxValue)
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.diagnostics_render)) },
        text = {
            Column(modifier = Modifier.fillMaxWidth()) {
                // --- Current path + adapter/backend + hint ---
                Text(
                    text = when (path) {
                        "gpu" -> stringResource(R.string.editor_diag_path_gpu)
                        "cpu" -> stringResource(R.string.editor_diag_path_cpu)
                        else -> stringResource(R.string.editor_diag_path_unknown)
                    },
                    style = MaterialTheme.typography.titleLarge,
                    fontWeight = FontWeight.Bold,
                    color = when (path) {
                        "gpu" -> PathGpuColor
                        "cpu" -> PathCpuColor
                        else -> MaterialTheme.colorScheme.onSurfaceVariant
                    },
                )
                Text(
                    text = if (adapter.isEmpty()) {
                        stringResource(R.string.editor_diag_no_gpu)
                    } else if (backend.isEmpty()) {
                        adapter
                    } else {
                        "$adapter · $backend"
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (diagnostics != null) {
                    // Two independent sub-paths: the Surface engine draws the
                    // live preview, the offscreen renderer draws the bitmap
                    // preview and exports. "Which one is on the CPU" is the
                    // question that actually narrows the problem down.
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
                        modifier = Modifier.padding(top = RumoSpacing.xs),
                    ) {
                        MetaPill(
                            text = stringResource(
                                R.string.editor_diag_pill_surface,
                                stringResource(
                                    if (diagnostics.engineOk) {
                                        R.string.editor_diag_path_gpu
                                    } else {
                                        R.string.editor_diag_path_cpu
                                    },
                                ),
                            ),
                        )
                        MetaPill(
                            text = stringResource(
                                R.string.editor_diag_pill_offscreen,
                                stringResource(
                                    if (diagnostics.previewOk) {
                                        R.string.editor_diag_path_gpu
                                    } else {
                                        R.string.editor_diag_path_cpu
                                    },
                                ),
                            ),
                        )
                    }
                }
                if (videoReport.isNotEmpty()) {
                    // The heading is wider than the source: the list has long
                    // held more than video — the same lines carry SVG layers
                    // (`videoStatusReport`), and a "Video" caption above them
                    // would name the wrong thing.
                    Text(
                        text = stringResource(R.string.editor_diag_layer_status),
                        style = MaterialTheme.typography.titleSmall,
                    )
                    videoReport.forEach { line ->
                        Text(
                            text = line,
                            style = MaterialTheme.typography.bodySmall,
                            fontFamily = FontFamily.Monospace,
                        )
                    }
                }
                if (hint.isNotEmpty()) {
                    Text(
                        text = hint,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = RumoSpacing.xs),
                    )
                }
                if (diagnostics == null) {
                    Text(
                        text = stringResource(R.string.editor_diag_unavailable),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = RumoSpacing.xs),
                    )
                }

                // --- Rejected effects (shader failed to build) ---
                if (rejected.isNotEmpty()) {
                    SectionLabel(stringResource(R.string.editor_diag_rejected_effects))
                    for (fx in rejected) {
                        MetaPill(
                            text = fx,
                            tint = MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(bottom = RumoSpacing.xs),
                        )
                    }
                }

                // --- Log ---
                SectionLabel(stringResource(R.string.editor_diag_log))
                if (entries.isEmpty()) {
                    PanelEmpty(
                        icon = Icons.Rounded.Terminal,
                        title = stringResource(R.string.editor_diag_empty),
                        hint = stringResource(R.string.editor_diag_empty_hint),
                    )
                } else {
                    SelectionContainer {
                        Column(
                            modifier = Modifier
                                .fillMaxWidth()
                                .heightIn(max = 220.dp)
                                .verticalScroll(scrollState),
                        ) {
                            for (entry in entries) {
                                EntryRow(entry)
                            }
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_close)) }
        },
        dismissButton = {
            Row {
                TextButton(onClick = onRefresh) { Text(stringResource(R.string.editor_diag_refresh)) }
                TextButton(
                    onClick = {
                        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE)
                            as? ClipboardManager
                        clipboard?.setPrimaryClip(
                            ClipData.newPlainText(
                                context.getString(R.string.editor_diag_clip_label),
                                diagnosticDump(diagnostics),
                            ),
                        )
                        Toast.makeText(
                            context,
                            context.getString(R.string.editor_diag_copied),
                            Toast.LENGTH_SHORT,
                        ).show()
                    },
                ) {
                    Text(stringResource(R.string.editor_diag_copy))
                }
                TextButton(onClick = onClear) { Text(stringResource(R.string.editor_diag_clear)) }
            }
        },
    )
}

@Composable
private fun EntryRow(entry: RumoBridge.RenderLogEntry) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = RumoSpacing.xs),
        verticalAlignment = Alignment.Top,
    ) {
        LevelTag(entry.level)
        Spacer(modifier = Modifier.size(RumoSpacing.s))
        Column(modifier = Modifier.weight(1f)) {
            if (entry.code.isNotEmpty()) {
                Text(
                    text = entry.code,
                    style = MaterialTheme.typography.labelSmall.merge(monoNumerals),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(
                text = entry.text,
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}

@Composable
private fun LevelTag(level: String) {
    val color = when (level) {
        "error" -> MaterialTheme.colorScheme.error
        "warn" -> PathCpuColor
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(4.dp))
            .background(color.copy(alpha = 0.16f))
            .padding(horizontal = 6.dp, vertical = 1.dp),
    ) {
        Text(
            text = level.uppercase(),
            style = MaterialTheme.typography.labelSmall.merge(monoNumerals),
            fontWeight = FontWeight.SemiBold,
            color = color,
        )
    }
}

/** Plain-text dump of the whole report (for the clipboard). */
private fun diagnosticDump(d: RumoBridge.RenderDiagnostics?): String {
    val sb = StringBuilder()
    sb.append("Rumo render diagnostics\n")
    sb.append("path: ").append(d?.path.orEmpty().ifEmpty { "unknown" }).append('\n')
    sb.append("adapter: ").append(d?.adapter.orEmpty()).append('\n')
    sb.append("backend: ").append(d?.backend.orEmpty()).append('\n')
    sb.append("hint: ").append(d?.hint.orEmpty()).append('\n')
    sb.append("surfaceEngine: ").append(if (d?.engineOk == true) "GPU" else "CPU").append('\n')
    sb.append("offscreen: ").append(if (d?.previewOk == true) "GPU" else "CPU").append('\n')
    val rejected = d?.rejectedEffects.orEmpty()
    sb.append("rejectedEffects: ")
        .append(if (rejected.isEmpty()) "(none)" else rejected.joinToString(", "))
        .append('\n')
    sb.append("entries:\n")
    for (e in d?.entries.orEmpty()) {
        sb.append(e.level.uppercase()).append(' ')
            .append(e.code).append(": ").append(e.text).append('\n')
    }
    return sb.toString()
}
