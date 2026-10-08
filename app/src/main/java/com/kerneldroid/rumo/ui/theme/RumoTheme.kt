// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.theme

import android.os.Build
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.material3.MotionScheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.material3.Shapes
import com.materialkolor.PaletteStyle

// AM-like palette: mint accent (AM FAB), amber secondary, sky-blue tertiary,
// deep neutral surfaces shared by home and editor so both read the same.
private val Mint = Color(0xFF00E5A0)
private val Amber = Color(0xFFF2B84B)
private val Sky = Color(0xFF8AB4F8)

private val RumoDarkColorScheme = darkColorScheme(
    primary = Mint,
    onPrimary = Color(0xFF00201A),
    primaryContainer = Color(0xFF00523D),
    onPrimaryContainer = Color(0xFF79F8D1),
    secondary = Amber,
    onSecondary = Color(0xFF3A2A00),
    secondaryContainer = Color(0xFF554000),
    onSecondaryContainer = Color(0xFFFFDF9B),
    tertiary = Sky,
    onTertiary = Color(0xFF002E5A),
    tertiaryContainer = Color(0xFF17476F),
    onTertiaryContainer = Color(0xFFD6E4FF),
    background = Color(0xFF101216),
    onBackground = Color(0xFFE2E3E8),
    surface = Color(0xFF16181D),
    onSurface = Color(0xFFE2E3E8),
    surfaceVariant = Color(0xFF2A2E36),
    onSurfaceVariant = Color(0xFFBEC3CD),
    surfaceContainerLowest = Color(0xFF0C0E12),
    surfaceContainerLow = Color(0xFF181A1F),
    surfaceContainer = Color(0xFF1D2026),
    surfaceContainerHigh = Color(0xFF22262C),
    surfaceContainerHighest = Color(0xFF2D3138),
    outline = Color(0xFF798089),
    outlineVariant = Color(0xFF3A4049),
    error = Color(0xFFFF6B6B),
    onError = Color(0xFF3B0909),
    errorContainer = Color(0xFF5C1616),
    onErrorContainer = Color(0xFFFFDAD6),
    inverseSurface = Color(0xFFE2E3E8),
    inverseOnSurface = Color(0xFF2A2C31),
    inversePrimary = Color(0xFF00B886),
)

private val RumoLightColorScheme = lightColorScheme(
    primary = Mint,
    onPrimary = Color(0xFF00392A),
    primaryContainer = Color(0xFF00523D),
    onPrimaryContainer = Color(0xFF79F8D1),
    secondary = Amber,
    onSecondary = Color(0xFF3A2A00),
    secondaryContainer = Color(0xFFFFDF9B),
    onSecondaryContainer = Color(0xFF3A2A00),
    tertiary = Color(0xFF345E9E),
    onTertiary = Color(0xFFFFFFFF),
    tertiaryContainer = Color(0xFFD6E3FF),
    onTertiaryContainer = Color(0xFF001B3F),
    background = Color(0xFFF4F7F5),
    onBackground = Color(0xFF191C1B),
    surface = Color(0xFFFAFCFB),
    onSurface = Color(0xFF191C1B),
    surfaceVariant = Color(0xFFDDE3DF),
    onSurfaceVariant = Color(0xFF3F4944),
    surfaceContainerLowest = Color(0xFFFFFFFF),
    surfaceContainerLow = Color(0xFFF2F5F3),
    surfaceContainer = Color(0xFFEBEFED),
    surfaceContainerHigh = Color(0xFFE5E9E7),
    surfaceContainerHighest = Color(0xFFDFE4E1),
    outline = Color(0xFF6F7974),
    outlineVariant = Color(0xFFBFC9C4),
    error = Color(0xFFBA1A1A),
    onError = Color(0xFFFFFFFF),
    errorContainer = Color(0xFFFFDAD6),
    onErrorContainer = Color(0xFF410002),
    inverseSurface = Color(0xFF2A2C31),
    inverseOnSurface = Color(0xFFE2E3E8),
    inversePrimary = Color(0xFF00B886),
)

private val RumoSeedColor = Color(0xFF6650A4)

/**
 * Seed-generated M3 scheme via material-kolor (same dep/coordinate as
 * Tomato/CrystalMusic/KArchiver). Used when the system gives no Material You
 * dynamic scheme (pre-31 or dynamic disabled) but seed theming is on.
 */
fun dynamicColorScheme(
    seedColor: Color = RumoSeedColor,
    isDark: Boolean,
    style: PaletteStyle = PaletteStyle.TonalSpot,
): ColorScheme = com.materialkolor.dynamicColorScheme(
    seedColor = seedColor,
    isDark = isDark,
    style = style,
)

private val RumoTypography = Typography()

/**
 * Shapes for the navigation screens (Home / Projects / Template / Settings).
 *
 * This is **not** the editor theme: the editor has its own, flat and restrained
 * one (RumoEditorTheme), and this file does not touch it. Here it is the
 * opposite — a circle and a larger radius on the segments, because on the
 * storefront a large shape is the expressiveness, while in the instrument panel
 * it would get in the way of reading the frame.
 *
 * The roles are set explicitly rather than left at the stock ones: without that
 * `segmentedShapes` gets default corners and the sections look like a cropped
 * rectangle.
 */
private val RumoNavShapes = Shapes(
    extraSmall = RoundedCornerShape(8.dp),
    small = RoundedCornerShape(12.dp),
    medium = RoundedCornerShape(16.dp),
    large = RoundedCornerShape(20.dp),
    largeIncreased = RoundedCornerShape(24.dp),
    extraLarge = RoundedCornerShape(28.dp),
    extraLargeIncreased = RoundedCornerShape(32.dp),
    extraExtraLarge = RoundedCornerShape(36.dp),
)

/**
 * The app colour scheme — one for everything, the editor included.
 *
 * Extracted from [RumoTheme] because the editor needs the same scheme but
 * **dark** regardless of the theme mode: it judges video, and you can only judge
 * it against a neutral dark background, and its categorical clip-kind swatches
 * are picked for the dark ladder. Keeping this in two places is a sure way to
 * drive them apart at the next seed edit.
 */
@Composable
fun rumoColorScheme(
    dark: Boolean,
    dynamic: Boolean,
    seed: Color,
    style: PaletteStyle = PaletteStyle.TonalSpot,
): ColorScheme {
    val context = LocalContext.current
    return when {
        dynamic && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> {
            if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        }
        dynamic -> dynamicColorScheme(seedColor = seed, isDark = dark, style = style)
        dark -> RumoDarkColorScheme
        else -> RumoLightColorScheme
    }
}

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun RumoTheme(
    // Dark by default: the app UI is dark everywhere (home + editor), per the
    // Alight-Motion-like look. Pass isSystemInDarkTheme() to follow the system.
    darkTheme: Boolean = true,
    // Dynamic color master switch. Order: system Material You (API 31+) ->
    // material-kolor seed scheme -> static Rumo palette below.
    dynamicColor: Boolean = true,
    seedColor: Color = RumoSeedColor,
    style: PaletteStyle = PaletteStyle.TonalSpot,
    // Live overrides from SettingsRepo (null = take the base parameter).
    forceDark: Boolean? = null,
    dynamicOverride: Boolean? = null,
    seedOverride: Color? = null,
    content: @Composable () -> Unit,
) {
    // NOTE: animateColorScheme is missing in material3 1.5.0-alpha26 — in Tomato
    // it is com.materialkolor.ktx.animateColorScheme from material-kolor. For now
    // we do not pull the ktx artifact: the scheme changes without animation.
    // Durations are NOT duplicated: all the specs are taken by the screens (W3)
    // from MaterialTheme.motionScheme / LocalMotionScheme (defaultSpatialSpec,
    // slowSpatialSpec, slowEffectsSpec etc.).
    val context = LocalContext.current
    val dark = forceDark ?: darkTheme
    val dynamic = dynamicOverride ?: dynamicColor
    val seed = seedOverride ?: seedColor
    val colorScheme = rumoColorScheme(dark = dark, dynamic = dynamic, seed = seed, style = style)
    MaterialExpressiveTheme(
        colorScheme = colorScheme,
        motionScheme = MotionScheme.expressive(),
        typography = RumoTypography,
        shapes = RumoNavShapes,
        content = content,
    )
}
