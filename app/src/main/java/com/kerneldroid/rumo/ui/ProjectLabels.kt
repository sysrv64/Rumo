// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.kerneldroid.rumo.R
import java.text.DateFormat
import java.util.Date

/**
 * "Edited <when>" for a project row, in the language the interface is set to.
 *
 * This is a composable rather than a method on [ProjectEntry] for two reasons.
 *
 * A `ProjectEntry` is a plain value read out of the project store: it has no
 * context, so it cannot reach a string resource, and its text came out English
 * whatever the interface was set to.
 *
 * The date is the second reason, and the less obvious one. `SimpleDateFormat`
 * with `Locale.getDefault()` follows the *phone*, not the language chosen in the
 * app — so a Russian interface on an English phone showed an English date, and
 * the row mixed two languages. The locale is taken from the localised context
 * here, which is the same context `stringResource` reads, so both halves of the
 * row agree.
 */
@Composable
fun projectSubtitle(entry: ProjectEntry): String {
    val context = LocalContext.current
    val locale = context.resources.configuration.locales[0]
    val when_ = remember(entry.lastModified, locale) {
        DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT, locale)
            .format(Date(entry.lastModified))
    }
    return stringResource(R.string.projects_edited_at, when_)
}
