// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.rumi

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import com.kerneldroid.aiengines.RumiHost
import com.kerneldroid.aiengines.RumiProject
import com.kerneldroid.aiengines.RumiWork
import com.kerneldroid.aiengines.rumi.RumiAgentRegistry
import com.kerneldroid.aiengines.rumi.RumiToolbox
import com.kerneldroid.rumo.ui.DockPage
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.ProjectAssets
import com.kerneldroid.rumo.ui.Routes
import com.kerneldroid.rumo.work.RumoWork

/**
 * The app's side of the assistant: the editor state, the navigation, the
 * reference folder's permission and the tool implementations.
 *
 * This is the seam the port turned inside out. Rumi used to reach into the editor
 * through `RumiToolHost`, defined next to the tools; now the module reaches the
 * app only through [RumiHost], and this class is the app's implementation of it.
 * It is one object and not several because all of it is "the app": splitting it
 * would be splitting the app from itself.
 */
class RumoRumiHost(
    private val editor: EditorState,
    override val context: Context,
    private val onNavigate: (String) -> Unit,
) : RumiHost, RumiToolHost {

    // --- RumiToolHost: what the tool implementations see ---

    override val state: EditorState get() = editor

    // --- RumiHost: what the module sees ---

    /**
     * A tool surface for one conversation.
     *
     * The main conversation needs the sub-agent registry so the `task*` tools
     * exist; a sub-agent gets `null` and a restricted set, and `mayMutate` closes
     * off edits for the roles that only look. The restriction lives in `RumiTools`
     * rather than in a prompt: a tool the model does not have, it will not call.
     */
    override fun toolSet(
        agents: RumiAgentRegistry?,
        only: Set<String>?,
        mayMutate: Boolean,
    ): RumiToolbox = RumiTools(this, agents = agents, only = only, mayMutate = mayMutate)

    /**
     * The assistant asked for access to the references folder. The screen raises
     * the system dialog off this flag and clears it back.
     */
    override var wantMediaAccess by mutableStateOf(false)
        private set

    /** The dialog is already shown — so that an answer is not taken for the answer to another question. */
    private var mediaAccessAsked by mutableStateOf(false)

    /** Access was granted and the conversation should resume on its own. */
    override var resumeAfterMediaAccess by mutableStateOf(false)
        private set

    override fun mediaAccessAnswered(granted: Boolean) {
        if (mediaAccessAsked && granted) resumeAfterMediaAccess = true
        mediaAccessAsked = false
    }

    override fun consumeMediaAccessRequest(): Boolean {
        if (!wantMediaAccess) return false
        wantMediaAccess = false
        return true
    }

    override fun consumeResume(): Boolean {
        if (!resumeAfterMediaAccess) return false
        resumeAfterMediaAccess = false
        return true
    }

    override fun missingMediaPermissions(): List<String> = ProjectAssets.missingPermissions(context)

    override fun requestMediaAccess(): String {
        if (ProjectAssets.missingPermissions(context).isEmpty()) {
            return "Access to the project's reference folder is already granted."
        }
        mediaAccessAsked = true
        wantMediaAccess = true
        return "Asked the user for read-media access; the dialog is on their screen " +
            "now. Wait for them to answer, then call media(action=list) again — the " +
            "conversation continues on its own once they do."
    }

    // Asked of the platform every time rather than remembered: the user may grant
    // access while the conversation is open.
    override fun mediaAccessGranted(): Boolean = ProjectAssets.readable(context)

    override fun navigate(action: String, panel: String?): String = when (action) {
        "open_panel" -> {
            val name = panel?.lowercase().orEmpty()
            val page = dockPageOf(name)
            when {
                page != null -> {
                    // The panel opens in the editor's dock, but this screen is not
                    // left: taking the assistant away from its own tab would hide
                    // the messages that asked for the change in the first place.
                    // The confirmation tells the model exactly this.
                    editor.setDockPage(page)
                    editor.setDockCollapsed(false)
                    // The enum name, not `labelRes`: this string goes to the model,
                    // and the model names the panel in the same lowercase form when
                    // it calls `open_panel`. A translated label would be a name the
                    // model cannot call back.
                    "The ${page.name.lowercase()} panel is now selected in the editor's dock; " +
                        "switch to the editor to see it."
                }
                name == "none" -> {
                    editor.setDockCollapsed(true)
                    "The editor's dock is collapsed; switch to the editor to see it."
                }
                else -> "unsupported"
            }
        }
        // The only thing that actually leaves the tab, and only because the model
        // asked for the editor itself.
        "open_editor" -> {
            // `keep = true` — both for the model and for the menu item. Without it
            // the editor treats the route as "start over" and calls `newProject`,
            // which rebuilds the layer list: a request to Rumi to build something
            // followed by "opened the editor" showed an empty "New Project 1", and
            // the assistant's work disappeared. The in-memory project is its only
            // copy until it is saved.
            onNavigate(Routes.editorRoute(editor.currentFileName.value, keep = true))
            "Opened the editor."
        }
        else -> "unsupported"
    }

    override val project: RumiProject
        get() = RumiProject(
            name = editor.projectName.value,
            layerCount = editor.layers.value.size,
            durationMs = editor.projectDurationMs.value,
            playheadMs = editor.playheadMs.value,
            canvasWidth = editor.canvasWidth.value,
            canvasHeight = editor.canvasHeight.value,
            backgroundArgb = editor.backgroundArgb.value,
        )

    override fun startChatWork(label: String): RumiWork {
        val job = RumoWork.start(RumoWork.Kind.CHAT, label)
        return object : RumiWork {
            override fun detail(text: String) = job.detail(text)
            override fun close() = job.close()
        }
    }
}

/**
 * An editor panel by the name the model calls it.
 *
 * Old names keep working: a saved conversation may hold a call written before the
 * panels were merged, and a call that suddenly means nothing is worse than a call
 * that means the same as before.
 */
internal fun dockPageOf(panel: String): DockPage? = when (panel) {
    "media" -> DockPage.MEDIA
    "audio" -> DockPage.AUDIO
    "layers" -> DockPage.LAYERS
    // One surface for the selected layer.
    "properties", "props", "effects", "inspector", "nodes" -> DockPage.ADJUST
    // Typefaces, their previews and which of them the selected text layer uses.
    // A separate page rather than part of `properties`: browsing fonts is not
    // editing the selected layer.
    "fonts", "font" -> DockPage.FONTS
    else -> null
}
