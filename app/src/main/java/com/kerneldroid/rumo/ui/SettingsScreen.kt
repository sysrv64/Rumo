// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.Manifest
import android.content.Context
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.os.Build
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.automirrored.rounded.KeyboardArrowRight
import androidx.compose.material.icons.rounded.AutoAwesome
import androidx.compose.material.icons.rounded.Check
import androidx.compose.material.icons.rounded.Language
import androidx.compose.material.icons.rounded.MonitorHeart
import androidx.compose.material.icons.rounded.Palette
import androidx.compose.material.icons.rounded.SaveAlt
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.AppLog
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.ui.nav.NavGroupSpacer
import com.kerneldroid.rumo.ui.nav.NavSegment
import com.kerneldroid.rumo.ui.nav.NavSegmentGap
import com.kerneldroid.rumo.ui.nav.NavSectionHeader
import com.kerneldroid.aiengines.rumi.RumiSettingsSection
import com.kerneldroid.aiengines.rumi.ai.AiSettingsSection
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

// Settings storage with no new deps: SharedPreferences "rumo_prefs"
// (theme_mode, dynamic, seed_argb) + a MutableStateFlow with a listener,
// so RumoTheme in MainActivity recomposes live.
object SettingsRepo {
    const val THEME_DARK = "dark"
    const val THEME_LIGHT = "light"
    const val THEME_SYSTEM = "system"

    private const val PREFS = "rumo_prefs"
    private const val KEY_THEME = "theme_mode"
    private const val KEY_DYNAMIC = "dynamic"
    private const val KEY_SEED = "seed_argb"
    private const val KEY_LANGUAGE = "language"
    private const val DEFAULT_SEED = 0xFF6650A4L

    data class Settings(
        val themeMode: String = THEME_DARK,
        val dynamic: Boolean = true,
        val seedArgb: Long = DEFAULT_SEED,
        /** Short tag (`en`, `ru`, `zh`); see [AppLanguage]. */
        val language: String = AppLanguage.English.tag,
    ) {
        val appLanguage: AppLanguage get() = AppLanguage.parse(language)
    }

    val SeedChoices: List<Long> = listOf(
        0xFF6650A4L,
        0xFF00E5A0L,
        0xFFF2B84BL,
        0xFF8AB4F8L,
        0xFFF44336L,
        0xFF4CAF50L,
    )

    private val _settings = MutableStateFlow(Settings())
    val settings: StateFlow<Settings> = _settings.asStateFlow()

    @Volatile
    private var inited = false
    private var prefs: SharedPreferences? = null
    private val listener =
        SharedPreferences.OnSharedPreferenceChangeListener { _, _ -> reload() }

    fun init(context: Context) {
        if (inited) return
        inited = true
        val p = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        prefs = p
        p.registerOnSharedPreferenceChangeListener(listener)
        reload()
    }

    private fun reload() {
        val p = prefs ?: return
        _settings.value = Settings(
            themeMode = p.getString(KEY_THEME, THEME_DARK) ?: THEME_DARK,
            dynamic = p.getBoolean(KEY_DYNAMIC, true),
            seedArgb = p.getLong(KEY_SEED, DEFAULT_SEED),
            // Absent means "never chosen", which is not the same as "chose
            // English": a fresh install follows the phone's language, and only an
            // explicit pick is written down. Storing the resolved value instead
            // would freeze the first launch's guess forever.
            language = p.getString(KEY_LANGUAGE, null) ?: AppLanguage.fromSystem().tag,
        )
    }

    fun setThemeMode(mode: String) {
        prefs?.edit()?.putString(KEY_THEME, mode)?.apply()
    }

    fun setLanguage(tag: String) {
        prefs?.edit()?.putString(KEY_LANGUAGE, tag)?.apply()
    }

    fun setDynamic(enabled: Boolean) {
        prefs?.edit()?.putBoolean(KEY_DYNAMIC, enabled)?.apply()
    }

    fun setSeed(argb: Long) {
        prefs?.edit()?.putLong(KEY_SEED, argb)?.apply()
    }
}

private val ThemeModes = listOf(
    SettingsRepo.THEME_DARK to R.string.settings_theme_dark,
    SettingsRepo.THEME_LIGHT to R.string.settings_theme_light,
    SettingsRepo.THEME_SYSTEM to R.string.settings_theme_system,
)

// ─── Settings hub ──────────────────────────────────────────────────────────
//
// Settings grew to three groups, and a flat list stopped coping: on a phone that
// is four screens of scrolling for two toggles. So the list became a table of
// contents, and each group's content became its own screen with its own
// header and a "back" button. The same trick as in Tomato: `SettingsMainScreen`
// lists the sections, `AppearanceSettings` / `TimerSettings` / `AlarmSettings`
// show the content.
//
// The sub-screens are routes, not local state: "back" from a sub-tab has to
// return into settings itself, not close the screen.

/**
 * A settings section.
 *
 * The title and summary are resource ids, not strings. An enum constant is built
 * once, at class-load time, so a string captured here would be frozen in
 * whatever language the process happened to start in — the language switch would
 * then leave the table of contents in the old language until the app restarted,
 * while everything around it changed.
 */
private enum class SettingsSection(
    val route: String,
    @StringRes val titleRes: Int,
    @StringRes val summaryRes: Int,
    val icon: ImageVector,
) {
    Appearance(
        Routes.SETTINGS_APPEARANCE,
        R.string.settings_appearance,
        R.string.settings_appearance_summary,
        Icons.Rounded.Palette,
    ),
    Rumi(
        Routes.SETTINGS_RUMI,
        R.string.settings_rumi,
        R.string.settings_rumi_summary,
        Icons.Rounded.AutoAwesome,
    ),
    Diagnostics(
        Routes.SETTINGS_DIAGNOSTICS,
        R.string.settings_diagnostics,
        R.string.settings_diagnostics_summary,
        Icons.Rounded.MonitorHeart,
    ),
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(onBack: () -> Unit, onNavigate: (String) -> Unit) {
    val settings by SettingsRepo.settings.collectAsState()

    NavSettingsScaffold(title = stringResource(R.string.settings_title), onBack = onBack) { padding ->
        LazyColumn(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 40.dp),
            verticalArrangement = Arrangement.spacedBy(NavSegmentGap),
        ) {
            val sections = SettingsSection.entries
            itemsIndexed(sections, key = { _, section -> section.route }) { index, section ->
                NavSegment(
                    index = index,
                    count = sections.size,
                    headline = stringResource(section.titleRes),
                    supporting = when (section) {
                        // A live summary instead of a generic line: how many
                        // colours are selected right now is visible in the table of contents.
                        SettingsSection.Appearance -> when (settings.themeMode) {
                            SettingsRepo.THEME_DARK -> stringResource(R.string.settings_theme_dark)
                            SettingsRepo.THEME_LIGHT -> stringResource(R.string.settings_theme_light)
                            else -> stringResource(R.string.settings_theme_system)
                        } + if (settings.dynamic) {
                            stringResource(R.string.settings_theme_suffix_dynamic)
                        } else {
                            stringResource(R.string.settings_theme_suffix_seed)
                        }

                        else -> stringResource(section.summaryRes)
                    },
                    leadingIcon = section.icon,
                    onClick = { onNavigate(section.route) },
                    trailing = {
                        Icon(Icons.AutoMirrored.Rounded.KeyboardArrowRight, contentDescription = null)
                    },
                )
            }
        }
    }
}

/** Shared header for settings sub-screens: a title + a return to the level above. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun NavSettingsScaffold(
    title: String,
    onBack: () -> Unit,
    content: @Composable (PaddingValues) -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(title) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Rounded.ArrowBack, contentDescription = stringResource(R.string.action_back))
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.surfaceContainer,
                ),
            )
        },
        containerColor = MaterialTheme.colorScheme.surfaceContainer,
        content = content,
    )
}

// ─── Appearance ────────────────────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsAppearanceScreen(onBack: () -> Unit) {
    val settings by SettingsRepo.settings.collectAsState()
    var showLanguage by remember { mutableStateOf(false) }

    NavSettingsScaffold(title = stringResource(R.string.settings_appearance), onBack = onBack) { padding ->
        LazyColumn(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 40.dp),
            verticalArrangement = Arrangement.spacedBy(NavSegmentGap),
        ) {
            item(key = "hdr-theme") { NavSectionHeader(stringResource(R.string.settings_section_theme)) }
            itemsIndexed(ThemeModes, key = { _, (mode, _) -> "theme-$mode" }) { index, (mode, label) ->
                NavSegment(
                    index = index,
                    count = ThemeModes.size,
                    headline = stringResource(label),
                    selected = settings.themeMode == mode,
                    onClick = { SettingsRepo.setThemeMode(mode) },
                )
            }

            item(key = "gap-lang") { NavGroupSpacer() }
            item(key = "hdr-language") {
                NavSectionHeader(stringResource(R.string.settings_section_language))
            }
            item(key = "language") {
                NavSegment(
                    index = 0,
                    count = 1,
                    headline = stringResource(R.string.settings_language),
                    // The current language is named in its own script, so the
                    // row is readable to someone who cannot read the interface.
                    supporting = stringResource(settings.appLanguage.labelRes),
                    leadingIcon = Icons.Rounded.Language,
                    onClick = { showLanguage = true },
                    trailing = {
                        Icon(Icons.AutoMirrored.Rounded.KeyboardArrowRight, contentDescription = null)
                    },
                )
            }

            item(key = "gap-look") { NavGroupSpacer() }
            item(key = "hdr-colors") { NavSectionHeader(stringResource(R.string.settings_section_colors)) }

            item(key = "dynamic") {
                NavSegment(
                    index = 0,
                    count = 1,
                    headline = stringResource(R.string.settings_dynamic_colors),
                    supporting = stringResource(R.string.settings_dynamic_supporting),
                    leadingIcon = Icons.Rounded.Palette,
                    onClick = { SettingsRepo.setDynamic(!settings.dynamic) },
                    trailing = {
                        Switch(checked = settings.dynamic, onCheckedChange = null)
                    },
                )
            }

            item(key = "gap-seed") { NavGroupSpacer() }
            item(key = "seed") {
                SeedSwatchGroup(
                    selected = settings.seedArgb,
                    onSelect = SettingsRepo::setSeed,
                )
            }
        }
    }

    // A dialog rather than a row of chips: three languages is exactly the size
    // where chips look like a filter and a list reads like a choice. It also
    // leaves room for the language's own name, which a chip row would not.
    if (showLanguage) {
        AlertDialog(
            onDismissRequest = { showLanguage = false },
            title = { Text(stringResource(R.string.settings_language)) },
            text = {
                Column {
                    AppLanguage.entries.forEach { language ->
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable {
                                    SettingsRepo.setLanguage(language.tag)
                                    showLanguage = false
                                }
                                .padding(vertical = 12.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(
                                selected = settings.appLanguage == language,
                                // The whole row is the target, so the button
                                // itself must not also be one: a second hit
                                // area inside the first swallows taps.
                                onClick = null,
                            )
                            Spacer(Modifier.width(12.dp))
                            Text(stringResource(language.labelRes))
                        }
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = { showLanguage = false }) {
                    Text(stringResource(R.string.action_close))
                }
            },
        )
    }
}

// ─── Rumi ──────────────────────────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsRumiScreen(onBack: () -> Unit) {
    NavSettingsScaffold(title = stringResource(R.string.settings_rumi), onBack = onBack) { padding ->
        LazyColumn(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 40.dp),
            verticalArrangement = Arrangement.spacedBy(NavSegmentGap),
        ) {
            item(key = "hdr") { NavSectionHeader(stringResource(R.string.settings_section_connection)) }
            item(key = "rumi") { RumiSettingsSection() }
            // Generative services are their own section, not a continuation of
            // "Connection": connecting to a model and being able to create something
            // are different things, and they are enabled independently.
            item(key = "hdr-ai") { NavSectionHeader(stringResource(R.string.settings_section_ai)) }
            item(key = "ai") { AiSettingsSection() }
        }
    }
}

// ─── Diagnostics ───────────────────────────────────────────────────────────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsDiagnosticsScreen(onBack: () -> Unit) {
    var showDiagnostics by remember { mutableStateOf(false) }
    var diagnostics by remember { mutableStateOf<RumoBridge.RenderDiagnostics?>(null) }
    val logContext = LocalContext.current
    val logScope = rememberCoroutineScope()
    var logsSaving by remember { mutableStateOf(false) }
    var pendingLogSave by remember { mutableStateOf(false) }

    // One file (device + render diagnostics + Rust log + AppLog) in
    // Download/Rumo; on failure we show the cause, not "failed".
    fun saveLogs() {
        if (logsSaving) return
        logsSaving = true
        logScope.launch {
            val res = saveAllLogs(logContext)
            logsSaving = false
            val msg = when (res) {
                is SaveResult.Ok -> logContext.getString(R.string.diagnostics_logs_saved, res.path)
                is SaveResult.Failed -> logContext.getString(R.string.diagnostics_logs_failed, res.reason)
            }
            Toast.makeText(logContext, msg, Toast.LENGTH_LONG).show()
        }
    }

    // API 26-28: the public Download is unavailable without WRITE_EXTERNAL_STORAGE.
    val logPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        AppLog.info("storage", "log save: WRITE_EXTERNAL_STORAGE granted=$granted")
        val resume = granted && pendingLogSave
        pendingLogSave = false
        if (resume) {
            saveLogs()
        } else {
            Toast.makeText(
                logContext,
                logContext.getString(R.string.diagnostics_storage_permission),
                Toast.LENGTH_LONG,
            ).show()
        }
    }

    NavSettingsScaffold(title = stringResource(R.string.settings_diagnostics), onBack = onBack) { padding ->
        LazyColumn(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 40.dp),
            verticalArrangement = Arrangement.spacedBy(NavSegmentGap),
        ) {
            item(key = "hdr") { NavSectionHeader(stringResource(R.string.diagnostics_section_render)) }
            item(key = "diag") {
                Column(verticalArrangement = Arrangement.spacedBy(NavSegmentGap)) {
                    NavSegment(
                        index = 0,
                        count = 2,
                        headline = stringResource(R.string.diagnostics_render),
                        supporting = stringResource(R.string.diagnostics_render_supporting),
                        leadingIcon = Icons.Rounded.MonitorHeart,
                        onClick = {
                            diagnostics = RumoBridge.renderDiagnostics()
                            showDiagnostics = true
                        },
                        trailing = {
                            Icon(Icons.AutoMirrored.Rounded.KeyboardArrowRight, contentDescription = null)
                        },
                    )

                    NavSegment(
                        index = 1,
                        count = 2,
                        headline = stringResource(R.string.diagnostics_save_logs),
                        supporting = stringResource(R.string.diagnostics_save_logs_supporting),
                        leadingIcon = Icons.Rounded.SaveAlt,
                        enabled = !logsSaving,
                        onClick = {
                            val needsPermission = Build.VERSION.SDK_INT <= Build.VERSION_CODES.P &&
                                ContextCompat.checkSelfPermission(
                                    logContext,
                                    Manifest.permission.WRITE_EXTERNAL_STORAGE,
                                ) != PackageManager.PERMISSION_GRANTED
                            if (needsPermission) {
                                AppLog.info(
                                    "storage",
                                    "log save: request WRITE_EXTERNAL_STORAGE (API ${Build.VERSION.SDK_INT})",
                                )
                                pendingLogSave = true
                                logPermissionLauncher.launch(Manifest.permission.WRITE_EXTERNAL_STORAGE)
                            } else {
                                saveLogs()
                            }
                        },
                        trailing = {
                            Text(
                                text = stringResource(if (logsSaving) R.string.action_saving else R.string.action_save),
                                style = MaterialTheme.typography.labelLarge,
                                color = MaterialTheme.colorScheme.primary,
                            )
                        },
                    )
                }
            }
        }
    }

    if (showDiagnostics) {
        RenderDiagnosticsDialog(
            diagnostics = diagnostics,
            onRefresh = { diagnostics = RumoBridge.renderDiagnostics() },
            onClear = {
                RumoBridge.clearRenderDiagnostics()
                diagnostics = RumoBridge.renderDiagnostics()
            },
            onDismiss = { showDiagnostics = false },
        )
    }
}

/**
 * The seed picker group: a label and a row of circles, all in one capsule.
 *
 * A separate group rather than a row-segment: swatches are not "a row with a
 * title", and should not pretend to be one. There used to be a "Seed color" row
 * that nothing happened to — a real button that does nothing is worse than no
 * button at all.
 *
 * The shape was chosen for the same reason as the rest of the screen: the circle is
 * filled with the colour, and the selected one is marked with a checkmark and the
 * accent outline, not the outline alone — otherwise it is invisible on a dark seed.
 */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
private fun SeedSwatchGroup(selected: Long, onSelect: (Long) -> Unit) {
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(NavSegmentGap),
    ) {
        NavSectionHeader(stringResource(R.string.settings_seed_color))
        Surface(
            color = ListItemDefaults.segmentedColors().containerColor,
            shape = MaterialTheme.shapes.large,
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(14.dp),
            ) {
                SettingsRepo.SeedChoices.forEach { argb ->
                    val isSelected = selected == argb
                    Box(
                        modifier = Modifier
                            .size(44.dp)
                            .clip(CircleShape)
                            .background(Color(argb))
                            .then(
                                if (isSelected) {
                                    Modifier.border(
                                        3.dp,
                                        MaterialTheme.colorScheme.primary,
                                        CircleShape,
                                    )
                                } else {
                                    Modifier
                                },
                            )
                            .clickable { onSelect(argb) },
                        contentAlignment = Alignment.Center,
                    ) {
                        if (isSelected) {
                            Icon(
                                imageVector = Icons.Rounded.Check,
                                contentDescription = stringResource(R.string.state_selected),
                                tint = Color.White,
                            )
                        }
                    }
                }
            }
        }
    }
}
