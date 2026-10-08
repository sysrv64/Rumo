// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.ContextCompat
import com.kerneldroid.rumo.data.AppLog
import com.kerneldroid.rumo.data.RumoBridge
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** Report file name: rumo-logs-YYYYMMDD-HHMMSS.txt. */
fun logReportFileName(now: Date = Date()): String =
    "rumo-logs-${SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(now)}.txt"

/**
 * The state of the permission to write to the public Download: it only matters
 * on API 26-28, from 29 on the write goes through MediaStore and needs no
 * permission.
 */
fun storagePermissionState(context: Context): String = when {
    Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q ->
        "not required on API ${Build.VERSION.SDK_INT} (MediaStore)"
    ContextCompat.checkSelfPermission(
        context,
        Manifest.permission.WRITE_EXTERNAL_STORAGE,
    ) == PackageManager.PERMISSION_GRANTED -> "granted"
    else -> "denied"
}

/**
 * One text report for diagnosing a problem: build and device, a render
 * diagnostics summary, the native Rust log (which also carries export failures
 * and GPU fallbacks) and the Kotlin [AppLog] journal. The section order is
 * fixed — see `saveAllLogs`.
 */
fun buildLogReport(context: Context): String {
    val sb = StringBuilder(16 * 1024)
    sb.append("Rumo log report\n")

    // --- 1. Header: build + device + permission ---
    val pkg = context.packageName
    var versionName = "?"
    var versionCode = -1L
    try {
        val pi = context.packageManager.getPackageInfo(pkg, 0)
        versionName = pi.versionName ?: "?"
        versionCode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            pi.longVersionCode
        } else {
            @Suppress("DEPRECATION") val vc = pi.versionCode
            vc.toLong()
        }
    } catch (t: Throwable) {
        AppLog.warn("logs", "getPackageInfo($pkg) failed: ${AppLog.describe(t)}")
    }
    sb.append("generated: ").append(isoNow()).append('\n')
    sb.append("applicationId: ").append(pkg).append('\n')
    sb.append("versionName: ").append(versionName).append('\n')
    sb.append("versionCode: ").append(versionCode).append('\n')
    sb.append("android: ").append(Build.VERSION.RELEASE)
        .append(" (SDK_INT ").append(Build.VERSION.SDK_INT).append(")\n")
    sb.append("device: ").append(Build.MANUFACTURER).append(' ').append(Build.MODEL).append('\n')
    sb.append("abis: ").append(Build.SUPPORTED_ABIS.joinToString(", ")).append('\n')
    sb.append("storagePermission: ").append(storagePermissionState(context)).append('\n')

    // --- 2. Render diagnostics summary ---
    val d = RumoBridge.renderDiagnostics()
    sb.append("--- render diagnostics ---\n")
    sb.append("path: ").append(d?.path.orEmpty().ifEmpty { "unknown" }).append('\n')
    sb.append("adapter: ").append(d?.adapter.orEmpty()).append('\n')
    sb.append("backend: ").append(d?.backend.orEmpty()).append('\n')
    sb.append("hint: ").append(d?.hint.orEmpty()).append('\n')
    sb.append("engineOk: ").append(d?.engineOk ?: false).append('\n')
    sb.append("previewOk: ").append(d?.previewOk ?: false).append('\n')
    val rejected = d?.rejectedEffects.orEmpty()
    sb.append("rejectedEffects: ")
        .append(if (rejected.isEmpty()) "(none)" else rejected.joinToString(", "))
        .append('\n')

    // --- 3. Native Rust log ---
    sb.append("--- rust render log ---\n")
    val entries = d?.entries.orEmpty()
    if (entries.isEmpty()) {
        sb.append("(empty)\n")
    } else {
        for (e in entries) {
            sb.append(e.seq).append(' ').append(e.level.uppercase()).append(' ')
            if (e.code.isNotEmpty()) sb.append(e.code).append(' ')
            sb.append(e.text).append('\n')
        }
    }

    // --- 4. Kotlin journal ---
    sb.append("--- kotlin app log ---\n")
    sb.append(AppLog.text())

    // --- 5. Environment ---
    sb.append("--- environment ---\n")
    sb.append("nativeLibraryLoaded: ").append(RumoBridge.isLoaded()).append('\n')
    sb.append("engineVersion: ").append(RumoBridge.engineVersion()).append('\n')
    sb.append("renderPath(diagnostics): ")
        .append(RumoBridge.renderPathFromDiagnostics() ?: "(unavailable)").append('\n')
    sb.append("renderPath(legacy): ").append(RumoBridge.renderPath()).append('\n')
    sb.append("exportLastError: ")
        .append(RumoBridge.exportLastError().ifEmpty { "(none)" }).append('\n')
    sb.append("lastExportError(AppLog): ")
        .append(com.kerneldroid.rumo.data.Exporter.lastError.ifEmpty { "(none)" })
        .append('\n')
    sb.append("appLogEntries: ").append(AppLog.snapshot().size).append('\n')
    return sb.toString()
}

/**
 * Save ONE report file into Download/Rumo (sections in order: header →
 * render diagnostics → rust render log → kotlin app log → environment).
 * A failure is not swallowed: the reason goes to AppLog and into
 * [SaveResult.Failed].
 */
suspend fun saveAllLogs(context: Context): SaveResult {
    val name = logReportFileName()
    val text = buildLogReport(context)
    AppLog.info("logs", "saving report $name (${text.length} chars) to Download/$RUMO_DOWNLOAD_DIR")
    val res = saveTextToDownloads(context, text, name)
    when (res) {
        is SaveResult.Ok -> AppLog.info("logs", "report saved: ${res.path}")
        is SaveResult.Failed -> AppLog.error("logs", "report save failed: ${res.reason}")
    }
    return res
}

/** ISO-8601 local time — the same stamp in every report. */
private fun isoNow(): String =
    SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ssZ", Locale.US).format(Date())
