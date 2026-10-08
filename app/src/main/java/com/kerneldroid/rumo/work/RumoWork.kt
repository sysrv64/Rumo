// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.work

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.ContextCompat
import com.kerneldroid.rumo.data.AppLog
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Work that has to survive to the end even if the user leaves the app.
 *
 * ## Why
 *
 * A model response takes minutes, and so does an export. While the app is on screen
 * an ordinary coroutine is enough; as soon as the user leaves, the system may kill the
 * process and the work is cut off silently — no error, no trace, in the middle of the
 * response. That is the worst kind of cut-off: the user comes back and sees the same
 * thing they left, and does not know that nothing is happening.
 *
 * The process is kept alive by [RumoWorkService] — a foreground service with a
 * notification. What lives here is only the bookkeeping: what exactly is running, what
 * to say about it, and when it all ended.
 *
 * ## The boundary between bookkeeping and the work
 *
 * The work lives in the app's coroutines, not in the service, and that is not a
 * compromise. An export needs the editor, engine and project state; a model response
 * needs the conversation history and the model catalogue. A service that took this on
 * would have to be handed all of the above — that is, to assemble a second copy of the
 * app somewhere else, which would diverge from the first at the first edit. The service
 * holds the process and shows what is going on; the work happens where it happens.
 *
 * ## One piece of work — one [Job]
 *
 * A handle, not an id that must not be forgotten to release: [Job.close] is idempotent
 * and is called from `finally`, so both an error and a user stop release the work the
 * same way. The id inside is an implementation detail, and the caller has no need to
 * know it.
 */
object RumoWork {

    /**
     * What exactly is running.
     *
     * The foreground service type depends on this, and the type is not a label: the
     * system uses it to count the time budget and decide what the app is allowed to do.
     * An export is media processing, a conversation is data transfer; calling an export
     * `dataSync` would mean taking someone else's budget and one day getting a refusal
     * where it is not meaningful.
     */
    enum class Kind {
        /** A model response: network, waiting, a stream of text. */
        CHAT,

        /** Video encoding: CPU, frames, muxer. */
        EXPORT,
    }

    /**
     * One piece of work.
     *
     * [progress] is `null` when the progress is unknown: a model response has none and
     * cannot, and a bar that lies about percentages is worse than no bar.
     */
    data class Item(
        val id: String,
        val kind: Kind,
        val title: String,
        val detail: String = "",
        val progress: Float? = null,
    )

    private val _items = MutableStateFlow<List<Item>>(emptyList())

    /** Everything running right now. An empty list means no work. */
    val items: StateFlow<List<Item>> = _items.asStateFlow()

    /**
     * Time to ask for the notification permission.
     *
     * It is requested at the moment work starts, not at launch: without the permission
     * the service still runs, but its notification is invisible — and it is invisible
     * exactly when it is the only proof that work is running.
     */
    private val _needsPermission = MutableStateFlow(false)
    val needsPermission: StateFlow<Boolean> = _needsPermission.asStateFlow()

    private var appContext: Context? = null
    private var counter = 0L

    /**
     * Whether the service is running right now.
     *
     * Needed so as not to start it a second time: the service watches [items] and picks
     * up new work by itself, so the **first** piece of work requires a call and all the
     * following ones do not. This is not an optimisation: the system rejects a call from
     * the background, and an extra call is an extra chance of a refusal.
     */
    @Volatile
    private var serviceRunning = false

    /**
     * Whether the app is visible.
     *
     * A foreground service can only be started while the app is on screen (or under one
     * of the few exemptions, which we do not have). There is no one to ask the system
     * "may I", so `MainActivity` holds the answer: it alone knows when the app appeared
     * and when it left.
     */
    @Volatile
    private var visible = false

    /** Called from [com.kerneldroid.rumo.RumoApp] once per process. */
    fun install(context: Context) {
        appContext = context.applicationContext
    }

    /** The dialog was answered — we do not ask again in this process. */
    fun permissionAnswered() {
        _needsPermission.value = false
    }

    /**
     * The app appeared or left.
     *
     * Returning to the screen is also a reason to start the service again if work is
     * running but there is no service: it may have been refused while the app was in
     * the background, or killed along with the memory. Now that is allowed, and the work
     * is protected again.
     */
    fun setVisible(visible: Boolean) {
        this.visible = visible
        if (visible && _items.value.isNotEmpty() && !serviceRunning) startService()
    }

    /** Called by the service: it started. */
    internal fun onServiceStarted() {
        serviceRunning = true
    }

    /** Called by the service: it stopped. */
    internal fun onServiceStopped() {
        serviceRunning = false
    }

    /**
     * Take work under supervision.
     *
     * The service is started right here rather than by a separate call: work without a
     * service is work the system may kill, and splitting these two actions would allow
     * the second to be forgotten.
     */
    fun start(kind: Kind, title: String, detail: String = ""): Job {
        val id = "${kind.name.lowercase()}-${++counter}"
        _items.value = _items.value + Item(id, kind, title, detail)
        // The service is already running — it will see the new work itself, through the
        // list. A second call here would be not a safety net but a risk: the system
        // rejects it from the background, and that rejection is a crash, not a refusal.
        if (!serviceRunning) startService()
        return Job(id)
    }

    /**
     * Start the service.
     *
     * It cannot be started from the background: the system answers
     * `ForegroundServiceStartNotAllowedException`, and it answers **not here** but later
     * — inside `Service.onStartCommand`, when the service calls `startForeground`. So
     * the check sits before the call, not around it: wrapping `startForegroundService`
     * in `runCatching` is useless, it does not throw.
     *
     * A refusal is no reason to crash and no reason to give up on the work: it will run
     * unprotected and survive until the system kills the process. That is worse than
     * with protection, and better than a conversation cut off in the middle.
     */
    private fun startService() {
        val context = appContext ?: return
        askForNotifications(context)
        if (!visible) {
            AppLog.warn(TAG, "work service not started: the app is in the background")
            return
        }
        runCatching {
            ContextCompat.startForegroundService(context, RumoWorkService.intent(context))
        }.onFailure {
            AppLog.warn(TAG, "work service refused: ${it.message}")
        }
    }

    /** Whether work with this [id] is running. */
    internal fun isActive(id: String): Boolean = _items.value.any { it.id == id }

    internal fun update(id: String, detail: String?, progress: Float?) {
        _items.value = _items.value.map { item ->
            if (item.id != id) {
                item
            } else {
                item.copy(
                    detail = detail ?: item.detail,
                    progress = progress ?: item.progress,
                )
            }
        }
    }

    internal fun finish(id: String) {
        _items.value = _items.value.filterNot { it.id == id }
    }

    /**
     * A handle for one piece of work.
     *
     * All methods may be called after [close] and any number of times: work released
     * twice is not an error but the ordinary course of events when a user stop and the
     * end of a turn arrive at the same time.
     */
    class Job internal constructor(private val id: String) {

        private var closed = false

        /** What is happening right now, in words. */
        fun detail(text: String) {
            if (closed) return
            update(id, text, null)
        }

        /** [fraction] from 0 to 1. `null` means the progress is unknown, which is not the same as zero. */
        fun progress(fraction: Float, text: String? = null) {
            if (closed) return
            update(id, text, fraction.coerceIn(0f, 1f))
        }

        /** The work ended — in any way. Idempotent. */
        fun close() {
            if (closed) return
            closed = true
            finish(id)
        }
    }

    private fun askForNotifications(context: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        val granted = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED
        if (!granted) _needsPermission.value = true
    }

    private const val TAG = "rumo-work"
}
