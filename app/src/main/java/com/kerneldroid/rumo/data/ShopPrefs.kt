// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.data

import android.content.Context
import android.content.SharedPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Shop settings: the GitHub token, the update-check schedule, the default font.
 *
 * ## Why the token is here and not in secure storage
 *
 * `EncryptedSharedPreferences` would have required
 * `androidx.security:security-crypto` — a library Jetpack has moved to
 * deprecated — for protection against a sandbox read that cannot happen without
 * root anyway. The assistant key lives in ordinary `SharedPreferences` ([
 * com.kerneldroid.rumo.ui.rumi.RumiSettings]), and a second, "more secure" way
 * of storing the same class of secret would only have driven the behaviours
 * apart.
 *
 * ## Why only for effects and templates
 *
 * The font list goes to Google Fonts and to `raw.githubusercontent.com`, where
 * the token is not needed at all. The GitHub API without a token gives 60
 * requests per hour per core and 10 per minute for search, which is not enough
 * to browse the storefront — so the token is only asked for where it is
 * actually needed.
 */
object ShopPrefs {
    private const val PREFS = "rumo_shop"
    private const val KEY_TOKEN = "github_token"
    private const val KEY_CHECK_UPDATES = "check_updates"
    private const val KEY_INTERVAL_HOURS = "update_interval_hours"
    private const val KEY_LAST_CHECK = "last_update_check"
    private const val KEY_PREVIEW_IN_LIST = "preview_in_list"
    private const val KEY_DEFAULT_FONT = "default_font_family"
    private const val KEY_SHOW_BUILT_IN = "show_built_in_templates"

    /** Hours between automatic release checks by default. */
    const val DEFAULT_INTERVAL_HOURS = 24

    data class State(
        /** Personal GitHub token; empty means anonymous requests. */
        val token: String = "",
        /** Check for releases automatically. */
        val checkUpdates: Boolean = true,
        /** How many hours between repeated checks. */
        val intervalHours: Int = DEFAULT_INTERVAL_HOURS,
        /** When the last check ran (epoch ms), 0 means never. */
        val lastCheck: Long = 0L,
        /** Draw font previews right in the shop list. */
        val previewInList: Boolean = true,
        /** Font family for new text layers; empty means the built-in one. */
        val defaultFontFamily: String = "",
        /**
         * Show the built-in templates in the Templates section.
         *
         * A switch rather than a deletion: the built-in templates are the only
         * thing in the section that works without a network and without a
         * token, and removing them for good would leave the section empty on
         * first launch. Whoever finds them in the way can turn them off with a
         * single switch.
         */
        val showBuiltInTemplates: Boolean = true,
    ) {
        /** Token for the `Authorization` header, or null if there is none. */
        fun credential(): String? = token.ifEmpty { null }
    }

    private val _state = MutableStateFlow(State())
    val state: StateFlow<State> = _state.asStateFlow()

    @Volatile
    private var prefs: SharedPreferences? = null

    private val listener =
        SharedPreferences.OnSharedPreferenceChangeListener { _, _ -> reload() }

    fun init(context: Context) {
        if (prefs != null) return
        val p = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        prefs = p
        p.registerOnSharedPreferenceChangeListener(listener)
        reload()
    }

    private fun reload() {
        val p = prefs ?: return
        _state.value = State(
            token = p.getString(KEY_TOKEN, "") ?: "",
            checkUpdates = p.getBoolean(KEY_CHECK_UPDATES, true),
            intervalHours = p.getInt(KEY_INTERVAL_HOURS, DEFAULT_INTERVAL_HOURS)
                .coerceIn(1, 24 * 30),
            lastCheck = p.getLong(KEY_LAST_CHECK, 0L),
            previewInList = p.getBoolean(KEY_PREVIEW_IN_LIST, true),
            defaultFontFamily = p.getString(KEY_DEFAULT_FONT, "") ?: "",
            showBuiltInTemplates = p.getBoolean(KEY_SHOW_BUILT_IN, true),
        )
    }

    fun setToken(token: String) {
        prefs?.edit()?.putString(KEY_TOKEN, token.trim())?.apply()
    }

    fun setCheckUpdates(enabled: Boolean) {
        prefs?.edit()?.putBoolean(KEY_CHECK_UPDATES, enabled)?.apply()
    }

    fun setIntervalHours(hours: Int) {
        prefs?.edit()?.putInt(KEY_INTERVAL_HOURS, hours.coerceIn(1, 24 * 30))?.apply()
    }

    fun setPreviewInList(enabled: Boolean) {
        prefs?.edit()?.putBoolean(KEY_PREVIEW_IN_LIST, enabled)?.apply()
    }

    fun setDefaultFontFamily(family: String) {
        prefs?.edit()?.putString(KEY_DEFAULT_FONT, family)?.apply()
    }

    fun setShowBuiltInTemplates(show: Boolean) {
        prefs?.edit()?.putBoolean(KEY_SHOW_BUILT_IN, show)?.apply()
    }

    /** Marks the check moment so that the throttling survives a restart. */
    fun markChecked(at: Long = System.currentTimeMillis()) {
        prefs?.edit()?.putLong(KEY_LAST_CHECK, at)?.apply()
    }

    /**
     * Is it time to check for releases.
     *
     * The spread of a quarter of the interval is so that the check does not land
     * at the same moment for everyone who opened the app at the same time.
     */
    fun isCheckDue(now: Long = System.currentTimeMillis()): Boolean {
        val s = _state.value
        if (!s.checkUpdates) return false
        if (s.lastCheck <= 0L) return true
        val interval = s.intervalHours.toLong() * 3_600_000L
        val jitter = interval / 4
        return now - s.lastCheck >= interval + jitter
    }
}
