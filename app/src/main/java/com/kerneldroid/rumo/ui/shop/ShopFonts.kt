// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.shop

import android.content.Context
import androidx.compose.foundation.Image
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.CheckCircle
import androidx.compose.material.icons.rounded.Download
import androidx.compose.material.icons.rounded.Search
import androidx.compose.material.icons.rounded.TextFields
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedListItem
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.ui.DockContentInset
import com.kerneldroid.rumo.data.FontCatalog
import com.kerneldroid.rumo.data.FontStore
import com.kerneldroid.rumo.data.GoogleFonts
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.data.ShopPrefs
import com.kerneldroid.rumo.ui.nav.NavEmptyState
import com.kerneldroid.rumo.ui.nav.NavSectionHeader
import com.kerneldroid.rumo.ui.nav.NavSegmentGap
import com.kerneldroid.rumo.ui.nav.navSegmentedColors
import com.kerneldroid.rumo.ui.nav.navSegmentedShapes
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * The font section: the Google Fonts catalogue, a preview of every family and
 * installation.
 *
 * ## Why the engine draws the preview
 *
 * The preview is `nativeFontPreview`: the same shaper, the same atlas and the
 * same composer as a text layer. Drawing the preview with platform means
 * (`Typeface` + `Canvas`) would have been shorter, but then the storefront would
 * show one picture while the editor drew another, and the discrepancy would
 * surface for the user, not here.
 *
 * ## Why preview loading is limited
 *
 * The catalogue has about two thousand families, and "show a preview for every
 * one" literally means downloading the whole catalogue. So a preview is only
 * fetched for the rows the user actually sees, the face is cached on disk, and
 * the preview engine keeps the last twelve faces and is rebuilt beyond that. The
 * "preview in list" setting turns loading off entirely — then the preview lives
 * in the detail sheet.
 */
internal class ShopFontsState(
    private val context: Context,
    private val scope: CoroutineScope,
    val images: ImageLoader,
) {
    var catalog by mutableStateOf<List<GoogleFonts.Family>>(emptyList())
    var loading by mutableStateOf(false)
    var error by mutableStateOf<String?>(null)
    var query by mutableStateOf("")
    var category by mutableStateOf("")
    var latinOnly by mutableStateOf(true)
    var installed by mutableStateOf<List<FontStore.Font>>(emptyList())
    var busy by mutableStateOf<Set<String>>(emptySet())
    var selected by mutableStateOf<GoogleFonts.Family?>(null)

    /** Families whose preview is being drawn right now. */
    var previewsInFlight by mutableStateOf<Set<String>>(emptySet())

    private val faces = HashMap<String, GoogleFonts.Face?>()
    private val faceInFlight = HashSet<String>()
    private val previewRequested = HashSet<String>()

    /** How many faces were downloaded for previews during this session. */
    private var previewCount = 0

    /**
     * Reads the installed fonts from disk.
     *
     * Blocking: do not call on the main thread. Kept for cases when the list must
     * be re-read after a change (install, remove) — on entering the tab the list
     * arrives through [adoptInstalled] already read.
     */
    suspend fun refreshInstalled() {
        adoptInstalled(withContext(Dispatchers.IO) { FontStore.installed(context) })
    }

    /** Accepts a list already read from disk. */
    fun adoptInstalled(list: List<FontStore.Font>) {
        installed = list
    }

    fun load(force: Boolean) {
        if (loading) return
        loading = true
        error = null
        scope.launch {
            val snapshot = FontCatalog.load(context, force)
            catalog = snapshot?.families ?: emptyList()
            if (snapshot == null) error = context.getString(R.string.shop_fonts_catalogue_unavailable)
            loading = false
        }
    }

    /** The engine's installed families: the row uses them to tell what is already in. */
    fun installedFor(family: GoogleFonts.Family): FontStore.Font? =
        installed.firstOrNull { it.family == family.name || it.displayName == family.name }

    /**
     * Asks for a family preview.
     *
     * Once per family and with a shared per-session limit: the list is long, and
     * without a limit scrolling to the end would download the whole catalogue.
     */
    fun requestPreview(family: GoogleFonts.Family, sizePx: Int, argb: Int) {
        val key = previewKey(family.name, sizePx, argb)
        if (images.bitmap(key) != null || images.hasFailed(key)) return
        if (!previewRequested.add(key)) return
        if (previewCount >= PREVIEW_BUDGET) {
            images.markFailed(key)
            return
        }
        previewCount++
        previewsInFlight = previewsInFlight + family.name
        scope.launch {
            val bytes = fontBytes(family)
            val image = if (bytes == null) {
                null
            } else {
                renderPreview(family.name, family.name, bytes, sizePx, argb)
            }
            if (image == null) images.markFailed(key) else images.put(key, image)
            previewsInFlight = previewsInFlight - family.name
        }
    }

    /**
     * A preview of an installed font.
     *
     * Separate from [requestPreview], because there is nothing to download: the
     * bytes are already on disk. An installed font's row therefore shows itself
     * at once instead of waiting for the network.
     */
    fun requestInstalledPreview(font: FontStore.Font, sizePx: Int, argb: Int) {
        val key = previewKey(font.displayName, sizePx, argb)
        if (images.bitmap(key) != null || images.hasFailed(key)) return
        if (!previewRequested.add(key)) return
        previewsInFlight = previewsInFlight + font.displayName
        scope.launch {
            val bytes = withContext(Dispatchers.IO) { FontStore.bytes(context, font.family) }
            val image = if (bytes == null) {
                null
            } else {
                // The text is the list caption, while the face is addressed by
                // the engine name: for a downloaded file they may not match.
                renderPreview(font.family, font.displayName, bytes, sizePx, argb)
            }
            if (image == null) images.markFailed(key) else images.put(key, image)
            previewsInFlight = previewsInFlight - font.displayName
        }
    }

    /** A ready preview, if it is already drawn. */
    fun preview(family: GoogleFonts.Family, sizePx: Int, argb: Int): ImageBitmap? =
        images.bitmap(previewKey(family.name, sizePx, argb))

    /** A preview of an installed font — by name, without a catalogue entry. */
    fun previewByName(name: String, sizePx: Int, argb: Int): ImageBitmap? =
        images.bitmap(previewKey(name, sizePx, argb))

    fun isPreviewing(name: String): Boolean = name in previewsInFlight

    fun isBusy(family: GoogleFonts.Family): Boolean = family.name in busy

    /**
     * Installation: the face, the licence text, the record in storage.
     *
     * The licence is downloaded right here, because Google Fonts requires it to
     * be distributed together with the font, not a single copyright line shown.
     */
    fun install(family: GoogleFonts.Family) {
        if (family.name in busy) return
        busy = busy + family.name
        scope.launch {
            val face = face(family)
            val result = if (face == null) {
                null
            } else {
                withContext(Dispatchers.IO) {
                    // The install limit, not the preview one: installation is an
                    // explicit action by the user, and the file here is the one
                    // they asked for.
                    val reply = GoogleFonts.download(face, INSTALL_LIMIT_BYTES)
                    if (!reply.ok || reply.bytes.isEmpty()) {
                        null
                    } else {
                        val entry = FontStore.install(
                            context = context,
                            displayName = family.name,
                            source = SOURCE,
                            license = face.license,
                            copyright = face.copyright,
                            bytes = reply.bytes,
                        )
                        if (entry != null) {
                            GoogleFonts.licenseText(face)?.let {
                                FontStore.saveLicense(context, entry, it)
                            }
                        }
                        entry
                    }
                }
            }
            busy = busy - family.name
            if (result == null) {
                error = context.getString(R.string.shop_fonts_install_failed, family.name)
            } else {
                error = null
                refreshInstalled()
                // The installed font lives in the main engine — previews come
                // from it too, and a repeated download for display is no longer
                // needed.
                invalidatePreview(family)
            }
        }
    }

    fun remove(font: FontStore.Font) {
        busy = busy + font.displayName
        scope.launch {
            withContext(Dispatchers.IO) { FontStore.remove(context, font.family) }
            busy = busy - font.displayName
            refreshInstalled()
        }
    }

    fun setDefault(family: String) {
        ShopPrefs.setDefaultFontFamily(family)
    }

    private fun invalidatePreview(family: GoogleFonts.Family) {
        previewRequested.removeAll { it.startsWith(family.name + "@") }
    }

    /** The face bytes: from storage if installed, otherwise downloaded. */
    private suspend fun fontBytes(family: GoogleFonts.Family): ByteArray? {
        val local = installedFor(family)
        if (local != null) {
            FontStore.bytes(context, local.family)?.let { return it }
        }
        val face = face(family) ?: return null
        return withContext(Dispatchers.IO) {
            val reply = GoogleFonts.download(face, PREVIEW_LIMIT_BYTES)
            if (reply.ok && reply.bytes.isNotEmpty()) reply.bytes else null
        }
    }

    /** A family face with a cache: probing buckets costs requests, no point repeating it. */
    private suspend fun face(family: GoogleFonts.Family): GoogleFonts.Face? {
        faces[family.name]?.let { return it }
        if (family.name in faceInFlight) return null
        faceInFlight += family.name
        val resolved = withContext(Dispatchers.IO) { GoogleFonts.resolveFace(family) }
        faceInFlight -= family.name
        faces[family.name] = resolved
        return resolved
    }

    private suspend fun renderPreview(
        family: String,
        text: String,
        bytes: ByteArray,
        sizePx: Int,
        argb: Int,
    ): ImageBitmap? = withContext(Dispatchers.Default) {
        val decoded = RumoBridge.fontPreview(
            family = family,
            fontBytes = bytes,
            text = text,
            sizePx = sizePx.toFloat(),
            weight = 400,
            argb = argb,
            pad = 2,
        ) ?: return@withContext null
        decodedToImage(decoded.width, decoded.height, decoded.rgba)
    }

    companion object {
        const val SOURCE = "google-fonts"

        /** How many faces are downloaded for previews per session. */
        const val PREVIEW_BUDGET = 60

        /** The face weight limit for a preview: more is already megabytes per row. */
        const val PREVIEW_LIMIT_BYTES = 3L * 1024L * 1024L

        /**
         * The face weight limit for installation.
         *
         * Separate from the preview limit and much higher: a variable font is one
         * file with all the axes, and it is several times bigger than a static
         * one (Google Sans Flex is 4.15 MB). A shared limit would have meant
         * "variable fonts cannot be installed", which is what it was: the preview
         * did not fit, and the install button went dark along with it.
         */
        const val INSTALL_LIMIT_BYTES = 64L * 1024L * 1024L

        /**
         * The preview key: name, size and **colour**.
         *
         * The colour is part of the key because it is part of the picture. Dark
         * ink on a dark theme gives exactly what is seen as empty space, so the
         * colour is taken from the theme (`onSurface`) rather than set by a
         * constant; without it in the key, a theme change would leave a picture
         * in the cache that is invisible on the new background.
         */
        fun previewKey(name: String, sizePx: Int, argb: Int): String =
            "font:$name@$sizePx#${argb.toUInt().toString(16)}"
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ShopFontsSection(
    state: ShopFontsState,
    modifier: Modifier = Modifier,
) {
    val density = LocalDensity.current
    val previewPx = remember(density) { with(density) { 22.dp.toPx() }.toInt().coerceIn(16, 96) }
    // The preview ink is the theme's text colour, not a constant. A dark theme
    // with a dark constant gave exactly what looks like an empty line: the
    // letters are there, but invisible on a surface of the same tone.
    val inkArgb = MaterialTheme.colorScheme.onSurface.toArgb()



    val visible = remember(state.catalog, state.query, state.category, state.latinOnly) {
        filterCatalog(state.catalog, state.query, state.category, state.latinOnly)
    }

    Column(modifier = modifier.fillMaxSize()) {
        OutlinedTextField(
            value = state.query,
            onValueChange = { state.query = it },
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp),
            singleLine = true,
            leadingIcon = { Icon(Icons.Rounded.Search, contentDescription = null) },
            placeholder = { Text(stringResource(R.string.shop_fonts_search)) },
        )
        // One scrollable row, not wrapping onto lines. There are seven filters,
        // and on a narrow screen wrapping gave two or three rows of different
        // lengths — the list started at different heights depending on which chip
        // was selected. Scrolling keeps the height constant and does not hide the
        // filters past the visible part: they are there, just further right.
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            FilterChip(
                selected = state.category.isEmpty(),
                onClick = { state.category = "" },
                label = { Text(stringResource(R.string.shop_fonts_filter_all)) },
            )
            CATEGORIES.forEach { (id, labelRes) ->
                FilterChip(
                    selected = state.category == id,
                    onClick = { state.category = if (state.category == id) "" else id },
                    label = { Text(stringResource(labelRes)) },
                )
            }
            FilterChip(
                selected = state.latinOnly,
                onClick = { state.latinOnly = !state.latinOnly },
                label = { Text(stringResource(R.string.shop_fonts_filter_latin)) },
            )
        }

        state.error?.let { message ->
            Text(
                text = message,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }

        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            contentPadding = PaddingValues(
                start = 16.dp,
                end = 16.dp,
                top = 4.dp,
                bottom = DockContentInset + 24.dp,
            ),
            verticalArrangement = Arrangement.spacedBy(NavSegmentGap),
        ) {
            if (state.installed.isNotEmpty()) {
                item(key = "installed-header") {
                    NavSectionHeader(stringResource(R.string.shop_installed))
                }
                itemsIndexed(
                    state.installed,
                    key = { _, f -> "installed:${f.family}" },
                ) { index, font ->
                    val preview = state.previewByName(font.displayName, previewPx, inkArgb)
                    if (preview == null) {
                        LaunchedEffect(font.family, previewPx, inkArgb) {
                            state.requestInstalledPreview(font, previewPx, inkArgb)
                        }
                    }
                    SegmentedListItem(
                        selected = false,
                        onClick = { state.setDefault(font.family) },
                        shapes = navSegmentedShapes(index, state.installed.size),
                        colors = navSegmentedColors(),
                        modifier = Modifier.fillMaxWidth(),
                        content = {
                            FontNameLine(preview, font.displayName, installed = true)
                        },
                        supportingContent = {
                            Text(
                                text = if (ShopPrefs.state.value.defaultFontFamily == font.family) {
                                    stringResource(R.string.shop_fonts_default_for_new, font.license)
                                } else {
                                    stringResource(R.string.shop_fonts_tap_to_default, font.license)
                                },
                                maxLines = 1,
                            )
                        },
                        trailingContent = {
                            Icon(
                                imageVector = Icons.Rounded.CheckCircle,
                                contentDescription = null,
                                tint = MaterialTheme.colorScheme.primary,
                            )
                        },
                    )
                }
                item(key = "catalog-header") {
                    NavSectionHeader(stringResource(R.string.shop_fonts_catalog_header))
                }
            }

            if (visible.isEmpty()) {
                item(key = "empty") {
                    NavEmptyState(
                        title = if (state.loading) {
                            stringResource(R.string.shop_fonts_empty_loading_title)
                        } else {
                            stringResource(R.string.shop_fonts_empty_title)
                        },
                        message = if (state.loading) {
                            stringResource(R.string.shop_fonts_empty_loading_message)
                        } else {
                            stringResource(R.string.shop_fonts_empty_message)
                        },
                        icon = Icons.Rounded.TextFields,
                        actionLabel = if (state.loading) {
                            null
                        } else {
                            stringResource(R.string.shop_fonts_clear_filters)
                        },
                        onAction = {
                            state.query = ""
                            state.category = ""
                            state.latinOnly = false
                        },
                    )
                }
            }

            itemsIndexed(visible, key = { _, f -> "cat:${f.name}" }) { index, family ->
                val preview = state.preview(family, previewPx, inkArgb)
                // A preview is requested only for the rows that reached
                // composition — that is, for the visible ones.
                if (ShopPrefs.state.value.previewInList && preview == null) {
                    LaunchedEffect(family.name, previewPx, inkArgb) {
                        state.requestPreview(family, previewPx, inkArgb)
                    }
                }
                val entry = state.installedFor(family)
                FontCatalogRow(
                    index = index,
                    count = visible.size,
                    family = family,
                    preview = preview,
                    loading = state.isPreviewing(family.name),
                    installed = entry != null,
                    busy = state.isBusy(family),
                    onOpen = { state.selected = family },
                )
            }
        }
    }

    state.selected?.let { family ->
        FontDetailSheet(
            family = family,
            state = state,
            previewPx = (previewPx * 2).coerceAtMost(160),
            inkArgb = inkArgb,
            onDismiss = { state.selected = null },
        )
    }
}

@Composable
private fun FontNameLine(
    preview: ImageBitmap?,
    name: String,
    installed: Boolean,
) {
    if (preview != null) {
        Image(
            bitmap = preview,
            contentDescription = name,
            modifier = Modifier
                .fillMaxWidth()
                .height(26.dp),
            contentScale = ContentScale.Fit,
            alignment = Alignment.CenterStart,
        )
    } else {
        Text(
            text = name,
            maxLines = 1,
            style = MaterialTheme.typography.titleMedium,
            color = if (installed) {
                MaterialTheme.colorScheme.primary
            } else {
                MaterialTheme.colorScheme.onSurface
            },
        )
    }
}

@Composable
private fun FontCatalogRow(
    index: Int,
    count: Int,
    family: GoogleFonts.Family,
    preview: ImageBitmap?,
    loading: Boolean,
    installed: Boolean,
    busy: Boolean,
    onOpen: () -> Unit,
) {
    SegmentedListItem(
        selected = false,
        onClick = onOpen,
        shapes = navSegmentedShapes(index, count),
        colors = navSegmentedColors(),
        modifier = Modifier.fillMaxWidth(),
        content = {
            if (preview != null) {
                Image(
                    bitmap = preview,
                    contentDescription = family.name,
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(26.dp),
                    contentScale = ContentScale.Fit,
                    alignment = Alignment.CenterStart,
                )
            } else {
                Text(
                    text = family.name,
                    maxLines = 1,
                    style = MaterialTheme.typography.titleMedium,
                )
            }
        },
        supportingContent = {
            val categoryLabel = fontCategoryLabel(family.category)
            val loadingSuffix = stringResource(R.string.shop_fonts_loading_preview)
            Text(
                text = buildString {
                    append(categoryLabel)
                    if (family.weights.isNotEmpty()) {
                        append(" · ")
                        append(family.weights.joinToString("/"))
                    }
                    if (loading) append(loadingSuffix)
                },
                maxLines = 1,
            )
        },
        trailingContent = {
            when {
                busy -> Text("…", style = MaterialTheme.typography.titleMedium)
                installed -> Icon(
                    imageVector = Icons.Rounded.CheckCircle,
                    contentDescription = stringResource(R.string.shop_installed),
                    tint = MaterialTheme.colorScheme.primary,
                )
                else -> Icon(
                    imageVector = Icons.Rounded.Download,
                    contentDescription = stringResource(R.string.shop_fonts_download_cd),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
    )
}

/**
 * A family card: previews in two sizes, metadata and installation.
 *
 * Separate from the list row, because the list cannot afford to download a face
 * for every row while the card can: the user opened it themselves.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FontDetailSheet(
    family: GoogleFonts.Family,
    state: ShopFontsState,
    previewPx: Int,
    inkArgb: Int,
    onDismiss: () -> Unit,
) {
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)

    var face by remember(family.name) { mutableStateOf<GoogleFonts.Face?>(null) }
    var bytes by remember(family.name) { mutableStateOf<ByteArray?>(null) }
    var license by remember(family.name) { mutableStateOf<String?>(null) }
    var failed by remember(family.name) { mutableStateOf(false) }
    // The face is there but the preview will not be: the file is bigger than the
    // preview limit. This is not a load failure and not a reason to forbid
    // installation — variable fonts weigh megabytes (Google Sans Flex is 4.15 MB
    // at a 3 MB preview limit), and "preview unavailable" used to mean "cannot
    // download either".
    var previewTooLarge by remember(family.name) { mutableStateOf(false) }

    // The card downloads the face itself: the list did not, and without the
    // download there would be nothing to show but the name in the ordinary font.
    LaunchedEffect(family.name) {
        val result = withContext(Dispatchers.IO) {
            val resolved = GoogleFonts.resolveFace(family) ?: return@withContext null
            val reply = GoogleFonts.download(resolved, ShopFontsState.PREVIEW_LIMIT_BYTES)
            if (reply.ok && reply.bytes.isNotEmpty()) {
                Triple(resolved, reply.bytes, GoogleFonts.licenseText(resolved))
            } else {
                // Metadata matters more than the picture: installation works off it.
                Triple(resolved, null, null)
            }
        }
        if (result == null) {
            failed = true
        } else {
            face = result.first
            bytes = result.second
            license = result.third
            previewTooLarge = result.second == null
        }
    }

    val bigPreview = remember(family.name, bytes, previewPx, inkArgb) {
        val data = bytes ?: return@remember null
        val decoded = RumoBridge.fontPreview(
            family = family.name,
            fontBytes = data,
            text = family.name,
            sizePx = previewPx.toFloat(),
            weight = 400,
            argb = inkArgb,
            pad = 4,
        ) ?: return@remember null
        decodedToImage(decoded.width, decoded.height, decoded.rgba)
    }

    val installedEntry = state.installedFor(family)
    val isDefault = ShopPrefs.state.value.defaultFontFamily == installedEntry?.family

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheet) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 24.dp)
                .padding(bottom = 32.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(family.name, style = MaterialTheme.typography.headlineSmall)

            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(96.dp),
                contentAlignment = Alignment.CenterStart,
            ) {
                when {
                    bigPreview != null -> Image(
                        bitmap = bigPreview,
                        contentDescription = family.name,
                        modifier = Modifier.fillMaxWidth(),
                        contentScale = ContentScale.Fit,
                        alignment = Alignment.CenterStart,
                    )
                    failed -> Text(
                        text = stringResource(R.string.shop_fonts_preview_unavailable),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    else -> Text(
                        text = stringResource(R.string.shop_fonts_preview_loading),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            val categoryLabel = fontCategoryLabel(family.category)
            val italicSuffix = stringResource(R.string.shop_fonts_italic)
            Text(
                text = buildString {
                    append(categoryLabel)
                    append(" · ")
                    append(family.weights.joinToString("/").ifEmpty { "—" })
                    if (family.hasItalic) append(italicSuffix)
                },
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                text = stringResource(
                    R.string.shop_fonts_subsets,
                    family.subsets.joinToString(", ").ifEmpty { "—" },
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            face?.let { f ->
                Text(
                    text = stringResource(
                        R.string.shop_fonts_file,
                        f.fileName,
                        f.bucket,
                        f.license,
                    ),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (f.copyright.isNotEmpty()) {
                    Text(
                        text = f.copyright,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            license?.let { text ->
                Text(
                    text = text.lineSequence().firstOrNull { it.isNotBlank() }
                        ?: stringResource(R.string.shop_fonts_licence),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                if (installedEntry == null) {
                    // Installation depends only on the face metadata, not on
                    // whether the face fit within the preview limit.
                    Button(
                        onClick = { state.install(family) },
                        enabled = face != null && !state.isBusy(family),
                    ) {
                        Icon(
                            Icons.Rounded.Download,
                            contentDescription = null,
                            modifier = Modifier.size(18.dp),
                        )
                        Text(
                            text = if (state.isBusy(family)) {
                                stringResource(R.string.shop_fonts_installing)
                            } else {
                                stringResource(R.string.shop_fonts_install)
                            },
                        )
                    }
                } else {
                    Button(onClick = { state.setDefault(installedEntry.family) }) {
                        Text(
                            if (isDefault) {
                                stringResource(R.string.shop_fonts_default_button)
                            } else {
                                stringResource(R.string.shop_fonts_make_default)
                            },
                        )
                    }
                    TextButton(
                        onClick = {
                            state.remove(installedEntry)
                            onDismiss()
                        },
                    ) {
                        Text(stringResource(R.string.shop_fonts_remove))
                    }
                }
            }

            if (previewTooLarge) {
                Text(
                    text = stringResource(
                        R.string.shop_fonts_preview_too_large,
                        ShopFontsState.PREVIEW_LIMIT_BYTES / (1024 * 1024),
                    ),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (installedEntry == null && face == null && !failed) {
                Text(
                    text = stringResource(R.string.shop_fonts_install_pending),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun rememberCoroutineScopeSafe(): CoroutineScope =
    androidx.compose.runtime.rememberCoroutineScope()

/** The catalogue categories: an id for the filter and a caption. */
private val CATEGORIES = listOf(
    "Sans Serif" to R.string.shop_fonts_filter_sans,
    "Serif" to R.string.shop_fonts_filter_serif,
    "Display" to R.string.shop_fonts_filter_display,
    "Handwriting" to R.string.shop_fonts_filter_script,
    "Monospace" to R.string.shop_fonts_filter_mono,
)

/**
 * The human-readable name of a catalogue category.
 *
 * Resolved here, not in [GoogleFonts.Family.categoryLabel]: the token
 * (`"sans serif"`) is what the filter compares against and the data layer has no
 * `Context` to resolve a resource with. An unknown token falls back to the same
 * capitalised token the data layer would have produced.
 */
@Composable
private fun fontCategoryLabel(category: String): String = when (category.lowercase()) {
    "sans serif" -> stringResource(R.string.shop_fonts_category_sans_serif)
    "serif" -> stringResource(R.string.shop_fonts_category_serif)
    "display" -> stringResource(R.string.shop_fonts_category_display)
    "handwriting" -> stringResource(R.string.shop_fonts_category_handwriting)
    "monospace" -> stringResource(R.string.shop_fonts_category_monospace)
    else -> category.replaceFirstChar { it.uppercase() }
}

/**
 * Selection and ordering of the list.
 *
 * The order is by `popularity`, where smaller means more popular (Roboto is 2,
 * ABeeZee is 100): this is Google Fonts' own order, and it is more useful than
 * alphabetical, because the catalogue has two thousand families.
 */
internal fun filterCatalog(
    catalog: List<GoogleFonts.Family>,
    query: String,
    category: String,
    latinOnly: Boolean,
): List<GoogleFonts.Family> {
    val q = query.trim().lowercase()
    return catalog
        .filter { family ->
            (q.isEmpty() || family.name.lowercase().contains(q)) &&
                (category.isEmpty() || family.category == category) &&
                (!latinOnly || family.latin)
        }
        .sortedBy { if (it.popularity <= 0) Int.MAX_VALUE else it.popularity }
}
