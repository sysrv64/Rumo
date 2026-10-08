// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.work

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import com.kerneldroid.rumo.ui.SettingsRepo
import com.kerneldroid.rumo.ui.withAppLanguage
import androidx.core.graphics.drawable.IconCompat
import com.kerneldroid.rumo.MainActivity
import com.kerneldroid.rumo.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch

/**
 * A foreground service: keeps the process alive while work is running.
 *
 * ## Why this one specifically
 *
 * Without it, the user leaving the app means the system may kill the process at any
 * moment — and both a model response and an export are cut off mid-work, silently. A
 * foreground service with a notification is the only way to tell the system "this is
 * still needed"; at the same time the notification honestly shows what is happening.
 *
 * ## What it does not do
 *
 * It does not perform the work. The app renders the frames, the app holds the model
 * request; the service only keeps the process from dying and shows the state. See
 * [RumoWork] for why the boundary runs exactly here.
 *
 * ## The service type is not a label
 *
 * The type is chosen by the work: an export is `mediaProcessing`, a conversation is
 * `dataSync`. The system uses the type to count the time budget and decide what the
 * app is allowed to do; calling an export `dataSync` would mean taking someone else's
 * budget and one day getting a refusal where it is not meaningful. When works run at
 * the same time the types are OR-ed into a mask — that is the stock way to say "both
 * are running".
 *
 * ## Wake lock
 *
 * A foreground service does **not** keep the CPU awake: it only changes the rules of
 * process killing. Without `PARTIAL_WAKE_LOCK` the screen goes off, the CPU falls
 * asleep and the export stalls halfway — formally alive, in fact stopped.
 *
 * ## Timeout
 *
 * Since API 35, `dataSync` and `mediaProcessing` have a time budget, and the system
 * calls [onTimeout] when it runs out. Not stopping in response is an app crash, so the
 * handler is here rather than "sometime later".
 *
 * ## Stopping
 *
 * The service watches [RumoWork.items] and leaves by itself when the list empties.
 * That way the end of work and the stop of the service are one event, not two that have
 * to be coordinated: stopping the service cannot be forgotten, because what stops it is
 * not the caller but the fact.
 */
class RumoWorkService : Service() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var wakeLock: PowerManager.WakeLock? = null

    /** When the notification was last posted. See [post]. */
    private var lastPostAt = 0L

    /** How many works the last post had: a change in the number is a reason to post at once. */
    private var lastCount = -1

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        // Read the chosen language before anything is drawn from resources: the
        // service can outlive the activity, and on a process restart it is the
        // first thing to run — without this the notification would come out in
        // the phone's language while the interface is in the chosen one.
        SettingsRepo.init(applicationContext)
        RumoWork.onServiceStarted()
    }

    /**
     * This service's strings in the language the user chose, not the phone's.
     *
     * The interface picks its language with a context carrying the wanted locale
     * (see `MainActivity`), and a notification built from `this` would ignore
     * that entirely: the two would disagree whenever the phone is set to a
     * language other than the one chosen in the app.
     */
    private fun localized(): Context =
        withAppLanguage(SettingsRepo.settings.value.appLanguage)

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // The notification is posted in any case, even if there is no work any more.
        //
        // The service was started via `startForegroundService`, and that obliges: not
        // showing a notification is a crash, not "the service just was not needed". The
        // work may have ended between `start` and this call — then we are obliged to show
        // the notification and stop immediately after.
        val items = RumoWork.items.value
        if (!promote(items)) {
            // The system refused the foreground — meaning the service was started from
            // somewhere it may not be. Stopping here is mandatory: a service that will
            // never show a notification will be killed by the system, and killed with an
            // exception if we try again.
            stopSelf()
            return START_NOT_STICKY
        }
        lastPostAt = System.currentTimeMillis()
        lastCount = items.size
        if (items.isEmpty()) {
            stopSelf()
            return START_NOT_STICKY
        }
        acquireWakeLock()
        scope.launch {
            RumoWork.items.collect { current ->
                if (current.isEmpty()) {
                    stopSelf()
                    return@collect
                }
                post(current, force = false)
            }
        }
        // Do not restart after the process is killed: the work lives in the app's
        // memory, and a resurrected service would show the progress of something that is
        // already gone.
        return START_NOT_STICKY
    }

    /**
     * The per-type time budget has run out.
     *
     * Since API 35, `dataSync` and `mediaProcessing` are bounded, and the system demands
     * a stop. Not stopping is an app crash, so here there is only the stop and a log
     * entry: work that did not finish within the allotted time will not get faster from
     * the service staying.
     *
     * Both forms are overridden: the one-argument one appeared in API 35, the
     * two-argument one in API 36, and on the newer level the system calls the second.
     * Leaving it unoverridden would mean not stopping where that is mandatory.
     */
    override fun onTimeout(startId: Int) {
        handleTimeout(startId)
    }

    override fun onTimeout(startId: Int, fgsType: Int) {
        handleTimeout(startId)
    }

    private fun handleTimeout(startId: Int) {
        Log.w(TAG, "foreground service timed out (startId=$startId); stopping")
        stopSelf(startId)
    }

    override fun onDestroy() {
        scope.cancel()
        releaseWakeLock()
        RumoWork.onServiceStopped()
        super.onDestroy()
    }

    /**
     * Post the notification.
     *
     * Returns `false` if there was nothing to post: the list is empty, and there is
     * nothing to call `startForeground` with.
     *
     * Updates arrive more often than they are worth showing: an export reports every
     * frame, and the system limits the rate of notification updates and starts dropping
     * the too-frequent ones. So posting happens at most once per [MIN_POST_INTERVAL_MS],
     * while a change in the number of works is posted immediately: that is not progress
     * but news.
     */
    private fun post(items: List<RumoWork.Item>, force: Boolean) {
        if (items.isEmpty()) return
        val now = System.currentTimeMillis()
        val countChanged = items.size != lastCount
        if (!force && !countChanged && now - lastPostAt < MIN_POST_INTERVAL_MS) return
        lastPostAt = now
        lastCount = items.size
        if (!promote(items)) stopSelf()
    }

    /**
     * Show the notification and become a foreground service.
     *
     * `startForeground` both updates the notification and changes the service type:
     * there is no separate "update the notification" for a foreground service, and
     * that is convenient — the type has to match what is running right now.
     *
     * The wrapping is not for looks. The system may refuse — for instance if the
     * service was started from the background — and the refusal arrives as an exception
     * **from here**, not from `startForegroundService`, which only schedules the start.
     * Wrapping the first call and not this one would mean guarding in the wrong place:
     * on a device it looked like an app crash when the service started.
     *
     * Returns `false` if the service must not exist here.
     */
    private fun promote(items: List<RumoWork.Item>): Boolean = try {
        ServiceCompat.startForeground(
            this,
            NOTIFICATION_ID,
            buildNotification(items),
            typeMask(items),
        )
        true
    } catch (t: Throwable) {
        // A refusal is not a failure of the work: it runs in the app and will continue
        // unprotected until the system kills the process. Dropping it because of a
        // notification is not allowed.
        Log.w(TAG, "startForeground refused: ${t.message}")
        false
    }

    /** The active works' types OR-ed into a mask. */
    private fun typeMask(items: List<RumoWork.Item>): Int {
        var mask = 0
        items.forEach { item ->
            mask = mask or when (item.kind) {
                RumoWork.Kind.CHAT -> ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
                RumoWork.Kind.EXPORT -> ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROCESSING
            }
        }
        return if (mask == 0) ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC else mask
    }

    private fun notificationManager(): NotificationManager? =
        ContextCompat.getSystemService(this, NotificationManager::class.java)

    /**
     * A notification about what is running.
     *
     * One work — its own name and detail; several — how many, and then the detail goes
     * to the first: listing them in the notification would mean showing a list where the
     * answer "is it still running" is what is needed.
     *
     * On API 36+ this is also a live update: the progress is drawn by the system's
     * `ProgressStyle` rather than our own bar, and the same notification is shown as a
     * chip in the status bar. Promotion is requested but the system decides —
     * [RumoWork.canPromote] says whether it will grant it, so as not to ask in vain.
     */
    private fun buildNotification(items: List<RumoWork.Item>): Notification {
        ensureChannel()
        val first = items.firstOrNull()
        val single = items.singleOrNull()
        val title = when {
            first == null -> getString(R.string.app_name)
            single != null -> single.title
            else -> localized().getString(R.string.work_running_many, items.size)
        }
        val text = when {
            first == null -> ""
            single != null -> single.detail
            else -> first.title + if (first.detail.isEmpty()) "" else " · ${first.detail}"
        }
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val builder = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_work)
            .setContentTitle(title)
            .setContentText(text)
            .setContentIntent(open)
            .setOngoing(true)
            .setSilent(true)
            .setShowWhen(false)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
            .setStyle(progressStyle(single))
            // An update is the same notification, not a new one. Without this the system
            // treats every post as a new event: it pops up, zooms and sounds, while an
            // export posts progress hundreds of times.
            .setOnlyAlertOnce(true)
            // Text for the status-bar chip: there is no room there for a title and a
            // detail, and "42%" reads while "Export MP4 · 42%" does not. The chip holds
            // 96dp, so the long version would not fit anyway.
            .setShortCriticalText(
                single?.progress?.let { "${(it * 100).toInt().coerceIn(0, 100)}%" } ?: title,
            )
            // Promotion is requested but not guaranteed: the system decides for itself
            // and may refuse. A refusal is an ordinary notification, not the absence of
            // one, so there is no "was refused" branch in the code.
            //
            // There is deliberately no version gate here: both lines above are
            // `androidx.core` methods, and it decides for itself what of this the current
            // level understands (verified against the bytecode: `setShortCriticalText`
            // goes into extras below API 36, `setRequestPromotedOngoing` always sets an
            // extra). A platform gate would not only be a redundant branch but also wrong:
            // the real promotion threshold is Android 16.1, while `Build.VERSION.SDK_INT`
            // is the same on 16.0 and 16.1.
            .setRequestPromotedOngoing(true)
        return builder.build()
    }

    /**
     * The progress for the system bar.
     *
     * One work with a known progress — a determinate bar; everything else —
     * indeterminate. A bar on invented numbers is worse than none: it says "this is how
     * it is going" and lies.
     */
    private fun progressStyle(single: RumoWork.Item?): NotificationCompat.ProgressStyle {
        val style = NotificationCompat.ProgressStyle()
        val fraction = single?.progress
        if (fraction != null) {
            style.setProgress((fraction * 100f).toInt().coerceIn(0, 100))
            style.setProgressIndeterminate(false)
            style.setProgressTrackerIcon(IconCompat.createWithResource(this, R.drawable.ic_work))
        } else {
            style.setProgressIndeterminate(true)
        }
        return style
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = notificationManager() ?: return
        if (manager.getNotificationChannel(CHANNEL_ID) != null) return
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                localized().getString(R.string.work_channel),
                // LOW: a progress notification is not an event. Sound and pop-up for it
                // would be an intrusion for the message "still running".
                NotificationManager.IMPORTANCE_LOW,
            ).apply {
                description = localized().getString(R.string.work_channel_description)
                setShowBadge(false)
            },
        )
    }

    /**
     * Take the lock for the duration of the work.
     *
     * Without a timeout, and that is not carelessness: it is released in `onDestroy`,
     * and that is always called when the service stops — including on a timeout. If the
     * process is killed before it can do so, the system will release the lock itself: it
     * belongs to the process. A timeout here would be a third way to release it, and a
     * third way is a redundant branch, not a safety net.
     */
    private fun acquireWakeLock() {
        if (wakeLock != null) return
        val power = ContextCompat.getSystemService(this, PowerManager::class.java) ?: return
        wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, WAKE_LOCK_TAG).apply {
            setReferenceCounted(false)
            acquire()
        }
    }

    private fun releaseWakeLock() {
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
    }

    companion object {
        private const val TAG = "rumo-work"
        private const val CHANNEL_ID = "rumo_work"
        private const val NOTIFICATION_ID = 1001
        private const val WAKE_LOCK_TAG = "rumo:work"

        /** At most once per second. See [post]. */
        private const val MIN_POST_INTERVAL_MS = 1_000L

        fun intent(context: Context): Intent = Intent(context, RumoWorkService::class.java)
    }
}
