// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo

import android.Manifest
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivityResultRegistryOwner
import androidx.activity.compose.LocalOnBackPressedDispatcherOwner
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.togetherWith
import androidx.compose.material3.MaterialTheme
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalContext
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.zIndex
import androidx.navigation.NavHostController
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import com.kerneldroid.rumo.data.AppLog
import com.kerneldroid.rumo.data.FontStore
import com.kerneldroid.rumo.data.ShopPrefs
import com.kerneldroid.rumo.ui.EditorScreen
import com.kerneldroid.rumo.ui.withAppLanguage
import com.kerneldroid.rumo.ui.theme.RumoEditorTheme
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.HomeScreen
import com.kerneldroid.rumo.ui.NodeLayoutRepo
import com.kerneldroid.rumo.ui.ProjectAssets
import com.kerneldroid.rumo.ui.ProjectsScreen
import com.kerneldroid.rumo.ui.Routes
import com.kerneldroid.rumo.ui.SaveResult
import com.kerneldroid.rumo.ui.SettingsRepo
import com.kerneldroid.rumo.ui.DockRoutes
import com.kerneldroid.rumo.ui.RumoBottomBar
import com.kerneldroid.rumo.ui.SettingsAppearanceScreen
import com.kerneldroid.rumo.ui.SettingsDiagnosticsScreen
import com.kerneldroid.rumo.ui.SettingsRumiScreen
import com.kerneldroid.rumo.ui.SettingsScreen
import com.kerneldroid.rumo.ui.rumi.RumoRumiHost
import com.kerneldroid.rumo.ui.shop.ShopScreen
import com.kerneldroid.aiengines.rumi.RumiController
import com.kerneldroid.aiengines.rumi.RumiScreen
import com.kerneldroid.rumo.work.RumoWork
import com.kerneldroid.aiengines.rumi.RumiSettings
import com.kerneldroid.aiengines.rumi.ai.AiServices
import com.kerneldroid.rumo.ui.theme.RumoTheme
import com.kerneldroid.rumo.ui.theme.rumoColorScheme

// Tabs replace each other (no growing back stack); the editor is pushed on top
// so Back returns to the tab that opened it.
private fun NavHostController.navigateTo(route: String) {
    if (route == Routes.EDITOR || route.startsWith(Routes.EDITOR + "?")) {
        navigate(route)
        return
    }
    navigate(route) {
        popUpTo(Routes.HOME) { saveState = true }
        launchSingleTop = true
        restoreState = true
    }
}

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        SettingsRepo.init(applicationContext)
        NodeLayoutRepo.init(applicationContext)
        RumiSettings.init(applicationContext)
        // The generative services' keys live in their own storage: connecting to a
        // model and being able to create something are enabled independently, and a
        // shared settings file would force reading one for the sake of the other.
        AiServices.init(applicationContext)
        ShopPrefs.init(applicationContext)
        setContent {
            val state = remember { EditorState() }

            // The notification permission is requested at the moment work starts, not at
            // launch: without it the foreground service still runs, but the notification
            // is invisible — and it is invisible exactly when it is the only proof that a
            // model response or an export is still going. Asking at first launch would
            // mean requesting permission for something the user was not doing at that
            // moment.
            val notificationsAllowed = rememberLauncherForActivityResult(
                ActivityResultContracts.RequestPermission(),
            ) { RumoWork.permissionAnswered() }
            val needsNotifications by RumoWork.needsPermission.collectAsState()
            LaunchedEffect(needsNotifications) {
                if (!needsNotifications) return@LaunchedEffect
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
                    RumoWork.permissionAnswered()
                    return@LaunchedEffect
                }
                notificationsAllowed.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
            // The project's reference folder exists for as long as the project
            // does, wherever the user is: the folder is where material goes, and
            // the assistant can ask about it from its own tab without the editor
            // ever having been shown. Idempotent — one lookup once the app's note
            // is in place. Collected rather than read as state so a new project
            // name does not recompose the whole app.
            LaunchedEffect(Unit) {
                // Downloaded fonts are registered in the engine at the app level, not in
                // the shop: a text layer has to find its face even when the shop tab has
                // never been opened. Blocking disk work — off the main thread.
                withContext(Dispatchers.IO) { FontStore.registerAll(applicationContext) }
            }
            LaunchedEffect(Unit) {
                // The same for effects: an effect installed yesterday has to be in the
                // menu today too, even when nobody opened the shop.
                state.loadInstalledEffects(applicationContext)
            }
            LaunchedEffect(Unit) {
                state.projectName.collect { name ->
                    val created = ProjectAssets.ensureFolder(applicationContext, name)
                    if (created is SaveResult.Failed) {
                        AppLog.warn("assets", "folder for `$name` not created: ${created.reason}")
                    }
                }
            }
            val settings by SettingsRepo.settings.collectAsState()
            // The same "darkness" for the app and for the editor: the editor used to be
            // forced dark and did not flip to light along with the other tabs, even
            // though its palette is derived from the scheme and holds the light stepped
            // ramp just like the dark one.
            val dark = when (settings.themeMode) {
                SettingsRepo.THEME_LIGHT -> false
                SettingsRepo.THEME_SYSTEM -> isSystemInDarkTheme()
                else -> true
            }
            // The language switch lives here, at the root, and not in a
            // per-screen helper: `stringResource` reads
            // `LocalContext.current.resources`, so one context carrying the
            // chosen locale translates every screen, every dialog and every
            // `getString` inside them at once.
            //
            // Not `recreate()`: the editor's `EditorState` is remembered inside
            // this composition, and restarting the activity to change a locale
            // would throw away an open project with unsaved edits. Swapping the
            // context recomposes the interface instead, which is instant and
            // loses nothing.
            val baseContext = LocalContext.current
            val localizedContext = remember(settings.appLanguage) {
                baseContext.withAppLanguage(settings.appLanguage)
            }
            CompositionLocalProvider(
                LocalContext provides localizedContext,
                // Swapping the context takes two owners with it, and nothing else in
                // this tree provides either one. `LocalActivityResultRegistryOwner` and
                // `LocalOnBackPressedDispatcherOwner` are each derived from
                // `LocalContext.current as? …Owner` when nobody provides them, which
                // held only while the context was the activity — and a context from
                // `createConfigurationContext` is neither. So every
                // `rememberLauncherForActivityResult` and every `BackHandler` below this
                // line found no owner and threw: the assistant's screen, which asks for
                // the media permission and handles Back itself, died on the launcher
                // before it had drawn anything. The activity is still the owner; the
                // context has just stopped implying it, so it is named here.
                LocalActivityResultRegistryOwner provides this,
                LocalOnBackPressedDispatcherOwner provides this,
            ) {
                RumoTheme(
                    forceDark = dark,
                    dynamicOverride = settings.dynamic,
                    seedOverride = Color(settings.seedArgb),
                ) {
                    // The editor's scheme: the same as the app's, including light mode.
                    // Dynamic colour and the seed apply as everywhere.
                    val editorScheme = rumoColorScheme(
                        dark = dark,
                        dynamic = settings.dynamic,
                        seed = Color(settings.seedArgb),
                    )
                    val navController = rememberNavController()
                    // The conversation lives here, not in the tab's screen.
                    //
                    // `NavHost` destroys the screen when leaving a tab, and `RumiSession` died
                    // with it: the history disappeared and the turn that was running was cut
                    // off. The conversation's owner now sits next to the dock — where
                    // everything else that survives a route change lives.
                    val rumi = remember(state) {
                        RumiController(
                            host = RumoRumiHost(
                                editor = state,
                                context = applicationContext,
                                onNavigate = { navController.navigateTo(it) },
                            ),
                        )
                    }
                    DisposableEffect(rumi) {
                        onDispose { rumi.close() }
                    }
                    // The conversation is saved when the app goes to the background too.
                    // The turn itself is written as it goes (see `RumiSession.autosave`), but
                    // "left the screen" is the last moment we know we are still alive: after
                    // that the system may kill the process, and the unsaved would vanish
                    // silently.
                    val lifecycleOwner = LocalLifecycleOwner.current
                    DisposableEffect(lifecycleOwner, rumi) {
                        val observer = LifecycleEventObserver { _, event ->
                            when (event) {
                                // Sub-agent conversations are part of the conversation too, and
                                // are saved by the same gesture.
                                Lifecycle.Event.ON_STOP -> {
                                    rumi.persistNow()
                                    RumoWork.setVisible(false)
                                }
                                // Returning to the screen is the only time a foreground
                                // service can be started at all, so `RumoWork` learns about it
                                // from here.
                                Lifecycle.Event.ON_START -> RumoWork.setVisible(true)
                                else -> Unit
                            }
                        }
                        lifecycleOwner.lifecycle.addObserver(observer)
                        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
                    }
                    // The dock lives here, not in every screen.
                    //
                    // `RumoBottomBar` used to be called from each screen's `Scaffold`, and on
                    // tab switches `NavHost` destroyed one screen and created another — that
                    // is, the dock was rebuilt with the item already selected.
                    // `AnimatedVisibility` does not animate on the first composition (there is
                    // no state change to animate from), so the label expanded instantly: the
                    // animation was missing not because of the spec but because no state
                    // transition existed.
                    //
                    // In Tomato the dock sits in `AppScreen`'s `bottomBar`, and `NavDisplay`
                    // changes only the content above it — a single instance lives all the time
                    // and sees the selected item change. Now it is the same here.
                    val navBackStackEntry by navController.currentBackStackEntryAsState()
                    val currentRoute = navBackStackEntry?.destination?.route

                    // The dock is an overlay on top of the content, not a `Scaffold` slot.
                    //
                    // In a `Scaffold` slot a strip is reserved for the dock, and it is painted
                    // with the container colour — a rectangle ended up behind the floating
                    // panel that should not be there. The slot also added its own status-bar
                    // inset on top of the inner screens' insets, hence the emptiness above the
                    // header.
                    //
                    // As an overlay (`zIndex(1f)`, as in Tomato) the content passes under the
                    // dock and it is genuinely transparent behind it. In exchange the lists
                    // keep a bottom reserve for themselves — see `DockContentInset`.
                    val motionScheme = MaterialTheme.motionScheme
                    // Whether the dock is hidden. It lives at the navigation level because the
                    // dock is drawn here, while the Rumi tab asks for the hiding; it is reset
                    // on every route change.
                    var dockHidden by remember { mutableStateOf(false) }
                    LaunchedEffect(currentRoute) { dockHidden = false }
                    Box(modifier = Modifier.fillMaxSize()) {
                        NavHost(
                            navController = navController,
                            startDestination = Routes.HOME,
                            modifier = Modifier.fillMaxSize(),
                            // A crossfade on **one** duration showed nothing: both tabs were
                            // semi-transparent for the whole transition and blended into a
                            // smooth mix that, on two identical backgrounds, reads as "no
                            // animation".
                            //
                            // Here the exit is fast and the entrance is normal: the leaving tab
                            // manages to fade into the background, a dimming is visible for a
                            // frame between them, and the next one appears over it. This is
                            // fade-through from Material 3, assembled from `MotionScheme` specs
                            // rather than custom `tween`s with curves: TASTE.md §9 forbids
                            // hand-rolled Easing in navigation.
                            //
                            // There is deliberately no scale here. There used to be — 92% → 100%
                            // — and it cost more than it gave: scaling a whole screen forces
                            // Compose to give it its own layer and redraw the screen into a
                            // texture every frame. On a sped-up animation scale the same frames
                            // fit into half the time, and the extra work reads as jank. The
                            // dimming gives the difference in durations, not a transform.
                            enterTransition = {
                                fadeIn(animationSpec = motionScheme.defaultEffectsSpec())
                            },
                            exitTransition = {
                                fadeOut(animationSpec = motionScheme.fastEffectsSpec())
                            },
                            popEnterTransition = {
                                fadeIn(animationSpec = motionScheme.defaultEffectsSpec())
                            },
                            popExitTransition = {
                                fadeOut(animationSpec = motionScheme.fastEffectsSpec())
                            },
                        ) {
                        composable(Routes.HOME) {
                            HomeScreen(onNavigate = navController::navigateTo)
                        }
                        // Rumi is a tab, not a sheet: the assistant edits the same
                        // EditorState the editor screen shows, so switching tabs is
                        // switching between the conversation and its result.
                        composable(Routes.RUMI) {
                            RumiScreen(
                                controller = rumi,
                                onOpenSettings = { navController.navigate(Routes.SETTINGS) },
                                dockHidden = dockHidden,
                                onDockHidden = { dockHidden = it },
                            )
                        }
                        composable(Routes.PROJECTS) {
                            ProjectsScreen(onNavigate = navController::navigateTo)
                        }
                        composable(Routes.SHOP) {
                            ShopScreen(
                                onNavigate = navController::navigateTo,
                                // An installed effect lives in the engine catalogue that the
                                // editor keeps in memory: without this it would appear in the
                                // menu only after a restart.
                                onLibraryChanged = { state.loadInstalledEffects(applicationContext) },
                            )
                        }
                        composable(Routes.SETTINGS) {
                            SettingsScreen(
                                onBack = { navController.popBackStack() },
                                onNavigate = navController::navigate,
                            )
                        }
                        // The settings sub-tabs. The transition to them is directional, as in
                        // the editor: this is a second level, not a neighbouring tab.
                        composable(Routes.SETTINGS_APPEARANCE) {
                            SettingsAppearanceScreen(onBack = { navController.popBackStack() })
                        }
                        composable(Routes.SETTINGS_RUMI) {
                            SettingsRumiScreen(onBack = { navController.popBackStack() })
                        }
                        composable(Routes.SETTINGS_DIAGNOSTICS) {
                            SettingsDiagnosticsScreen(onBack = { navController.popBackStack() })
                        }
                        composable(
                            route = Routes.EDITOR + "?file={file}&keep={keep}",
                            arguments = listOf(
                                navArgument("file") {
                                    type = NavType.StringType
                                    nullable = true
                                    defaultValue = null
                                },
                                navArgument("keep") {
                                    type = NavType.StringType
                                    nullable = true
                                    defaultValue = null
                                },
                            ),
                        ) { backStackEntry ->
                            // The editor's theme, not the app's: the editor is a dark neutral
                            // panel, while the home screen and the chat keep their own
                            // (TASTE.md). The wrapper is here rather than inside EditorScreen,
                            // so that no editor dialog ends up outside it.
                            //
                            // The editor's transition is directional: you reach it from the
                            // project list, from a template and from the FAB, and you have to
                            // return the same way. Tabs with neighbours do not work like that.
                            RumoEditorTheme(scheme = editorScheme, isDark = dark) {
                                AnimatedContent(
                                    targetState = backStackEntry,
                                    transitionSpec = {
                                        (slideInHorizontally(
                                            animationSpec = motionScheme.slowSpatialSpec(),
                                            initialOffsetX = { it / 4 },
                                        ) + fadeIn(motionScheme.slowEffectsSpec()))
                                            .togetherWith(
                                                fadeOut(motionScheme.fastEffectsSpec())
                                            )
                                    },
                                    label = "editor",
                                ) { entry ->
                                    EditorScreen(
                                        state = state,
                                        fileName = entry.arguments?.getString("file"),
                                        keepCurrent = entry.arguments?.getString("keep") == "1",
                                        onBack = { navController.popBackStack() },
                                        onOpenSettings = {
                                            navController.navigate(Routes.SETTINGS)
                                        },
                                    )
                                }
                            }
                        }
                        }

                        // The dock is on only four tabs: the editor has its own panel, and a
                        // navigation dock is not needed there.
                        //
                        // On the Rumi tab it goes away by itself: a conversation is reading and
                        // typing, and the tab bar only takes height from the input field the
                        // whole time. The state lives here rather than in the screen, because
                        // the dock is drawn here; it is reset on every route change — hiding
                        // belongs to the tab, not to the app.
                        if (currentRoute in DockRoutes) {
                            AnimatedVisibility(
                                visible = !dockHidden,
                                // Leaving downward, not dissolving in place: the panel has to
                                // read as having slid off the edge, otherwise it "blinks".
                                enter = slideInVertically(
                                    animationSpec = motionScheme.defaultSpatialSpec(),
                                ) { height -> height } +
                                    fadeIn(animationSpec = motionScheme.defaultEffectsSpec()),
                                exit = slideOutVertically(
                                    animationSpec = motionScheme.defaultSpatialSpec(),
                                ) { height -> height } +
                                    fadeOut(animationSpec = motionScheme.fastEffectsSpec()),
                                modifier = Modifier
                                    .align(Alignment.BottomCenter)
                                    .zIndex(1f),
                            ) {
                                RumoBottomBar(
                                    currentRoute = currentRoute ?: Routes.HOME,
                                    onNavigate = navController::navigateTo,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}
