// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo

import android.app.Application
import com.kerneldroid.rumo.work.RumoWork

/**
 * Process entry point.
 *
 * Needed for exactly one thing: [RumoWork] must be able to raise a
 * foreground service from anywhere, and the service needs a context. Asking
 * every caller for it would mean threading `Context` through the engine, the
 * assistant session and export — three places that need it for nothing else.
 *
 * `Application.onCreate` is called before everything else, so this is also the
 * place for any other process-wide initialisation if one appears.
 */
class RumoApp : Application() {
    override fun onCreate() {
        super.onCreate()
        RumoWork.install(this)
    }
}
