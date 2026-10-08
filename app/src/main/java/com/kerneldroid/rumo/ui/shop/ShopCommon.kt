// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.shop

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.kerneldroid.rumo.data.GitHubApi
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Shared across the shop sections: the image cache, preview loading and small
 * details.
 *
 * ## Why without an image-loading library
 *
 * The storefront needs exactly two images — a small preview from the repository
 * and a bitmap preview of a font — and both are decoded with the stock
 * `BitmapFactory`. Coil would have pulled another network stack, a cache and a
 * thread pool into the APK for two calls that already fit into a couple of dozen
 * lines.
 */

/**
 * LRU cache on `LinkedHashMap` in access mode.
 *
 * Needed because the storefront shows a long list: without a limit the preview
 * cache would grow to the size of the whole catalogue, and font previews are
 * bitmap images too.
 */
internal class LruCache<K, V>(private val max: Int) {
    private val map = object : LinkedHashMap<K, V>(16, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<K, V>): Boolean =
            size > max
    }

    fun get(key: K): V? = map[key]

    fun put(key: K, value: V) {
        map[key] = value
    }

    fun containsKey(key: K): Boolean = map.containsKey(key)
}

/**
 * Asynchronous image loader by URL.
 *
 * [bitmap] is read during composition and returns null while the image is not
 * there; [request] starts a load exactly once per URL — repeated calls during
 * recomposition do nothing, because the state changes in `mutableStateMapOf`,
 * and Compose itself redraws the row when the image arrives.
 */
internal class ImageLoader(private val scope: CoroutineScope) {
    private val cache = LruCache<String, ImageBitmap>(96)
    private val inFlight = HashSet<String>()
    private val failed = HashSet<String>()

    /** Ready images; the key is the URL. */
    val ready = mutableStateMapOf<String, ImageBitmap>()

    /** Images that will not load any more: so we do not ask for them again. */
    val broken = mutableStateMapOf<String, Unit>()

    fun bitmap(url: String): ImageBitmap? = ready[url] ?: cache.get(url)?.also { ready[url] = it }

    fun hasFailed(url: String): Boolean = broken.containsKey(url)

    /**
     * Asks to load an image if it is not loaded yet and has not failed.
     *
     * [limitBytes] is the weight limit: a preview in the list that weighs more is
     * worth neither traffic nor memory, and it is better to show a placeholder.
     */
    fun request(url: String, token: String? = null, limitBytes: Long = 512L * 1024L) {
        if (url.isEmpty()) return
        if (ready.containsKey(url) || cache.containsKey(url) || failed.contains(url)) return
        if (!inFlight.add(url)) return
        scope.launch {
            val decoded = withContext(Dispatchers.IO) {
                val reply = GitHubApi.download(url, token, limitBytes)
                if (!reply.ok || reply.bytes.isEmpty()) {
                    null
                } else {
                    try {
                        BitmapFactory.decodeByteArray(reply.bytes, 0, reply.bytes.size)
                    } catch (_: Throwable) {
                        null
                    }
                }
            }
            inFlight.remove(url)
            if (decoded == null) {
                failed += url
                broken[url] = Unit
            } else {
                val image = decoded.asImageBitmap()
                cache.put(url, image)
                ready[url] = image
            }
        }
    }

    /** Puts an already ready image (the engine raster) into the same cache. */
    fun put(url: String, image: ImageBitmap) {
        cache.put(url, image)
        ready[url] = image
    }

    /** Marks the URL as failed so the list does not ask for it again. */
    fun markFailed(url: String) {
        failed += url
        broken[url] = Unit
    }

    fun isBusy(url: String): Boolean = inFlight.contains(url)
}

/** The engine raster (RGBA8, straight alpha) into an `ImageBitmap` for Compose. */
internal fun decodedToImage(
    width: Int,
    height: Int,
    rgba: ByteArray,
): ImageBitmap? {
    if (width <= 0 || height <= 0 || rgba.size != width * height * 4) return null
    val pixels = IntArray(width * height)
    var src = 0
    for (i in pixels.indices) {
        val r = rgba[src].toInt() and 0xFF
        val g = rgba[src + 1].toInt() and 0xFF
        val b = rgba[src + 2].toInt() and 0xFF
        val a = rgba[src + 3].toInt() and 0xFF
        pixels[i] = (a shl 24) or (r shl 16) or (g shl 8) or b
        src += 4
    }
    return try {
        Bitmap.createBitmap(pixels, width, height, Bitmap.Config.ARGB_8888).asImageBitmap()
    } catch (_: Throwable) {
        null
    }
}

/**
 * A place for a preview: an image, a placeholder or an icon.
 *
 * One detail for all sections: an empty rectangle with no explanation reads as a
 * broken image, so the "still loading" and "failed to load" states have their
 * own look.
 */
@Composable
internal fun PreviewBox(
    image: ImageBitmap?,
    modifier: Modifier = Modifier,
    size: Dp = 44.dp,
    corner: Dp = 12.dp,
    fallbackIcon: ImageVector? = null,
    loading: Boolean = false,
) {
    Box(
        modifier = modifier
            .size(size)
            .clip(RoundedCornerShape(corner))
            .background(MaterialTheme.colorScheme.surfaceContainerHighest),
        contentAlignment = Alignment.Center,
    ) {
        when {
            image != null -> Image(
                bitmap = image,
                contentDescription = null,
                modifier = Modifier.fillMaxSize(),
                contentScale = ContentScale.Crop,
            )
            loading -> Text(
                text = "…",
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            fallbackIcon != null -> Icon(
                imageVector = fallbackIcon,
                contentDescription = null,
                modifier = Modifier.size(size / 2),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
