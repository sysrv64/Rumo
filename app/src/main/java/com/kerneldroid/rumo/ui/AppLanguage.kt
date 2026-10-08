// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.content.Context
import android.content.res.Configuration
import androidx.annotation.StringRes
import com.kerneldroid.rumo.R
import java.util.Locale

/**
 * The languages the interface is offered in.
 *
 * ## Why the label is a resource, not a field
 *
 * A language picker shows every language **in its own script** — `English`,
 * `Русский`, `中文` — whatever the interface is currently set to. That is the
 * point of a picker: someone who cannot read the current language must still be
 * able to find their own. So the labels do not follow the selected locale, and
 * they cannot be ordinary translatable strings that differ per `values-<lang>`.
 *
 * They live in the default `values/strings.xml` for the same reason, and the
 * enum carries only the resource id. Keeping them there rather than inline in
 * this file also keeps this file ASCII: the Cyrillic and the Han characters
 * belong in a resource file, which is what a resource file is for.
 *
 * ## Why a tag and not a `Locale`
 *
 * What is persisted is the short tag (`en`, `ru`, `zh`), so a settings file
 * written by this build stays readable by a later one that knows more languages
 * — and so a value nobody recognises degrades to English instead of throwing.
 */
enum class AppLanguage(val tag: String, @StringRes val labelRes: Int) {
    English("en", R.string.language_english),
    Russian("ru", R.string.language_russian),
    Chinese("zh", R.string.language_chinese),
    ;

    /**
     * The locale to resolve this choice to, given what the phone is set to.
     *
     * Chinese is the only one that needs the phone's opinion. "Chinese" is two
     * scripts, and the picker deliberately does not offer them as two languages —
     * a Traditional reader looking for their own language should not have to
     * know which of two entries is theirs. So the script follows the phone:
     * Traditional where the phone asks for it, Simplified otherwise, and the
     * resources carry both (`values-zh` and `values-b+zh+Hant`).
     *
     * The region list is a heuristic and an honest one: Java's `Locale` does not
     * infer a script from a region, so `zh-TW` reports no script at all. Taiwan,
     * Hong Kong and Macau are Traditional; everywhere else that speaks Chinese
     * in this app's audience is Simplified.
     */
    fun locale(system: Locale): Locale = when (this) {
        Chinese -> {
            val traditional = system.language == "zh" &&
                (system.script == "Hant" || system.country in TRADITIONAL_REGIONS)
            Locale.forLanguageTag(if (traditional) "zh-Hant" else "zh")
        }

        else -> Locale.forLanguageTag(tag)
    }

    companion object {
        /** Regions whose Chinese is written in Traditional characters. */
        private val TRADITIONAL_REGIONS = setOf("TW", "HK", "MO")

        /**
         * The language for a stored tag, or English for anything unknown.
         *
         * A settings file can hold a tag this build does not know — it was
         * written by a newer build, or edited by hand. Falling back to English
         * is the only answer that keeps the interface readable; refusing to
         * start, or showing blank labels, would be worse than the wrong language.
         */
        fun parse(tag: String?): AppLanguage = entries.firstOrNull { it.tag == tag } ?: English

        /**
         * What a fresh install should start in.
         *
         * The system language if it is one of the three, English otherwise. This
         * is not a fourth option in the picker — it only decides the initial
         * value, so that someone whose phone is already Russian does not have to
         * go find the setting before the app is readable.
         */
        fun fromSystem(): AppLanguage {
            val system = Locale.getDefault().language
            return entries.firstOrNull { it.tag == system } ?: English
        }
    }
}

/**
 * A [Context] whose resources resolve in [language].
 *
 * Compose's `stringResource` reads `LocalContext.current.resources`, so a
 * context created with the wanted locale is enough to translate the whole
 * interface — no per-app-language plumbing, no `AppCompatDelegate`, and nothing
 * that needs the activity to be an `AppCompatActivity`. The alternative would
 * have meant adding appcompat as a dependency purely to change a locale.
 *
 * The base configuration is copied rather than built from scratch, so the
 * screen size, density and night mode the system reported survive; only the
 * locale is replaced.
 */
fun Context.withAppLanguage(language: AppLanguage): Context {
    val locale = language.locale(resources.configuration.locales[0])
    val config = Configuration(resources.configuration)
    config.setLocale(locale)
    return createConfigurationContext(config)
}
