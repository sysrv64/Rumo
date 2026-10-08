// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.panels

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.GraphicEq
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.RestartAlt
import androidx.compose.material.icons.rounded.VolumeUp
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.LayerKindUi
import com.kerneldroid.rumo.ui.LayerUi
import com.kerneldroid.rumo.ui.formatTime
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.monoNumerals

/**
 * Audio page. Everything here is wired to the Rust audio bridge:
 * `nativeAudioOpen/Play/Pause/Seek/Close`. There is no gain stage in the engine,
 * so no volume slider is offered — only verbs the engine can actually execute.
 */
@Composable
fun AudioPanel(
    state: EditorState,
    layers: List<LayerUi>,
    selectedId: String?,
    playheadMs: Long,
    isPlaying: Boolean,
    onSelect: (String) -> Unit,
    onImportAudio: () -> Unit,
    onToggleTransport: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val attached by state.audioAttached.collectAsState()
    val tracks = layers.filter { it.kind == LayerKindUi.AUDIO }
    val attachedCount = tracks.count { it.id in attached }

    Column(modifier = modifier.fillMaxSize()) {
        PanelHeader(
            title = stringResource(R.string.panel_audio),
            subtitle = stringResource(R.string.panel_audio_subtitle, attachedCount, tracks.size),
            actions = {
                // Global transport, not a per-track solo: the engine has no gain
                // stage and playback is clocked by the first live track.
                IconButton(
                    onClick = onToggleTransport,
                    enabled = attachedCount > 0,
                ) {
                    Icon(
                        imageVector = if (isPlaying) {
                            Icons.Rounded.Pause
                        } else {
                            Icons.Rounded.PlayArrow
                        },
                        contentDescription = stringResource(
                            if (isPlaying) {
                                R.string.panel_pause_playback
                            } else {
                                R.string.panel_resume_playback
                            },
                        ),
                    )
                }
            },
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
            item {
                Column(verticalArrangement = Arrangement.spacedBy(RumoSpacing.s)) {
                    ActionTile(
                        icon = Icons.Rounded.MusicNote,
                        label = stringResource(R.string.panel_audio_import),
                        onClick = onImportAudio,
                    )
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(
                            imageVector = Icons.Rounded.VolumeUp,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.size(16.dp),
                        )
                        Spacer(modifier = Modifier.size(RumoSpacing.s))
                        Text(
                            text = stringResource(
                                if (isPlaying) {
                                    R.string.panel_audio_playhead_playing
                                } else {
                                    R.string.panel_audio_playhead_paused
                                },
                                formatTime(playheadMs),
                            ),
                            style = MaterialTheme.typography.labelSmall.merge(monoNumerals),
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            if (tracks.isEmpty()) {
                item {
                    PanelEmpty(
                        icon = Icons.Rounded.GraphicEq,
                        title = stringResource(R.string.panel_audio_empty),
                        hint = stringResource(R.string.panel_audio_empty_hint),
                    )
                }
            }
            items(tracks, key = { it.id }) { layer ->
                val live = layer.id in attached
                SelectedRow(
                    selected = layer.id == selectedId,
                    onClick = { onSelect(layer.id) },
                ) {
                    KindMark(kind = LayerKindUi.AUDIO, size = 34.dp)
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            text = layer.name,
                            style = MaterialTheme.typography.bodyMedium,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            text = stringResource(
                                if (live) {
                                    R.string.panel_audio_decoded
                                } else {
                                    R.string.panel_audio_no_player
                                },
                                formatTime(layer.durationMs),
                            ),
                            style = MaterialTheme.typography.labelSmall,
                            color = if (live) {
                                LayerKindUi.AUDIO.mark
                            } else {
                                MaterialTheme.colorScheme.error
                            },
                        )
                    }
                    IconButton(
                        onClick = { state.seekTrackToPlayhead(layer.id) },
                        enabled = live,
                    ) {
                        Icon(
                            imageVector = Icons.Rounded.RestartAlt,
                            contentDescription = stringResource(R.string.panel_audio_seek),
                            modifier = Modifier.size(20.dp),
                        )
                    }
                    IconButton(
                        onClick = { onToggleTransport() },
                        enabled = live,
                    ) {
                        Icon(
                            imageVector = if (isPlaying) {
                                Icons.Rounded.Pause
                            } else {
                                Icons.Rounded.PlayArrow
                            },
                            contentDescription = stringResource(
                                if (isPlaying) {
                                    R.string.panel_pause_playback
                                } else {
                                    R.string.panel_resume_playback
                                },
                            ),
                            modifier = Modifier.size(20.dp),
                        )
                    }
                }
            }
        }
    }
}
