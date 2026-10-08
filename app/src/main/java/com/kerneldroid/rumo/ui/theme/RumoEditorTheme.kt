// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.MotionScheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Video editor tokens: a dark neutral instrument panel (TASTE.md).
 *
 * The palette is **derived** from the app's dark scheme ([editorPaletteFrom]),
 * so the hue comes from the seed, like the other tabs. But it is not the scheme:
 * the ladder of steps, the mutedness of the chrome and the place of the accent
 * are set here, because the tool must stay quiet — all the colour in the window
 * belongs to the content, the clips and their kinds.
 *
 * The surface ladder exists exactly to differ from one another in value, with no
 * outlines: `page` → `well` → `panel` → `raised` → `field`. The timeline strips
 * are **darker** than the panel they lie in: the timeline is a well into which
 * the clips are placed, and a strip lighter than the panel would read as content.
 *
 * The source of the direction and the values is the Concat theme
 * (`ui/theme/dark.slint`, `theme.slint`) — the palette theory and the ladder),
 * recalculated for the finger: there the sizes are desktop px, and it is exactly
 * the lack of adaptation for a smartphone that this reference is criticised for.
 */
@Immutable
data class EditorPalette(
    // The surface ladder.
    val page: Color,
    val well: Color,
    val panel: Color,
    val raised: Color,
    val field: Color,
    val fieldHover: Color,
    val fieldActive: Color,
    // Ink.
    val fg: Color,
    val muted: Color,
    val dim: Color,
    // Lines: translucent so they lie on any step.
    val line: Color,
    val lineStrong: Color,
    // Accent: interaction and selection.
    val accent: Color,
    val onAccent: Color,
    val accentWell: Color,
    val onAccentWell: Color,
    // Signal: only the playhead and the "now" line.
    val signal: Color,
    val onSignal: Color,
    val danger: Color,
    val onDanger: Color,
    val dangerWell: Color,
    val onDangerWell: Color,
    /**
     * Whether this is a dark step.
     *
     * Formerly this was derived by a reference comparison with `EditorDark`,
     * which broke on any palette not built from those two constants — and now
     * the palette is built from the app scheme, that is, always "not that one".
     */
    val isDark: Boolean,
)

/** The editor's dark palette: the fallback when there is no scheme. */
val EditorDark = EditorPalette(
    page = Color(0xFF0B0B0D),
    well = Color(0xFF131316),
    panel = Color(0xFF1B1B1F),
    raised = Color(0xFF232329),
    field = Color(0xFF2A2A31),
    fieldHover = Color(0xFF33333B),
    fieldActive = Color(0xFF3D3D47),
    fg = Color(0xFFF2F2F5),
    muted = Color(0xFF9A9AA2),
    dim = Color(0xFF6B6B74),
    line = Color(0x14FFFFFF),
    lineStrong = Color(0x29FFFFFF),
    accent = Color(0xFF00E5A0),
    onAccent = Color(0xFF04231A),
    accentWell = Color(0xFF0E2E26),
    onAccentWell = Color(0xFF7BF3CD),
    // The playhead is the only place that needs a colour different both from the
    // accent and from all clip kinds: it must be visible over any of them. Lime
    // is not taken by any kind (the kinds are purple, pink, green, amber), so it
    // reads as "now" and is confused neither with the selection nor with the
    // audio track.
    signal = Color(0xFFCBF53F),
    onSignal = Color(0xFF1B2200),
    danger = Color(0xFFFF6B6B),
    onDanger = Color(0xFF2A0A0A),
    dangerWell = Color(0xFF3A1A1A),
    onDangerWell = Color(0xFFFFB4B4),
    isDark = true,
)

/**
 * The light palette — in case the editor ever gets one.
 *
 * Deliberately unused right now: video is judged by colour, and you can only
 * judge it against a neutral dark background. All clip kinds are tuned for the
 * dark ladder, and the light one requires them to be recalculated, not swapped
 * in.
 */
val EditorLight = EditorPalette(
    page = Color(0xFFF4F4F6),
    well = Color(0xFFE8E8EC),
    panel = Color(0xFFFFFFFF),
    raised = Color(0xFFF1F1F4),
    field = Color(0xFFE9E9EE),
    fieldHover = Color(0xFFE0E0E6),
    fieldActive = Color(0xFFD6D6DE),
    fg = Color(0xFF15151A),
    muted = Color(0xFF5C5C66),
    dim = Color(0xFF8A8A94),
    line = Color(0x14000000),
    lineStrong = Color(0x29000000),
    accent = Color(0xFF00A874),
    onAccent = Color(0xFFFFFFFF),
    accentWell = Color(0xFFD3F5E8),
    onAccentWell = Color(0xFF00654A),
    signal = Color(0xFF7A9200),
    onSignal = Color(0xFFFFFFFF),
    danger = Color(0xFFC53030),
    onDanger = Color(0xFFFFFFFF),
    dangerWell = Color(0xFFFBE3E3),
    onDangerWell = Color(0xFF7A1B1B),
    isDark = false,
)

/** The current editor palette inside [RumoEditorTheme]. */
val MaterialTheme.editor: EditorPalette
    @Composable get() = LocalEditorPalette.current

/**
 * The editor ladder derived from the app scheme.
 *
 * ## Why a derivation and not a substitution
 *
 * Formerly the editor carried its own palette of hex constants and did **not**
 * obey dynamic colour: the doc comment of [EditorPalette] said so outright. The
 * user overruled that, but simply substituting `MaterialTheme.colorScheme` here
 * is not possible — the editor has its own task, and it rests on two things:
 *
 * 1. **The steps of the ladder must differ from one another.** `page` → `well` →
 *    `panel` → `raised` → `field` are not shades of one colour but five levels of
 *    nesting. If you take the scheme roles as is, in some schemes the steps come
 *    together, and the panel stops reading as a panel.
 * 2. **The timeline strips are darker than the panel they lie in.** The timeline
 *    is a well into which clips are placed; a strip lighter than the panel would
 *    read as content.
 *
 * So the steps are taken from the neutral ladder of the scheme (with
 * material-kolor it is monotone by construction), while the ink and the accent
 * come from its roles. The hue comes from the seed, the structure stays.
 *
 * ## What stays fixed
 *
 * `signal` is the playhead. It must be visible over a clip of any kind, and the
 * kinds (purple, pink, green, amber) are fixed in [RumoKind], because that is
 * identification of content, not chrome. A scheme role cannot be taken for the
 * playhead: the seed may give a colour matching a clip kind, and "now" would stop
 * reading.
 */
fun editorPaletteFrom(scheme: ColorScheme, isDark: Boolean): EditorPalette {
    // A semi-tone between two steps — the scheme's steps are spaced wider than
    // the pressed and hovered field states need.
    fun lift(base: Color, toward: Color, amount: Float): Color =
        lerp(base, toward, amount)

    val ink = scheme.onSurface
    val inkMuted = scheme.onSurfaceVariant
    val field = scheme.surfaceContainerHighest

    return EditorPalette(
        page = scheme.surfaceContainerLowest,
        well = scheme.surfaceContainerLow,
        panel = scheme.surfaceContainer,
        raised = scheme.surfaceContainerHigh,
        field = field,
        fieldHover = lift(field, ink, 0.05f),
        fieldActive = lift(field, ink, 0.11f),
        fg = ink,
        muted = inkMuted,
        // `dim` is quieter than `muted`: a caption that must not fight the value.
        dim = lift(inkMuted, scheme.surfaceContainer, 0.35f),
        // The lines are translucent so they lie on any step the same way.
        line = scheme.outlineVariant.copy(alpha = 0.5f),
        lineStrong = scheme.outline.copy(alpha = 0.8f),
        accent = scheme.primary,
        onAccent = scheme.onPrimary,
        accentWell = scheme.primaryContainer,
        onAccentWell = scheme.onPrimaryContainer,
        // Lime: not from the scheme, see the doc comment.
        signal = PlayheadSignal,
        onSignal = OnPlayheadSignal,
        danger = scheme.error,
        onDanger = scheme.onError,
        dangerWell = scheme.errorContainer,
        onDangerWell = scheme.onErrorContainer,
        isDark = isDark,
    )
}

/**
 * The playhead colour.
 *
 * The only editor colour that stays outside the scheme: it must be visible over
 * a clip of any kind, and the kinds are fixed. Lime is not taken by any of them.
 */
val PlayheadSignal = Color(0xFFCBF53F)
val OnPlayheadSignal = Color(0xFF1B2200)

/**
 * The M3 roles the panels use, mapped onto the ladder.
 *
 * A mapping, not a rewrite of the panels: a panel written against
 * `MaterialTheme.colorScheme.surfaceContainer` lands on the right step with not a
 * single change in itself. `secondaryContainer` goes to `raised` — formerly it
 * filled the selected navigation slot, and exactly that fill on every slot read
 * as "MVP" (TASTE.md).
 */
fun EditorPalette.toColorScheme(): ColorScheme {
    val base = if (isDark) darkColorScheme() else lightColorScheme()
    return base.copy(
        primary = accent,
        onPrimary = onAccent,
        primaryContainer = accentWell,
        onPrimaryContainer = onAccentWell,
        secondary = muted,
        onSecondary = panel,
        secondaryContainer = raised,
        onSecondaryContainer = fg,
        tertiary = muted,
        onTertiary = panel,
        tertiaryContainer = raised,
        onTertiaryContainer = fg,
        background = page,
        onBackground = fg,
        surface = panel,
        onSurface = fg,
        surfaceVariant = raised,
        onSurfaceVariant = muted,
        surfaceContainerLowest = well,
        surfaceContainerLow = panel,
        surfaceContainer = raised,
        surfaceContainerHigh = field,
        surfaceContainerHighest = fieldActive,
        surfaceDim = page,
        surfaceBright = fieldActive,
        outline = lineStrong,
        outlineVariant = line,
        error = danger,
        onError = onDanger,
        errorContainer = dangerWell,
        onErrorContainer = onDangerWell,
        scrim = Color(0xCC000000),
    )
}

/**
 * The editor scale: contrast between caption and value.
 *
 * It is this, and not the typeface, that distinguishes a tool from a toy — the
 * caption is small and muted, the value larger and brighter. There is
 * deliberately no font of its own: that is a resource and a licence, while the
 * gain comes from the scale, not the typeface.
 */
object EditorType {
    private val tnum = "tnum"

    /** Micro-caption: units, states, axis labels. */
    val micro = TextStyle(fontSize = 10.sp, lineHeight = 12.sp, fontWeight = FontWeight.Medium)

    /** Caption: a field name, a group header. */
    val label = TextStyle(fontSize = 11.sp, lineHeight = 14.sp, fontWeight = FontWeight.Medium)

    /** Body: list rows, clip captions. */
    val body = TextStyle(fontSize = 12.sp, lineHeight = 16.sp)

    /** Value: the number in a field, the name of the selected object. */
    val value = TextStyle(fontSize = 14.sp, lineHeight = 18.sp, fontWeight = FontWeight.Medium)

    /** Panel title. */
    val title = TextStyle(fontSize = 17.sp, lineHeight = 22.sp, fontWeight = FontWeight.Medium)

    /** Timecode: the only place with monospaced digits and weight 600. */
    val timecode = TextStyle(
        fontFamily = FontFamily.Monospace,
        fontFeatureSettings = tnum,
        fontSize = 24.sp,
        lineHeight = 28.sp,
        fontWeight = FontWeight.SemiBold,
    )

    /** Small timecode: transport, clips. */
    val timecodeSmall = TextStyle(
        fontFamily = FontFamily.Monospace,
        fontFeatureSettings = tnum,
        fontSize = 11.sp,
        lineHeight = 14.sp,
        fontWeight = FontWeight.Medium,
    )
}

/** The M3 roles on the editor scale, so that the existing panels read correctly. */
fun editorTypography(): Typography {
    val base = Typography()
    return base.copy(
        labelSmall = EditorType.micro,
        labelMedium = EditorType.label,
        labelLarge = EditorType.body,
        bodySmall = EditorType.body,
        bodyMedium = EditorType.body.copy(fontSize = 13.sp, lineHeight = 18.sp),
        bodyLarge = EditorType.value,
        titleSmall = EditorType.value,
        titleMedium = EditorType.title,
        titleLarge = EditorType.title,
        headlineSmall = EditorType.timecode,
    )
}

/**
 * The editor radii.
 *
 * No pills at all: a filled pill on every navigation slot is exactly the "MVP"
 * look. `extraSmall` is a field, `small` a card and a clip, `medium` a panel and
 * a sheet, `large` only a modal window.
 */
fun editorShapes(): Shapes = Shapes(
    extraSmall = RoundedCornerShape(4.dp),
    small = RoundedCornerShape(6.dp),
    medium = RoundedCornerShape(8.dp),
    large = RoundedCornerShape(12.dp),
    extraLarge = RoundedCornerShape(16.dp),
)

/**
 * The editor theme.
 *
 * A wrapper around the editor content, not the theme of the whole app: the home
 * screen and the Rumi chat stay on their own theme, they have a different task.
 *
 * ## Colour
 *
 * The scheme comes from outside — the same as the whole app's, and **in the same
 * mode**: a light theme gives a light editor. That way the editor obeys dynamic
 * colour, the seed and the user's choice, like the other tabs. The palette is
 * derived from it ([editorPaletteFrom]) rather than substituted: the editor has
 * its own ladder of steps, and it must be preserved in both modes — the steps are
 * taken from the neutral ladder of the scheme, which is monotone by construction
 * in the light one too.
 *
 * [isDark] is needed separately from the scheme, because it selects the base M3
 * scheme ([EditorPalette.toColorScheme]): roles that are not in the palette are
 * taken from it.
 *
 * [MotionScheme.standard] instead of expressive: spring and bounce in the editor
 * chrome are part of the same "toy-ness". Direct manipulation (playhead, scrub,
 * drag) is not animated at all, and that is in the components, not here.
 */
@Composable
fun RumoEditorTheme(
    scheme: ColorScheme,
    isDark: Boolean = true,
    content: @Composable () -> Unit,
) {
    val palette = editorPaletteFrom(scheme, isDark = isDark)
    androidx.compose.runtime.CompositionLocalProvider(LocalEditorPalette provides palette) {
        MaterialTheme(
            colorScheme = palette.toColorScheme(),
            shapes = editorShapes(),
            typography = editorTypography(),
            motionScheme = MotionScheme.standard(),
            content = content,
        )
    }
}

/** The current palette; see [MaterialTheme.editor]. */
val LocalEditorPalette = androidx.compose.runtime.staticCompositionLocalOf { EditorDark }
