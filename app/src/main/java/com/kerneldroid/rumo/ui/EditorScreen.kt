// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.graphics.Bitmap
import android.net.Uri
import android.os.Build
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.Toast
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.Spring
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateRotation
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.AccountTree
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.AspectRatio
import androidx.compose.material.icons.rounded.BugReport
import androidx.compose.material.icons.rounded.FolderOpen
import androidx.compose.material.icons.rounded.Image
import androidx.compose.material.icons.rounded.MoreVert
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.automirrored.rounded.Redo
import androidx.compose.material.icons.rounded.Save
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material.icons.rounded.Share
import androidx.compose.material.icons.rounded.SkipNext
import androidx.compose.material.icons.rounded.SkipPrevious
import androidx.compose.material.icons.automirrored.rounded.Undo
import androidx.compose.material.icons.rounded.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size as ComposeSize
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import com.kerneldroid.rumo.R
import com.kerneldroid.rumo.data.AppLog
import com.kerneldroid.rumo.ui.ProjectAssets
import com.kerneldroid.rumo.data.Exporter
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.work.RumoWork
import com.kerneldroid.rumo.ui.panels.NodeGraphOverlay
import com.kerneldroid.rumo.ui.theme.DockTokens
import com.kerneldroid.rumo.ui.theme.RumoSpacing
import com.kerneldroid.rumo.ui.theme.hapticConfirm
import com.kerneldroid.rumo.ui.theme.hapticReject
import com.kerneldroid.rumo.ui.theme.hapticToggle
import com.kerneldroid.rumo.ui.theme.monoNumerals
import java.io.File
import java.nio.ByteBuffer
import kotlin.math.abs
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.roundToInt
import kotlin.math.sin
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Editor shell.
 *
 * Fixed skeleton, top to bottom: slim app bar -> preview (aspect-locked, height
 * capped) -> persistent timeline -> transport -> resizable dock. Nothing except
 * the dock's own drag changes the preview's size, which is what used to make the
 * whole screen jump when a shape got selected.
 */
@Composable
fun EditorScreen(
    state: EditorState,
    onBack: () -> Unit,
    fileName: String? = null,
    /**
     * Show the project already in memory instead of loading [fileName] or
     * starting a new one. This is how work the assistant did is reached: until
     * it is saved, the in-memory project is the only copy, and the plain
     * `editor` route would replace it with an empty one.
     */
    keepCurrent: Boolean = false,
    onOpenSettings: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    var showAddSheet by remember { mutableStateOf(false) }
    var showUnsavedDialog by remember { mutableStateOf(false) }
    var openFailed by remember { mutableStateOf(false) }
    val dockPage by state.dockPage.collectAsState()
    // Preview view pref: "Fit" (exact 16:9) ↔ "Fill" (zoom to cover the editing
    // area, sides cropped). In-memory only — it is a per-session view mode.
    var previewFill by remember { mutableStateOf(false) }
    // Dock pulled down by the user (or by the assistant) to look at the picture
    // alone; see DockGrabBar.
    val dockCollapsed by state.dockCollapsed.collectAsState()
    var showNodeGraph by remember { mutableStateOf(false) }
    var showOverflow by remember { mutableStateOf(false) }
    var showCanvas by remember { mutableStateOf(false) }
    var showRenderDiagnostics by remember { mutableStateOf(false) }

    val layers by state.layers.collectAsState()
    val projectName by state.projectName.collectAsState()
    val playheadMs by state.playheadMs.collectAsState()
    val projectDurationMs by state.projectDurationMs.collectAsState()
    val isPlaying by state.isPlaying.collectAsState()
    val renderDiagnostics by state.renderDiagnostics.collectAsState()
    val engineDriven by state.engineActive.collectAsState()
    val engineReady by state.engineReady.collectAsState()
    val surfaceFailed by state.surfaceFailed.collectAsState()
    val hasEdits by state.hasEdits.collectAsState()
    // Undo/redo live in the overflow menu, but their enabled state belongs to
    // the top bar's own state: reading it here keeps the menu stateless.
    val canUndo by state.canUndo.collectAsState()
    val canRedo by state.canRedo.collectAsState()
    val selectedId by state.selectedId.collectAsState()
    val lockedIds by state.locked.collectAsState()

    val shapes = layers.filter { it.visible && it.kind == LayerKindUi.SHAPE }
    val textLayers = layers.filter { it.visible && it.kind == LayerKindUi.TEXT }
    val mediaLayers = layers.filter { it.visible && it.kind == LayerKindUi.MEDIA }
    val selectedLayer = layers.find { it.id == selectedId }
    // The inspector follows the selection: picking an object on the timeline or in the preview is
    // an intention to *edit* it, so the dock switches to "Properties".
    //
    // It used to require that a library (Media/Audio) was open, while by
    // default the dock is open on "Layers". So the first tap on a clip "to
    // fix it" left the user on the layer list: the "selected → editing" link
    // was not confirmed, and "Properties" had to be hunted for by hand. Now "Layers" is in the
    // condition too.
    //
    // But selecting a row **in the layer list itself** is not an intention to edit: there
    // you select in order to reorder or delete. So it is marked and the dock does not
    // move — otherwise the list would yank itself out from under the finger.
    var selectionFromLayerList by remember { mutableStateOf(false) }
    LaunchedEffect(selectedId) {
        if (selectedId != null &&
            !selectionFromLayerList &&
            (dockPage == DockPage.MEDIA ||
                dockPage == DockPage.AUDIO ||
                dockPage == DockPage.LAYERS)
        ) {
            state.setDockPage(DockPage.ADJUST)
        }
    }

    val isPortrait = LocalConfiguration.current.orientation == Configuration.ORIENTATION_PORTRAIT
    val appContext = LocalContext.current
    val screenHaptic = LocalHapticFeedback.current
    val ioScope = rememberCoroutineScope()
    var showExportDialog by remember { mutableStateOf(false) }
    val snackbarHostState = remember { SnackbarHostState() }
    // The dock height. The screen owns it, not the dock: only the screen knows how much
    // the preview has left. `rememberSaveable` — to survive an activity
    // recreation, should it happen anyway.
    var dockHeightDp by rememberSaveable { mutableFloatStateOf(Float.NaN) }
    // Deleting a layer: say what happened and offer undo in one tap.
    val scope = rememberCoroutineScope()
    // Read here, not inside the lambda: `stringResource` is a composable call and
    // the snackbar is shown from a suspend function.
    val layerDeletedMessage = stringResource(R.string.editor_layer_deleted)
    val layerDeletedUndo = stringResource(R.string.editor_undo)
    fun deleteLayerWithUndo(id: String) {
        state.removeLayer(id)
        scope.launch {
            val result = snackbarHostState.showSnackbar(
                message = layerDeletedMessage,
                actionLabel = layerDeletedUndo,
                duration = SnackbarDuration.Short,
            )
            if (result == SnackbarResult.ActionPerformed) state.undo()
        }
    }
    var showExportSettings by remember { mutableStateOf(false) }
    var exportProgress by remember { mutableStateOf<Float?>(null) }
    var exportError by remember { mutableStateOf<String?>(null) }
    // An export waiting for write permission (API 26-28): we start only after a
    // positive answer to the WRITE_EXTERNAL_STORAGE request.
    var pendingExportSettings by remember { mutableStateOf<ExportSettings?>(null) }
    var previewSizePx by remember { mutableStateOf(IntSize.Zero) }

    // The preset catalog is kept at screen level: the settings dialog opens
    // again without unnecessary JNI calls.
    val exportPresets = remember { RumoBridge.resolutionPresets() }
    val exportFpsOptions = remember { RumoBridge.frameRateOptions() }
    val exportPortraitAvailable = remember { RumoBridge.aspectPresets().any { it.h > it.w } }

    // Bitmap-path thumbnail of the first MEDIA layer (offscreen export reads the
    // same bitmap, so it lives here and not inside the preview composable).
    val mediaUri = mediaLayers.firstOrNull { it.uri != null }?.uri
    var frameThumbnail by remember(mediaUri) { mutableStateOf<ImageBitmap?>(null) }
    LaunchedEffect(mediaUri) {
        frameThumbnail = null
        if (mediaUri == null) return@LaunchedEffect
        withContext(Dispatchers.IO) {
            try {
                val bytes = readUriBytes(appContext, Uri.parse(mediaUri)) ?: return@withContext
                val decoded = RumoBridge.decodeImage(bytes) ?: return@withContext
                state.stageTexture(mediaUri, decoded)
                val bmp = Bitmap.createBitmap(
                    decoded.width,
                    decoded.height,
                    Bitmap.Config.ARGB_8888,
                )
                bmp.copyPixelsFromBuffer(ByteBuffer.wrap(decoded.rgba))
                frameThumbnail = bmp.asImageBitmap()
            } catch (_: Exception) {
                frameThumbnail = null
            }
        }
        state.gcTextures(mediaLayers.mapNotNull { it.uri }.toSet())
    }

    DisposableEffect(Unit) {
        // Context is needed for lazily opening video decoders in the frame.
        state.bindVideoContext(appContext)
        state.acquireEngine()
        onDispose { state.releaseEngine() }
    }

    LaunchedEffect(shapes.map { it.name }) {
        state.ensureMeshes(shapes.map { it.name })
    }

    // Encoding + moving into Download/Rumo. A separate function: it is also
    // called by the permission callback (API 26-28), which cannot be declared below
    // startMp4Export.
    fun runExportMp4(settings: ExportSettings) {
        val durationSnap = projectDurationMs
        // Resolution/fps are chosen by the user; the bitrate is computed by Rust
        // (a fallback constant if the symbol is missing). The frame is rendered straight at the
        // target size — the bitmap in exportMp4 is created at the same size.
        val width = settings.width
        val height = settings.height
        val fpsInt = settings.fps.coerceAtLeast(1)
        val fps = fpsInt.toFloat()
        val bitrate = RumoBridge.bitrateFor(width, height, fpsInt)
        showExportDialog = true
        exportProgress = 0f
        exportError = null
        // The export is placed under the service's supervision before the first frame: it runs for minutes,
        // and leaving the app must not kill it.
        val exportWork = RumoWork.start(
            RumoWork.Kind.EXPORT,
            appContext.getString(R.string.editor_export_running),
        )
        ioScope.launch {
          try {
            val tmp = File(appContext.cacheDir, exportMp4FileName())
            AppLog.info("export", "start ${width}x$height @$fpsInt fps -> ${tmp.name}")
            val ok = try {
                Exporter.exportMp4(
                    outPath = tmp.absolutePath,
                    width = width,
                    height = height,
                    fps = fps,
                    bitrate = bitrate,
                    durationMs = durationSnap,
                    frameAt = { t -> state.previewFrameExAt(t, width, height) },
                    onProgress = { p ->
                        exportProgress = p
                        // The percentages also go into the notification: the screen may be off,
                        // and the notification is then the only place where it is visible that
                        // the export has not stalled.
                        exportWork.progress(p, "${(p * 100).toInt().coerceIn(0, 100)}%")
                    },
                    // Sound goes as the second track of the same MP4 (docs/11 §11.5): the muxer
                    // requires all tracks before it starts, so the sources are given
                    // before the first frame — that is what exportMp4 does.
                    audioContext = appContext,
                    audioSources = state.audioSourcesForExport().map { (layer, uri) ->
                        Exporter.AudioSource(
                            uri = uri,
                            startMs = layer.startMs,
                            durationMs = layer.durationMs,
                            // A layer has no separate volume; a layer's alpha is
                            // its on-screen transparency, not the sound level,
                            // so silence is not imposed: 1.0 = as it is.
                            gain = 1f,
                        )
                    },
                )
            } catch (t: Exception) {
                // This used to be `catch (_: Exception) { false }` — and the
                // reason vanished. Now it goes both to the log and to the UI.
                AppLog.error("export", "exportMp4 threw", t)
                false
            }
            if (!ok) {
                runCatching { tmp.delete() }
                val reason = Exporter.lastError.ifEmpty { "unknown reason" }
                AppLog.error("export", "failed: $reason")
                exportError = appContext.getString(
                    R.string.editor_export_failed,
                    shortReason(reason),
                )
                screenHaptic.hapticReject()
                return@launch
            }
            when (val res = saveMp4ToDownloads(appContext, tmp, tmp.name)) {
                is SaveResult.Ok -> {
                    runCatching { tmp.delete() }
                    exportProgress = 1f
                    screenHaptic.hapticConfirm()
                    Toast.makeText(
                        appContext,
                        appContext.getString(R.string.editor_export_saved),
                        Toast.LENGTH_SHORT,
                    ).show()
                    AppLog.info("export", "saved: ${res.path}")
                }
                is SaveResult.Failed -> {
                    runCatching { tmp.delete() }
                    val msg = appContext.getString(
                        R.string.editor_export_write_failed,
                        shortReason(res.reason),
                    )
                    AppLog.error("export", msg)
                    exportError = msg
                    screenHaptic.hapticReject()
                    Toast.makeText(appContext, msg, Toast.LENGTH_LONG).show()
                }
            }
          } finally {
            // Cleared in `finally`, not in every branch: there are three branches, and forgetting
            // one would leave the notification and the process alive after the export.
            exportWork.close()
          }
        }
    }

    // API 26-28: without WRITE_EXTERNAL_STORAGE, MediaStore/File on Download will fail.
    // We ask BEFORE encoding and write the result to the log.
    val storagePermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        val pending = pendingExportSettings
        pendingExportSettings = null
        AppLog.info("storage", "WRITE_EXTERNAL_STORAGE granted=$granted")
        if (granted && pending != null) {
            runExportMp4(pending)
        } else {
            val msg = appContext.getString(R.string.diagnostics_storage_permission)
            AppLog.error("storage", msg)
            exportProgress = null
            exportError = msg
            showExportDialog = true
            screenHaptic.hapticReject()
            Toast.makeText(appContext, msg, Toast.LENGTH_LONG).show()
        }
    }

    fun startMp4Export(settings: ExportSettings) {
        val needsPermission = Build.VERSION.SDK_INT <= Build.VERSION_CODES.P &&
            ContextCompat.checkSelfPermission(
                appContext,
                Manifest.permission.WRITE_EXTERNAL_STORAGE,
            ) != PackageManager.PERMISSION_GRANTED
        if (needsPermission) {
            AppLog.info("storage", "request WRITE_EXTERNAL_STORAGE (API ${Build.VERSION.SDK_INT})")
            pendingExportSettings = settings
            storagePermissionLauncher.launch(Manifest.permission.WRITE_EXTERNAL_STORAGE)
            return
        }
        runExportMp4(settings)
    }

    fun saveProject(onDone: () -> Unit = {}) {
        val bytes = RumoBridge.projectFromJson(state.toJson())
        if (bytes == null) {
            AppLog.error(
                "project",
                "encode failed: projectFromJson returned null (${state.projectName.value})",
            )
            screenHaptic.hapticReject()
            // There used to be only a vibration here: the user pressed Save,
            // felt a rejection and did not know the reason — while the project remained
            // unsaved as they thought otherwise. Export and import announce
            // themselves with a toast; saving must do the same.
            Toast.makeText(
                appContext,
                appContext.getString(R.string.editor_save_encode_failed),
                Toast.LENGTH_LONG,
            ).show()
            return
        }
        ioScope.launch {
            val target = state.currentFileName.value
                ?: ProjectStore.fileNameFor(state.projectName.value)
            val actual = try {
                ProjectStore.save(appContext, target, bytes)
            } catch (t: Exception) {
                AppLog.error("project", "save $target failed", t)
                screenHaptic.hapticReject()
                Toast.makeText(
                    appContext,
                    appContext.getString(
                        R.string.editor_save_failed,
                        state.projectName.value,
                        t.message ?: appContext.getString(R.string.editor_save_unknown_error),
                    ),
                    Toast.LENGTH_LONG,
                ).show()
                return@launch
            }
            state.setCurrentFileName(actual)
            state.markSaved()
            AppLog.info("project", "saved $actual (${bytes.size} bytes)")
            screenHaptic.hapticConfirm()
            Toast.makeText(
                appContext,
                appContext.getString(R.string.editor_saved, state.projectName.value),
                Toast.LENGTH_SHORT,
            ).show()
            onDone()
        }
    }

    LaunchedEffect(fileName, keepCurrent) {
        if (keepCurrent) {
            // Nothing to load and nothing to discard: the project on screen is
            // the one to show, unsaved edits included.
            openFailed = false
        } else if (fileName == null) {
            state.newProject("New Project 1")
            openFailed = false
        } else {
            openFailed = false
            val bytes = ProjectStore.load(appContext, fileName)
            val json = bytes?.let { RumoBridge.projectToJson(it) }
            if (json != null && state.loadFromJson(json)) {
                state.setCurrentFileName(fileName)
                AppLog.info("project", "opened $fileName")
            } else {
                AppLog.warn(
                    "project",
                    "open $fileName failed (bytes=${bytes?.size ?: -1}, json=${json?.length ?: -1})",
                )
                state.newProject(fileName.removeSuffix(ProjectStore.EXT))
                state.setCurrentFileName(fileName)
                openFailed = true
            }
        }
    }

    val pickers = rememberMediaPicker { uri, mime ->
        when {
            mime?.startsWith("audio") == true -> {
                val name = queryDisplayName(appContext, uri, "audio")
                val id = state.addMediaLayer(
                    name,
                    LayerKindUi.AUDIO,
                    audioDurationMs(appContext, uri),
                    uri.toString(),
                )
                state.attachAudio(appContext, id, uri.toString())
            }
            mime?.startsWith("video") == true -> {
                // The probe is only by fd; Rust opens its own read window itself.
                // A failed probe does not block the import: the layer is created with the default
                // duration, and Rust will repeat the probe at render time.
                val name = queryDisplayName(appContext, uri, "video")
                // Probe the content descriptor first — cheap when it works — and
                // on failure fall back to a real file: the copy the app writes
                // into the project's folder is opened by path, which is the case
                // AMediaExtractor actually accepts. The path is remembered on
                // the layer so decoding uses the same descriptor, not another
                // one from the same provider that refuses.
                var info = videoInfo(appContext, uri)
                var localCopy: String? = null
                if (info == null) {
                    // The fallback needs the copy, and copying is suspending, so
                    // the whole import for this clip moves onto IO and finishes
                    // there. Nothing above this line is expensive: a display name
                    // and one probe.
                    val picked = uri
                    val name0 = name
                    val mime0 = mime
                    val project0 = state.projectName.value
                    ioScope.launch {
                        val copied = ProjectAssets.copyIn(appContext, project0, picked, mime0)
                        val absolute = externalStorageFile(copied.orEmpty())?.absolutePath
                        val probed = absolute?.let { videoInfoFromFile(it) }
                        finishVideoImport(appContext, state, picked, name0, mime0, probed, absolute)
                    }
                    showAddSheet = false
                    screenHaptic.hapticConfirm()
                    return@rememberMediaPicker
                }
                finishVideoImport(appContext, state, uri, name, mime, info, localCopy)
            }
            else -> {
                val name = queryDisplayName(appContext, uri, "image")
                state.addMediaLayer(
                    name,
                    LayerKindUi.MEDIA,
                    EditorState.DEFAULT_MIN_DURATION_MS,
                    uri.toString(),
                )
            }
        }
        // The project's folder is meant to be the one place its material lives,
        // so a picked reference is copied in when it is small enough to be worth
        // copying; the layer keeps pointing at where it came from.
        ioScope.launch {
            ProjectAssets.copyIn(appContext, state.projectName.value, uri, mime)
        }
        showAddSheet = false
        screenHaptic.hapticConfirm()
    }

    BackHandler(enabled = hasEdits) { showUnsavedDialog = true }
    BackHandler(enabled = showNodeGraph) { showNodeGraph = false }

    if (showCanvas) {
        val canvasW by state.canvasWidth.collectAsState()
        val canvasH by state.canvasHeight.collectAsState()
        val background by state.backgroundArgb.collectAsState()
        CanvasDialog(
            currentWidth = canvasW,
            currentHeight = canvasH,
            currentBackground = background,
            onApply = { w, h, bg ->
                state.applyCanvas(w, h, bg)
                showCanvas = false
                screenHaptic.hapticConfirm()
            },
            onDismiss = { showCanvas = false },
        )
    }

    Box(modifier = modifier.fillMaxSize()) {
        // Scaffold keeps the old app's window-inset behaviour: the top bar
        // consumes the status-bar inset itself (TopAppBarDefaults.windowInsets)
        // and the body inherits the navigation-bar inset at the bottom.
        Scaffold(
            containerColor = MaterialTheme.colorScheme.background,
            // A host for one thing: undoing a layer deletion. Deletion is the only
            // seemingly irreversible action of the panel, while Undo lives in the "⋮" menu of the top bar,
            // where no one looks after a miss (UX audit, T5).
            snackbarHost = { SnackbarHost(snackbarHostState) },
            topBar = {
                EditorTopBar(
                    projectName = projectName,
                    hasEdits = hasEdits,
                    openFailed = openFailed,
                    fill = previewFill,
                    canUndo = canUndo,
                    canRedo = canRedo,
                    onUndo = { state.undo() },
                    onRedo = { state.redo() },
                    onBack = { if (hasEdits) showUnsavedDialog = true else onBack() },
                    onAdd = { showAddSheet = true },
                    onSave = { saveProject() },
                    onExport = { showExportSettings = true },
                    onSettings = onOpenSettings,
                    onNodeGraph = { showNodeGraph = true },
                    onToggleFill = { previewFill = !previewFill },
                    onRenderDiagnostics = {
                        // The window opens with the current report: we read it once
                        // here (JNI + JSON), not in every frame.
                        state.refreshRenderDiagnostics()
                        showRenderDiagnostics = true
                    },
                    overflowOpen = showOverflow,
                    onOverflowOpenChange = { showOverflow = it },
                    onCanvas = { showCanvas = true },
                    onProjectFolder = {
                        // The scope is the composition's, so the toast is on the
                        // main thread; only the folder work hops to IO.
                        ioScope.launch {
                            val name = state.projectName.value
                            // Created first: opening a folder that does not exist
                            // yet would land on Download/Rumo instead.
                            ProjectAssets.ensureFolder(appContext, name)
                            if (!ProjectAssets.openFolder(appContext, name)) {
                                Toast.makeText(
                                    appContext,
                                    appContext.getString(
                                        R.string.editor_folder_hint,
                                        ProjectAssets.folderOf(name),
                                    ),
                                    Toast.LENGTH_LONG,
                                ).show()
                            }
                        }
                    },
                )
            },
        ) { scaffoldPadding ->
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(scaffoldPadding)
                    .background(MaterialTheme.colorScheme.background),
            ) {
                val editorModifier = Modifier
                    .fillMaxSize()
                    .padding(horizontal = RumoSpacing.m)

                if (isPortrait) {
                    // The dead band came from giving the preview a `weight(1f)`
                    // slot 2.4× taller than a 16:9 frame (507dp slot, 208dp frame):
                    // the surplus was pure black. The preview now takes its
                    // intrinsic height, the timeline and transport stay compact and
                    // the dock — a fixed pane that scrolls inside — takes every
                    // remaining dp, so no space is left unowned.
                    BoxWithConstraints(modifier = editorModifier) {
                        // How much the rail needs and how much can be given to the dock.
                        //
                        // The rail with a selection asks for 308dp (four surfaces
                        // plus "Done"), while `panelFloorHeight` = 268dp was a
                        // lower estimate that missed: the bottom surface
                        // was clipped and could not be tapped. The real
                        // minimum is computed from the slots.
                        val railSlots = DockPage.entries.size + if (selectedId != null) 1 else 0
                        val railNeeds = DockTokens.railContentHeight(railSlots)
                        val minDock = DockTokens.handleZone + DockTokens.railSlot
                        // The preview floor: the frame must stay usable, otherwise
                        // the panel's gain is paid for with a blind preview.
                        val previewFloor = 140.dp
                        val roomForDock = (
                            maxHeight - DockTokens.transportHeight - previewFloor -
                                (if (previewFill) 0.dp else DockTokens.timelineEstimate)
                            ).coerceAtLeast(minDock)
                        val maxDock = roomForDock.coerceAtMost(maxHeight * 0.8f)
                        val autoDock = railNeeds.coerceIn(minDock, maxDock)
                        val dockHeight = if (dockHeightDp.isNaN()) {
                            autoDock
                        } else {
                            dockHeightDp.dp.coerceIn(minDock, maxDock)
                        }

                        // The preview is flexible, the dock is fixed.
                        //
                        // It used to be the other way round: the dock took `weight(1f)`, that is,
                        // the remainder, while the preview in Fit mode was weightless and had no
                        // maximum. A `Column` measures a weightless child by the
                        // remainder, and the frame height is computed from that same `maxHeight`
                        // (`frameWidth = min(maxWidth, maxHeight * aspect)`), so for a
                        // tall canvas the frame took up **the whole** column height.
                        // After that the remainder for the dock was exactly 0, and the panel
                        // disappeared — that was the complaint "if Preview is large, the dock
                        // goes all the way down".
                        //
                        // Now `weight(1f)` is on the preview: the weightless neighbours
                        // (timeline, transport, dock) are measured first and get
                        // their heights, and the preview takes whatever is left. No
                        // mode can starve the dock any more.
                        Column(modifier = Modifier.fillMaxSize()) {
                            PreviewArea(
                                state = state,
                                shapes = shapes,
                                textLayers = textLayers,
                                mediaLayers = mediaLayers,
                                selectedId = selectedId,
                                selectedLayer = selectedLayer,
                                thumbnail = frameThumbnail,
                                useSurface = engineReady && !surfaceFailed,
                                playheadMs = playheadMs,
                                fill = previewFill,
                                onPreviewSize = {
                                    previewSizePx = it
                                    state.previewCanvasW = it.width.toFloat()
                                    state.previewCanvasH = it.height.toFloat()
                                },
                                // Flexible in all modes: `heightIn(min)` is a floor,
                                // so the frame does not collapse on a short screen.
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .weight(1f)
                                    .heightIn(min = 96.dp),
                            )
                            if (!previewFill) {
                                TimelineStrip(
                                    state = state,
                                    layers = layers,
                                    selectedId = selectedId,
                                    playheadMs = playheadMs,
                                    durationMs = projectDurationMs,
                                    onSelect = { state.selectLayer(it) },
                                    modifier = Modifier
                                        .fillMaxWidth()
                                        .padding(vertical = RumoSpacing.xs),
                                )
                            }
                            TransportBar(
                                playheadMs = playheadMs,
                                durationMs = projectDurationMs,
                                isPlaying = isPlaying,
                                onJumpStart = { state.jumpToStart() },
                                onJumpEnd = { state.jumpToEnd() },
                                onTogglePlay = { state.togglePlay() },
                                onSeek = { state.seekTo(it) },
                            )
                            EditorDock(
                                state = state,
                                layers = layers,
                                selectedLayer = selectedLayer,
                                selectedId = selectedId,
                                playheadMs = playheadMs,
                                isPlaying = isPlaying,
                                locked = selectedLayer?.id in lockedIds,
                                page = dockPage,
                                onPageChange = { state.setDockPage(it) },
                                onSelect = {
                                    // Selecting a row in the layer list, not an intention to edit.
                                    selectionFromLayerList = true
                                    state.selectLayer(it)
                                },
                                onImportImage = { pickers.launchImage() },
                                onImportAudio = { pickers.launchAudio() },
                                onImportVideo = { pickers.launchVideo() },
                                onAddLayer = { showAddSheet = true },
                                onDeleteLayer = { deleteLayerWithUndo(it) },
                                onOpenNodeGraph = { showNodeGraph = true },
                                onTogglePlayback = { state.togglePlay() },
                            onDeselect = { state.selectLayer(null) },
                                collapsed = dockCollapsed,
                                onCollapsedChange = { state.setDockCollapsed(it) },
                                // An explicit height, not the remainder: the dock has none of its
                                // own (the root is `fillMaxHeight`), and "how much is left"
                                // meant "how much the preview did not take".
                                onResize = { delta ->
                                    dockHeightDp = (dockHeight.value + delta)
                                        .coerceIn(minDock.value, maxDock.value)
                                },
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .height(
                                        if (dockCollapsed) {
                                            // A collapsed dock is a handle. `railSlot`,
                                            // not `railHeight`: the handle stands above
                                            // the rail, and the height must be its
                                            // slot, otherwise the handle does not fit.
                                            minDock
                                        } else {
                                            dockHeight
                                        },
                                    ),
                            )
                        }
                    }
                } else {
                    Row(
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(horizontal = RumoSpacing.m),
                        horizontalArrangement = Arrangement.spacedBy(RumoSpacing.m),
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            PreviewArea(
                                state = state,
                                shapes = shapes,
                                textLayers = textLayers,
                                mediaLayers = mediaLayers,
                                selectedId = selectedId,
                                selectedLayer = selectedLayer,
                                thumbnail = frameThumbnail,
                                useSurface = engineReady && !surfaceFailed,
                                playheadMs = playheadMs,
                                onPreviewSize = {
                                    previewSizePx = it
                                    state.previewCanvasW = it.width.toFloat()
                                    state.previewCanvasH = it.height.toFloat()
                                },
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .weight(1f),
                            )
                            TimelineStrip(
                                state = state,
                                layers = layers,
                                selectedId = selectedId,
                                playheadMs = playheadMs,
                                durationMs = projectDurationMs,
                                onSelect = { state.selectLayer(it) },
                                modifier = Modifier.padding(vertical = RumoSpacing.xs),
                            )
                            TransportBar(
                                playheadMs = playheadMs,
                                durationMs = projectDurationMs,
                                isPlaying = isPlaying,
                                onJumpStart = { state.jumpToStart() },
                                onJumpEnd = { state.jumpToEnd() },
                                onTogglePlay = { state.togglePlay() },
                                onSeek = { state.seekTo(it) },
                            )
                        }
                        EditorDock(
                            state = state,
                            layers = layers,
                            selectedLayer = selectedLayer,
                            selectedId = selectedId,
                            playheadMs = playheadMs,
                            isPlaying = isPlaying,
                            locked = selectedLayer?.id in lockedIds,
                            page = dockPage,
                            onPageChange = { state.setDockPage(it) },
                            onSelect = {
                                // Selecting a row in the layer list, not an intention to edit.
                                selectionFromLayerList = true
                                state.selectLayer(it)
                            },
                            onImportImage = { pickers.launchImage() },
                            onImportAudio = { pickers.launchAudio() },
                            onImportVideo = { pickers.launchVideo() },
                            onAddLayer = { showAddSheet = true },
                            onDeleteLayer = { deleteLayerWithUndo(it) },
                            onOpenNodeGraph = { showNodeGraph = true },
                            onTogglePlayback = { state.togglePlay() },
                            onDeselect = { state.selectLayer(null) },
                            collapsed = dockCollapsed,
                            onCollapsedChange = { state.setDockCollapsed(it) },
                            // Landscape is height-bound, so the preview column
                            // already fills its slot and keeps its 16:9 frame
                            // without a dead band; the dock stays a fixed-width
                            // pane that fills the height and scrolls inside.
                            modifier = Modifier.width(360.dp),
                        )
                    }
                }
            }
            }

        if (showNodeGraph) {
            NodeGraphOverlay(
                state = state,
                layer = selectedLayer,
                playheadMs = playheadMs,
                onDismiss = { showNodeGraph = false },
            )
        }

        if (showAddSheet) {
            AddSheet(
                state = state,
                onDismiss = { showAddSheet = false },
                pickers = pickers,
            )
        }
    }

    if (showExportSettings) {
        ExportSettingsDialog(
            presets = exportPresets,
            fpsOptions = exportFpsOptions,
            portraitAvailable = exportPortraitAvailable,
            onDismiss = { showExportSettings = false },
            onConfirm = { settings ->
                showExportSettings = false
                startMp4Export(settings)
            },
        )
    }

    if (showExportDialog) {
        Mp4ExportDialog(
            progress = exportProgress,
            error = exportError,
            onDismiss = { showExportDialog = false },
        )
    }

    if (showUnsavedDialog) {
        AlertDialog(
            onDismissRequest = { showUnsavedDialog = false },
            title = { Text(stringResource(R.string.editor_unsaved_title)) },
            text = { Text(stringResource(R.string.editor_unsaved_message)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        // The dialog closes only on a successful save:
                        // otherwise, on a failure it vanished, no exit happened, and
                        // the user was left on the screen without explanation.
                        saveProject {
                            showUnsavedDialog = false
                            onBack()
                        }
                    },
                ) {
                    Text(stringResource(R.string.editor_unsaved_save_exit))
                }
            },
            dismissButton = {
                Row {
                    TextButton(
                        onClick = {
                            state.markSaved()
                            showUnsavedDialog = false
                            onBack()
                        },
                    ) {
                        Text(stringResource(R.string.editor_unsaved_discard))
                    }
                    TextButton(onClick = { showUnsavedDialog = false }) {
                        Text(stringResource(R.string.editor_cancel))
                    }
                }
            },
        )
    }

    if (showRenderDiagnostics) {
        RenderDiagnosticsDialog(
            diagnostics = renderDiagnostics,
            videoReport = state.videoStatusReport(),
            onRefresh = { state.refreshRenderDiagnostics() },
            onClear = { state.clearRenderDiagnostics() },
            onDismiss = { showRenderDiagnostics = false },
        )
    }
}

/** A short reason for the UI: the full text goes to AppLog anyway. */
private fun shortReason(reason: String, max: Int = 160): String =
    if (reason.length <= max) reason else reason.take(max) + "…"

@Composable
private fun EditorTopBar(
    projectName: String,
    hasEdits: Boolean,
    openFailed: Boolean,
    fill: Boolean,
    canUndo: Boolean,
    canRedo: Boolean,
    onUndo: () -> Unit,
    onRedo: () -> Unit,
    onBack: () -> Unit,
    onAdd: () -> Unit,
    onSave: () -> Unit,
    onExport: () -> Unit,
    onSettings: () -> Unit,
    onNodeGraph: () -> Unit,
    onToggleFill: () -> Unit,
    onRenderDiagnostics: () -> Unit,
    onCanvas: () -> Unit,
    onProjectFolder: () -> Unit,
    overflowOpen: Boolean,
    onOverflowOpenChange: (Boolean) -> Unit,
) {
    val saveTint by animateColorAsState(
        targetValue = if (hasEdits) {
            MaterialTheme.colorScheme.primary
        } else {
            MaterialTheme.colorScheme.onSurfaceVariant
        },
    )
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surface)
            .windowInsetsPadding(TopAppBarDefaults.windowInsets)
            .padding(
                start = RumoSpacing.xs,
                end = RumoSpacing.xs,
                top = RumoSpacing.xs,
                bottom = RumoSpacing.xs,
            ),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        IconButton(onClick = onBack) {
            Icon(Icons.AutoMirrored.Rounded.ArrowBack, contentDescription = stringResource(R.string.action_back))
        }
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = projectName,
                style = MaterialTheme.typography.titleMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            // The path/engine/edited line used to sit here in every frame. It
            // was developer telemetry over a consumer surface: the render path
            // and the diagnostics report now live behind the overflow menu, and
            // the only state worth a line is a failed open.
            if (openFailed) {
                Text(
                    text = stringResource(R.string.editor_open_failed),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.error,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        IconButton(onClick = onAdd) {
            Icon(Icons.Rounded.Add, contentDescription = stringResource(R.string.editor_add_layer))
        }
        IconButton(onClick = onSave) {
            Icon(
                Icons.Rounded.Save,
                contentDescription = stringResource(R.string.editor_save_project),
                tint = saveTint,
            )
        }
        Box {
            IconButton(onClick = { onOverflowOpenChange(true) }) {
                Icon(Icons.Rounded.MoreVert, contentDescription = stringResource(R.string.editor_more))
            }
            DropdownMenu(
                expanded = overflowOpen,
                onDismissRequest = { onOverflowOpenChange(false) },
            ) {
                // Undo/redo first, disabled rather than hidden: a greyed item
                // is how every editor says "there is nothing to take back",
                // which a missing item cannot.
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.editor_undo)) },
                    onClick = {
                        onOverflowOpenChange(false)
                        onUndo()
                    },
                    enabled = canUndo,
                    leadingIcon = { Icon(Icons.AutoMirrored.Rounded.Undo, contentDescription = null) },
                )
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.editor_redo)) },
                    onClick = {
                        onOverflowOpenChange(false)
                        onRedo()
                    },
                    enabled = canRedo,
                    leadingIcon = { Icon(Icons.AutoMirrored.Rounded.Redo, contentDescription = null) },
                )
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.editor_node_graph)) },
                    onClick = {
                        onOverflowOpenChange(false)
                        onNodeGraph()
                    },
                    leadingIcon = {
                        Icon(Icons.Rounded.AccountTree, contentDescription = null)
                    },
                )
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.editor_canvas_menu)) },
                    onClick = {
                        onOverflowOpenChange(false)
                        onCanvas()
                    },
                    leadingIcon = {
                        Icon(Icons.Rounded.AspectRatio, contentDescription = null)
                    },
                )
                // The project's own folder, where the material goes. The menu is
                // the only place a path is worth naming, and opening the folder
                // is how the user finds it without reading one.
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.editor_project_folder)) },
                    onClick = {
                        onOverflowOpenChange(false)
                        onProjectFolder()
                    },
                    leadingIcon = {
                        Icon(Icons.Rounded.FolderOpen, contentDescription = null)
                    },
                )
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.editor_export_mp4)) },
                    onClick = {
                        onOverflowOpenChange(false)
                        onExport()
                    },
                    leadingIcon = { Icon(Icons.Rounded.Share, contentDescription = null) },
                )
                DropdownMenuItem(
                    text = {
                        Text(
                            stringResource(
                                if (fill) R.string.editor_fit_frame else R.string.editor_fill_frame,
                            ),
                        )
                    },
                    onClick = {
                        onOverflowOpenChange(false)
                        onToggleFill()
                    },
                    leadingIcon = {
                        Icon(Icons.Rounded.AspectRatio, contentDescription = null)
                    },
                )
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.diagnostics_render)) },
                    onClick = {
                        onOverflowOpenChange(false)
                        onRenderDiagnostics()
                    },
                    leadingIcon = {
                        Icon(Icons.Rounded.BugReport, contentDescription = null)
                    },
                )
                HorizontalDivider()
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.settings_title)) },
                    onClick = {
                        onOverflowOpenChange(false)
                        onSettings()
                    },
                    leadingIcon = { Icon(Icons.Rounded.Settings, contentDescription = null) },
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// On-canvas transform chrome (dashed border + 8 scale handles + rotation handle
// + "NN% · NN°" badge). It is one Compose overlay above the preview content, so
// the SurfaceView path and the CPU-Canvas path get the same controls without
// duplicating the chrome into `drawPreviewContent`.
// ---------------------------------------------------------------------------

/** The drawn size of a square handle. */
private val HandleDrawSize = 11.dp

/** The full hit area of a handle; twice the drawn size, so the finger lands. */
private val HandleHitSize = 24.dp

/** How far the rotate handle sticks out above the top edge (the stem). */
private val RotateStemLength = 30.dp

/** The layer's rectangle in preview px (in the overlay's coordinate system). */
private class LayerBoxPx(
    val centre: Offset,
    val halfW: Float,
    val halfH: Float,
    /** The box's rotation about its centre, degrees; 0 for MEDIA (the engine does not rotate it). */
    val rotation: Float,
) {
    /** Whether a point falls inside the box (checked in the layer's coordinate system). */
    fun contains(p: Offset, tolerance: Float = 0f): Boolean {
        val local = rotateAbout(p, centre, -rotation)
        return abs(local.x - centre.x) <= halfW + tolerance &&
            abs(local.y - centre.y) <= halfH + tolerance
    }
}

/** What the current gesture is pulling. */
private enum class DragMode { MOVE, SCALE, ROTATE }

/**
  * The state of one gesture. The layer and its starting box are fixed in
  * `onDragStart`: the drag moves/scales/rotates exactly the layer the
  * gesture started on, not the current selection (which changes along with the box
  * while the gesture runs).
 */
private class TransformDrag(
    val layerId: String,
    val mode: DragMode,
    val box: LayerBoxPx,
    val startPointer: Offset,
    val startScale: Float,
    val startRotation: Float,
)

private fun rotateAbout(p: Offset, pivot: Offset, deg: Float): Offset {
    if (deg == 0f || !deg.isFinite()) return p
    val rad = Math.toRadians(deg.toDouble())
    val c = cos(rad).toFloat()
    val s = sin(rad).toFloat()
    val dx = p.x - pivot.x
    val dy = p.y - pivot.y
    return Offset(pivot.x + dx * c - dy * s, pivot.y + dx * s + dy * c)
}

/**
  * The layer's extent in preview px. MEDIA and SHAPE — a square of "60% of the frame height ×
  * scale" (Rust: `preview_shape_triples` / `image_mesh`), TEXT — the real
  * layout bounds, brought from the 512×288 scene into preview px; while the layout
  * is not warmed up, TEXT takes the same heuristic square as before.
 *
  * An SVG layer is a special SHAPE: the engine fits the document so that its **longer**
  * side occupies that same box (`svg_meshes`), so when the document's size is known
  * the box repeats its proportion (`svgSize`), and the square remains
  * only as a fallback while the sizes are unknown.
 */
private fun layerBoxPx(
    layer: LayerUi,
    frameW: Float,
    frameH: Float,
    rotationDeg: Float,
    textBounds: FloatArray?,
    mediaSize: IntArray? = null,
    svgSize: FloatArray? = null,
): LayerBoxPx? {
    if (frameW <= 0f || frameH <= 0f) return null
    val centre = Offset(frameW / 2f + layer.offsetX, frameH / 2f + layer.offsetY)
    return when (layer.kind) {
        LayerKindUi.MEDIA -> {
            val side = frameH * 0.6f * layer.scale.coerceIn(LayerUi.MIN_SCALE, LayerUi.MAX_SCALE)
            if (side <= 0f) return null
            // The same `fitInside` as in the frame build: the selection box must
            // follow the photo's bounds, otherwise the scale handles pull something other than
            // what is visible on the screen.
            val fit = EditorState.fitInside(side, side, mediaSize?.get(0), mediaSize?.get(1))
            LayerBoxPx(centre, fit[0] / 2f, fit[1] / 2f, 0f)
        }
        LayerKindUi.SHAPE -> {
            val side = frameH * 0.6f * layer.scale.coerceIn(LayerUi.MIN_SCALE, LayerUi.MAX_SCALE)
            if (side <= 0f) return null
            val w = svgSize?.get(0) ?: 0f
            val h = svgSize?.get(1) ?: 0f
            if (w > 0f && h > 0f) {
                // Exactly the engine's rule (`svg_meshes`): the longer side is
                // `side`, the shorter follows the proportion. We compute in Float, not
                // through `fitInside`: that rounds the source to Int and would lose
                // the proportion of a small document (for example, a viewBox 3×1).
                val k = side / maxOf(w, h)
                LayerBoxPx(centre, w * k / 2f, h * k / 2f, rotationDeg)
            } else {
                LayerBoxPx(centre, side / 2f, side / 2f, rotationDeg)
            }
        }
        LayerKindUi.TEXT -> {
            // The layout bounds are given in scene px of PREVIEW_H height.
            val k = frameH / EditorState.PREVIEW_H.toFloat()
            val b = textBounds
            if (b != null && b.size >= 2 && b[0] > 0f && b[1] > 0f) {
                LayerBoxPx(centre, b[0] * k / 2f, b[1] * k / 2f, rotationDeg)
            } else {
                val side = frameH * 0.6f
                LayerBoxPx(centre, side / 2f, side / 2f, rotationDeg)
            }
        }
        LayerKindUi.AUDIO -> null
    }
}

/** The layer's rotation for the chrome: the engine does not rotate MEDIA (no angle array). */
private fun chromeRotationFor(layer: LayerUi, rotation: Float): Float =
    if (layer.kind == LayerKindUi.MEDIA) 0f else rotation

/** Whether the layer can be scaled: TEXT has no per-layer scale in the engine. */
private fun layerSupportsScale(layer: LayerUi): Boolean =
    layer.kind == LayerKindUi.MEDIA || layer.kind == LayerKindUi.SHAPE

/** Eight box handles (clockwise from the top-left) plus the rotate handle. */
private fun handlePositions(
    box: LayerBoxPx,
    stemPx: Float,
    inset: Float,
    frameW: Float,
    frameH: Float,
): List<Offset> {
    val c = box.centre
    val hw = box.halfW
    val hh = box.halfH
    val local = listOf(
        Offset(c.x - hw, c.y - hh),
        Offset(c.x, c.y - hh),
        Offset(c.x + hw, c.y - hh),
        Offset(c.x + hw, c.y),
        Offset(c.x + hw, c.y + hh),
        Offset(c.x, c.y + hh),
        Offset(c.x - hw, c.y + hh),
        Offset(c.x - hw, c.y),
        Offset(c.x, c.y - hh - stemPx),
    )
    val maxX = (frameW - inset).coerceAtLeast(inset)
    val maxY = (frameH - inset).coerceAtLeast(inset)
    return local.map { p ->
        // A handle that has gone off-frame (a large scale) sticks to its edge:
        // otherwise it could not be grabbed — the frame is clipped to its shape.
        val r = rotateAbout(p, c, box.rotation)
        Offset(r.x.coerceIn(inset, maxX), r.y.coerceIn(inset, maxY))
    }
}

/**
 * Preview. The frame is 16:9 and its size depends only on the available box, so
 * selecting a layer can never resize it (the old inline inspector is gone).
 * Touches reach the frame through an overlay, which works in both render paths.
 */
@Composable
private fun PreviewArea(
    state: EditorState,
    shapes: List<LayerUi>,
    textLayers: List<LayerUi>,
    mediaLayers: List<LayerUi>,
    selectedId: String?,
    selectedLayer: LayerUi?,
    thumbnail: ImageBitmap?,
    useSurface: Boolean,
    playheadMs: Long,
    fill: Boolean = false,
    onPreviewSize: (IntSize) -> Unit,
    modifier: Modifier = Modifier,
) {
    val haptic = LocalHapticFeedback.current
    val primary = MaterialTheme.colorScheme.primary
    var previewSize by remember { mutableStateOf(IntSize.Zero) }

    // Live values for the restart-proof gesture handlers. `shapes`,
    // `textLayers`, `mediaLayers` and `selectedLayer` are fresh instances on
    // every recomposition, so keying `pointerInput` on them restarted the
    // detector mid-gesture: the first move event recomposed, the key changed
    // and Compose cancelled the in-flight drag — the "jumps once, then stops"
    // bug. The detectors below are keyed on `Unit` and read the current values
    // through these snapshots instead. `previewSize` is itself a
    // `mutableStateOf` read inside the lambdas, so it stays current.
    val liveShapes by rememberUpdatedState(shapes)
    val liveTextLayers by rememberUpdatedState(textLayers)
    val liveMediaLayers by rememberUpdatedState(mediaLayers)
    val liveSelectedLayer by rememberUpdatedState(selectedLayer)
    val livePlayhead by rememberUpdatedState(playheadMs)

    // Handles are measured in px: the hit area does not depend on the frame size, so
    // 24 dp are the same on a small preview too.
    val handleHitPx = with(LocalDensity.current) { HandleHitSize.toPx() }
    val rotateStemPx = with(LocalDensity.current) { RotateStemLength.toPx() }

    /** The layer's box in frame px (see [layerBoxPx]). */
    fun boxOf(layer: LayerUi, w: Float, h: Float): LayerBoxPx? = layerBoxPx(
        layer = layer,
        frameW = w,
        frameH = h,
        rotationDeg = chromeRotationFor(layer, state.rotationAt(layer, livePlayhead)),
        textBounds = state.textBoundsFor(layer.id),
        mediaSize = layer.uri?.let { state.textureSizeFor(it) },
        // A proportion exists only for an SVG layer: a Material shape has no size of its
        // own, and an `svgSizeFor` request by its uri would be nonsense.
        svgSize = if (state.isSvgLayer(layer)) layer.uri?.let { state.svgSizeFor(it) } else null,
    )

    /**
      * The layer under a point. The traversal is in Rust's draw order (`build_ex_scene`:
      * SHAPE, then TEXT, then pictures), from the end, that is top-down through the
      * stack: the topmost layer wins. MEDIA is tested with its real
      * rectangle by the photo's proportions — not a square, otherwise a tap past
      * the visible part of the picture would land in the empty corners of the box.
     */
    fun hitTest(pos: Offset): String? {
        val w = previewSize.width.toFloat()
        val h = previewSize.height.toFloat()
        if (w <= 0f || h <= 0f) return null
        val draws = liveShapes + liveTextLayers + liveMediaLayers
        for (layer in draws.asReversed()) {
            val box = boxOf(layer, w, h) ?: continue
            if (box.contains(pos)) return layer.id
        }
        return null
    }

    /**
      * The index of the layer handle under a point (0..7 — the box, 8 — rotate), or null.
      * Only the handles the layer really supports are checked: TEXT
      * has no per-layer scale in the engine, MEDIA has no rotation (no angle array in
      * Rust's texture group) — dead controls must not respond to a gesture.
     */
    fun handleAt(pos: Offset, layer: LayerUi, box: LayerBoxPx, w: Float, h: Float): Int? {
        val half = handleHitPx / 2f
        val positions = handlePositions(box, rotateStemPx, half, w, h)
        var best: Int? = null
        var bestDist = Float.MAX_VALUE
        for (i in positions.indices) {
            if (i < 8 && !layerSupportsScale(layer)) continue
            if (i == 8 && layer.kind == LayerKindUi.MEDIA) continue
            val p = positions[i]
            if (abs(p.x - pos.x) > half || abs(p.y - pos.y) > half) continue
            val d = (p - pos).getDistanceSquared()
            if (d < bestDist) {
                bestDist = d
                best = i
            }
        }
        return best
    }

    BoxWithConstraints(
        modifier = modifier.then(if (fill) Modifier.clipToBounds() else Modifier),
        contentAlignment = Alignment.Center,
    ) {
        // The frame keeps the *canvas* aspect: width-bound in portrait,
        // height-bound in landscape. Nothing outside it is reserved, so no
        // letterbox band is ever left between the preview and the timeline.
        //
        // It used to be 16:9 hardcoded, which was true while every project was
        // 512×288; a vertical canvas would have been letterboxed inside a
        // horizontal card instead of filling it.
        val canvasW by state.canvasWidth.collectAsState()
        val canvasH by state.canvasHeight.collectAsState()
        val aspect = canvasW.toFloat() / canvasH.coerceAtLeast(1).toFloat()
        val frameWidth = minOf(maxWidth, maxHeight * aspect)
        val frameHeight = frameWidth / aspect
        // "Fill" zooms the same 16:9 card until it covers the slot and crops the
        // sides — the engine canvas keeps its 16:9 size, so only the view zooms.
        val coverScale = if (fill && frameWidth > 0.dp && frameHeight > 0.dp) {
            maxOf(maxWidth / frameWidth, maxHeight / frameHeight)
        } else {
            1f
        }

        Box(
            modifier = Modifier
                .size(frameWidth, frameHeight)
                .graphicsLayer {
                    scaleX = coverScale
                    scaleY = coverScale
                }
                .clip(RoundedCornerShape(18.dp))
                // A frame placeholder until the engine's first frame: a neutral
                // step of the scheme, not a constant — on a light theme a black
                // rectangle would read as a broken preview.
                .background(MaterialTheme.colorScheme.surfaceContainerLowest)
                .border(
                    width = 1.dp,
                    color = MaterialTheme.colorScheme.outlineVariant,
                    shape = RoundedCornerShape(18.dp),
                )
                .onSizeChanged {
                    previewSize = it
                    onPreviewSize(it)
                },
        ) {
            val hasContent = shapes.isNotEmpty() || textLayers.isNotEmpty() ||
                thumbnail != null || mediaLayers.isNotEmpty()
            if (!hasContent) {
                EmptyPreviewHint()
            } else if (useSurface) {
                // The engine owns the pixels through the SurfaceView. It is only
                // mounted once the engine object exists, so the surface is always
                // created by a path that can actually succeed; the touch overlay
                // below sits above it either way.
                SurfacePreview(state = state, modifier = Modifier.fillMaxSize())
            } else {
                // No engine, or the engine refused a surface: there is no frame.
                // A Compose approximation used to be drawn here, with a Rust
                // Bitmap over it when one existed; both are gone (docs/12
                // §12.4). The approximation never matched what the export
                // writes, and two renderers are two pictures.
                GpuUnavailablePreview()
            }

            // Touch overlay + transform chrome, in both render paths. The
            // detectors are keyed on `Unit` (see the live* snapshots above), so
            // no recomposition can cancel a gesture in flight. A locked layer is
            // selectable but not draggable/scalable/rotatable.
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    // A two-finger gesture: scale, rotate and move the selected
                    // layer — what used to be done only by handles with a 24dp
                    // touch area, which is hard to hit on a phone.
                    //
                    // A separate detector, not `detectTransformGestures`: that would
                    // also take the single-finger drag that works, and it must not be
                    // rewritten blind. Here the gesture engages
                    // only from the second finger and only then consumes
                    // events, so a tap and a single-finger drag go the
                    // same way as before — the detector is first in the chain and on one
                    // finger does not touch the stream at all.
                    .pointerInput(Unit) {
                        awaitEachGesture {
                            awaitFirstDown(requireUnconsumed = false)
                            val sel = liveSelectedLayer
                            val w = previewSize.width.toFloat()
                            val h = previewSize.height.toFloat()
                            val box = if (sel != null && w > 0f && h > 0f) {
                                boxOf(sel, w, h)
                            } else {
                                null
                            }
                            val locked = sel == null || sel.id in state.locked.value
                            val startScale = sel?.scale ?: 1f
                            val startRotation = sel?.let { state.rotationAt(it, livePlayhead) } ?: 0f
                            var began = false
                            var zoomAcc = 1f
                            var rotAcc = 0f
                            while (true) {
                                val event = awaitPointerEvent()
                                if (event.changes.none { it.pressed }) break
                                if (event.changes.count { it.pressed } < 2 ||
                                    sel == null || box == null || locked
                                ) {
                                    continue
                                }
                                if (!began) {
                                    began = true
                                    haptic.hapticToggle(true)
                                }
                                val zoom = event.calculateZoom()
                                val rot = event.calculateRotation()
                                val pan = event.calculatePan()
                                if (zoom.isFinite() && zoom > 0f) zoomAcc *= zoom
                                if (rot.isFinite()) rotAcc += rot
                                state.setScale(sel.id, startScale * zoomAcc)
                                if (rotAcc != 0f) {
                                    state.addKeyframe(
                                        sel.id,
                                        normalizeRotation(startRotation + rotAcc),
                                    )
                                }
                                if (pan.getDistance() > 0f) {
                                    state.moveLayer(sel.id, pan.x, pan.y)
                                }
                                // Consumed: otherwise a single-finger drag
                                // would move along with the pinch.
                                event.changes.forEach { it.consume() }
                            }
                            if (began) state.endGesture()
                        }
                    }
                    .pointerInput(Unit) {
                        detectTapGestures(
                            onTap = { pos ->
                                val w = previewSize.width.toFloat()
                                val h = previewSize.height.toFloat()
                                val sel = liveSelectedLayer
                                val box = sel?.let { boxOf(it, w, h) }
                                // A tap on a handle of the selected layer is not "selecting
                                // another layer": the selection does not change.
                                val onHandle = sel != null && box != null &&
                                    handleAt(pos, sel, box, w, h) != null
                                if (!onHandle) {
                                    val id = hitTest(pos)
                                    state.selectLayer(id)
                                    if (id != null) haptic.hapticToggle(true)
                                }
                            },
                        )
                    }
                    .pointerInput(Unit) {
                        // Per-gesture state lives in this coroutine's scope: it
                        // survives the whole drag and is reset on the next one.
                        var drag: TransformDrag? = null
                        detectDragGestures(
                            onDragStart = { pos ->
                                drag = null
                                val w = previewSize.width.toFloat()
                                val h = previewSize.height.toFloat()
                                // The lock is checked against the SPECIFIC layer, not
                                // the selected one: a locked selected layer
                                // must not forbid dragging the others.
                                if (w > 0f && h > 0f) {
                                    // 1. A handle of the selected layer.
                                    val sel = liveSelectedLayer
                                    val selBox = sel?.let { boxOf(it, w, h) }
                                    val handle = if (sel != null && selBox != null) {
                                        handleAt(pos, sel, selBox, w, h)
                                    } else {
                                        null
                                    }
                                    val selUnlocked = sel != null && sel.id !in state.locked.value
                                    if (sel != null && selBox != null && handle != null && selUnlocked) {
                                        drag = TransformDrag(
                                            layerId = sel.id,
                                            mode = if (handle == 8) DragMode.ROTATE else DragMode.SCALE,
                                            box = selBox,
                                            startPointer = pos,
                                            startScale = sel.scale,
                                            startRotation = state.rotationAt(sel, livePlayhead),
                                        )
                                        haptic.hapticToggle(true)
                                    } else {
                                        // 2. The body of the layer under the finger: we move
                                        // EXACTLY it, not the current selection.
                                        val id = hitTest(pos)
                                        state.selectLayer(id)
                                        val layer = id?.let { hit ->
                                            (liveShapes + liveTextLayers + liveMediaLayers)
                                                .firstOrNull { it.id == hit }
                                        }
                                        if (id != null && layer != null &&
                                            id !in state.locked.value
                                        ) {
                                            haptic.hapticToggle(true)
                                            drag = boxOf(layer, w, h)?.let { box ->
                                                TransformDrag(
                                                    layerId = id,
                                                    mode = DragMode.MOVE,
                                                    box = box,
                                                    startPointer = pos,
                                                    startScale = layer.scale,
                                                    startRotation =
                                                        state.rotationAt(layer, livePlayhead),
                                                )
                                            }
                                        }
                                    }
                                }
                            },
                            onDragEnd = {
                                drag = null
                                // The drag fed moveLayer/setScale/setAlpha, which
                                // coalesce onto one entry; this closes it so the
                                // next drag pushes its own.
                                state.endGesture()
                                haptic.hapticConfirm()
                            },
                            onDragCancel = {
                                drag = null
                                state.endGesture()
                            },
                            onDrag = { change, dragAmount ->
                                change.consume()
                                val d = drag ?: return@detectDragGestures
                                when (d.mode) {
                                    DragMode.MOVE ->
                                        state.moveLayer(d.layerId, dragAmount.x, dragAmount.y)

                                    DragMode.SCALE -> {
                                        // Uniform: the ratio of the distances from
                                        // the layer's centre; the aspect does not change.
                                        val r0 = (d.startPointer - d.box.centre).getDistance()
                                        val r1 = (change.position - d.box.centre).getDistance()
                                        if (r0 > 1f) {
                                            state.setScale(
                                                d.layerId,
                                                d.startScale * (r1 / r0),
                                            )
                                        }
                                    }

                                    DragMode.ROTATE -> {
                                        // The pivot is the layer's centre; we write the same
                                        // keyframe as the inspector slider.
                                        val v0 = d.startPointer - d.box.centre
                                        val v1 = change.position - d.box.centre
                                        if (v0.getDistance() > 1f && v1.getDistance() > 1f) {
                                            val delta = Math.toDegrees(
                                                (atan2(v1.y, v1.x) - atan2(v0.y, v0.x)).toDouble(),
                                            ).toFloat()
                                            state.addKeyframe(
                                                d.layerId,
                                                normalizeRotation(d.startRotation + delta),
                                            )
                                        }
                                    }
                                }
                            },
                        )
                    },
            ) {
                // The single source of the chrome: it lies over both render
                // paths (SurfaceView and CPU-Canvas) and duplicates nothing.
                Canvas(modifier = Modifier.fillMaxSize()) {
                    val sel = selectedLayer ?: return@Canvas
                    val w = size.width
                    val h = size.height
                    val box = layerBoxPx(
                        layer = sel,
                        frameW = w,
                        frameH = h,
                        rotationDeg = chromeRotationFor(sel, state.rotationAt(sel, playheadMs)),
                        textBounds = state.textBoundsFor(sel.id),
                        mediaSize = sel.uri?.let { state.textureSizeFor(it) },
                        svgSize = if (state.isSvgLayer(sel)) {
                            sel.uri?.let { state.svgSizeFor(it) }
                        } else {
                            null
                        },
                    ) ?: return@Canvas
                    drawTransformChrome(
                        box = box,
                        scale = sel.scale,
                        rotationDeg = chromeRotationFor(sel, state.rotationAt(sel, playheadMs)),
                        supportsScale = layerSupportsScale(sel),
                        supportsRotation = sel.kind != LayerKindUi.MEDIA,
                        chromeColor = primary,
                        stemPx = rotateStemPx,
                        handlePx = HandleDrawSize.toPx(),
                        hitPx = handleHitPx,
                    )
                }
            }

            // No frame chrome. The stage size, the selected layer's name, the
            // fit mode and the lock state used to be painted over the picture in
            // four chips; they were labels a user reads once and then only
            // covers the frame with. Fit/fill moved to the overflow menu, the
            // lock state lives on the layer row, and the diagnostics window
            // (which the path chip opened) is in that same menu.
        }
    }
}

/**
 * Shown when the engine has no surface to draw on.
 *
 * This state used to be filled by a Compose approximation of the frame, with a
 * Rust-rendered `Bitmap` drawn over it when one existed. Both are gone: the
 * approximation never matched what the export writes, and a preview that shows a
 * *different* picture from the one being exported is worse than a preview that
 * says it has none (docs/12 §12.4).
 */
@Composable
private fun GpuUnavailablePreview() {
    Column(
        modifier = Modifier.fillMaxSize(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Icon(
            imageVector = Icons.Rounded.Warning,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(28.dp),
        )
        Text(
            text = stringResource(R.string.editor_preview_gpu_unavailable),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Text(
            text = stringResource(R.string.editor_preview_gpu_unavailable_hint),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun EmptyPreviewHint() {
    Column(
        modifier = Modifier.fillMaxSize(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Icon(
            imageVector = Icons.Rounded.Image,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(28.dp),
        )
        Text(
            text = stringResource(R.string.editor_preview_empty),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Text(
            text = stringResource(R.string.editor_preview_empty_hint),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** Transport: one row of verbs, the timeline scrubber and the master timecode. */
@Composable
private fun TransportBar(
    playheadMs: Long,
    durationMs: Long,
    isPlaying: Boolean,
    onJumpStart: () -> Unit,
    onJumpEnd: () -> Unit,
    onTogglePlay: () -> Unit,
    onSeek: (Long) -> Unit,
) {
    val haptic = LocalHapticFeedback.current
    val playCorner by animateDpAsState(
        targetValue = if (isPlaying) 8.dp else 16.dp,
        animationSpec = spring(stiffness = Spring.StiffnessMedium),
    )
    val fraction = if (durationMs > 0L) {
        (playheadMs.toFloat() / durationMs.toFloat()).coerceIn(0f, 1f)
    } else {
        0f
    }
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .height(DockTokens.transportHeight)
            .padding(bottom = RumoSpacing.xs),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(44.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(RumoSpacing.xs),
        ) {
            IconButton(
                onClick = {
                    haptic.hapticConfirm()
                    onJumpStart()
                },
                modifier = Modifier.size(44.dp),
            ) {
                Icon(
                    Icons.Rounded.SkipPrevious,
                    contentDescription = stringResource(R.string.editor_transport_start),
                    modifier = Modifier.size(22.dp),
                )
            }
            FilledTonalIconButton(
                onClick = {
                    haptic.hapticToggle(!isPlaying)
                    onTogglePlay()
                },
                shape = RoundedCornerShape(playCorner),
                modifier = Modifier.size(44.dp),
            ) {
                Icon(
                    imageVector = if (isPlaying) Icons.Rounded.Pause else Icons.Rounded.PlayArrow,
                    contentDescription = stringResource(
                        if (isPlaying) R.string.editor_transport_pause else R.string.editor_transport_play,
                    ),
                    modifier = Modifier.size(22.dp),
                )
            }
            IconButton(
                onClick = {
                    haptic.hapticConfirm()
                    onJumpEnd()
                },
                modifier = Modifier.size(44.dp),
            ) {
                Icon(
                    Icons.Rounded.SkipNext,
                    contentDescription = stringResource(R.string.editor_transport_end),
                    modifier = Modifier.size(22.dp),
                )
            }
            // The ruler scrub strip and this slider are the same seek.
            Slider(
                value = fraction,
                onValueChange = { onSeek((it * durationMs).toLong()) },
                onValueChangeFinished = { haptic.hapticConfirm() },
                modifier = Modifier.weight(1f),
            )
            Text(
                text = formatTime(playheadMs),
                style = MaterialTheme.typography.labelMedium.merge(monoNumerals),
            )
            Text(
                text = " / ${formatTime(durationMs)}",
                style = MaterialTheme.typography.labelSmall.merge(monoNumerals),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

// Render owner for the SurfaceView preview path.
@Composable
fun SurfacePreview(
    state: EditorState,
    modifier: Modifier = Modifier,
) {
    AndroidView(
        factory = { ctx ->
            SurfaceView(ctx).apply {
                holder.addCallback(
                    object : SurfaceHolder.Callback {
                        override fun surfaceCreated(holder: SurfaceHolder) {
                            val frame = holder.surfaceFrame
                            state.engineSurfaceCreated(
                                holder.surface,
                                frame.width(),
                                frame.height(),
                            )
                        }

                        override fun surfaceChanged(
                            holder: SurfaceHolder,
                            format: Int,
                            w: Int,
                            h: Int,
                        ) {
                            state.engineSurfaceChanged(w, h)
                        }

                        override fun surfaceDestroyed(holder: SurfaceHolder) {
                            state.engineSurfaceDestroyed()
                        }
                    },
                )
            }
        },
        modifier = modifier,
    )
}

/** Normalisation of an angle into (-180, 180] — as with the inspector slider. */
private fun normalizeRotation(deg: Float): Float {
    if (!deg.isFinite()) return 0f
    var v = deg % 360f
    if (v > 180f) v -= 360f
    if (v <= -180f) v += 360f
    return v
}

/**
  * The chrome of the selected layer: a dashed box along the bounds, 8 square handles
  * (corners + side midpoints, all uniform scale), a rotate handle above the top
  * edge on a stem, and a `NN% · NN°` badge under the box. Drawn over both
  * render paths; `handlePx` is the square's size, the hit area is set separately
  * ([HandleHitSize]) and does not depend on the frame's size.
 */
private fun DrawScope.drawTransformChrome(
    box: LayerBoxPx,
    scale: Float,
    rotationDeg: Float,
    supportsScale: Boolean,
    supportsRotation: Boolean,
    chromeColor: Color,
    stemPx: Float,
    handlePx: Float,
    hitPx: Float,
) {
    val w = size.width
    val h = size.height
    // The same inset as the hit area: otherwise the drawn handle and its
    // hit zone would diverge at the frame's edge.
    val hitInset = hitPx / 2f
    val positions = handlePositions(box, stemPx, hitInset, w, h)
    val c = box.centre
    val hw = box.halfW
    val hh = box.halfH
    val rot = box.rotation

    // The dashed box (4 rotated edges — a Path with a transform is not needed).
    val dash = PathEffect.dashPathEffect(floatArrayOf(7.dp.toPx(), 5.dp.toPx()), 0f)
    val stroke = 1.6.dp.toPx()
    val corners = listOf(
        rotateAbout(Offset(c.x - hw, c.y - hh), c, rot),
        rotateAbout(Offset(c.x + hw, c.y - hh), c, rot),
        rotateAbout(Offset(c.x + hw, c.y + hh), c, rot),
        rotateAbout(Offset(c.x - hw, c.y + hh), c, rot),
    )
    for (i in 0 until 4) {
        drawLine(
            color = chromeColor,
            start = corners[i],
            end = corners[(i + 1) % 4],
            strokeWidth = stroke,
            pathEffect = dash,
        )
    }

    // The rotate handle's stem: from the middle of the top edge outwards.
    if (supportsRotation) {
        val topEdge = rotateAbout(Offset(c.x, c.y - hh), c, rot)
        drawLine(
            color = chromeColor,
            start = topEdge,
            end = positions[8],
            strokeWidth = stroke,
        )
    }

    // Eight handles; corner and side ones — the same uniform scale.
    val half = handlePx / 2f
    val handleRects = if (supportsScale) positions.subList(0, 8) else emptyList()
    for (p in handleRects) {
        drawRect(
            color = chromeColor,
            topLeft = Offset(p.x - half, p.y - half),
            size = ComposeSize(handlePx, handlePx),
        )
        drawRect(
            color = Color.Black.copy(alpha = 0.55f),
            topLeft = Offset(p.x - half, p.y - half),
            size = ComposeSize(handlePx, handlePx),
            style = Stroke(1.dp.toPx()),
        )
    }
    if (supportsRotation) {
        val p = positions[8]
        drawCircle(color = chromeColor, radius = half, center = p)
        drawCircle(
            color = Color.Black.copy(alpha = 0.55f),
            radius = half,
            center = p,
            style = Stroke(1.dp.toPx()),
        )
    }

    // The `NN% · NN°` badge: only the parameters actually available, so as not to
    // show controls the layer does not have (TEXT without scale, MEDIA without
    // rotation).
    val label = buildString {
        if (supportsScale) append("${(scale * 100f).roundToInt()}%")
        if (supportsScale && supportsRotation) append(" · ")
        if (supportsRotation) append("${rotationDeg.roundToInt()}°")
    }
    if (label.isEmpty()) return
    val textSize = 11.dp.toPx()
    val paint = android.graphics.Paint().apply {
        isAntiAlias = true
        textAlign = android.graphics.Paint.Align.LEFT
        color = android.graphics.Color.WHITE
        this.textSize = textSize
    }
    val padX = 6.dp.toPx()
    val padY = 3.dp.toPx()
    val textW = paint.measureText(label)
    val badgeH = textSize + padY * 2f
    val bottom = rotateAbout(Offset(c.x, c.y + hh), c, rot)
    val boxLeft = (bottom.x - textW / 2f - padX).coerceIn(2.dp.toPx(), (w - textW - padX * 2f).coerceAtLeast(2.dp.toPx()))
    val boxTop = bottom.y + half + 4.dp.toPx()
    drawContext.canvas.nativeCanvas.apply {
        drawRoundRect(
            boxLeft,
            boxTop,
            boxLeft + textW + padX * 2f,
            boxTop + badgeH,
            4.dp.toPx(),
            4.dp.toPx(),
            android.graphics.Paint().apply {
                isAntiAlias = true
                color = android.graphics.Color.argb(190, 0, 0, 0)
            },
        )
        drawText(label, boxLeft + padX, boxTop + padY + textSize * 0.82f, paint)
    }
}

// Degraded path (no engine frame yet): shapes are drawn by the engine, so here
/**
 * Create the layers for a picked clip, from a probe that has already run.
 *
 * One function for both routes into it — a probe that worked on the first try,
 * and the copy-then-probe fallback — because the two used to drift: the second
 * branch had its own copy of the audio-splitting and the naming, and a fix to
 * one would leave the other wrong. `localPath` is the app's own copy of the
 * clip, when there is one, and it is what the decoder will open.
 */
private fun finishVideoImport(
    context: Context,
    state: EditorState,
    uri: Uri,
    name: String,
    mime: String?,
    info: RumoBridge.VideoInfo?,
    localPath: String?,
) {
    val duration = info?.durationMs?.takeIf { it > 0L }
        ?: EditorState.DEFAULT_MIN_DURATION_MS
    val layerId = state.addMediaLayer(name, LayerKindUi.MEDIA, duration, uri.toString())
    localPath?.let { state.noteLocalVideoPath(layerId, it) }
    info?.fps?.let { state.noteVideoFps(layerId, it) }
    // The frame's own size is what keeps the aspect ratio; without it a clip is
    // fitted into a square again.
    if (info != null && info.width > 0 && info.height > 0) {
        state.noteVideoSize(uri.toString(), info.width, info.height)
    }
    // A clip's sound is not thrown away: it gets its own AUDIO layer over the
    // same file, because otherwise there is no way to mute the picture and keep
    // the sound, or the other way round. symphonia is already built with `isomp4`
    // and `aac`, so no separate decoder is involved.
    if (info != null && info.hasAudio) {
        val soundId = state.addMediaLayer(name, LayerKindUi.AUDIO, duration, uri.toString())
        state.attachAudio(context, soundId, uri.toString())
        // Two layers with the same name are indistinguishable in the timeline,
        // and only one of them is being muted.
        state.renameLayer(soundId, "$name · audio")
    }
    state.markVideoLayer(layerId)
    if (info == null) {
        // A fact, not a promise. "Will retry at render" was a promise the app
        // could not keep: a container that was never opened fails identically at
        // every later attempt. The bridge has logged why already.
        Toast.makeText(
            context,
            context.getString(R.string.editor_video_unreadable),
            Toast.LENGTH_LONG,
        ).show()
    }
    AppLog.info("media", "imported $name: ${info?.width}x${info?.height} local=${localPath != null}")
}
