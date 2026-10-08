// SPDX-License-Identifier: GPL-3.0-or-later
package com.kerneldroid.rumo.ui.rumi

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.net.Uri
import com.kerneldroid.rumo.data.AppLog
import com.kerneldroid.rumo.data.Exporter
import com.kerneldroid.rumo.data.FontStore
import com.kerneldroid.rumo.data.GoogleFonts
import com.kerneldroid.rumo.data.RumoBridge
import com.kerneldroid.rumo.data.ShopPrefs
import com.kerneldroid.rumo.ui.CustomEffectUi
import com.kerneldroid.rumo.ui.CustomPassUi
import com.kerneldroid.rumo.ui.CustomParamUi
import com.kerneldroid.rumo.ui.DownloadKind
import com.kerneldroid.rumo.ui.EaseUi
import com.kerneldroid.rumo.ui.EditorState
import com.kerneldroid.rumo.ui.Effect
import com.kerneldroid.rumo.ui.LayerKindUi
import com.kerneldroid.rumo.ui.KeyframeUi
import com.kerneldroid.rumo.ui.ProjectAssets
import com.kerneldroid.rumo.ui.ProjectStore
import android.os.Environment
import com.kerneldroid.rumo.ui.audioDurationMs
import com.kerneldroid.rumo.ui.queryDisplayName
import com.kerneldroid.rumo.ui.videoInfo
import com.kerneldroid.rumo.ui.SaveResult
import com.kerneldroid.rumo.ui.ShapeNames
import com.kerneldroid.rumo.ui.TransitionUi
import com.kerneldroid.rumo.ui.defaultEffectFor
import com.kerneldroid.rumo.ui.readUriBytes
import com.kerneldroid.aiengines.rumi.Prop
import com.kerneldroid.aiengines.rumi.RumiAgentRegistry
import com.kerneldroid.aiengines.rumi.RumiAgentRole
import com.kerneldroid.aiengines.rumi.RumiSearch
import com.kerneldroid.aiengines.rumi.RumiSettings
import com.kerneldroid.aiengines.rumi.RumiToolOutcome
import com.kerneldroid.aiengines.rumi.RumiToolSpec
import com.kerneldroid.aiengines.rumi.RumiToolbox
import com.kerneldroid.aiengines.rumi.bool
import com.kerneldroid.aiengines.rumi.num
import com.kerneldroid.aiengines.rumi.obj
import com.kerneldroid.aiengines.rumi.oneOf
import com.kerneldroid.aiengines.rumi.opt
import com.kerneldroid.aiengines.rumi.optText
import com.kerneldroid.aiengines.rumi.optTextOr
import com.kerneldroid.aiengines.rumi.req
import com.kerneldroid.aiengines.rumi.str
import com.kerneldroid.aiengines.rumi.ai.AiEntry
import com.kerneldroid.aiengines.rumi.ai.AiKind
import com.kerneldroid.aiengines.rumi.ai.AiOutcome
import com.kerneldroid.aiengines.rumi.ai.AiServices
import com.kerneldroid.aiengines.rumi.ai.AiTools
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import com.kerneldroid.rumo.ui.shapeOrdinalOf
import com.kerneldroid.rumo.ui.saveBytesToDownloads
import org.json.JSONArray
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.nio.IntBuffer

/**
 * What the assistant is allowed to drive.
 *
 * The tools reach the editor and the navigation graph through this interface
 * and nothing else, so the surface the model can touch is exactly what is
 * listed here — it cannot invent a capability the host does not implement.
 */
interface RumiToolHost {
    val state: EditorState
    val context: Context

    /**
     * Move around the app: `open_editor`, `home`, `close_panel`, or
     * `open_panel` with [panel]. Returns a short acknowledgement for the model.
     */
    fun navigate(action: String, panel: String?): String

    /** True when the app may read the references the user dropped in the project folder. */
    fun mediaAccessGranted(): Boolean

    /**
     * Ask for that access. Returns what the model should say about it — the
     * dialog is the user's, and a tool cannot wait for a human.
     */
    fun requestMediaAccess(): String
}

/**
 * The tools themselves.
 *
 * Every tool is a thin, honest wrapper over an editor operation: it reports what
 * it changed, refuses what it cannot do, and never silently succeeds. The two
 * tools that render — [snapshot] and [filmstrip] — exist so the model can look
 * at the result of its own work instead of guessing, and [compare_effects] and
 * [define_effect] give it the same evidence for effects.
 */
class RumiTools(
    private val host: RumiToolHost,
    /**
     * The sub-agent registry, if this tool set belongs to whoever is allowed to
     * spawn them.
     *
     * `null` means spawning is not allowed, and there will be no `task*` tools in
     * the list at all. That is how a sub-agent is built: it does not spawn
     * grandchildren. A tool the model sees but that always answers with a refusal
     * is worse than a missing one — it spends a turn on it.
     */
    private val agents: RumiAgentRegistry? = null,
    /**
     * Restriction of the set by name. `null` — the whole set.
     *
     * Needed by sub-agent roles: `explore` and `verifier` look and do not change,
     * and the restriction lives here rather than in the prompt. A tool the model
     * does not have, it will not call; a tool it is told "do not call" about, it
     * will call sooner or later.
     */
    private val only: Set<String>? = null,
    /**
     * Whether this set may change the project.
     *
     * Separate from [only], because a restriction by name does not express a
     * restriction by action: `media` has both `list` and `preview` and `add`. A
     * role allowed to look at references must look at them — and must not add
     * them. The refusal comes from the tool itself, not from the absence of the
     * tool, and says exactly what is not allowed.
     */
    private val mayMutate: Boolean = true,
) : RumiToolbox {
    private val state get() = host.state

    /** The properties `keys` can animate; rotation is the default. */
    private val keyProperties: Array<String> = EditorState.KeyTrack.entries.map { it.wire }.toTypedArray()

    /** Schemas handed to the provider for every tool. */
    val specs: List<RumiToolSpec> = mutableListOf<RumiToolSpec>().apply {
        add(
            RumiToolSpec(
                name = "project_state",
                description = "Read the whole project: resolution, duration, playhead, layers " +
                    "with their transform and effect chains, project-defined effects, and " +
                    "which layer is selected. Call this first, and again after a big change.",
                schemaJson = obj().toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "open_panel",
                description = "Show a panel in the editor, or switch screens. Panels are how the " +
                    "user sees what you are doing.",
                schemaJson = obj(
                    req(
                        "panel",
                        oneOf(
                            "Which panel to show: media, layers, properties, audio or fonts, " +
                                "or none to close it. `properties` is the surface for the selected " +
                                "layer — its transform, its appearance and its effect chain. " +
                                "`fonts` lists the faces available for text, with a preview of " +
                                "each; a face is chosen per layer with update_layer(font=…).",
                            "media", "layers", "properties", "audio", "fonts", "none",
                        ),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "select_layer",
                description = "Select a layer, so the inspector and the timeline show it.",
                schemaJson = obj(
                    req("layerId", str("Layer id from project_state.")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "seek",
                description = "Move the playhead to a time, in milliseconds.",
                schemaJson = obj(
                    req("timeMs", num("Time in milliseconds from the start of the project.")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "transport",
                description = "Playback control: play, pause, jump to the start, or jump to the end.",
                schemaJson = obj(
                    req("action", oneOf("What to do.", "play", "pause", "start", "end")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "add_text",
                description = "Add a text layer. Its size follows `scale`, so 1.0 is the default " +
                    "size and 2.0 is twice that; x and y are offsets from the centre of the " +
                    "frame in canvas pixels (the canvas is the project's own size; " +
                        "project_state reports it). A text layer has a colour, a face weight " +
                        "and an optional outline: the outline is drawn behind the glyphs, so it " +
                        "reads as a contour around the letters rather than as a thicker letter.",
                schemaJson = obj(
                    req("text", str("The text to show.")),
                    opt("scale", num("Size multiplier.", 0.05, 20.0)),
                    opt("argb", str("Colour as #RRGGBB or #AARRGGBB.")),
                    opt("weight", str("Face weight: a name (regular, medium, bold, black) or a number (100-900).")),
                    opt("strokePx", num("Outline thickness in canvas pixels; 0 for none.", 0.0, 20.0)),
                    opt("strokeArgb", str("Outline colour as #RRGGBB or #AARRGGBB.")),
                    opt("x", num("Horizontal offset from centre, scene pixels.")),
                    opt("y", num("Vertical offset from centre, scene pixels.")),
                    opt("alpha", num("Opacity, 0 to 1.", 0.0, 1.0)),
                    opt("durationMs", num("How long the layer lasts, milliseconds.", 1.0)),
                    opt("startMs", num("When the layer appears, milliseconds; 0 is the first frame.", 0.0)),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "add_shape",
                description = "Add a shape layer. The engine renders a fixed set of named " +
                    "shapes, so the name must be one of the ones listed here.",
                schemaJson = obj(
                    req("shape", oneOf("Shape name.", *ShapeNames.toTypedArray())),
                    opt("argb", str("Colour as #RRGGBB or #AARRGGBB.")),
                    opt("x", num("Horizontal offset from centre, scene pixels.")),
                    opt("y", num("Vertical offset from centre, scene pixels.")),
                    opt("scale", num("Size multiplier.", 0.05, 20.0)),
                    opt("alpha", num("Opacity, 0 to 1.", 0.0, 1.0)),
                    opt("durationMs", num("How long the layer lasts, milliseconds.", 1.0)),
                    opt("startMs", num("When the layer appears, milliseconds; 0 is the first frame.", 0.0)),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "keys",
                description = "Animate one property of a layer over time. A keyframe pins the " +
                    "value at one moment; between two keys the value follows the curve of " +
                    "the key it leaves, and outside them it holds. `property` picks which " +
                    "track the keys go on: rotation (the default), scale, position_x, " +
                    "position_y or alpha. An empty track means the property is not " +
                    "animated and the layer's own value is used everywhere, so `clear` " +
                    "restores that rather than leaving it at the last key. Use degrees " +
                    "for rotation and `value` for every other property: a multiplier " +
                    "for scale and alpha, canvas pixels for the two positions. " +
                    "`ease` shapes the motion: linear is the default and looks " +
                    "mechanical, ease_out settles, ease_in_out is the general-purpose " +
                    "one, snap overshoots and comes back, hold freezes then jumps, and " +
                    "hit leaves fast and stops dead. A curve can also be given as " +
                    "{x1,y1,x2,y2} or as cubic-bezier(...), where x is time (0..1) and " +
                    "y may pass 1 for an overshoot.",
                schemaJson = obj(
                    req(
                        "action",
                        oneOf(
                            "add pins the value at timeMs, ease sets the curve of the key " +
                                "already at that time, list reports the track with its " +
                                "curves, remove drops the key at that time, clear drops " +
                                "every key on that track.",
                            "add", "ease", "list", "remove", "clear",
                        ),
                    ),
                    req("layerId", str("Layer id from project_state.")),
                    opt(
                        "property",
                        oneOf(
                            "Which track the key goes on; rotation when left out.",
                            *keyProperties,
                        ),
                    ),
                    opt("timeMs", num("Time of the key, milliseconds; defaults to the playhead.")),
                    opt("degrees", num("Rotation at that time, in degrees; property=rotation only.", -3600.0, 3600.0)),
                    opt("value", num("Value for scale (multiplier), position_x/position_y (canvas pixels from centre) or alpha (0..1).", -100000.0, 100000.0)),
                    opt(
                        "ease",
                        str(
                            "Curve of the segment that starts at this key: a preset name " +
                                "(linear, hold, ease, ease_in, ease_out, ease_in_out, " +
                                "smooth, snap, hit) or cubic-bezier(x1,y1,x2,y2). " +
                                "Defaults to linear.",
                        ),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "update_layer",
                description = "Change one layer: text, colour, weight, outline, position, size, " +
                    "opacity, visibility, name or duration. Send only the fields you want to " +
                    "change.",
                schemaJson = obj(
                    req("layerId", str("Layer id from project_state.")),
                    opt("name", str("New label (ignored for shape layers).")),
                    opt("text", str("New text content, for text layers.")),
                    opt("argb", str("Colour as #RRGGBB or #AARRGGBB.")),
                    opt("weight", str("Face weight for text: a name (regular, medium, bold, black) or a number (100-900).")),
                    opt("strokePx", num("Outline thickness for text, in canvas pixels; 0 removes it.", 0.0, 20.0)),
                    opt("strokeArgb", str("Outline colour as #RRGGBB or #AARRGGBB.")),
                    opt("x", num("Horizontal offset from centre, scene pixels.")),
                    opt("y", num("Vertical offset from centre, scene pixels.")),
                    opt("scale", num("Size multiplier.", 0.05, 20.0)),
                    opt("alpha", num("Opacity, 0 to 1.", 0.0, 1.0)),
                    opt("visible", bool("Show or hide the layer.")),
                    opt("durationMs", num("How long the layer lasts, milliseconds.", 1.0)),
                    opt("startMs", num("When the layer appears, milliseconds; 0 is the first frame.", 0.0)),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "remove_layer",
                description = "Delete a layer and everything on it — its effect chain, its " +
                    "keyframes, its transition. The editor can undo this, but you cannot " +
                    "undo it for the user: remove what the user asked you to remove, not " +
                    "what merely looks unused.",
                schemaJson = obj(
                    req("layerId", str("Layer id from project_state.")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "set_transition",
                description = "Cross-fade a layer in against the visible layer below it: the layer " +
                    "below fades out while this one fades in, over one window. Set remove to " +
                    "clear it.",
                schemaJson = obj(
                    req("layerId", str("Layer id from project_state.")),
                    opt("startMs", num("When the fade starts, milliseconds.")),
                    opt("durationMs", num("How long the fade takes, milliseconds.", 1.0)),
                    opt("withPrevious", bool("Fade the layer below out at the same time.")),
                    opt("enabled", bool("Turn the transition off without removing it.")),
                    opt(
                        "ease",
                        str(
                            "Shape of the ramp: a preset name (linear, ease_out, snap, …) " +
                                "or cubic-bezier(x1,y1,x2,y2). Defaults to smooth, which is " +
                                "what every transition used before curves existed.",
                        ),
                    ),
                    opt("remove", bool("Remove the transition instead of setting one.")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "effects",
                description = "Work with a layer's effect chain. action=list returns the catalogue " +
                    "(built-in effects, project effects, and every parameter with its range). " +
                    "action=add appends an effect and answers with its effectId; if the layer " +
                    "is hidden, or the engine refused that effect, the answer says so, because " +
                    "in both cases the frame will look untouched.",
                schemaJson = obj(
                    req(
                        "action",
                        oneOf(
                            "list, add, set (write one parameter), remove, toggle, move (change order).",
                            "list", "add", "set", "remove", "toggle", "move",
                        ),
                    ),
                    opt("layerId", str("Layer id, required for everything except list.")),
                    opt("kind", str("Effect id from action=list, for action=add.")),
                    opt("effectId", str("Instance id from action=add, for set/remove/toggle/move.")),
                    opt("key", str("Parameter key, for action=set.")),
                    opt(
                        "value",
                        numbers(
                            "Parameter values, one per component of the kind: four for a " +
                                "colour, two to four for a vector, nine for mat3, sixteen for mat4.",
                        ),
                    ),
                    opt("delta", num("Order shift for action=move: -1 towards the start, +1 towards the end.")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "compare_effects",
                description = "Render the same frame three ways — as it is, with effect A, with " +
                    "effect B — and report how much each changes the picture. Use it before " +
                    "committing to an effect, and to check that a new effect does anything at " +
                    "all. `reproducible` says whether the baseline frame could be rendered " +
                    "twice identically; when it is false the numbers are noise, not evidence.",
                schemaJson = obj(
                    req("layerId", str("Layer to test on.")),
                    req("aKind", str("First effect id.")),
                    opt("aParams", numbers("Values for the first effect, in slot order.")),
                    req("bKind", str("Second effect id.")),
                    opt("bParams", numbers("Values for the second effect, in slot order.")),
                    opt("timeMs", num("Frame to test at; defaults to the playhead.")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "define_effect",
                description = "Create a project effect from a WGSL module of your own. The " +
                    "engine parses and validates it, requires the `Params` struct to match the " +
                    "parameters you declare, in order, and then renders a probe frame to confirm " +
                    "the effect changes the picture; only then can it be used.\n\n" +
                    "The module must not declare a vertex stage or a binding of its own: the " +
                    "host supplies both. These are the bindings you may use —\n" +
                    "  @group(0) @binding(0) var input_tex: texture_2d<f32>;   // the layer on " +
                    "pass 0, the previous pass's output after that\n" +
                    "  @group(0) @binding(1) var input_smp: sampler;\n" +
                    "  @group(0) @binding(2) var<uniform> frame: Frame;        // see below\n" +
                    "  @group(0) @binding(3) var origin_tex: texture_2d<f32>;  // the untouched " +
                    "layer, on every pass\n" +
                    "  @group(1) @binding(0) var<uniform> params: Params;\n" +
                    "Frame is host-owned: `size: vec2<f32>`, `time: f32` (seconds into the " +
                    "timeline), `pass_index: f32`, `texel_input: vec2<f32>`, `texel_origin: " +
                    "vec2<f32>`.\n" +
                    "Entry point signature: `@fragment fn name(@location(0) uv: vec2<f32>) -> " +
                    "@location(0) vec4<f32>`, with uv (0,0) at the top-left.\n" +
                    "`struct Params` must declare exactly the members your parameters describe, " +
                    "in order: a scalar kind is one `f32` named `key`; `color` is four `f32` " +
                    "named `key_r`, `key_g`, `key_b`, `key_a`; `vec2/vec3/vec4` and `mat3/mat4` " +
                    "are one member named `key` of that type. The host packs values into the " +
                    "struct's real layout, so vectors and matrices land where WGSL expects " +
                    "them — use them freely for warps, tints and transforms.\n" +
                    "Passes: up to eight, each with a `shrink` exponent (0..6, down to 1/64). " +
                    "Pass 0 reads the layer in `input_tex`; later passes read the previous " +
                    "pass's output there while `origin_tex` still holds the untouched layer, " +
                    "which is what a bloom or a multi-scale effect needs.",
                schemaJson = obj(
                    req("id", str("Short id, a-z 0-9 and _ only; how effects are referenced.")),
                    req("label", str("Human label for the effect panel.")),
                    req(
                        "source",
                        str(
                            "WGSL module. Declare `struct Frame` and `struct Params`, sample " +
                                "`input_tex` through `input_smp`, and one @fragment entry point " +
                                "per pass. No @vertex stage: the host supplies it.",
                        ),
                    ),
                    req(
                        "passes",
                        arrayOfObjects(
                            "Render passes in order; the last one produces the layer.",
                            obj(
                                req("entry", str("Fragment entry point name.")),
                                opt(
                                    "shrink",
                                    num(
                                        "Downscale exponent for this pass: 0 keeps full size, " +
                                            "6 is 1/64.",
                                        0.0,
                                        6.0,
                                    ),
                                ),
                            ),
                        ),
                    ),
                    opt(
                        "params",
                        arrayOfObjects(
                            "Parameters, in the order of `struct Params` fields.",
                            obj(
                                req("key", str("Field name in `struct Params`.")),
                                opt("label", str("Label for the panel.")),
                                opt(
                                    "kind",
                                    oneOf(
                                        "float, angle, int, bool, choice, color, vec2, " +
                                            "vec3, vec4, mat3 or mat4.",
                                        "float", "angle", "int", "bool", "choice", "color",
                                        "vec2", "vec3", "vec4", "mat3", "mat4",
                                    ),
                                ),
                                opt("min", num("Lowest value.")),
                                opt("max", num("Highest value.")),
                                opt("default", numbers("Default values, one per component.")),
                                opt("unit", str("Unit shown in the panel, e.g. px.")),
                                opt("choices", strings("Labels, for kind=choice.")),
                            ),
                        ),
                    ),
                    opt("space", oneOf("Colour space: display or linear.", "display", "linear")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "svg_paint",
                description = "Draw vector art: write SVG source and put it on the timeline as a " +
                    "real SVG layer. The engine parses and tessellates it into coloured " +
                    "geometry and draws it like any other shape layer — it scales without " +
                    "pixelating and takes the usual transform, keyframes, effects and " +
                    "transitions. It is not rasterised to a PNG.\n\n" +
                    "The source is written into the project folder (so the user owns the file " +
                    "and can reuse it) and then added as a layer; the tool renders a probe frame " +
                    "and answers with the numbers that show it drew something, plus a PNG.\n\n" +
                    "What the renderer supports today:\n" +
                    "  - paths, polygons, polylines, lines, rects, circles and ellipses; " +
                    "`fill`, `stroke`, `stroke-width`, `opacity` and `transform`; multiple " +
                    "`<path>`/shape elements.\n" +
                    "  - gradients (linear/radial) are FLATTENED to one colour each: the colour " +
                    "is kept but the transition is not. The count is reported back, so you know " +
                    "how many were flattened.\n" +
                    "  - NOT supported: `<pattern>`, group opacity (`opacity` on a `<g>`), " +
                    "embedded raster images, CSS stylesheets and `<use>`. Unsupported elements " +
                    "are skipped and counted, not silently dropped.\n" +
                    "Prefer flat fills and a handful of paths; that is what survives tessellation " +
                    "intact.\n\n" +
                    "This makes a *vector* layer. If the document needs what the renderer " +
                    "refuses — text, filters, masks, clip paths or patterns (the count comes " +
                    "back as `skipped`) — use `svg_rasterize` instead: it renders the whole " +
                    "document as a picture, which does not scale.",
                schemaJson = obj(
                    req(
                        "svg",
                        str(
                            "The SVG source text itself. Include the root <svg> element with a " +
                                "viewBox (or width/height); it is drawn as authored, so its own " +
                                "colours are used — the layer has no tint.",
                        ),
                    ),
                    opt(
                        "name",
                        str(
                            "File name for the asset; `.svg` is added if missing. Defaults to " +
                                "`drawing.svg`. It always lands in the project's own folder " +
                                "(Download/Rumo/<project>/), which is what the Media panel lists.",
                        ),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "svg_rasterize",
                description = "Render SVG source as a PICTURE: the whole document is rasterised " +
                    "and added as an ordinary image layer, so everything renders — text, " +
                    "filters, masks, clip paths, patterns, group opacity — including what the " +
                    "vector renderer skips. This rasterises, so it does NOT scale: enlarging " +
                    "it resamples and blurs. Prefer `svg_paint` when the document is vector-only " +
                    "(see its `skipped` count); use this one when the vector path refuses or " +
                    "degrades it.\n\n" +
                    "The PNG is written into the project folder and added as a media layer; " +
                    "the tool renders a probe frame and answers with the raster size, the saved " +
                    "file path and the difference the layer makes to the frame. `sizePx` fixes " +
                    "the one resolution the picture has — set it now, because scaling the layer " +
                    "up later only blurs.",
                schemaJson = obj(
                    req(
                        "svg",
                        str("The SVG source text itself, including the root <svg> element."),
                    ),
                    opt(
                        "name",
                        str(
                            "File name for the picture; `.png` is added if missing. Defaults to " +
                                "`drawing.png`. It lands in the project's own folder " +
                                "(Download/Rumo/<project>/).",
                        ),
                    ),
                    opt(
                        "sizePx",
                        num(
                            "Resolution of the larger side, in pixels; defaults to the larger " +
                                "canvas side. This is the picture's only resolution.",
                            1.0, 4096.0,
                        ),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "media",
                description = "Work with the project's references: pictures, video and audio. " +
                    "They live in the project's own folder — `Download/Rumo/<project>/` — which " +
                    "exists from the moment the project does, and the user drops files there " +
                    "from a file manager, so `list` is how you find out what you have been " +
                    "given. `preview` shows you one — you get the picture itself, so look " +
                    "before you place something: a file name says nothing about what is in the " +
                    "frame, and a wrong clip costs a re-render to notice. `add` puts one on the " +
                    "timeline as a layer. A picture keeps its own pixels (no colour changes), a " +
                    "video is probed for its real length, and audio is attached so it plays " +
                    "with the timeline. When the app has not been allowed to read the user's " +
                    "own files yet, this call asks for that permission itself and says so in " +
                    "`access`; the answer's `assets` then holds only what the app wrote there, " +
                    "so call again after the user answers.",
                schemaJson = obj(
                    req(
                        "action",
                        oneOf(
                            "list shows the project's references; preview shows you one as a " +
                                "picture; add puts one on the timeline; probe reports one's size " +
                                "and length without adding it.",
                            "list", "add", "probe", "preview",
                        ),
                    ),
                    opt(
                        "source",
                        str(
                            "For add, probe and preview: the file name inside the project " +
                                "folder, or a content:// uri the app already has access to.",
                        ),
                    ),
                    opt(
                        "kind",
                        oneOf(
                            "What the reference is, for add. Guessed from the extension when " +
                                "left out.",
                            "image", "video", "audio",
                        ),
                    ),
                    opt("durationMs", num("How long the layer lasts; defaults to the media's own length.", 1.0)),
                    opt("startMs", num("When the layer appears, milliseconds; 0 is the first frame.", 0.0)),
                    opt("x", num("Horizontal offset from centre, scene pixels.")),
                    opt("y", num("Vertical offset from centre, scene pixels.")),
                    opt("scale", num("Size multiplier.", 0.05, 20.0)),
                    opt("alpha", num("Opacity, 0 to 1.", 0.0, 1.0)),
                    opt(
                        "timeMs",
                        num(
                            "For preview of a video: which moment to look at. Defaults to the " +
                                "first frame, which is often black — ask for a time inside the " +
                                "clip when the start tells you nothing.",
                            0.0,
                        ),
                    ),
                    opt(
                        "maxSide",
                        num(
                            "For preview: the long side of the picture you get back, 64 to " +
                                "1024. Defaults to 512 — enough to judge framing and content.",
                            64.0,
                            1024.0,
                        ),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "get_media_pool",
                description = "Everything you have been given, with the numbers you need to " +
                    "place it: for each file in the project folder, its real length in " +
                    "milliseconds, frame rate, pixel size and which streams it carries. Call " +
                    "this before you cut, trim or place anything — a timecode you invented for a " +
                    "source is the one mistake you cannot see afterwards, because the timeline " +
                    "accepts it and the render simply has nothing there. It also reports which " +
                    "files are already on the timeline, so you can tell what is imported from " +
                    "what is in use, and the timeline's own extent, so you know the bounds you " +
                    "are editing inside.",
                schemaJson = obj(
                    opt(
                        "probe",
                        bool(
                            "Read each file's header for length, frame rate and streams. On by " +
                                "default; turn it off when you only need names and sizes, because " +
                                "probing opens every file.",
                        ),
                    ),
                    opt(
                        "includeTimeline",
                        bool(
                            "Also report the layers currently on the timeline and the project's " +
                                "extent. On by default.",
                        ),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "analyze",
                description = "Listen to a piece of audio and find its beats. Returns the tempo, " +
                    "how sure the estimate is, and the beat times with their loudness — which is " +
                    "how you cut and place things on the beat instead of guessing. Works on any " +
                    "reference in the project folder that is audio (or a video's soundtrack is " +
                    "not read: give it an audio file).",
                schemaJson = obj(
                    req("action", oneOf("Only beats, for now.", "beats")),
                    req(
                        "source",
                        str("File name inside the project folder, or a content:// uri."),
                    ),
                    opt(
                        "maxBeats",
                        num(
                            "How many beat times to return, from the start; the total count and " +
                                "the mean interval are reported either way.",
                            1.0,
                            512.0,
                        ),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "batch_timeline_edl",
                description = "Apply a whole list of timeline edits in one call: split, " +
                    "ripple_delete, insert_clip, duck_audio, add_subtitles. Use this instead " +
                    "of one narrow edit per change whenever you are doing more than a couple — " +
                    "a hundred cuts sent one at a time means a hundred round trips, and worse, " +
                    "the timeline moves under you between them, so cut ninety is computed " +
                    "against a document that no longer exists. Here the whole list is applied " +
                    "in one pass in the editor core and comes back as one document, so it is " +
                    "atomic and it undoes as a single step. Operations that cannot apply do " +
                    "not fail the batch: they are listed in `skipped` with the reason, and the " +
                    "rest still run. `duck_audio` is always skipped — the engine has no audio " +
                    "gain stage, so a level change is not representable; cut the audio or split " +
                    "it instead.",
                schemaJson = obj(
                    req(
                        "ops",
                        arrayOfObjects(
                            "The edits, in order. Each is an object with `op` naming the " +
                                "operation and its own fields; the rest of the object is " +
                                "specific to that operation.",
                            // Every op has a different shape, so the item is left open
                            // rather than enumerated: the core is what validates an op,
                            // and it answers with `skipped` and a reason rather than
                            // rejecting the whole batch.
                            JSONObject().put("type", "object"),
                        ),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "analyze_audio_stream",
                description = "Find the speech in a recording: where it starts and stops, " +
                    "where the silences are, and the exact times you can cut at. Call this " +
                    "before cutting anything longer than a sentence — you cannot cut twenty " +
                    "minutes of talking at the right places by guessing, because the pauses " +
                    "are not evenly spaced and the words are not where you think they are. " +
                    "The pauses are found locally, by listening to the file, so this works " +
                    "with no network. A transcript with per-word times is not part of this " +
                    "answer: the build has no speech recogniser linked in, and `transcript` " +
                    "is therefore always null rather than invented.",
                schemaJson = obj(
                    req(
                        "source",
                        str("File name inside the project folder, or a content:// uri."),
                    ),
                    opt(
                        "minSilenceMs",
                        num(
                            "How long a gap must be to count as a pause you can cut at. " +
                                "Shorter gaps are absorbed into the surrounding speech, which " +
                                "is what keeps a cut from landing between two words. Defaults " +
                                "to 300, the value the local detector is tuned for.",
                            50.0,
                            5000.0,
                        ),
                    ),
                    opt(
                        "maxCuts",
                        num(
                            "How many cut points to return, from the start. The segment and " +
                                "silence counts are reported in full either way.",
                            1.0,
                            2048.0,
                        ),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "fonts",
                description = "Text fonts. `list` reports what is installed and which family " +
                    "new text layers start with. `preview` draws a family into a picture so you " +
                    "can look at it before choosing it — the picture comes from the same shaper " +
                    "and the same atlas the text layer uses, so it is what the layer will look " +
                    "like, not an approximation. `install` downloads a family from Google Fonts " +
                    "and makes it available to every project on this device. `set_default` picks " +
                    "the family new text layers start with. A single layer takes its own family " +
                    "through `update_layer` with the `font` field; the name to pass is the one " +
                    "`list` reports, because a downloaded file is addressed by the name it " +
                    "carries, which is not always the catalogue's spelling.",
                schemaJson = obj(
                    req(
                        "action",
                        oneOf(
                            "list reads the installed families; preview draws one; install " +
                                "downloads one from Google Fonts; set_default chooses the family " +
                                "new text layers use; remove deletes an installed family.",
                            "list", "preview", "install", "set_default", "remove",
                        ),
                    ),
                    opt(
                        "family",
                        str(
                            "Family name. For `preview`, `install`, `set_default` and `remove` " +
                                "this is required; an installed family is named as `list` " +
                                "reports it, an uninstalled one by its Google Fonts name.",
                        ),
                    ),
                    opt(
                        "text",
                        str("Sample text for `preview`; defaults to the family's own name."),
                    ),
                    opt("sizePx", num("Preview point size in pixels.", 8.0, 256.0)),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "project",
                description = "Save the project, open another one, or start a new one. Nothing " +
                    "you change is persisted until it is saved: `save` writes it into the " +
                    "project folder, where it appears on Home and in the Projects tab. `open` " +
                    "switches to a project the user already has — call it with no `file` to " +
                    "get the list, then again with one of the names it returned; the project " +
                    "that is open is saved first, so switching never loses work. `new` starts " +
                    "an empty one, and does the same. Every action answers with the project " +
                    "now open and the folder its references live in, so a listing afterwards " +
                    "is never against the wrong project.",
                schemaJson = obj(
                    req(
                        "action",
                        oneOf(
                            "save writes the project out; open switches to a saved one; new " +
                                "starts an empty one; canvas sets the frame the project is " +
                                "composed in.",
                            "save", "new", "open", "canvas",
                        ),
                    ),
                    opt("name", str("Project name, used for `new` and for the first save.")),
                    opt(
                        "file",
                        str(
                            "For `open`: the project's file name (with or without `.rumo`), " +
                                "or its display name, as returned by a call with no `file`. " +
                                "Leave it out to get the list of what can be opened.",
                        ),
                    ),
                    opt(
                        "width",
                        num(
                            "Canvas width in pixels, 16 to 8192. For `new` and for `canvas`.",
                            16.0,
                            8192.0,
                        ),
                    ),
                    opt(
                        "height",
                        num(
                            "Canvas height in pixels, 16 to 8192. For `new` and for `canvas`.",
                            16.0,
                            8192.0,
                        ),
                    ),
                    opt(
                        "background",
                        str(
                            "Background colour: #RRGGBB, #AARRGGBB (alpha first, so " +
                                "#80000000 is half-transparent), a decimal colour, or the " +
                                "word `transparent`. For `new` and `canvas`. The background " +
                                "is a flat colour and nothing else: it is not a layer, so it " +
                                "carries no effects, opacity or keyframes of its own.",
                        ),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "snapshot",
                description = "Render one frame to a PNG in Download/Rumo and answer with it, so " +
                    "you can see what the project looks like right now. Also returns simple " +
                    "statistics for when you cannot see images, the list of layers that were " +
                    "actually in the frame, and what the engine says about it. Read `warnings` " +
                    "and `engine` before concluding anything from the picture: they name a " +
                    "refused shader, a CPU-path frame, or a layer that was left out.",
                schemaJson = obj(
                    opt("timeMs", num("Frame to render; defaults to the playhead.")),
                    opt("width", num("Width in pixels, 64 to 1920.", 64.0, 1920.0)),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "filmstrip",
                description = "Render a range of the timeline as one contact sheet of evenly " +
                    "spaced frames, plus a short MP4 of that range. This is how you check what " +
                    "a change did over time.",
                schemaJson = obj(
                    opt("fromMs", num("Range start; defaults to 0.")),
                    opt("toMs", num("Range end; defaults to the end of the project.")),
                    opt("frames", num("Frames in the sheet, 2 to 12.", 2.0, 12.0)),
                    opt("width", num("Width of each frame, 96 to 960.", 96.0, 960.0)),
                    opt("clip", bool("Also write a short MP4 of the range to Download/Rumo.")),
                    opt("clipFps", num("Frames per second for the clip, 6 to 30.", 6.0, 30.0)),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "web_search",
                description = "Search the web and read what the pages say. Use this for " +
                    "anything that could have changed since you were trained — library and " +
                    "API versions, current syntax, release notes, prices, who currently holds " +
                    "a role — and for any fact you would otherwise be guessing at. Guessing " +
                    "is the failure mode this exists to remove: a confident wrong answer is " +
                    "worse than a searched one, because it looks like knowledge. Returns a " +
                    "numbered list of pages with their text; refer to them by number.",
                schemaJson = obj(
                    req("query", str("A focused, self-contained search query.")),
                    opt(
                        "numResults",
                        num(
                            "How many pages to read, 1 to 20. Five is enough for most " +
                                "questions; more is context spent on pages you will not use.",
                            1.0,
                            20.0,
                        ),
                    ),
                    opt(
                        "category",
                        str(
                            "Optional hint about the kind of page wanted: company, " +
                                "publication, news, personal site, financial report, people.",
                        ),
                    ),
                ).toString(),
            ),
        )
        if (agents != null) addAgentTools()
    }

    /**
     * Sub-agent management tools.
     *
     * Five, not one: start, look, read on, append, stop. Reducing them to "start
     * and wait" would mean that long work holds the main conversation, and a long
     * sub-agent report has nowhere to go — it is either entirely in context or
     * lost.
     *
     * They are added only to whoever was given them: a sub-agent's `agents` is
     * empty, and it does not spawn grandchildren. This is not restriction for its
     * own sake — an agent tree without a bound stops being surveyable, and the
     * user cannot keep track of it.
     */
    private fun MutableList<RumiToolSpec>.addAgentTools() {
        add(
            RumiToolSpec(
                name = "task",
                description = "Start a sub-agent: a second assistant with its own " +
                    "conversation, the same project and the same tools. Use it for work " +
                    "that is self-contained and whose detail you do not need in this " +
                    "conversation — a survey across many layers, a long check, an " +
                    "independent second opinion on a change you just made. It answers with " +
                    "one final text; everything in between stays in its own conversation, " +
                    "which the user can open by tapping its card. Set runInBackground only " +
                    "when you have other work to do while it runs: a foreground task " +
                    "blocks this conversation until it answers.",
                schemaJson = obj(
                    req("description", str("Short title for the card, three to six words.")),
                    req(
                        "prompt",
                        str("The whole job, self-contained. It cannot see this conversation."),
                    ),
                    opt(
                        "agent_name",
                        oneOf(
                            "Which kind of sub-agent to use. explore and verifier cannot " +
                                "change the project; worker can.",
                            *RumiAgentRole.entries.map { it.id }.toTypedArray(),
                        ),
                    ),
                    opt(
                        "runInBackground",
                        bool("Start it and keep working here instead of waiting for it."),
                    ),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "task_query",
                description = "List the sub-agents of this conversation with their state and " +
                    "how far they got, or describe one by id. Use it after a background " +
                    "task, or when you have lost track of what is still running.",
                schemaJson = obj(
                    opt("task_id", str("One task to describe. Omit for all of them.")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "task_output",
                description = "Read what a sub-agent has said so far. Its final answer is " +
                    "the text you want; while it is still running you get its progress " +
                    "instead.",
                schemaJson = obj(
                    req("task_id", str("The id the spawn returned.")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "task_append",
                description = "Send a sub-agent another message. If it has finished this " +
                    "starts it again with everything it already knows — that is how a " +
                    "follow-up is asked without repeating the job. If it is still running " +
                    "the message is delivered when the current turn ends.",
                schemaJson = obj(
                    req("task_id", str("The id the spawn returned.")),
                    req("content", str("What to tell it.")),
                ).toString(),
            ),
        )
        add(
            RumiToolSpec(
                name = "task_stop",
                description = "Stop a sub-agent. Its conversation stays readable; only the " +
                    "work stops. Use it when the job no longer matters, not when you are " +
                    "impatient — a stopped task cannot answer.",
                schemaJson = obj(
                    req("task_id", str("The id the spawn returned.")),
                    opt("reason", str("Why, for the transcript.")),
                ).toString(),
            ),
        )
    }

    /**
     * The schemas that make sense right now.
     *
     * Web search is shown to the model only when it is configured: a tool that
     * always answers "the key is not set" spends a turn and promises a capability
     * the user does not have. It is checked on every request, not when the list is
     * built: the user turns search on in settings and comes back — this must work
     * without a new conversation.
     *
     * The generative tools are built the same way, but their list is additionally
     * rebuilt: it depends on which services are ready right now, and each one's
     * schema (`voice`, `aspectRatio`, `durationSeconds`) is built from those
     * services. A frozen schema would promise the model fields of a service the
     * user has just switched off.
     */
    override val activeSpecs: List<RumiToolSpec>
        get() {
            val configured = specs.filter { spec ->
                (only == null || spec.name in only) &&
                    (RumiSettings.state.value.searchReady || spec.name != "web_search")
            }
            return configured + AiTools.activeSpecs(AiServices.state.value, only)
        }

    /**
     * Run one tool. Returns a failure outcome rather than throwing.
     *
     * [callId] is needed only by sub-agents: the transcript uses it to find the
     * card while the work is still going on. A call's result returns `agentId`,
     * but that happens at the end, whereas the card is needed from the start —
     * otherwise the sub-agent's work could be watched only once there is nothing
     * left to watch.
     */
    // No default for `callId` here: Kotlin forbids repeating a default on an
    // override, and the interface in the module already carries it. Callers that
    // hold this concrete type rather than `RumiToolbox` have to pass it.
    override suspend fun call(
        name: String,
        argumentsJson: String,
        callId: String,
    ): RumiToolOutcome {
        val args = runCatching { JSONObject(argumentsJson.ifBlank { "{}" }) }.getOrElse {
            return fail("arguments are not a JSON object: ${it.message}")
        }
        return try {
            when (name) {
                "get_media_pool" -> mediaPool(args)
                "project_state" -> projectState()
                "open_panel" -> openPanel(args)
                "select_layer" -> selectLayer(args)
                "seek" -> seek(args)
                "transport" -> transport(args)
                "add_text" -> addText(args)
                "add_shape" -> addShape(args)
                "keys" -> keys(args)
                "update_layer" -> updateLayer(args)
                "remove_layer" -> removeLayer(args)
                "set_transition" -> setTransition(args)
                "effects" -> effects(args)
                "compare_effects" -> compareEffects(args)
                "define_effect" -> defineEffect(args)
                "svg_paint" -> svgPaint(args)
                "fonts" -> fonts(args)
                "svg_rasterize" -> svgRasterize(args)
                "media" -> media(args)
                "speak" -> aiGenerate(args, AiKind.Speech)
                "generate_sound" -> aiGenerate(args, AiKind.Sound)
                "generate_image" -> aiGenerate(args, AiKind.Image)
                "generate_video" -> aiGenerate(args, AiKind.Video)
                "analyze" -> analyze(args)
                "analyze_audio_stream" -> analyzeAudioStream(args)
                "web_search" -> webSearch(args)
                "task" -> agentSpawn(args, callId)
                "task_query" -> agentQuery(args)
                "task_output" -> agentOutput(args)
                "task_append" -> agentAppend(args)
                "task_stop" -> agentStop(args)
                "batch_timeline_edl" -> batchTimelineEdl(args)
                "project" -> project(args)
                "snapshot" -> snapshot(args)
                "filmstrip" -> filmstrip(args)
                else -> fail("unknown tool `$name`")
            }
        } catch (t: Throwable) {
            AppLog.error(TAG, "tool $name threw: ${AppLog.describe(t)}")
            fail("$name failed: ${AppLog.describe(t)}")
        }
    }

    // --- Sub-agents ---

    /**
     * Start a sub-agent.
     *
     * In the foreground the call does not return until the sub-agent answers:
     * that is the point of the foreground — the main conversation waits for the
     * result in order to use it right away. In the background an identifier is
     * returned, and the answer can be picked up via `task_output`.
     *
     * The role is checked, not silently substituted on a typo: an `agent_name`
     * that does not exist is a call error, and it must be reported rather than
     * handing out `worker` and doing something other than what was asked.
     */
    /** The sub-agent spawned by this call, if it already exists. */
    override fun agentIdForCall(callId: String): String? =
        agents?.agentForCall(callId)?.id

    private suspend fun agentSpawn(args: JSONObject, callId: String): RumiToolOutcome {
        val registry = agents ?: return fail("sub-agents are not available here")
        val prompt = args.optString("prompt", "").trim()
        if (prompt.isEmpty()) return fail("prompt is required")
        val description = args.optString("description", "").trim()
        val roleName = args.optTextOr("agent_name", RumiAgentRole.WORKER.id)
        val role = RumiAgentRole.of(roleName) ?: return fail(
            "agent_name must be one of ${RumiAgentRole.entries.joinToString(", ") { it.id }}",
        )
        val background = args.optBoolean("runInBackground", false)
        val agent = registry.spawn(
            description = description.ifEmpty { prompt.take(48) },
            prompt = prompt,
            role = role,
            callId = callId,
        )
        val summary = "${agent.description} · ${role.id}"
        if (background) {
            return RumiToolOutcome(
                text = JSONObject()
                    .put("ok", true)
                    .put("task_id", agent.id)
                    .put("agent", role.id)
                    .put("status", "running")
                    .put(
                        "note",
                        "Started in the background. The user can watch it by tapping its " +
                            "card in the transcript. Read the answer with task_output " +
                            "(\"${agent.id}\") once it is done; it is not finished yet, so do " +
                            "not describe its result to the user now.",
                    )
                    .toString(),
                summary = summary,
                agentId = agent.id,
            )
        }
        agent.await()
        return RumiToolOutcome(
            text = agent.report(),
            summary = summary,
            agentId = agent.id,
            // A failed sub-agent is a failed call: the main agent must see this
            // as an error and decide what to do, not take a failure report for a
            // result.
            ok = agent.succeeded,
        )
    }

    private fun agentQuery(args: JSONObject): RumiToolOutcome {
        val registry = agents ?: return fail("sub-agents are not available here")
        val id = args.optTextOr("task_id", "")
        if (id.isNotEmpty()) {
            val agent = registry.find(id) ?: return fail("no task with id $id")
            return RumiToolOutcome(text = agent.describe(), summary = agent.description)
        }
        val all = registry.list()
        return RumiToolOutcome(
            text = if (all.isEmpty()) {
                "No sub-agents have been started in this conversation."
            } else {
                all.joinToString("\n") { it.describe() }
            },
            summary = if (all.isEmpty()) "none" else "${all.size}",
        )
    }

    private fun agentOutput(args: JSONObject): RumiToolOutcome {
        val registry = agents ?: return fail("sub-agents are not available here")
        val id = args.optTextOr("task_id", "")
        if (id.isEmpty()) return fail("task_id is required")
        val agent = registry.find(id) ?: return fail("no task with id $id")
        return RumiToolOutcome(text = agent.report(), summary = agent.description)
    }

    private fun agentAppend(args: JSONObject): RumiToolOutcome {
        val registry = agents ?: return fail("sub-agents are not available here")
        val id = args.optTextOr("task_id", "")
        if (id.isEmpty()) return fail("task_id is required")
        val content = args.optString("content", "").trim()
        if (content.isEmpty()) return fail("content is required")
        val agent = registry.find(id) ?: return fail("no task with id $id")
        agent.append(content)
        return RumiToolOutcome(
            text = JSONObject()
                .put("ok", true)
                .put("task_id", id)
                .put(
                    "note",
                    if (agent.busy) {
                        "Delivered. It is still working; the message will be read when the " +
                            "current turn ends."
                    } else {
                        "Delivered, and it is working on it now."
                    },
                )
                .toString(),
            summary = agent.description,
            agentId = id,
        )
    }

    private fun agentStop(args: JSONObject): RumiToolOutcome {
        val registry = agents ?: return fail("sub-agents are not available here")
        val id = args.optTextOr("task_id", "")
        if (id.isEmpty()) return fail("task_id is required")
        val agent = registry.find(id) ?: return fail("no task with id $id")
        val reason = args.optTextOr("reason", "")
        agent.stop(reason)
        return RumiToolOutcome(
            text = JSONObject()
                .put("ok", true)
                .put("task_id", id)
                .put("status", "stopped")
                .put("note", "Stopped. Its conversation is still readable.")
                .toString(),
            summary = agent.description,
            agentId = id,
        )
    }

    // --- Reading ---

    /**
     * Web search through Exa.
     *
     * The key is read at call time, not taken from the state when the tools are
     * built: it may appear in settings while the conversation is open. The network
     * call blocks, so it goes to `IO`.
     */
    private suspend fun webSearch(args: JSONObject): RumiToolOutcome {
        val query = args.optString("query", "").trim()
        if (query.isEmpty()) return fail("query is required")

        val key = RumiSettings.state.value.exaKey
        if (key.isEmpty()) {
            return fail("Web search has no API key. Add one in Settings → Rumi → Web search.")
        }
        val count = args.optInt("numResults", RumiSearch.DEFAULT_RESULTS)
        val category = args.optText("category")

        val outcome = withContext(Dispatchers.IO) {
            RumiSearch.search(query, count, category, key)
        }
        return when (outcome) {
            is RumiSearch.Outcome.Failed -> fail(outcome.reason)
            is RumiSearch.Outcome.Ok -> RumiToolOutcome(
                text = RumiSearch.format(outcome.hits),
                // An empty result is not a search failure: there simply are no
                // pages, and saying that to the model is more honest than showing a
                // tool refusal.
                summary = when {
                    outcome.hits.isEmpty() -> "no results · $query"
                    else -> "$query · ${outcome.hits.size}"
                },
            )
        }
    }

    /**
     * The pool: every file the user has been given, with the numbers needed to
     * place it, plus what is already on the timeline.
     *
     * ## Why this exists next to `media`
     *
     * `media(list)` answers "what have I been given" — names, kinds, sizes. That
     * is enough to pick a file and not enough to *cut* one: without a real
     * duration, a frame rate and a pixel size, the model has to invent timecodes,
     * and an invented timecode is the failure it cannot see afterwards — the
     * timeline accepts it and the render simply has nothing there. `media(probe)`
     * does report a length, but one file per call.
     *
     * So this is the whole pool in one answer, probed once, with the two things
     * that keep a long edit honest: which files are already in use, and how far
     * the timeline itself extends.
     */
    private suspend fun mediaPool(args: JSONObject): RumiToolOutcome {
        val project = state.projectName.value
        val access = mediaAccess()
        val assets = ProjectAssets.list(host.context, project)
        val probe = args.optBoolean("probe", true)
        val withTimeline = args.optBoolean("includeTimeline", true)

        val files = JSONArray()
        for (asset in assets) {
            val entry = JSONObject()
            entry.put("name", asset.name)
            entry.put("kind", asset.kind.name.lowercase())
            entry.put("bytes", asset.sizeBytes)
            entry.put("uri", asset.uri.toString())
            if (probe) {
                // Probing opens the file, so a single unreadable one must not
                // sink the whole answer: its metadata is reported as null and the
                // reason is carried alongside.
                try {
                    when (asset.kind) {
                        ProjectAssets.Kind.VIDEO -> {
                            val info = videoInfo(host.context, asset.uri)
                            if (info != null) {
                                entry.put("durationMs", info.durationMs)
                                entry.put("width", info.width)
                                entry.put("height", info.height)
                                entry.put("fps", info.fps.toDouble())
                                entry.put("hasAudio", info.hasAudio)
                                entry.put("streams", if (info.hasAudio) "video+audio" else "video")
                            } else {
                                entry.put("probeError", "the container did not report a video track")
                            }
                        }
                        ProjectAssets.Kind.AUDIO -> {
                            val ms = audioDurationMs(host.context, asset.uri)
                            if (ms > 0L) {
                                entry.put("durationMs", ms)
                                entry.put("streams", "audio")
                            } else {
                                entry.put("probeError", "the container did not report a duration")
                            }
                        }
                        ProjectAssets.Kind.IMAGE -> {
                            // An image has no timeline of its own: it lasts exactly
                            // as long as its layer says, so a duration here would be
                            // a lie the model would then place things against.
                            entry.put("streams", "image")
                        }
                        ProjectAssets.Kind.SVG -> {
                            // A vector has no time of its own either, but it does
                            // have a parse: it says how much geometry comes out.
                            val parsed = RumoBridge.svgValidate(readUriBytes(host.context, asset.uri) ?: ByteArray(0))
                            if (parsed != null && parsed.ok) {
                                entry.put("streams", "svg")
                                entry.put("shapes", parsed.shapes)
                            } else {
                                entry.put("streams", "svg")
                                entry.put("probeError", parsed?.error ?: "the engine could not parse it")
                            }
                        }
                        ProjectAssets.Kind.OTHER -> entry.put("streams", "unknown")
                    }
                } catch (t: Throwable) {
                    entry.put("probeError", t.message ?: t::class.simpleName ?: "unreadable")
                }
            }
            files.put(entry)
        }

        val out = JSONObject()
        out.put("project", project)
        out.put("access", access)
        out.put("probed", probe)
        out.put("files", files)

        if (withTimeline) {
            val layers = state.layers.value
            val used = JSONArray()
            for (layer in layers) {
                if (layer.uri == null) continue
                val row = JSONObject()
                row.put("layer", layer.name)
                row.put("layerId", layer.id)
                row.put("kind", layer.kind.name.lowercase())
                row.put("uri", layer.uri)
                row.put("startMs", layer.startMs)
                row.put("durationMs", layer.durationMs)
                used.put(row)
            }
            out.put("inUse", used)
            // The extent is the last frame anything reaches, which is the bound
            // every edit has to fit inside.
            var extent = 0L
            for (layer in layers) {
                val end = layer.startMs + layer.durationMs
                if (end > extent) extent = end
            }
            out.put("timelineDurationMs", extent)
        }

        return RumiToolOutcome(text = out.toString())
    }

    private fun projectState(): RumiToolOutcome {
        val layers = state.layers.value
        val custom = state.customEffects.value
        // Layers are drawn **in list order**, and the layer's kind no longer
        // affects that.
        //
        // Formerly the engine drew in groups — first all shapes, then text, then
        // images — and the order had to be reported separately, because the list
        // meant nothing to it: a shape could not end up above an image, and an SVG
        // (that is the SHAPE kind) and text could not end up above a photo. Now the
        // group does not matter, and `paintOrder` is simply the position in the
        // list: 0 is drawn first, that is, it lies below everything.
        val paintOrder = HashMap<String, Int>()
        var paint = 0
        layers.filter { it.visible }.forEach { paintOrder[it.id] = paint++ }
        val array = JSONArray()
        layers.forEach { layer ->
            val effects = JSONArray()
            layer.effects.forEach { effect ->
                val params = JSONArray()
                effect.params.forEach { params.put(it.toDouble()) }
                effects.put(
                    JSONObject()
                        .put("effectId", effect.id)
                        .put("kind", effect.kindId)
                        .put("enabled", effect.enabled)
                        .put("params", params),
                )
            }
            array.put(
                JSONObject()
                    .put("id", layer.id)
                    .put("kind", layer.kind.name)
                    .put("name", layer.name)
                    .put("visible", layer.visible)
                    .put("argb", "#%08X".format(layer.argb.toInt()))
                    .put("x", layer.offsetX.toDouble())
                    .put("y", layer.offsetY.toDouble())
                    .put("scale", layer.scale.toDouble())
                    .put("alpha", layer.alpha.toDouble())
                    .put("durationMs", layer.durationMs)
                    // Where the layer sits on the timeline. A model that cannot
                    // see this cannot schedule anything, and it is the whole
                    // point of startMs: every layer here would otherwise look
                    // as if it began at zero.
                    .put("startMs", layer.startMs)
                    // Which layer is the frame itself. Its `name` is the engine's
                    // word for the shape ("Frame"), which says nothing about what
                    // the layer is for, and a model that does not know which one
                    // it is will happily add a second full-frame shape instead of
                    // putting the effect on the background that already exists.
                    .put("background", EditorState.isBackground(layer))
                    .put("text", layer.text)
                    // Reported only for text: a shape has no weight and no
                    // outline, and an always-present null would invite the model
                    // to set them on one.
                    .apply {
                        if (layer.kind == LayerKindUi.TEXT) {
                            put("weight", layer.textWeight)
                            // An empty name means the built-in face; that way the
                            // model sees both "which font is set" and "is one set at
                            // all".
                            put("font", layer.textFamily)
                            put("strokePx", layer.strokePx.toDouble())
                            put("strokeArgb", "#%08X".format(layer.strokeArgb.toInt()))
                        }
                    }
                    .put("locked", layer.id in state.locked.value)
                    .put("paintIndex", paintOrder[layer.id] ?: JSONObject.NULL)
                    .apply {
                        var rotation = JSONArray()
                        layer.keys.forEach { key ->
                            rotation.put(
                                JSONObject().put("t", key.timeMs).put("degrees", key.value.toDouble()),
                            )
                        }
                        put("rotationKeys", rotation)
                    }
                    // The other animatable tracks, reported only when the layer
                    // has any: an empty track is "not animated", and a model that
                    // sees a phantom key list will try to edit one.
                    .apply {
                        fun track(list: List<KeyframeUi>): JSONArray {
                            val out = JSONArray()
                            list.forEach { key ->
                                out.put(JSONObject().put("t", key.timeMs).put("v", key.value.toDouble()))
                            }
                            return out
                        }
                        if (layer.scaleKeys.isNotEmpty()) put("scaleKeys", track(layer.scaleKeys))
                        if (layer.xKeys.isNotEmpty()) put("positionXKeys", track(layer.xKeys))
                        if (layer.yKeys.isNotEmpty()) put("positionYKeys", track(layer.yKeys))
                        if (layer.alphaKeys.isNotEmpty()) put("alphaKeys", track(layer.alphaKeys))
                    }
                    .put("effects", effects)
                    .apply {
                        layer.transition?.let { tr ->
                            put(
                                "transition",
                                JSONObject()
                                    .put("startMs", tr.startMs)
                                    .put("durationMs", tr.durationMs)
                                    .put("withPrevious", tr.withPrevious)
                                    .put("enabled", tr.enabled),
                            )
                        }
                    },
            )
        }
        val customs = JSONArray()
        custom.forEach { customs.put(it.id) }
        // Installed ones are a separate field: they are available for use but are
        // not part of the project and will not travel along with it.
        val installed = JSONArray()
        state.installedEffects.forEach {
            installed.put(JSONObject().put("id", it.id).put("label", it.label))
        }
        return RumiToolOutcome(
            JSONObject()
                .put("ok", true)
                .put("project", state.projectName.value)
                .put("file", state.currentFileName.value ?: JSONObject.NULL)
                .put("canvasWidth", state.canvasWidth.value)
                .put("canvasHeight", state.canvasHeight.value)
                .put("backgroundArgb", "#%08X".format(state.backgroundArgb.value.toInt()))
                .put(
                    "canvasNote",
                    "Layer offsets, x/y and the effect geometry are in canvas pixels: the " +
                        "canvas is ${state.canvasWidth.value}x${state.canvasHeight.value}.",
                )
                .put("durationMs", state.projectDurationMs.value)
                .put("playheadMs", state.playheadMs.value)
                .put("playing", state.isPlaying.value)
                .put("unsavedChanges", state.hasEdits.value)
                .put("selectedLayerId", state.selectedId.value ?: JSONObject.NULL)
                .put("layers", array)
                .put("projectEffects", customs)
                .put("installedEffects", installed)
                .put("paintNote", "paintIndex is the order the frame is painted in, lowest " +
                    "first: all visible shapes, then all text layers, then all pictures. " +
                    "A layer with a null paintIndex is hidden and is not drawn at all.")
                .put("keyProperties", JSONArray(keyProperties))
                .put("keyNote", "The animatable tracks are " +
                    keyProperties.joinToString(", ") + ". `keys` writes to one of them; " +
                    "a track that is absent or empty on a layer is not animated, and the " +
                    "layer's own value is used at every moment. A layer is in frame " +
                    "from startMs up to but not including startMs + durationMs, so a " +
                    "clip whose startMs is past the playhead is not drawn at all.")
                .toString(),
            summary = "${layers.size} layer(s)",
        )
    }

    // --- Moving around ---

    private fun openPanel(args: JSONObject): RumiToolOutcome {
        val panel = args.optTextOr("panel", "none").lowercase()
        val ack = host.navigate("open_panel", panel)
        return RumiToolOutcome(ack, summary = panel)
    }

    private fun selectLayer(args: JSONObject): RumiToolOutcome {
        val id = args.optText("layerId").orEmpty()
        val layer = state.layers.value.firstOrNull { it.id == id }
            ?: return fail("no layer with id `$id`")
        state.selectLayer(id)
        return RumiToolOutcome("selected `\"${layer.name}\"` ($id)", summary = layer.name)
    }

    private fun seek(args: JSONObject): RumiToolOutcome {
        val t = args.optDouble("timeMs", Double.NaN)
        if (t.isNaN()) return fail("timeMs is required")
        val clamped = t.toLong().coerceIn(0L, state.projectDurationMs.value)
        state.seekTo(clamped)
        return RumiToolOutcome("playhead at ${clamped}ms", summary = "${clamped}ms")
    }

    private fun transport(args: JSONObject): RumiToolOutcome {
        val action = args.optText("action").orEmpty()
        when (action) {
            "play" -> if (!state.isPlaying.value) state.togglePlay()
            "pause" -> if (state.isPlaying.value) state.togglePlay()
            "start" -> state.jumpToStart()
            "end" -> state.jumpToEnd()
            else -> return fail("action must be play, pause, start or end")
        }
        return RumiToolOutcome(
            "transport: $action (playhead ${state.playheadMs.value}ms)",
            summary = action,
        )
    }

    // --- Changing the project ---

    private fun addText(args: JSONObject): RumiToolOutcome {
        // Read by hand rather than through the shared helper: an empty string is
        // a legitimate value here (it clears a layer's text), while a JSON null
        // is not, and the helper treats both as absent.
        val text = if (args.has("text") && !args.isNull("text")) args.optText("text").orEmpty() else ""
        if (text.isBlank()) return fail("text must not be blank")
        val before = state.layers.value.map { it.id }.toSet()
        // One tool call is one undo entry, not one per field it sets.
        state.beginUndoBatch()
        try {
            state.addTextLayer(text)
            val id = state.layers.value.firstOrNull { it.id !in before }?.id
                ?: return fail("the layer was not added")
            // The rest of the fields go through the same path as update_layer, so
            // there is exactly one place that knows how a field maps to the model.
            applyFields(id, args)
            val added = state.layers.value.firstOrNull { it.id == id } ?: return fail("the layer was not added")
            return RumiToolOutcome(
                "added text layer $id: \"${added.text.orEmpty()}\"",
                summary = text.take(40),
            )
        } finally {
            state.endUndoBatch()
        }
    }

    private fun addShape(args: JSONObject): RumiToolOutcome {
        val shape = args.optText("shape").orEmpty()
        if (ShapeNames.none { it.equals(shape, ignoreCase = true) }) {
            return fail("`$shape` is not a shape the engine can draw; use one of ${ShapeNames.joinToString(", ")}")
        }
        val name = ShapeNames.first { it.equals(shape, ignoreCase = true) }
        val before = state.layers.value.map { it.id }.toSet()
        state.beginUndoBatch()
        try {
            state.addShape(name)
            val id = state.layers.value.firstOrNull { it.id !in before }?.id
                ?: return fail("the layer was not added")
            applyFields(id, args)
            return RumiToolOutcome(
                "added shape layer $id (`$name`)",
                summary = name,
            )
        } finally {
            state.endUndoBatch()
        }
    }

    private fun keys(args: JSONObject): RumiToolOutcome {
        val layerId = args.optText("layerId").orEmpty()
        val layer = state.layers.value.firstOrNull { it.id == layerId }
            ?: return fail("no layer with id `$layerId`")
        // `rotation` stays the default so a call written before the other tracks
        // existed keeps meaning exactly what it meant.
        val track = EditorState.KeyTrack.fromWire(args.optText("property").orEmpty().ifBlank { "rotation" })
            ?: return fail("property must be one of ${keyProperties.joinToString(", ")}")
        val t = args.optDouble("timeMs", state.playheadMs.value.toDouble()).toLong().coerceAtLeast(0L)
        state.beginUndoBatch()
        try {
            when (args.optText("action").orEmpty()) {
                "add" -> {
                    // The time is the tool's, not the playhead's: moving the user's
                    // cursor as a side effect of pinning a key would be a surprise,
                    // and it is what the first cut of this did.
                    //
                    // `degrees` is only read for rotation; the other tracks take
                    // `value`, whose unit is the track's own (a multiplier for scale
                    // and alpha, canvas pixels for the two positions).
                    val degrees = args.optDouble("degrees", Double.NaN)
                    val value = if (track == EditorState.KeyTrack.ROTATION) {
                        if (degrees.isNaN()) null else degrees.toFloat()
                    } else {
                        args.optDouble("value", Double.NaN).takeIf { !it.isNaN() }?.toFloat()
                    }
                    // The curve is optional and belongs to the key being added;
                    // without it the key leaves along the straight line, which is
                    // what every key written before curves meant.
                    val ease = easeArg(args) ?: return fail(
                        "`ease` must be a preset name (${easeNames()}) or " +
                            "{x1,y1,x2,y2} in 0..1 for x",
                    )
                    if (track == EditorState.KeyTrack.ROTATION) {
                        state.addKeyframeAt(layerId, t, value, ease)
                    } else if (value != null) {
                        state.addPropertyKeyframe(layerId, t, track, value, ease)
                    } else {
                        return fail("`value` is required when property is ${track.wire}")
                    }
                    val now = state.keysOf(state.layers.value.first { it.id == layerId }, track)
                    val at = now.firstOrNull { it.timeMs == t }
                    return RumiToolOutcome(
                        JSONObject()
                            .put("ok", true)
                            .put("layerId", layerId)
                            .put("property", track.wire)
                            .put("timeMs", t)
                            // Both names are reported: `degrees` keeps the old shape
                            // of the answer intact, `value` is what a non-rotation
                            // key actually holds.
                            .put("degrees", at?.value?.toDouble() ?: JSONObject.NULL)
                            .put("value", at?.value?.toDouble() ?: JSONObject.NULL)
                            .put("ease", easeWire(at?.ease))
                            .put("keys", now.size)
                            .put("playheadMs", state.playheadMs.value)
                            .toString(),
                        summary = "${track.wire} @${t}ms",
                    )
                }
                "remove" -> {
                    if (state.keysOf(layer, track).none { it.timeMs == t }) {
                        return fail("layer $layerId has no ${track.wire} key at ${t}ms")
                    }
                    if (track == EditorState.KeyTrack.ROTATION) {
                        state.removeKeyframe(layerId, t)
                    } else {
                        state.removePropertyKeyframe(layerId, t, track)
                    }
                    val left = state.keysOf(state.layers.value.first { it.id == layerId }, track)
                    return RumiToolOutcome(
                        "removed the ${track.wire} key at ${t}ms; ${left.size} left",
                        summary = "removed @${t}ms",
                    )
                }
                "ease" -> {
                    // A key must already exist: inventing one here would need a
                    // value, and a curve is not a value.
                    val ease = easeArg(args) ?: return fail(
                        "`ease` must be a preset name (${easeNames()}) or " +
                            "{x1,y1,x2,y2} in 0..1 for x",
                    )
                    if (!state.setKeyEase(layerId, track, t, ease)) {
                        return fail("layer $layerId has no ${track.wire} key at ${t}ms")
                    }
                    return RumiToolOutcome(
                        "curve of the ${track.wire} key at ${t}ms: ${easeWire(ease)}",
                        summary = "ease @${t}ms",
                    )
                }
                "clear" -> {
                    if (track == EditorState.KeyTrack.ROTATION) {
                        layer.keys.forEach { state.removeKeyframe(layerId, it.timeMs) }
                    } else {
                        state.clearPropertyKeys(layerId, track)
                    }
                    return RumiToolOutcome(
                        "cleared every ${track.wire} key on $layerId",
                        summary = "cleared",
                    )
                }
                "list" -> {
                    val now = state.keysOf(state.layers.value.first { it.id == layerId }, track)
                    val arr = JSONArray()
                    for (key in now) {
                        arr.put(
                            JSONObject()
                                .put("t", key.timeMs)
                                .put("v", key.value.toDouble())
                                .put("ease", easeWire(key.ease)),
                        )
                    }
                    return RumiToolOutcome(
                        JSONObject()
                            .put("ok", true)
                            .put("layerId", layerId)
                            .put("property", track.wire)
                            .put("keys", arr)
                            .toString(),
                        summary = "${now.size} ${track.wire} keys",
                    )
                }
                else -> return fail("action must be add, ease, list, remove or clear")
            }
        } finally {
            state.endUndoBatch()
        }
    }

    /**
     * The `ease` argument of a tool call: a preset name, or an explicit
     * `{x1,y1,x2,y2}` curve.
     *
     * `null` means "the argument is there and is wrong" — as opposed to an
     * absent argument, which means the straight line. The two are different
     * answers: one is a caller mistake, the other is a document without curves.
     */
    private fun easeArg(args: JSONObject): EaseUi? {
        if (!args.has("ease") || args.isNull("ease")) return EaseUi()
        val raw = args.opt("ease")
        if (raw is String) {
            val text = raw.trim()
            EaseUi.byName(text.lowercase())?.let { return it }
            // A curve spelled `cubic-bezier(0.34, 1.56, 0.64, 1)`, the way a model
            // tends to write it, is worth accepting: the same four numbers as the
            // object form.
            return cubicBezierArg(text)
        }
        val o = args.optJSONObject("ease") ?: return null
        val x1 = o.optDouble("x1", Double.NaN)
        val y1 = o.optDouble("y1", Double.NaN)
        val x2 = o.optDouble("x2", Double.NaN)
        val y2 = o.optDouble("y2", Double.NaN)
        if (listOf(x1, y1, x2, y2).any { it.isNaN() }) return null
        // `x` is time and must stay monotonic, so it is clamped here rather than
        // rejected: a caller who wrote 1.2 meant "as late as possible".
        return EaseUi(
            EaseUi.Kind.CUBIC,
            x1.coerceIn(0.0, 1.0).toFloat(),
            y1.toFloat(),
            x2.coerceIn(0.0, 1.0).toFloat(),
            y2.toFloat(),
        )
    }

    /** `cubic-bezier(x1, y1, x2, y2)`, the spelling a model reaches for. */
    private fun cubicBezierArg(text: String): EaseUi? {
        val open = text.indexOf('(')
        if (!text.startsWith("cubic-bezier") || open < 0 || !text.endsWith(")")) return null
        val parts = text.substring(open + 1, text.length - 1).split(',')
        if (parts.size != 4) return null
        val nums = parts.map { it.trim().toDoubleOrNull() ?: return null }
        return EaseUi(
            EaseUi.Kind.CUBIC,
            nums[0].coerceIn(0.0, 1.0).toFloat(),
            nums[1].toFloat(),
            nums[2].coerceIn(0.0, 1.0).toFloat(),
            nums[3].toFloat(),
        )
    }

    /** The names a caller may use, for an error message that can be acted on. */
    private fun easeNames(): String = EaseUi.PRESETS.joinToString(", ") { it.first }

    /** How a curve is reported back: its preset name, else its four numbers. */
    private fun easeWire(ease: EaseUi?): Any {
        if (ease == null) return JSONObject.NULL
        ease.presetName()?.let { return it }
        return when (ease.kind) {
            EaseUi.Kind.CUBIC -> "cubic-bezier(${ease.x1}, ${ease.y1}, ${ease.x2}, ${ease.y2})"
            EaseUi.Kind.HOLD -> "hold"
            EaseUi.Kind.LINEAR -> "linear"
        }
    }

    private fun updateLayer(args: JSONObject): RumiToolOutcome {
        val id = args.optText("layerId").orEmpty()
        val layer = state.layers.value.firstOrNull { it.id == id }
            ?: return fail("no layer with id `$id`")
        if (layer.id in state.locked.value) return fail("layer $id is locked")
        // One tool call is one undo entry even when it sets four fields.
        state.beginUndoBatch()
        val changed = try {
            applyFields(id, args)
        } finally {
            state.endUndoBatch()
        }
        return if (changed.isEmpty()) {
            fail("no recognised field to change")
        } else {
            RumiToolOutcome("updated $id: ${changed.joinToString(", ")}", summary = changed.joinToString(", "))
        }
    }

    /** Apply every recognised field; returns the names actually applied. */
    private fun applyFields(id: String, args: JSONObject): List<String> {
        val changed = mutableListOf<String>()
        args.optText("name").orEmpty().takeIf { it.isNotBlank() }?.let {
            state.renameLayer(id, it)
            changed += "name"
        }
        if (args.has("text") && !args.isNull("text")) {
            state.setText(id, args.optText("text").orEmpty())
            changed += "text"
        }
        args.optText("argb").orEmpty().takeIf { it.isNotBlank() }?.let { raw ->
            val argb = parseArgb(raw) ?: return@let
            state.setColor(id, argb)
            changed += "colour"
        }
        // Weight takes a name or a number: a model that thinks in typography says
        // "bold", one that thinks in numbers says 700, and both mean the same
        // face. Only text layers have a weight, so a SHAPE ignores it silently —
        // the alternative is refusing a whole call over one inapplicable field.
        textWeightOf(args)?.let { weight ->
            state.setTextWeight(id, weight)
            changed += "weight"
        }
        // The font is set by family name — the one `list_fonts` returned. An
        // installation does not refuse a name that does not exist: the layer will
        // be drawn with the built-in face, and that is better than losing the text
        // entirely.
        args.optText("font").orEmpty().takeIf { it.isNotBlank() }?.let { family ->
            state.setTextFamily(id, family)
            changed += "font"
        }
        if (args.has("stroke") || args.has("strokePx")) {
            val px = if (args.has("strokePx")) {
                args.optDouble("strokePx", 0.0).toFloat()
            } else {
                args.optDouble("stroke", 0.0).toFloat()
            }
            state.setStrokePx(id, px)
            changed += "outline"
        }
        args.optText("strokeArgb").orEmpty().takeIf { it.isNotBlank() }?.let { raw ->
            val argb = parseArgb(raw) ?: return@let
            state.setStrokeArgb(id, argb)
            changed += "outlineColour"
        }
        val x = args.optDouble("x", Double.NaN)
        val y = args.optDouble("y", Double.NaN)
        if (!x.isNaN() || !y.isNaN()) {
            val layer = state.layers.value.firstOrNull { it.id == id }
            state.setOffset(
                id,
                if (x.isNaN()) layer?.offsetX ?: 0f else x.toFloat(),
                if (y.isNaN()) layer?.offsetY ?: 0f else y.toFloat(),
            )
            changed += "position"
        }
        if (args.has("scale")) {
            state.setScale(id, args.optDouble("scale", 1.0).toFloat())
            changed += "scale"
        }
        if (args.has("alpha")) {
            state.setAlpha(id, args.optDouble("alpha", 1.0).toFloat())
            changed += "alpha"
        }
        if (args.has("visible")) {
            val want = args.optBoolean("visible", true)
            val layer = state.layers.value.firstOrNull { it.id == id }
            if (layer != null && layer.visible != want) {
                state.toggleVisibility(id)
                changed += "visibility"
            }
        }
        if (args.has("durationMs")) {
            state.setDuration(id, args.optDouble("durationMs", 5000.0).toLong())
            changed += "duration"
        }
        if (args.has("startMs")) {
            state.setStartMs(id, args.optDouble("startMs", 0.0).toLong())
            changed += "start"
        }
        return changed
    }

    private fun removeLayer(args: JSONObject): RumiToolOutcome {
        val id = args.optText("layerId").orEmpty()
        val layer = state.layers.value.firstOrNull { it.id == id }
            ?: return fail("no layer with id `$id`")
        if (id in state.locked.value) return fail("layer $id is locked")
        val effects = layer.effects.size
        state.removeLayer(id)
        return RumiToolOutcome(
            JSONObject()
                .put("ok", true)
                .put("removed", id)
                .put("name", layer.name)
                .put("kind", layer.kind.name)
                .put("effects", effects)
                .put("layersLeft", state.layers.value.size)
                .toString(),
            summary = "removed ${layer.name}",
        )
    }

    private fun setTransition(args: JSONObject): RumiToolOutcome {
        val id = args.optText("layerId").orEmpty()
        state.layers.value.firstOrNull { it.id == id } ?: return fail("no layer with id `$id`")
        if (args.optBoolean("remove", false)) {
            state.setTransition(id, null)
            return RumiToolOutcome("removed the transition on $id", summary = "removed")
        }
        val existing = state.layers.value.firstOrNull { it.id == id }?.transition
        val ease = easeArg(args) ?: return fail(
            "`ease` must be a preset name (${easeNames()}) or {x1,y1,x2,y2} in 0..1 for x",
        )
        val transition = TransitionUi(
            startMs = args.optDouble("startMs", existing?.startMs?.toDouble() ?: 0.0).toLong(),
            durationMs = args.optDouble("durationMs", existing?.durationMs?.toDouble() ?: 500.0).toLong(),
            withPrevious = args.optBoolean("withPrevious", existing?.withPrevious ?: true),
            enabled = args.optBoolean("enabled", existing?.enabled ?: true),
            ease = ease,
        )
        state.setTransition(id, transition)
        val applied = state.layers.value.firstOrNull { it.id == id }?.transition
        return RumiToolOutcome(
            "transition on $id: starts ${applied?.startMs}ms, ${applied?.durationMs}ms, " +
                "withPrevious=${applied?.withPrevious}, ease=${easeWire(applied?.ease)}",
            summary = "${applied?.durationMs}ms cross-fade",
        )
    }

    private fun effects(args: JSONObject): RumiToolOutcome {
        when (args.optText("action").orEmpty()) {
            "list" -> {
                val catalogue = JSONArray()
                state.effectCatalogue.forEach { descriptor ->
                    val params = JSONArray()
                    descriptor.params.forEach { p ->
                        val defaults = JSONArray()
                        p.default.forEach { defaults.put(it.toDouble()) }
                        val choices = JSONArray()
                        p.choices.forEach { choices.put(it) }
                        params.put(
                            JSONObject()
                                .put("key", p.key)
                                .put("label", p.label)
                                .put("kind", p.kind)
                                .put("min", p.min.toDouble())
                                .put("max", p.max.toDouble())
                                .put("default", defaults)
                                .put("choices", choices)
                                .put("unit", p.unit)
                                .put("slot", p.slot),
                        )
                    }
                    catalogue.put(
                        JSONObject()
                            .put("kind", descriptor.id)
                            .put("label", descriptor.label)
                            .put("slots", descriptor.slots)
                            .put("passes", descriptor.passes)
                            .put("custom", descriptor.custom)
                            .put("params", params),
                    )
                }
                return RumiToolOutcome(
                    JSONObject().put("ok", true).put("effects", catalogue).toString(),
                    summary = "${state.effectCatalogue.size} effect(s)",
                )
            }
            "add" -> {
                val layerId = args.optText("layerId").orEmpty()
                val kind = args.optText("kind").orEmpty()
                state.layers.value.firstOrNull { it.id == layerId }
                    ?: return fail("no layer with id `$layerId`")
                if (kind.isEmpty()) return fail("kind is required")
                if (state.effectCatalogue.none { it.id == kind }) {
                    return fail("`$kind` is not in the effect catalogue; call action=list")
                }
                val addedId = state.addEffect(layerId, kind)
                    ?: return fail("the effect was not added")
                val added = layerEffects(layerId).firstOrNull { it.id == addedId }
                    ?: return fail("the effect was not added")
                // An effect the driver refused is kept in the chain but skipped
                // when drawing, so the frame will look untouched. Saying so here
                // is the difference between a silent no-op and a named reason.
                val engine = engineReport(listOf(kind))
                val refusal = engine.optJSONArray("warnings")
                    ?.let { arr -> (0 until arr.length()).map { arr.optTextOr(it, "") } }
                    ?.firstOrNull { it.startsWith("The engine refused") }
                val hidden = state.layers.value
                    .firstOrNull { it.id == layerId }
                    ?.takeIf { !it.visible }
                return RumiToolOutcome(
                    JSONObject()
                        .put("ok", true)
                        .put("effectId", added.id)
                        .put("kind", kind)
                        .put("layerId", layerId)
                        .apply { refusal?.let { put("warning", it) } }
                        .apply {
                            if (hidden != null) {
                                put(
                                    "warning",
                                    "layer $layerId is hidden, so its whole chain — including " +
                                        "this effect — is left out of the frame. Make the " +
                                        "layer visible (update_layer visible=true) before " +
                                        "judging the effect.",
                                )
                            }
                        }
                        .toString(),
                    summary = kind,
                )
            }
            "set" -> {
                val layerId = args.optText("layerId").orEmpty()
                val effectId = args.optText("effectId").orEmpty()
                val key = args.optText("key").orEmpty()
                val values = doubles(args.optJSONArray("value"))
                if (key.isEmpty() || values == null) return fail("key and value are required")
                val effect = layerEffects(layerId).firstOrNull { it.id == effectId }
                    ?: return fail("no effect `$effectId` on layer `$layerId`")
                state.setEffectParam(layerId, effectId, key, values)
                val applied = layerEffects(layerId).firstOrNull { it.id == effectId }
                    ?.params
                    ?.joinToString(", ") { "%.3f".format(it) }
                return RumiToolOutcome(
                    "effect $effectId (`${effect.kindId}`) params now [$applied]",
                    summary = key,
                )
            }
            "remove" -> {
                val layerId = args.optText("layerId").orEmpty()
                val effectId = args.optText("effectId").orEmpty()
                val effect = layerEffects(layerId).firstOrNull { it.id == effectId }
                    ?: return fail("no effect `$effectId` on layer `$layerId`")
                state.removeEffect(layerId, effectId)
                return RumiToolOutcome("removed `${effect.kindId}` from $layerId", summary = effect.kindId)
            }
            "toggle" -> {
                val layerId = args.optText("layerId").orEmpty()
                val effectId = args.optText("effectId").orEmpty()
                val effect = layerEffects(layerId).firstOrNull { it.id == effectId }
                    ?: return fail("no effect `$effectId` on layer `$layerId`")
                state.toggleEffect(layerId, effectId)
                val now = layerEffects(layerId).firstOrNull { it.id == effectId }
                return RumiToolOutcome(
                    "effect $effectId is now ${if (now?.enabled == true) "on" else "off"}",
                    summary = if (now?.enabled == true) "on" else "off",
                )
            }
            "move" -> {
                val layerId = args.optText("layerId").orEmpty()
                val effectId = args.optText("effectId").orEmpty()
                val delta = args.optInt("delta", 0)
                layerEffects(layerId).firstOrNull { it.id == effectId }
                    ?: return fail("no effect `$effectId` on layer `$layerId`")
                if (delta == 0) return fail("delta must not be 0")
                state.moveEffect(layerId, effectId, delta)
                val order = layerEffects(layerId).joinToString(" -> ") { it.kindId }
                return RumiToolOutcome("chain order: $order", summary = "moved")
            }
            else -> return fail("action must be list, add, set, remove, toggle or move")
        }
    }

    private suspend fun compareEffects(args: JSONObject): RumiToolOutcome {
        val layerId = args.optText("layerId").orEmpty()
        val aKind = args.optText("aKind").orEmpty()
        val bKind = args.optText("bKind").orEmpty()
        state.layers.value.firstOrNull { it.id == layerId } ?: return fail("no layer with id `$layerId`")
        val base = layerEffects(layerId)
        val a = variant(base, aKind, doubles(args.optJSONArray("aParams"))) ?: return fail(
            "`$aKind` is not in the effect catalogue; call effects(action=list)",
        )
        val b = variant(base, bKind, doubles(args.optJSONArray("bParams"))) ?: return fail(
            "`$bKind` is not in the effect catalogue; call effects(action=list)",
        )
        val t = args.optDouble("timeMs", state.playheadMs.value.toDouble()).toLong()
        val (w, h) = probeSize()
        prepareFrame()
        // The baseline is rendered twice: the whole comparison rests on it, so it
        // is also the frame that tells us whether the comparison means anything.
        val plain = measure(t, w, h, layerId, base)
            ?: return fail("could not render the frame without effects")
        val withA = state.effectProbeAt(t, w, h, layerId, a)
            ?: return fail("could not render with `$aKind`")
        val withB = state.effectProbeAt(t, w, h, layerId, b)
            ?: return fail("could not render with `$bKind`")
        val statsA = diffStats(plain.px, withA)
        val statsB = diffStats(plain.px, withB)
        val statsAb = diffStats(withA, withB)
        val engine = engineReport(listOf(aKind, bKind))
        val warnings = stabilityWarning(plain)
        engine.optJSONArray("warnings")?.let { extra ->
            for (i in 0 until extra.length()) warnings.put(extra.optTextOr(i, ""))
        }
        return RumiToolOutcome(
            JSONObject()
                .put("ok", true)
                .put("projectFile", state.currentFileName.value ?: JSONObject.NULL)
                .put("frame", t)
                .put("size", "$w x $h")
                .put("reproducible", plain.stable)
                .put("baselineDrift", plain.driftValue)
                .put("a", JSONObject().put("kind", aKind).put("vsOriginal", statsA))
                .put("b", JSONObject().put("kind", bKind).put("vsOriginal", statsB))
                .put("aVsB", statsAb)
                .put("warnings", warnings)
                .put("engine", engine)
                .put("layers", frameContents())
                .put("note", "meanDiff is 0..255; changed is the share of pixels that moved " +
                    "by more than 8. A variant with a zero meanDiff did nothing — but only " +
                    "when reproducible is true.")
                .toString(),
            summary = "$aKind ${statsA.optDouble("meanDiff", 0.0)} vs " +
                "$bKind ${statsB.optDouble("meanDiff", 0.0)}",
        )
    }

    private suspend fun defineEffect(args: JSONObject): RumiToolOutcome {
        val id = args.optText("id").orEmpty().trim().lowercase()
        val passes = ArrayList<CustomPassUi>()
        args.optJSONArray("passes")?.let { arr ->
            for (i in 0 until arr.length()) {
                val o = arr.optJSONObject(i) ?: continue
                passes += CustomPassUi(
                    entry = o.optText("entry").orEmpty(),
                    shrink = o.optInt("shrink", 0),
                )
            }
        }
        val params = ArrayList<CustomParamUi>()
        args.optJSONArray("params")?.let { arr ->
            for (i in 0 until arr.length()) {
                val o = arr.optJSONObject(i) ?: continue
                val choices = ArrayList<String>()
                o.optJSONArray("choices")?.let { cs ->
                    for (j in 0 until cs.length()) choices += cs.optTextOr(j, "")
                }
                params += CustomParamUi(
                    key = o.optText("key").orEmpty(),
                    label = o.optTextOr("label", o.optText("key").orEmpty()),
                    kind = o.optTextOr("kind", "float").lowercase(),
                    min = o.optDouble("min", 0.0).toFloat(),
                    max = o.optDouble("max", 1.0).toFloat(),
                    default = doubles(o.optJSONArray("default")) ?: listOf(1f, 0f, 0f, 0f),
                    unit = o.optText("unit").orEmpty(),
                    choices = choices,
                )
            }
        }
        val effect = CustomEffectUi(
            id = id,
            label = args.optTextOr("label", id),
            space = args.optTextOr("space", "display").lowercase(),
            passes = passes,
            params = params,
            source = args.optText("source").orEmpty(),
        )
        val reject = state.defineCustomEffect(effect)
        if (reject != null) {
            return fail(reject, summary = "rejected")
        }
        // `space` is descriptive: the engine keeps every layer in one RGBA8
        // display-space buffer and the chain runs there, so a module that assumed
        // linear light would be wrong in a way the author cannot see. Saying so
        // is better than letting the declaration imply otherwise.
        val spaceWarning = if (effect.space.equals("linear", ignoreCase = true)) {
            "space=linear is not honoured yet: the chain runs on the layer's display-space " +
                "RGBA8 buffer, so do the conversion inside the shader if you need linear light."
        } else {
            null
        }
        // It compiled. Now check it does something: render the frame with it and
        // without it. A module can be perfectly valid and still output identity.
        val probe = probeDefinedEffect(effect)
        val message = JSONObject()
            .put("ok", true)
            .put("id", effect.id)
            .put("slots", effect.params.sumOf { it.slots })
            .put("passes", effect.passes.size)
            .put("check", probe)
            .apply { spaceWarning?.let { put("warning", it) } }
        return RumiToolOutcome(
            message.toString(),
            summary = if (probe?.optBoolean("changed") == true) "defined, changes the frame" else "defined",
        )
    }

    /**
     * Render the selected layer with and without a freshly defined effect.
     *
     * The comparison is only worth anything on a reproducible frame, so the
     * baseline is rendered twice first; an unstable frame is reported as such
     * instead of being passed off as "the effect does nothing".
     */
    private suspend fun probeDefinedEffect(effect: CustomEffectUi): JSONObject? {
        val layer = state.layers.value.lastOrNull { it.visible && it.effects.isNotEmpty() }
            ?: state.layers.value.lastOrNull { it.visible }
            ?: return null
        val base = layer.effects
        val appended = defaultEffectFor(effect.id, state.customsJson()) ?: return null
        val (w, h) = probeSize()
        val t = state.playheadMs.value
        prepareFrame()
        val plain = measure(t, w, h, layer.id, base) ?: return null
        val with = state.effectProbeAt(t, w, h, layer.id, base + appended) ?: return null
        val stats = diffStats(plain.px, with)
        val engine = engineReport(listOf(effect.id))
        return stats
            .put("layerId", layer.id)
            .put("reproducible", plain.stable)
            .put("changed", plain.stable && stats.optDouble("meanDiff", 0.0) > STABLE_EPSILON)
            .put("engine", engine)
            .put(
                "hint",
                "changed=false means the frame is identical with and without the effect: check " +
                    "that the shader reads input_tex, that the parameters you declared are the " +
                    "ones it uses, and that `engine.warnings` is empty.",
            )
    }

    /**
     * Draw a vector: write an SVG into the project folder, make a layer out of it
     * and show in numbers that it really drew.
     *
     * The parse goes **before** the write: a rejected source must not leave a file
     * in the folder that looks like a working asset. If the engine symbol is
     * missing (an old .so), the tool says exactly that and does not report "done"
     * — otherwise the model would consider the picture drawn when it is not.
     */
    private suspend fun svgPaint(args: JSONObject): RumiToolOutcome {
        val source = args.optText("svg") ?: return fail("svg is required: the SVG source text")
        val bytes = source.toByteArray(Charsets.UTF_8)
        if (bytes.size > SVG_MAX_BYTES) {
            return fail("the SVG is ${bytes.size} bytes; the limit is $SVG_MAX_BYTES")
        }
        val validation = RumoBridge.svgValidate(bytes)
            ?: return fail(
                "this build has no SVG parser (nativeSvgValidate is missing), so nothing was " +
                    "written or added",
            )
        if (!validation.ok) {
            return fail("the engine rejected the SVG: ${validation.error.ifEmpty { "no reason given" }}", summary = "rejected")
        }
        val project = state.projectName.value
        val asset = ProjectAssets.writeSvg(
            host.context,
            project,
            args.optTextOr("name", "drawing.svg"),
            bytes,
        ) ?: return fail(
            "the SVG parsed but could not be written into the project folder " +
                "(Download/${ProjectAssets.folderOf(project)})",
        )
        val uri = asset.uri.toString()
        val layerId = state.addSvgLayer(asset.name, uri)
        // Without an id the layer will not get into the frame, and the probe would
        // honestly show zero.
        val svgId = state.ensureSvgRegistered(uri)
            ?: return fail(
                "the SVG was written to ${asset.name} but the engine could not register it: " +
                    (state.svgFailure(uri) ?: "no reason given"),
            )
        prepareFrame()
        val (w, h) = probeSize()
        val layer = state.layers.value.firstOrNull { it.id == layerId }
        val duration = layer?.durationMs ?: EditorState.DEFAULT_MIN_DURATION_MS
        // The layer is added at 0 and lives until `duration`; the playhead may sit
        // outside that window, and then the probe would show an empty frame through
        // a timing error.
        val t = state.playheadMs.value.coerceIn(0L, (duration - 1L).coerceAtLeast(0L))
        val with = state.probeFrameAt(t, w, h, emptySet())
            ?: return fail("could not render a probe frame")
        val again = state.probeFrameAt(t, w, h, emptySet())
            ?: return fail("could not render a probe frame")
        val without = state.probeFrameAt(t, w, h, setOf(layerId))
            ?: return fail("could not render a probe frame")
        val drift = diffStats(with, again).optDouble("meanDiff", 0.0)
        val draw = diffStats(without, with)
        val png = toBitmap(with, w, h)?.let { encodePng(it) }
        val path = png?.let { save(it, "rumi-svg-${System.currentTimeMillis()}.png") }
        val message = JSONObject()
            .put("ok", true)
            .put("layerId", layerId)
            .put("name", asset.name)
            .put("svgId", svgId)
            .put("file", path ?: "not saved")
            .put(
                "svg",
                JSONObject()
                    .put("width", validation.width)
                    .put("height", validation.height)
                    .put("shapes", validation.shapes)
                    .put("flattenedGradients", validation.flattenedGradients)
                    .put("skipped", validation.skipped),
            )
            .put("draw", draw)
            .put("reproducible", drift <= STABLE_EPSILON)
            .put("engine", engineReport(emptyList()))
            .put(
                "note",
                "`draw.meanDiff` is the per-channel difference between the frame with and " +
                    "without this layer (0..255), and `draw.changed` the share of pixels that " +
                    "moved: changed=0 means the layer is in the project but nothing reached the " +
                    "frame — look at `layers` via snapshot and `engine.warnings`. " +
                    "`svg.flattenedGradients` counts gradients reduced to one colour; " +
                    "`svg.skipped` counts elements the renderer does not support.",
            )
        if (validation.skipped > 0) {
            message.put(
                "warning",
                "${validation.skipped} element(s) were skipped: patterns, group opacity and " +
                    "embedded images are not supported.",
            )
        }
        return RumiToolOutcome(
            message.toString(),
            png = png,
            path = path,
            summary = "${validation.shapes} shape(s) · ${asset.name}",
        )
    }

    /**
     * Fonts: list, preview, installation, default font.
     *
     * The preview is drawn by the engine, not the platform: the point of the tool
     * is for the model to see **the same** result the layer will get. Something
     * drawn with `Typeface` would be similar but not identical, and the discrepancy
     * would surface for the user.
     */
    private suspend fun fonts(args: JSONObject): RumiToolOutcome {
        return when (val action = args.optTextOr("action", "list")) {
            "list" -> fontsList()
            "preview" -> fontsPreview(args)
            "install" -> fontsInstall(args)
            "set_default" -> fontsSetDefault(args)
            "remove" -> fontsRemove(args)
            else -> fail("unknown fonts action `$action`")
        }
    }

    private fun fontsList(): RumiToolOutcome {
        val installed = FontStore.installed(host.context)
        val default = ShopPrefs.state.value.defaultFontFamily
        val array = JSONArray()
        for (font in installed) {
            array.put(
                JSONObject()
                    .put("family", font.family)
                    .put("name", font.displayName)
                    .put("license", font.license)
                    .put("source", font.source),
            )
        }
        val message = JSONObject()
            .put("ok", true)
            // An empty string reads as "no font", and that is exactly what it
            // means: a layer with an empty `font` is typeset in the built-in face.
            .put("default", default.ifEmpty { "built-in monospace" })
            .put("installed", array)
            .put(
                "note",
                "A layer's own font is set with update_layer(font=...). Pass a family " +
                    "exactly as it appears here; a name the engine does not know makes the " +
                    "layer fall back to the built-in face rather than lose its text.",
            )
        return RumiToolOutcome(
            message.toString(),
            summary = "${installed.size} installed",
        )
    }

    private suspend fun fontsPreview(args: JSONObject): RumiToolOutcome {
        val family = args.optText("family") ?: return fail("family is required for preview")
        val sample = args.optTextOr("text", family)
        val size = args.optDouble("sizePx", 48.0).toInt().coerceIn(8, 256)
        val bytes = fontBytesFor(family) ?: return fail(
            "`$family` is neither installed nor found in Google Fonts. Call fonts(list) to " +
                "see what is installed, or install it first.",
        )
        val decoded = RumoBridge.fontPreview(
            family = family,
            fontBytes = bytes,
            text = sample,
            sizePx = size.toFloat(),
            weight = 400,
            argb = 0xFF000000.toInt(),
            pad = 6,
        ) ?: return fail(
            "the engine could not draw `$family`: the face did not register, or the sample " +
                "text has no ink.",
        )
        val bitmap = rgbaToBitmap(decoded)
            ?: return fail("the preview (${decoded.width}x${decoded.height}) is not a valid image")
        // The text is drawn on a transparent background, and the model will see a
        // transparent PNG against whatever background its viewer provides. The
        // backing is white and opaque — so that "how the font looks" does not depend
        // on whether the viewer's theme is light or dark.
        val opaque = flattenOnWhite(bitmap)
        val png = encodePng(opaque) ?: return fail("the preview could not be encoded as PNG")
        val path = save(png, "rumi-font-${slugForFile(family)}.png")
        val message = JSONObject()
            .put("ok", true)
            .put("family", family)
            .put("sizePx", size)
            .put("pixels", JSONObject().put("width", decoded.width).put("height", decoded.height))
            .put("file", path ?: "not saved")
        return RumiToolOutcome(message.toString(), png = png, path = path, summary = family)
    }

    private suspend fun fontsInstall(args: JSONObject): RumiToolOutcome {
        val family = args.optText("family") ?: return fail("family is required for install")
        if (FontStore.isInstalled(host.context, family)) {
            return RumiToolOutcome(
                JSONObject().put("ok", true).put("family", family).put("alreadyInstalled", true)
                    .toString(),
                summary = "$family already installed",
            )
        }
        val entry = googleFamily(family)
        val face = GoogleFonts.resolveFace(entry)
            ?: return fail("Google Fonts has no repository directory for `$family`")
        val reply = GoogleFonts.download(face)
        if (!reply.ok || reply.bytes.isEmpty()) {
            return fail(
                "the download of `$family` failed (${face.fileName}); nothing was installed.",
            )
        }
        val installed = FontStore.install(
            context = host.context,
            displayName = family,
            source = "google-fonts",
            license = face.license,
            copyright = face.copyright,
            bytes = reply.bytes,
        ) ?: return fail("`$family` did not parse as a font; nothing was installed.")
        // The licence is stored next to the font: Google Fonts requires it to be
        // distributed together with the face, not to keep a single copyright.
        GoogleFonts.licenseText(face)?.let { FontStore.saveLicense(host.context, installed, it) }
        val message = JSONObject()
            .put("ok", true)
            .put("family", installed.family)
            .put("license", face.license)
            .put("file", face.fileName)
            .put(
                "note",
                "Use this exact `family` string in update_layer(font=...) — it is the name " +
                    "the file carries, which may differ from the catalogue spelling.",
            )
        return RumiToolOutcome(message.toString(), summary = "${installed.family} installed")
    }

    private fun fontsSetDefault(args: JSONObject): RumiToolOutcome {
        val family = args.optText("family") ?: return fail("family is required for set_default")
        val installed = FontStore.installed(host.context)
        val match = installed.firstOrNull { it.family == family || it.displayName == family }
        // An empty string is legitimate: it is a return to the built-in face, and
        // refusing it would mean not allowing the choice to be undone.
        if (family.isNotEmpty() && match == null) {
            return fail(
                "`$family` is not installed, so it cannot be the default. Call fonts(list), " +
                    "or fonts(install) first.",
            )
        }
        ShopPrefs.setDefaultFontFamily(match?.family.orEmpty())
        val message = JSONObject()
            .put("ok", true)
            .put("default", match?.family ?: "built-in monospace")
        return RumiToolOutcome(message.toString(), summary = match?.family ?: "built-in")
    }

    private suspend fun fontsRemove(args: JSONObject): RumiToolOutcome {
        val family = args.optText("family") ?: return fail("family is required for remove")
        val removed = withContext(Dispatchers.IO) { FontStore.remove(host.context, family) }
        if (!removed) return fail("`$family` is not installed")
        if (ShopPrefs.state.value.defaultFontFamily == family) ShopPrefs.setDefaultFontFamily("")
        val message = JSONObject()
            .put("ok", true)
            .put("family", family)
            .put(
                "note",
                "Layers already using this family keep the name and now draw with the " +
                    "built-in face; their text is not lost.",
            )
        return RumiToolOutcome(message.toString(), summary = "$family removed")
    }

    /** The face bytes: from storage if installed, otherwise from Google Fonts. */
    private suspend fun fontBytesFor(family: String): ByteArray? {
        FontStore.bytes(host.context, family)?.let { return it }
        val installed = FontStore.installed(host.context)
            .firstOrNull { it.displayName == family }
        if (installed != null) {
            FontStore.bytes(host.context, installed.family)?.let { return it }
        }
        val face = GoogleFonts.resolveFace(googleFamily(family)) ?: return null
        val reply = GoogleFonts.download(face, FONT_PREVIEW_LIMIT_BYTES)
        return if (reply.ok && reply.bytes.isNotEmpty()) reply.bytes else null
    }

    /**
     * A catalogue entry by a single name.
     *
     * `resolveFace` uses only the name and the directory computed from it, so the
     * remaining fields are empty: pulling the whole Google Fonts catalogue for one
     * preview would be three megabytes instead of one request.
     */
    private fun googleFamily(name: String): GoogleFonts.Family = GoogleFonts.Family(
        name = name,
        category = "",
        subsets = emptyList(),
        popularity = 0,
        weights = emptyList(),
        hasItalic = false,
    )

    /** A file name from a family name: only letters, digits and a hyphen. */
    private fun slugForFile(name: String): String =
        name.lowercase().map { if (it.isLetterOrDigit()) it else '-' }
            .joinToString("")
            .trim('-')
            .replace(Regex("-+"), "-")
            .ifEmpty { "font" }

    /**
     * Rasterise an SVG into a picture (the legacy path).
     *
     * A separate tool rather than a flag on [svgPaint]: the result is not a vector
     * layer but an ordinary picture, which has a different cost (it does not scale)
     * and a different holder ([EditorState.addMediaLayer] + staged texture). A flag
     * would make the model guess which layer it would get.
     *
     * The raster is obtained **before** the write: a rasterisation failure must not
     * leave a file in the folder that looks like a working asset.
     */
    private suspend fun svgRasterize(args: JSONObject): RumiToolOutcome {
        val source = args.optText("svg") ?: return fail("svg is required: the SVG source text")
        val bytes = source.toByteArray(Charsets.UTF_8)
        if (bytes.size > SVG_MAX_BYTES) {
            return fail("the SVG is ${bytes.size} bytes; the limit is $SVG_MAX_BYTES")
        }
        // The major side: by default the canvas's major side, so that the picture is
        // no smaller than the frame; the ceiling is the engine limit, above which it
        // will refuse.
        val major = maxOf(state.canvasWidth.value, state.canvasHeight.value)
        val requested = args.optDouble("sizePx", Double.NaN)
        val size = (if (requested.isNaN()) major else requested.toInt())
            .coerceIn(1, RumoBridge.SVG_RASTER_MAX_SIDE)
        val decoded = RumoBridge.svgRasterize(bytes, size)
            ?: return fail(
                "the engine could not rasterise the SVG at ${size}px — a malformed document, " +
                    "a raster above the 16 Mi-pixel cap, or a build without the rasteriser. " +
                    "Nothing was written.",
            )
        val bitmap = rgbaToBitmap(decoded)
            ?: return fail("the raster (${decoded.width}x${decoded.height}) is not a valid image")
        val png = encodePng(bitmap)
            ?: return fail("the raster could not be encoded as PNG, so nothing was written")
        val project = state.projectName.value
        val asset = ProjectAssets.writePicture(
            host.context,
            project,
            args.optTextOr("name", "drawing.png"),
            png,
        ) ?: return fail(
            "the raster rendered but could not be written into the project folder " +
                "(Download/${ProjectAssets.folderOf(project)})",
        )
        val uri = asset.uri.toString()
        val layerId = state.addMediaLayer(
            asset.name,
            LayerKindUi.MEDIA,
            EditorState.DEFAULT_MIN_DURATION_MS,
            uri,
        )
        // The texture is staged at once: a MEDIA layer gets into the frame only
        // with a staged texture, and nobody else will create it on the assistant
        // tab.
        state.stageTexture(uri, decoded)
        prepareFrame()
        val (w, h) = probeSize()
        val layer = state.layers.value.firstOrNull { it.id == layerId }
        val duration = layer?.durationMs ?: EditorState.DEFAULT_MIN_DURATION_MS
        val t = state.playheadMs.value.coerceIn(0L, (duration - 1L).coerceAtLeast(0L))
        val with = state.probeFrameAt(t, w, h, emptySet())
            ?: return fail("could not render a probe frame")
        val again = state.probeFrameAt(t, w, h, emptySet())
            ?: return fail("could not render a probe frame")
        val without = state.probeFrameAt(t, w, h, setOf(layerId))
            ?: return fail("could not render a probe frame")
        val drift = diffStats(with, again).optDouble("meanDiff", 0.0)
        val draw = diffStats(without, with)
        val probePng = toBitmap(with, w, h)?.let { encodePng(it) }
        val path = probePng?.let { save(it, "rumi-svg-raster-${System.currentTimeMillis()}.png") }
        val message = JSONObject()
            .put("ok", true)
            .put("layerId", layerId)
            .put("name", asset.name)
            .put(
                "raster",
                JSONObject().put("width", decoded.width).put("height", decoded.height),
            )
            .put("file", path ?: "not saved")
            .put("draw", draw)
            .put("reproducible", drift <= STABLE_EPSILON)
            .put("engine", engineReport(emptyList()))
            .put(
                "note",
                "This layer is a picture, not vector geometry: it does not scale without " +
                    "blurring — pick a larger `sizePx` instead of scaling it up. " +
                    "`draw.meanDiff` is the per-channel difference between the frame with and " +
                    "without this layer (0..255), and `draw.changed` the share of pixels that " +
                    "moved: changed=0 means the layer is in the project but nothing reached " +
                    "the frame — check `engine.warnings` and the layer's uri.",
            )
        return RumiToolOutcome(
            message.toString(),
            png = probePng,
            path = path,
            summary = "${decoded.width}x${decoded.height} png · ${asset.name}",
        )
    }

    // --- References: pictures, video, audio ---

    /**
     * The project's reference folder, as the assistant sees it.
     *
     * A project's material is a folder next to the projects, which the user fills
     * from a file manager. Reading it needs the read-media permission because
     * those files belong to the shared storage and not to this app; without it
     * the listing says exactly that, because "the folder is empty" and "I am not
     * allowed to look" are different answers and only one of them is true.
     */
    private suspend fun media(args: JSONObject): RumiToolOutcome {
        val action = args.optTextOr("action", "")
        val project = state.projectName.value
        val access = mediaAccess()
        return when (action) {
            "list" -> {
                val assets = ProjectAssets.list(host.context, project)
                val array = JSONArray()
                assets.forEach { asset ->
                    array.put(
                        JSONObject()
                            .put("name", asset.name)
                            .put("kind", asset.kind.name.lowercase())
                            .put("bytes", asset.sizeBytes)
                            .put("uri", asset.uri.toString()),
                    )
                }
                val message = JSONObject()
                    .put("ok", true)
                    .put("project", project)
                    .put("folder", folderLabel(project))
                    .put("assets", array)
                    .put("access", access)
                if (!access.optBoolean("granted")) {
                    message.put(
                        "note",
                        "Read-media access is not granted, so this list holds only the files " +
                            "the app itself wrote into the folder; anything the user put there " +
                            "themselves is invisible until they answer the dialog that is on " +
                            "their screen now. Do not tell them to change a setting by hand and " +
                            "do not guess file names: wait, then call media(action=list) again.",
                    )
                }
                RumiToolOutcome(
                    message.toString(),
                    summary = if (access.optBoolean("granted")) {
                        "${assets.size} reference(s)"
                    } else {
                        "${assets.size} reference(s) · no read-media access"
                    },
                )
            }
            "add", "probe", "preview" -> {
                val source = args.optTextOr("source", "")
                if (source.isEmpty()) return fail("source is required: a file name in the " +
                    "project folder, or a content uri")
                val asset = resolveReference(project, source)
                    ?: return fail(
                        "`$source` is not in the project's folder and is not a uri I can read" +
                            if (access.optBoolean("granted")) {
                                "; call media(action=list) to see what is there"
                            } else {
                                ", and read-media access is not granted yet, so the user's own " +
                                    "files there are invisible; the dialog asking for it is on " +
                                    "their screen now, so wait and call media(action=list) again"
                            },
                    )
                val kind = kindOf(args.optTextOr("kind", ""), asset.kind)
                if (action == "probe") {
                    return RumiToolOutcome(probeReference(asset, kind).toString(), summary = asset.name)
                }
                if (action == "preview") return previewReference(asset, kind, args)
                if (!mayMutate) {
                    return fail(
                        "this sub-agent cannot add to the timeline: its role is to look and " +
                            "report. Use preview or probe to see the reference, and say what " +
                            "you found — the assistant that started you will do the adding.",
                    )
                }
                addReference(asset, kind, args)
            }
            else -> fail("action must be list, add, probe or preview")
        }
    }

    /**
     * Show a reference without adding it.
     *
     * Why this is needed: a file name says nothing about what is in the frame. The
     * model picks a clip by name and size, puts it on the timeline and learns it
     * was the wrong one only from a snapshot — that is, after the work is done.
     * Here it looks at the candidate before the edit, and "the wrong one" costs one
     * call instead of a redo.
     *
     * The picture goes the same way as a frame snapshot: a PNG in `Download/Rumo`
     * plus pixels in the response, so the model sees it if it accepts images.
     *
     * Audio cannot be shown, and that is stated plainly rather than as a tool
     * refusal: "nothing to look at" and "it broke" are different things.
     */
    private suspend fun previewReference(
        asset: ProjectAssets.Asset,
        kind: ProjectAssets.Kind,
        args: JSONObject,
    ): RumiToolOutcome {
        val maxSide = args.optDouble("maxSide", PREVIEW_SIDE.toDouble()).toInt()
            .coerceIn(64, 1024)
        val timeMs = args.optDouble("timeMs", 0.0).toLong().coerceAtLeast(0L)

        // Without an explicit `DecodedImage?`: with it the variable's type stays
        // nullable, and the check below does not narrow it to non-null.
        val decoded = when (kind) {
            ProjectAssets.Kind.IMAGE -> withContext(Dispatchers.IO) {
                readUriBytes(host.context, asset.uri)?.let { RumoBridge.decodeImage(it, maxSide) }
            }
            ProjectAssets.Kind.SVG -> withContext(Dispatchers.IO) {
                readUriBytes(host.context, asset.uri)
                    ?.let { RumoBridge.svgRasterize(it, maxSide) }
            }
            ProjectAssets.Kind.VIDEO -> withContext(Dispatchers.IO) {
                videoFrame(asset.uri, timeMs)
            }
            ProjectAssets.Kind.AUDIO -> return RumiToolOutcome(
                JSONObject()
                    .put("ok", true)
                    .put("name", asset.name)
                    .put("kind", "audio")
                    .put(
                        "durationMs",
                        withContext(Dispatchers.IO) { audioDurationMs(host.context, asset.uri) },
                    )
                    .put(
                        "note",
                        "Sound has no picture to look at. `probe` reports its length, and " +
                            "`analyze_audio_stream` reports what is in it.",
                    )
                    .toString(),
                summary = "${asset.name} · audio",
            )
            ProjectAssets.Kind.OTHER -> return fail(
                "`${asset.name}` is not a kind I can show. Pass kind=image, video or svg.",
            )
        } ?: return fail(
            if (kind == ProjectAssets.Kind.VIDEO) {
                "the frame at ${timeMs}ms of `${asset.name}` could not be decoded — the clip " +
                    "may be shorter than that, or the codec is not one this build reads. " +
                    "Nothing was added."
            } else {
                "`${asset.name}` could not be read or decoded, so there is nothing to show. " +
                    "Nothing was added."
            },
        )

        val bitmap = rgbaToBitmap(decoded)
            ?: return fail("the picture (${decoded.width}x${decoded.height}) is not a valid image")
        // A video frame arrives in its own size: 4K is 33 MB of pixels, and
        // encoding them to PNG for a picture nobody reads in full means spending
        // seconds and tens of megabytes. The image and the SVG are already reduced
        // by the engine.
        val shown = fitWithin(bitmap, maxSide)
        // The backing is opaque: the model will see a PNG with a transparent
        // background against whatever background its viewer provides, and "empty"
        // instead of a picture is a conclusion it will draw confidently.
        val png = encodePng(flattenOnWhite(shown))
            ?: return fail("the picture could not be encoded as PNG")
        val path = save(png, "rumi-preview-${slugForFile(asset.name)}.png")

        val message = JSONObject()
            .put("ok", true)
            .put("name", asset.name)
            .put("kind", kind.name.lowercase())
            .put("pixels", JSONObject().put("width", shown.width).put("height", shown.height))
            .put("file", path ?: "not saved")
        if (kind == ProjectAssets.Kind.VIDEO) message.put("timeMs", timeMs)
        message.put(
            "note",
            "This is the reference itself, not the project. Nothing was added to the " +
                "timeline; call media(action=add, source=...) to place it.",
        )
        return RumiToolOutcome(
            message.toString(),
            png = png,
            path = path,
            summary = if (kind == ProjectAssets.Kind.VIDEO) {
                "${asset.name} @ ${timeMs}ms"
            } else {
                asset.name
            },
        )
    }

    /**
     * A video frame as RGBA, without adding it to the project.
     *
     * `nativeVideoFrameAt` hands back bare pixels with no size, so the size is
     * taken from the same handle separately: building a Bitmap from a guess at the
     * width would mean drawing garbage from correct bytes.
     *
     * The handle is closed in `finally`: the decoder holds a file descriptor, and a
     * preview that leaves them behind breaks the next open of the same clip.
     */
    private fun videoFrame(uri: Uri, timeMs: Long): RumoBridge.DecodedImage? {
        val pfd = try {
            host.context.contentResolver.openFileDescriptor(uri, "r")
        } catch (_: Exception) {
            null
        } ?: return null
        return pfd.use {
            val handle = RumoBridge.videoOpen(it.fd, 0L) ?: return null
            try {
                val info = RumoBridge.videoInfo(handle) ?: return null
                val rgba = RumoBridge.videoFrameAt(handle, timeMs) ?: return null
                if (info.width <= 0 || info.height <= 0) return null
                if (rgba.size != info.width * info.height * 4) return null
                RumoBridge.DecodedImage(info.width, info.height, rgba)
            } finally {
                RumoBridge.videoClose(handle)
            }
        }
    }

    /**
     * Read access to the shared storage, asked for from the tool itself.
     *
     * The dialog belongs to this app and the model is the only thing that knows
     * the folder is needed *now*, so the ask happens here rather than being
     * written into a hint for the user to act on: a model that can only say
     * "grant it yourself" cannot finish the job it was given. A tool cannot wait
     * for a human tap, so the answer carries what is already true plus a sentence
     * for the model, and the next call reads the folder again.
     */
    private fun mediaAccess(): JSONObject {
        if (host.mediaAccessGranted()) return JSONObject().put("granted", true)
        return JSONObject()
            .put("granted", false)
            .put("requested", true)
            .put("note", host.requestMediaAccess())
    }

    /** A file in the project's folder, or a uri the app can already read. */
    /**
     * The absolute path of the project's references folder.
     *
     * Responses carry the full path, not `Download/Rumo/<project>`: the short
     * string reads as "somewhere in Download", and when the assistant names it to
     * the user, they cannot tell their project's folder from a neighbouring one —
     * that is exactly what "it is looking in the wrong place" looks like. The full
     * path can be opened in a file manager and checked with the eyes.
     *
     * It is built from the project name, not from the requested string: the folder
     * belongs to the project that is **open**, and that is the only thing that
     * makes the response consistent with what the next `media(action=list)` will
     * read.
     */
    private fun folderLabel(project: String): String {
        @Suppress("DEPRECATION")
        val root = Environment.getExternalStorageDirectory()?.absolutePath
            ?: "/storage/emulated/0"
        return "$root/Download/${ProjectAssets.folderOf(project)}"
    }

    private suspend fun resolveReference(project: String, source: String): ProjectAssets.Asset? {
        if (source.startsWith("content://") || source.startsWith("file://")) {
            val uri = Uri.parse(source)
            return ProjectAssets.Asset(
                name = queryDisplayName(host.context, uri, "reference"),
                uri = uri,
                kind = ProjectAssets.Kind.OTHER,
                sizeBytes = 0L,
                addedAt = System.currentTimeMillis(),
            )
        }
        return ProjectAssets.find(host.context, project, source)
    }

    private fun kindOf(requested: String, guess: ProjectAssets.Kind): ProjectAssets.Kind = when (requested) {
        "image" -> ProjectAssets.Kind.IMAGE
        "video" -> ProjectAssets.Kind.VIDEO
        "audio" -> ProjectAssets.Kind.AUDIO
        "svg" -> ProjectAssets.Kind.SVG
        else -> guess
    }

    private fun probeReference(asset: ProjectAssets.Asset, kind: ProjectAssets.Kind): JSONObject {
        val out = JSONObject()
            .put("ok", true)
            .put("name", asset.name)
            .put("kind", kind.name.lowercase())
            .put("bytes", asset.sizeBytes)
            .put("uri", asset.uri.toString())
        when (kind) {
            ProjectAssets.Kind.VIDEO -> videoInfo(host.context, asset.uri)?.let { info ->
                out.put("width", info.width).put("height", info.height).put("durationMs", info.durationMs)
            } ?: out.put("note", "the video could not be probed; the engine will try again at render time")
            ProjectAssets.Kind.AUDIO -> out.put("durationMs", audioDurationMs(host.context, asset.uri))
            ProjectAssets.Kind.IMAGE -> {
                val bytes = readUriBytes(host.context, asset.uri)
                val decoded = bytes?.let { RumoBridge.decodeImage(it) }
                if (decoded != null) {
                    out.put("width", decoded.width).put("height", decoded.height)
                } else {
                    out.put("note", "the picture could not be decoded")
                }
            }
            ProjectAssets.Kind.SVG -> {
                // The vector is not decoded into a texture: it has its own check,
                // which also reports how many shapes and how many gradients were
                // reduced to one colour.
                val parsed = RumoBridge.svgValidate(readUriBytes(host.context, asset.uri) ?: ByteArray(0))
                if (parsed != null && parsed.ok) {
                    out.put("width", parsed.width).put("height", parsed.height)
                    out.put("shapes", parsed.shapes)
                    out.put("flattenedGradients", parsed.flattenedGradients)
                    out.put("skipped", parsed.skipped)
                } else {
                    out.put("note", parsed?.error?.ifEmpty { "the engine could not parse it" }
                        ?: "this build has no SVG parser")
                }
            }
            ProjectAssets.Kind.OTHER -> out.put(
                "note",
                "unrecognised kind; pass kind=image, video, audio or svg to add it",
            )
        }
        return out
    }

    /**
     * Put one reference on the timeline.
     *
     * The three kinds are added the way the editor's own picker adds them, on
     * purpose: a second path that decides lengths and staging differently would
     * drift, and the picture case is exactly where a missing staging step shows
     * up as a layer that is silently absent from the frame.
     */
    private suspend fun addReference(
        asset: ProjectAssets.Asset,
        kind: ProjectAssets.Kind,
        args: JSONObject,
    ): RumiToolOutcome {
        val uriText = asset.uri.toString()
        val before = state.layers.value.map { it.id }.toSet()
        val probed: Long? = when (kind) {
            ProjectAssets.Kind.VIDEO -> videoInfo(host.context, asset.uri)?.durationMs?.takeIf { it > 0L }
            ProjectAssets.Kind.AUDIO -> audioDurationMs(host.context, asset.uri).takeIf { it > 0L }
            else -> null
        }
        val duration = args.optDouble("durationMs", Double.NaN)
        val length = when {
            !duration.isNaN() -> duration.toLong().coerceAtLeast(1L)
            probed != null -> probed
            else -> EditorState.DEFAULT_MIN_DURATION_MS
        }
        // startMs, scale, x/y and alpha are NOT set here: applyFields below owns
        // that mapping, so there is exactly one place that knows how an argument
        // becomes a property of a layer.
        state.beginUndoBatch()
        try {
            when (kind) {
                ProjectAssets.Kind.AUDIO -> {
                    val id = state.addMediaLayer(asset.name, LayerKindUi.AUDIO, length, uriText)
                    state.attachAudio(host.context, id, uriText)
                }
                ProjectAssets.Kind.VIDEO -> {
                    val id = state.addMediaLayer(asset.name, LayerKindUi.MEDIA, length, uriText)
                    state.markVideoLayer(id)
                }
                ProjectAssets.Kind.SVG -> {
                    // An SVG is not a texture but geometry: a SHAPE layer with a uri.
                    // Without registration it would silently fail to reach the frame,
                    // so it is checked here rather than deferred until the frame.
                    state.addSvgLayer(asset.name, uriText)
                    if (state.ensureSvgRegistered(uriText) == null) {
                        return fail(
                            "the SVG could not be registered: " +
                                (state.svgFailure(uriText) ?: "unknown reason"),
                        )
                    }
                }
                else -> {
                    state.addMediaLayer(asset.name, LayerKindUi.MEDIA, length, uriText)
                    // Stage the pixels now: a picture layer enters a frame only once
                    // its texture exists, and nothing else is running to create it
                    // while the assistant tab is the one on screen.
                    val bytes = readUriBytes(host.context, asset.uri)
                    val decoded = bytes?.let { RumoBridge.decodeImage(it) }
                    if (decoded != null) {
                        state.stageTexture(uriText, decoded)
                    } else {
                        return fail("the picture could not be decoded, so it was not added")
                    }
                }
            }
            val id = state.layers.value.firstOrNull { it.id !in before }?.id
                ?: return fail("the layer was not added")
            applyFields(id, args)
            val layer = state.layers.value.firstOrNull { it.id == id }
            val message = JSONObject()
                .put("ok", true)
                .put("layerId", id)
                .put("name", layer?.name ?: asset.name)
                .put("kind", kind.name.lowercase())
                .put("durationMs", layer?.durationMs ?: length)
                .put("startMs", layer?.startMs ?: 0L)
                .put("probed", probed != null)
            if (probed == null && kind != ProjectAssets.Kind.IMAGE && kind != ProjectAssets.Kind.SVG) {
                message.put(
                    "note",
                    "the media's own length could not be read, so the layer got a default one; " +
                        "set durationMs explicitly if that matters",
                )
            }
            return RumiToolOutcome(message.toString(), summary = "${kind.name.lowercase()} ${asset.name}")
        } finally {
            state.endUndoBatch()
        }
    }

    // --- Generating: speech, sound, pictures, video ---

    /**
     * One entry point for all four generative tools.
     *
     * The difference between them is only the kind of service and the shape of the
     * result. The service choice, the refusal when none is ready and the honest
     * error text are the same for all, and splitting them into four copies would
     * create four places where an error can get lost.
     */
    private suspend fun aiGenerate(args: JSONObject, kind: AiKind): RumiToolOutcome {
        val settings = AiServices.state.value
        val ready = settings.readyOf(kind)
        if (ready.isEmpty()) {
            // A tool with no ready service is not shown to the model, so only a
            // call that survived a settings change between the request and the
            // response lands here; this must be said plainly, not by silence.
            return fail(
                "no ${kind.id} service is switched on with a key, so nothing was generated. " +
                    "Add one in Settings → Rumi assistant → AI services.",
            )
        }
        val wanted = args.optText("service")
        val entry = AiTools.resolve(settings, kind, wanted)
            ?: return fail(
                "`$wanted` is not a ready ${kind.id} service. Ready: " +
                    ready.joinToString(", ") { "`${it.id}`" },
            )
        if (!mayMutate) {
            return fail(
                "this sub-agent cannot add generated media to the timeline: its role is to look " +
                    "and report. Say what you needed generated, and the assistant that started " +
                    "you will generate it.",
            )
        }
        return when (val outcome = AiTools.run(entry, args)) {
            is AiOutcome.Failed -> fail(outcome.text)
            is AiOutcome.Pending -> RumiToolOutcome(
                JSONObject()
                    .put("ok", true)
                    .put("pending", true)
                    .put("operation", outcome.operation)
                    .put("waitedMs", outcome.waitedMs)
                    .put(
                        "note",
                        "The render is still running; nothing was added to the timeline. Call " +
                            "`generate_video` again with `operation` set to this name to collect " +
                            "it — that never starts a second render, so it costs nothing to wait.",
                    )
                    .toString(),
                summary = "video still rendering",
            )
            is AiOutcome.Media -> placeGenerated(entry, kind, outcome, args)
        }
    }

    /**
     * Put the generated result into the project the same way a reference is put.
     *
     * The same steps as [addReference], and deliberately so: a picture must be
     * staged, otherwise the layer silently fails to reach the frame, and audio must
     * be attached, otherwise it will not play with the timeline. A second path that
     * decided length and staging its own way would diverge from the first at the
     * first engine change.
     */
    private suspend fun placeGenerated(
        entry: AiEntry,
        kind: AiKind,
        media: AiOutcome.Media,
        args: JSONObject,
    ): RumiToolOutcome {
        // We decode the picture before the write: a file that "exists but does not
        // read" looks like a working asset, while the layer will not draw from it.
        val picture = if (kind == AiKind.Image) {
            RumoBridge.decodeImage(media.bytes)
                ?: return fail(
                    "the picture came back as ${media.bytes.size} bytes but is not an image " +
                        "this build can decode, so nothing was written or added",
                )
        } else {
            null
        }
        val project = state.projectName.value
        val asset = ProjectAssets.writeMedia(
            host.context,
            project,
            args.optTextOr("name", media.fileName),
            media.bytes,
            media.mime,
        ) ?: return fail(
            "the ${kind.id} was generated but could not be written into the project folder " +
                "(Download/${ProjectAssets.folderOf(project)}); nothing was added",
        )
        val uri = asset.uri.toString()
        // We take the length from the file itself, not from the requested value: for
        // TTS it depends on the text, and for Veo the service may have returned a
        // different size, and a layer stretched to an invented length would diverge
        // from the audio.
        val probed: Long? = when (kind) {
            AiKind.Video -> videoInfo(host.context, asset.uri)?.durationMs?.takeIf { it > 0L }
            AiKind.Speech, AiKind.Sound -> audioDurationMs(host.context, asset.uri).takeIf { it > 0L }
            AiKind.Image -> null
        }
        val length = probed ?: EditorState.DEFAULT_MIN_DURATION_MS

        state.beginUndoBatch()
        val id = try {
            val created = when (kind) {
                AiKind.Image -> {
                    val layer = state.addMediaLayer(asset.name, LayerKindUi.MEDIA, length, uri)
                    val decoded = picture
                        ?: return fail("the picture could not be decoded, so it was not added")
                    state.stageTexture(uri, decoded)
                    layer
                }
                AiKind.Speech, AiKind.Sound -> {
                    val layer = state.addMediaLayer(asset.name, LayerKindUi.AUDIO, length, uri)
                    state.attachAudio(host.context, layer, uri)
                    layer
                }
                AiKind.Video -> {
                    val layer = state.addMediaLayer(asset.name, LayerKindUi.MEDIA, length, uri)
                    state.markVideoLayer(layer)
                    layer
                }
            }
            // startMs, scale, x/y and alpha are NOT set here: their owner is
            // applyFields, so that an argument becomes a layer property in one
            // place, as with the other tools.
            applyFields(created, args)
            created
        } finally {
            state.endUndoBatch()
        }

        val layer = state.layers.value.firstOrNull { it.id == id }
        val message = JSONObject()
            .put("ok", true)
            .put("layerId", id)
            .put("name", layer?.name ?: asset.name)
            .put("kind", kind.id)
            .put("service", entry.id)
            .put("model", entry.model)
            .put("bytes", media.bytes.size)
            .put("file", asset.name)
            .put("folder", folderLabel(project))
            .put("durationMs", layer?.durationMs ?: length)
        if (picture != null) {
            message.put(
                "pixels",
                JSONObject().put("width", picture.width).put("height", picture.height),
            )
        }
        if (probed == null && kind != AiKind.Image) {
            message.put(
                "note",
                "the media's own length could not be read, so the layer got a default one; " +
                    "set durationMs explicitly if that matters",
            )
        }
        return RumiToolOutcome(
            message.toString(),
            summary = "${kind.id} via ${entry.label} · ${asset.name}",
        )
    }

    // --- Listening ---

    /**
     * Beats in an audio reference.
     *
     * The analysis itself is Rust (`nativeAudioAnalyzeBeats`), decoded with the
     * same pure-Rust decoder the player uses. What happens here is only the
     * paperwork: find the file, hand over its bytes, and keep the answer small
     * enough to be useful — a ten-minute track has hundreds of beats, and a list
     * of hundreds is not a finding, it is a dump.
     */
    /**
     * Speech and pauses in a recording.
     *
     * ## What this answers, and what it deliberately does not
     *
     * The pauses are found locally: the file is decoded and run through a
     * speech/silence detector in the engine, so this works offline and nothing
     * leaves the device. That is the part that makes a long cut possible — the
     * model gets the real pause map instead of guessing where sentences end.
     *
     * The transcript is **not** answered. `transcript` is always null and
     * `transcriptNote` says why: this build has no speech recogniser linked in.
     * That is a deliberate refusal rather than an omission — a transcript without
     * word timings cannot place subtitles, and a plausible-looking transcript
     * invented here would be indistinguishable from a real one to whoever reads
     * it next. See `docs/09-rumi-assistant.md` for what linking one would take.
     */
    /**
     * A batch of timeline edits, applied in the engine in one pass.
     *
     * ## Why the batch lives in Rust
     *
     * The project document is the engine's (`rumo-core`), and the operations are
     * expressed against that document — keyframe partitioning on a split, ripple
     * arithmetic, overlap rules. Reimplementing them here would be a second copy of
     * the model's rules in a second language, and the two copies would disagree
     * about the first edge case nobody tested. So the ops go over as JSON, the core
     * applies them, and what comes back is a whole document.
     *
     * The document is adopted as **one** edit ([EditorState.applyProjectDocument]),
     * which is what makes a hundred cuts undo in one press.
     *
     * A batch is never thrown away over one bad operation: `skipped` carries the
     * reasons and the rest stand. The model is told what did not apply so it can
     * correct the next call rather than assume the timeline matches its intent.
     */
    private suspend fun batchTimelineEdl(args: JSONObject): RumiToolOutcome {
        val ops = args.optJSONArray("ops")
            ?: return fail("ops is required and must be an array of operations")
        if (ops.length() == 0) return fail("ops is empty; there is nothing to apply")
        val projectJson = state.toJson()
        val replyJson = withContext(Dispatchers.IO) {
            RumoBridge.applyEdl(projectJson, ops.toString())
        } ?: return fail("the engine has no batch editing in this build")
        val reply = runCatching { JSONObject(replyJson) }.getOrNull()
            ?: return fail("the engine sent an unreadable answer")
        if (!reply.optBoolean("ok", false)) {
            return fail(reply.optTextOr("error", "the batch could not be applied"))
        }
        val project = reply.optJSONObject("project")
            ?: return fail("the engine applied the batch but returned no document")
        val adopted = state.applyProjectDocument(project.toString())
        if (!adopted) {
            return fail("the engine's document could not be read back into the editor")
        }
        val applied = reply.optJSONArray("applied") ?: JSONArray()
        val skipped = reply.optJSONArray("skipped") ?: JSONArray()
        val out = JSONObject()
            .put("ok", true)
            .put("requested", ops.length())
            .put("appliedCount", applied.length())
            .put("skippedCount", skipped.length())
            .put("applied", applied)
            .put("skipped", skipped)
            .put("timelineDurationMs", timelineExtentMs())
        if (skipped.length() > 0) {
            out.put(
                "note",
                "Some operations did not apply; they are listed in `skipped` with the " +
                    "reason and the timeline does not contain them. Read them before the " +
                    "next call.",
            )
        }
        return RumiToolOutcome(text = out.toString())
    }

    /** How far the timeline reaches, in ms: the bound every edit has to fit inside. */
    private fun timelineExtentMs(): Long {
        var extent = 0L
        for (layer in state.layers.value) {
            val end = layer.startMs + layer.durationMs
            if (end > extent) extent = end
        }
        return extent
    }

    private suspend fun analyzeAudioStream(args: JSONObject): RumiToolOutcome {
        val source = args.optTextOr("source", "")
        if (source.isEmpty()) return fail("source is required")
        val asset = resolveReference(state.projectName.value, source)
            ?: return fail("`$source` is not in the project's folder and is not a uri I can read")
        // The whole file has to reach the decoder, so this uses the same generous
        // cap as beat analysis rather than the picture-sized one.
        val bytes = withContext(Dispatchers.IO) {
            runCatching { readUriBytes(host.context, asset.uri, AUDIO_BYTE_LIMIT) }.getOrNull()
        } ?: return fail("`${asset.name}` could not be read (or is larger than the audio limit)")
        val json = withContext(Dispatchers.IO) { RumoBridge.audioAnalyzeSpeech(bytes) }
            ?: return fail("the engine has no speech analysis in this build")
        val reply = runCatching { JSONObject(json) }.getOrNull()
            ?: return fail("the engine sent an unreadable answer")
        if (!reply.optBoolean("ok", false)) {
            return fail(reply.optTextOr("error", "the audio could not be analysed"))
        }

        val segments = reply.optJSONArray("segments") ?: JSONArray()
        val silences = reply.optJSONArray("silences") ?: JSONArray()
        val cuts = reply.optJSONArray("cutPoints") ?: JSONArray()

        // `minSilenceMs` is honoured by filtering here rather than by re-running
        // the detector: the engine reports every gap, so asking for longer pauses
        // is a selection over what it already found, not a second pass.
        val minSilence = args.optDouble("minSilenceMs", 0.0).toLong()
        val kept = JSONArray()
        for (i in 0 until silences.length()) {
            val gap = silences.optJSONObject(i) ?: continue
            val from = gap.optLong("startMs", 0L)
            val to = gap.optLong("endMs", 0L)
            if (minSilence > 0L && to - from < minSilence) continue
            kept.put(JSONObject().put("fromMs", from).put("toMs", to).put("ms", to - from))
        }

        val cutLimit = args.optDouble("maxCuts", DEFAULT_MAX_CUTS.toDouble()).toInt()
            .coerceIn(1, 2048)
        val shownCuts = JSONArray()
        for (i in 0 until minOf(cutLimit, cuts.length())) {
            shownCuts.put(cuts.optLong(i, 0L))
        }

        val out = JSONObject()
            .put("ok", true)
            .put("source", asset.name)
            .put("durationMs", reply.optLong("durationMs", 0L))
            .put("speechRatio", reply.optDouble("speechRatio", 0.0))
            .put("onsetThreshold", reply.optDouble("onsetThreshold", 0.0))
            .put("offsetThreshold", reply.optDouble("offsetThreshold", 0.0))
            .put("speechCount", segments.length())
            .put("speech", segments)
            .put("silenceCount", kept.length())
            .put("silences", kept)
            .put("cutCount", cuts.length())
            .put("cutPoints", shownCuts)
            .put("transcript", JSONObject.NULL)
            .put(
                "transcriptNote",
                "No speech recogniser is linked in this build, so there are no words and " +
                    "no per-word times. Cut and place using the pauses above; do not write " +
                    "subtitles from this answer.",
            )
        return RumiToolOutcome(text = out.toString())
    }

    private suspend fun analyze(args: JSONObject): RumiToolOutcome {
        if (args.optTextOr("action", "") != "beats") return fail("action must be beats")
        val source = args.optTextOr("source", "")
        if (source.isEmpty()) return fail("source is required")
        val asset = resolveReference(state.projectName.value, source)
            ?: return fail("`$source` is not in the project's folder and is not a uri I can read")
        // The whole file has to reach the decoder, and audio files are not small:
        // the cap is the one place a long track could be cut short, so it is far
        // above the read limit used for pictures.
        val bytes = withContext(Dispatchers.IO) {
            runCatching { readUriBytes(host.context, asset.uri, AUDIO_BYTE_LIMIT) }.getOrNull()
        } ?: return fail("`${asset.name}` could not be read (or is larger than the audio limit)")
        val json = withContext(Dispatchers.IO) { RumoBridge.audioAnalyzeBeats(bytes) }
            ?: return fail("the engine has no beat analysis in this build")
        val reply = runCatching { JSONObject(json) }.getOrNull()
            ?: return fail("the engine sent an unreadable answer")
        if (!reply.optBoolean("ok", false)) {
            return fail(reply.optTextOr("error", "the audio could not be analysed"))
        }
        val all = reply.optJSONArray("beats") ?: JSONArray()
        val limit = args.optDouble("maxBeats", DEFAULT_MAX_BEATS.toDouble()).toInt()
            .coerceIn(1, 512)
        val shown = JSONArray()
        for (i in 0 until minOf(limit, all.length())) {
            val beat = all.optJSONObject(i) ?: continue
            shown.put(
                JSONObject()
                    .put("t", beat.optLong("t", 0L))
                    .put("strength", beat.optDouble("strength", 0.0)),
            )
        }
        val times = (0 until all.length()).mapNotNull { all.optJSONObject(it)?.optLong("t", -1L) }
            .filter { it >= 0L }
        val meanInterval = if (times.size >= 2) {
            (times.last() - times.first()).toDouble() / (times.size - 1)
        } else {
            null
        }
        val message = JSONObject()
            .put("ok", true)
            .put("source", asset.name)
            .put("bpm", reply.opt("bpm") ?: JSONObject.NULL)
            .put("confidence", reply.optTextOr("confidence", "uncertain"))
            .put("durationMs", reply.optLong("durationMs", 0L))
            .put("beatCount", times.size)
            .put("beats", shown)
            .put("beatsShown", shown.length())
        meanInterval?.let { message.put("meanIntervalMs", it) }
        message.put(
            "note",
            "Beat times are milliseconds into the track. meanIntervalMs is the average spacing " +
                "of all beats; place cuts on the strong ones rather than on every beat.",
        )
        return RumiToolOutcome(
            message.toString(),
            summary = "${times.size} beats, ${reply.opt("bpm") ?: "?"} bpm",
        )
    }

    // --- Persisting ---

    /**
     * Save the project, or start a new one.
     *
     * Until this runs, everything the assistant did exists only in memory: the
     * editor route with no file starts a *new* project, so an unsaved one is not
     * merely missing from the lists — it is unreachable. Saving is what makes it
     * a file the user can open.
     */
    private suspend fun project(args: JSONObject): RumiToolOutcome {
        val action = args.optText("action").orEmpty()
        val name = args.optText("name").orEmpty().trim()
        when (action) {
            "save" -> {
                if (name.isNotEmpty() && state.currentFileName.value == null) {
                    state.setProjectName(name)
                }
                val actual = saveCurrent()
                    ?: return fail("the project could not be written, so nothing was saved")
                return RumiToolOutcome(
                    JSONObject()
                        .put("ok", true)
                        .put("file", actual)
                        .put("name", state.projectName.value)
                        .put(
                            "folder",
                            folderLabel(state.projectName.value),
                        )
                        .put("note", "saved to the project folder; it is listed on Home and in " +
                            "the Projects tab, and opens from either. Material for it goes in the " +
                            "folder above.")
                        .toString(),
                    summary = actual,
                )
            }
            "canvas" -> {
                val width = args.optDouble("width", Double.NaN)
                val height = args.optDouble("height", Double.NaN)
                val background = args.optText("background")
                if (width.isNaN() && height.isNaN() && background == null) {
                    return fail("give width, height, background, or all three")
                }
                val w = if (width.isNaN()) state.canvasWidth.value else width.toInt()
                val h = if (height.isNaN()) state.canvasHeight.value else height.toInt()
                // An incomprehensible colour is a refusal, not a silent substitution
                // of the previous one: formerly "transparent" and any typo simply did
                // nothing, and that looked like "the background will not budge".
                val argb = if (background == null) {
                    state.backgroundArgb.value
                } else {
                    parseArgb(background) ?: return fail(
                        "`$background` is not a colour I can read. Use #RRGGBB, #AARRGGBB " +
                            "(eight digits, alpha first), a decimal colour, or the word " +
                            "`transparent`.",
                    )
                }
                state.applyCanvas(w, h, argb)
                return RumiToolOutcome(
                    JSONObject()
                        .put("ok", true)
                        .put("canvasWidth", state.canvasWidth.value)
                        .put("canvasHeight", state.canvasHeight.value)
                        .put("backgroundArgb", "#%08X".format(state.backgroundArgb.value.toInt()))
                        .put(
                            "note",
                            "the frame is now ${state.canvasWidth.value}x" +
                                "${state.canvasHeight.value}; layer offsets are in canvas pixels, " +
                                "so check anything that was placed against the old size",
                        )
                        .toString(),
                    summary = "${state.canvasWidth.value}x${state.canvasHeight.value}",
                )
            }
            "new" -> {
                // Starting a project throws away what is in memory, so the open
                // project is written out first whenever it has anything to lose.
                // Refusing instead — the first cut of this — made the action
                // useless in practice: by the time a user asks for a new project
                // the assistant has usually already touched the open one, so
                // "unsaved" was true in every case that mattered. Losing work is
                // worse than creating a file nobody asked for, and the answer
                // says which file that was.
                var rescued: String? = null
                if (state.hasEdits.value) {
                    rescued = saveCurrent() ?: return fail(
                        "the project that is open could not be saved, and starting a new one " +
                            "would lose it",
                    )
                }
                val width = args.optDouble("width", state.canvasWidth.value.toDouble()).toInt()
                val height = args.optDouble("height", state.canvasHeight.value.toDouble()).toInt()
                val wanted = args.optText("background")
                val background = if (wanted == null) {
                    state.backgroundArgb.value
                } else {
                    parseArgb(wanted) ?: return fail(
                        "`$wanted` is not a colour I can read. Use #RRGGBB, #AARRGGBB, a " +
                            "decimal colour, or the word `transparent`.",
                    )
                }
                state.newProject(
                    name.ifEmpty { "New Project 1" },
                    width = width,
                    height = height,
                    background = background,
                )
                state.setCurrentFileName(null)
                // The folder comes into existence with the project, not with the
                // first import: "where do I drop the references" needs an answer
                // before the user goes looking for one.
                val folder = ensureReferenceFolder()
                val layers = state.layers.value
                return RumiToolOutcome(
                    JSONObject()
                        .put("ok", true)
                        .put("project", state.projectName.value)
                        .put("layers", layers.size)
                        .put("layerIds", JSONArray(layers.map { it.id }))
                        .put(
                            "folder",
                            folderLabel(state.projectName.value),
                        )
                        .apply { rescued?.let { put("previousSavedTo", it) } }
                        .apply {
                            if (folder == null) {
                                put(
                                    "folderWarning",
                                    "the reference folder could not be created; references can " +
                                        "still be imported through the picker",
                                )
                            }
                        }
                        .put(
                            "note",
                            (if (rescued != null) {
                                "the project that was open is saved as `$rescued` before " +
                                    "starting this one, so nothing was lost"
                            } else {
                                "started from the app's default layers; call project(action=save) " +
                                    "when the work is done so the user can open it"
                            }) +
                                ". The user can put pictures, video or sound into the folder above " +
                                "and call media(action=list) to see them.",
                        )
                        .toString(),
                    summary = state.projectName.value,
                )
            }
            "open" -> {
                val wanted = args.optText("file").orEmpty().trim()
                    .ifEmpty { args.optText("name").orEmpty().trim() }
                val saved = withContext(Dispatchers.IO) { ProjectStore.list(host.context) }

                // No name is not an error but the question "what can be opened at
                // all": the assistant does not see the project list from anywhere
                // else, and a refusal here would force it to guess names.
                if (wanted.isEmpty()) {
                    val files = JSONArray()
                    saved.forEach { entry ->
                        // The folder is deliberately not here: it is derived from
                        // the name inside the document, not from the file name, and
                        // for a project renamed before this build they diverge.
                        // Naming a wrong folder is worse than naming none — it is
                        // reported by `open` anyway once the project is read.
                        files.put(
                            JSONObject()
                                .put("file", entry.fileName)
                                .put("name", entry.displayName),
                        )
                    }
                    return RumiToolOutcome(
                        JSONObject()
                            .put("ok", true)
                            .put("open", state.projectName.value)
                            .put("projects", files)
                            .put(
                                "note",
                                "Call again with `file` set to one of these to switch. The " +
                                    "answer then names the folder that project's references " +
                                    "live in, and that folder is what `media(action=list)` " +
                                    "reads from then on.",
                            )
                            .toString(),
                        summary = "${saved.size} saved project(s)",
                    )
                }

                // We accept both the file name and the display name: the model sees
                // the latter in the list, while `file` in the response is the former,
                // and demanding it guess which one it brought would guarantee a miss
                // out of nowhere.
                val leaf = wanted.substringAfterLast('/').substringAfterLast('\\')
                val entry = saved.firstOrNull { it.fileName.equals(leaf, ignoreCase = true) }
                    ?: saved.firstOrNull {
                        it.fileName.equals(leaf + ProjectStore.EXT, ignoreCase = true)
                    }
                    ?: saved.firstOrNull { it.displayName.equals(leaf, ignoreCase = true) }
                    ?: return fail(
                        "there is no saved project called `$wanted`. Saved: " +
                            saved.joinToString(", ") { it.displayName }.ifEmpty { "(none)" } +
                            " — call project(action=open) with no `file` for the full list.",
                    )

                // The current project is saved before the swap — for the same reason
                // as in `new`: by the time of a switch request the assistant has
                // usually already done something in it, and "unsaved" is true almost
                // always.
                var rescued: String? = null
                if (state.hasEdits.value) {
                    rescued = saveCurrent() ?: return fail(
                        "the project that is open could not be saved, and opening another " +
                            "would lose it",
                    )
                }

                val bytes = withContext(Dispatchers.IO) {
                    ProjectStore.load(host.context, entry.fileName)
                } ?: return fail("`${entry.fileName}` could not be read")
                val json = RumoBridge.projectToJson(bytes)
                    ?: return fail("`${entry.fileName}` is not a project this build can read")
                if (!state.loadFromJson(json)) {
                    return fail("`${entry.fileName}` opened but its contents could not be applied")
                }
                state.setCurrentFileName(entry.fileName)
                state.markSaved()
                // The references folder is the same as the open project's: the name
                // comes from the document itself, not from the requested string, so
                // the response says where `media` will look from now on.
                val folder = folderLabel(state.projectName.value)
                return RumiToolOutcome(
                    JSONObject()
                        .put("ok", true)
                        .put("project", state.projectName.value)
                        .put("file", entry.fileName)
                        .put("layers", state.layers.value.size)
                        .put("folder", folder)
                        .apply { rescued?.let { put("previousSavedTo", it) } }
                        .put(
                            "note",
                            "This is now the open project. Its references are the files in " +
                                "`$folder` — call media(action=list) to see them, and do not " +
                                "assume anything from the project that was open before.",
                        )
                        .toString(),
                    summary = entry.displayName,
                )
            }
            else -> return fail("action must be save, new, open or canvas")
        }
    }

    /**
     * Write the open project through the same store the editor's save button
     * uses. Returns the file name, or null when it could not be written.
     */
    private suspend fun saveCurrent(): String? {
        val bytes = RumoBridge.projectFromJson(state.toJson()) ?: return null
        val target = state.currentFileName.value
            ?: ProjectStore.fileNameFor(state.projectName.value)
        val actual = runCatching { ProjectStore.save(host.context, target, bytes) }.getOrNull()
            ?: return null
        state.setCurrentFileName(actual)
        state.markSaved()
        // A project that exists has a folder to put material in. The editor makes
        // one too, but the assistant can save without the editor ever being shown.
        ensureReferenceFolder()
        return actual
    }

    /**
     * The project's reference folder, made to exist. Null when it could not be
     * created — a warning, never a failure: the project is saved either way, and
     * the folder only says where its material goes.
     */
    private suspend fun ensureReferenceFolder(): String? {
        val name = state.projectName.value
        return when (val result = ProjectAssets.ensureFolder(host.context, name)) {
            is SaveResult.Ok -> result.path
            is SaveResult.Failed -> {
                AppLog.warn("assets", "folder for `$name` not created: ${result.reason}")
                null
            }
        }
    }

    // --- Seeing the result ---

    /**
     * Bring the frame's resources into existence before measuring anything.
     *
     * A frame is built only from layers whose resource is already there: a text
     * layer needs its layout handle, a picture needs its staged texture. Both are
     * created asynchronously by the editor screen, and the Rumi tab does not
     * compose the editor — so without this step a frame silently loses its text
     * or picture layer, and two snapshots taken one after the other differ
     * because the first ran before the resource appeared. Measuring that frame
     * is meaningless, and it is exactly what "the effect changes nothing" looks
     * like from the outside.
     */
    private suspend fun prepareFrame() {
        val layers = state.layers.value
        state.ensureMeshes(
            layers.filter { it.visible && it.kind == LayerKindUi.SHAPE }.map { it.name },
        )
        for (layer in layers) {
            if (!layer.visible) continue
            when (layer.kind) {
                LayerKindUi.TEXT -> state.prewarmLayout(layer)
                LayerKindUi.MEDIA -> {
                    val uri = layer.uri ?: continue
                    if (state.hasTexture(uri)) continue
                    val bytes = readUriBytes(host.context, Uri.parse(uri)) ?: continue
                    val decoded = RumoBridge.decodeImage(bytes) ?: continue
                    state.stageTexture(uri, decoded)
                }
                else -> Unit
            }
        }
    }

    /**
     * One frame plus a verdict on whether it can be trusted.
     *
     * The same frame is rendered twice: if two renders of the same state
     * disagree, the numbers are noise rather than a measurement — the engine can
     * fall back to the CPU path when the worker is busy, and a cold first render
     * is the most likely moment for that. Saying so is the difference between a
     * model drawing conclusions from a bad frame and a model knowing it cannot.
     */
    private fun measure(
        t: Long,
        width: Int,
        height: Int,
        layerId: String,
        effects: List<Effect>?,
    ): Measured? {
        val first = state.effectProbeAt(t, width, height, layerId, effects) ?: return null
        val second = state.effectProbeAt(t, width, height, layerId, effects) ?: return null
        return Measured(first, diffStats(first, second))
    }

    /** A rendered frame and how far a second render of it drifted. */
    private class Measured(val px: IntArray, val drift: JSONObject) {
        /** Mean absolute channel difference between the two renders, 0..255. */
        val driftValue: Double get() = drift.optDouble("meanDiff", 0.0)

        val stable: Boolean get() = driftValue <= STABLE_EPSILON
    }

    /**
     * What the engine says about itself for this frame: which path drew it, and
     * which effects it refused to build.
     *
     * Without this, "the effect did nothing" cannot be told apart from "the
     * driver rejected the shader" or "the CPU path is drawing this frame" — three
     * different causes with three different fixes, and the first is a lie in two
     * of them.
     */
    private fun engineReport(kinds: List<String>): JSONObject {
        val diagnostics = RumoBridge.renderDiagnostics()
        val rejected = diagnostics?.rejectedEffects.orEmpty()
        val warnings = mutableListOf<String>()
        if (diagnostics != null && !diagnostics.previewOk) {
            warnings += "This frame was drawn on the CPU path (${diagnostics.path}), not the GPU: " +
                "project-defined effects are skipped there by design, and built-in results can " +
                "differ slightly from the GPU ones."
        }
        kinds.forEach { kind ->
            if (rejected.none { it == kind }) return@forEach
            val reason = diagnostics?.entries
                ?.lastOrNull { it.code == "effect_pipeline_rejected" && it.text.contains("`$kind`") }
                ?.text
            warnings += if (reason != null) {
                "The engine refused `$kind`: $reason"
            } else {
                "The engine refused `$kind` and draws the layer without it."
            }
        }
        // `hint` is the engine's own one-line reason for being where it is
        // ("why the preview is on CPU instead of GPU"), and it is the only field
        // that can turn "the frame came from the CPU path" into something
        // actionable.
        if (diagnostics != null && diagnostics.hint.isNotEmpty()) {
            warnings += "The engine says: ${diagnostics.hint}"
        }
        return JSONObject()
            .put("path", diagnostics?.path ?: "unknown")
            .put("adapter", diagnostics?.adapter ?: "")
            .put("backend", diagnostics?.backend ?: "")
            .put("previewOnGpu", diagnostics?.previewOk ?: false)
            .put("surfaceOnGpu", diagnostics?.engineOk ?: false)
            .put("hint", diagnostics?.hint ?: "")
            .put("rejected", JSONArray(rejected))
            .put("warnings", JSONArray(warnings))
    }

    /**
     * Which layers actually reached the frame that was just rendered.
     *
     * A layer drops out of the frame — and takes its effect chain with it, since
     * the chain is addressed by draw index — when it is hidden, when its text
     * has no layout handle yet, when its picture has no staged texture, or when
     * a shape name resolves to no geometry. Every one of those is silent, and a
     * chain attached to a layer that is not drawn does nothing at all, which is
     * indistinguishable from "the effect is broken" unless the frame's contents
     * are reported.
     */
    private fun frameContents(): JSONArray {
        val out = JSONArray()
        state.layers.value.forEach { layer ->
            val skipped = when {
                !layer.visible -> "hidden"
                layer.kind == LayerKindUi.SHAPE ->
                    if (shapeOrdinalOf(layer.name) < 0) "shape has no geometry" else null
                layer.kind == LayerKindUi.TEXT ->
                    if (!state.hasLayout(layer.id)) "no text layout yet" else null
                layer.kind == LayerKindUi.MEDIA -> {
                    val uri = layer.uri
                    when {
                        uri == null -> "no media attached"
                        !state.hasTexture(uri) ->
                            "no texture yet (picture not staged, or a video frame is not ready)"
                        else -> null
                    }
                }
                else -> "not drawn by the frame builder"
            }
            out.put(
                JSONObject()
                    .put("id", layer.id)
                    .put("name", layer.name)
                    .apply { skipped?.let { put("skipped", it) } },
            )
        }
        return out
    }

    private fun stabilityWarning(measured: Measured): JSONArray {
        val warnings = JSONArray()
        if (!measured.stable) {
            warnings.put(
                "Two renders of the same frame differ by ${"%.3f".format(measured.driftValue)} " +
                    "(mean per channel, 0..255), so this frame is not reproducible and the " +
                    "numbers below are not a measurement. Render again before concluding " +
                    "anything about an effect.",
            )
        }
        return warnings
    }

    private suspend fun snapshot(args: JSONObject): RumiToolOutcome {
        val t = args.optDouble("timeMs", state.playheadMs.value.toDouble()).toLong()
        val width = args.optDouble("width", SNAPSHOT_W.toDouble()).toInt().coerceIn(64, 1920)
        val height = (width.toFloat() * canvasAspect()).toInt().coerceAtLeast(1)
        prepareFrame()
        val measured = measure(t, width, height, "", null)
            ?: return fail("could not render a frame at ${t}ms")
        val bitmap = toBitmap(measured.px, width, height)
            ?: return fail("the frame was not the right size")
        val png = encodePng(bitmap) ?: return fail("could not encode the frame as PNG")
        val name = "rumi-frame-${t}ms-${System.currentTimeMillis()}.png"
        val path = save(png, name)
        val engine = engineReport(state.layers.value.flatMap { l -> l.effects.map { it.kindId } })
        val warnings = stabilityWarning(measured)
        engine.optJSONArray("warnings")?.let { extra ->
            for (i in 0 until extra.length()) warnings.put(extra.optTextOr(i, ""))
        }
        return RumiToolOutcome(
            JSONObject()
                .put("ok", true)
                .put("project", state.projectName.value)
                .put("projectFile", state.currentFileName.value ?: JSONObject.NULL)
                .put("frame", t)
                .put("width", width)
                .put("height", height)
                .put("file", path ?: "not saved")
                .put("reproducible", measured.stable)
                .put("mean", meanColour(measured.px))
                .put("opaque", opaqueFraction(measured.px))
                .put("lumaGrid", lumaGrid(measured.px, width, height))
                .put("warnings", warnings)
                .put("engine", engine)
                .put("layers", frameContents())
                .put("note", "lumaGrid is a 4x4 grid of mean luma (0..1) across the frame; " +
                    "`layers` lists what was in the frame and why anything was left out.")

                .toString(),
            png = png,
            path = path,
            summary = "$width×$height @ ${t}ms",
        )
    }

    private suspend fun filmstrip(args: JSONObject): RumiToolOutcome {
        val duration = state.projectDurationMs.value
        val from = args.optDouble("fromMs", 0.0).toLong().coerceIn(0L, duration)
        val to = args.optDouble("toMs", duration.toDouble()).toLong().coerceIn(from, duration)
        val frames = args.optDouble("frames", 4.0).toInt().coerceIn(2, 12)
        val width = args.optDouble("width", 320.0).toInt().coerceIn(96, 960)
        val height = (width.toFloat() * canvasAspect()).toInt().coerceAtLeast(1)

        val times = ArrayList<Long>(frames)
        for (i in 0 until frames) {
            // The last frame is the end of the range, so the sheet covers it.
            times += if (frames == 1) from else from + (to - from) * i / (frames - 1)
        }
        prepareFrame()
        val shots = ArrayList<Bitmap>(frames)
        val stats = JSONArray()
        var reproducible = true
        times.forEach { t ->
            val measured = measure(t, width, height, "", null)
            val bmp = measured?.let { toBitmap(it.px, width, height) }
            if (measured == null || bmp == null) {
                stats.put(JSONObject().put("t", t).put("ok", false))
            } else {
                if (!measured.stable) reproducible = false
                shots += bmp
                stats.put(
                    JSONObject()
                        .put("t", t)
                        .put("ok", true)
                        .put("mean", meanColour(measured.px))
                        .put("opaque", opaqueFraction(measured.px)),
                )
            }
        }
        if (shots.isEmpty()) return fail("no frame could be rendered in $from..${to}ms")

        val sheet = contactSheet(shots, width, height) ?: return fail("could not build the sheet")
        val png = encodePng(sheet) ?: return fail("could not encode the sheet")
        val path = save(png, "rumi-strip-${from}-${to}ms-${System.currentTimeMillis()}.png")

        var clip: String? = null
        if (args.optBoolean("clip", false) && to > from) {
            clip = writeClip(from, to, width, args.optDouble("clipFps", 12.0).toInt().coerceIn(6, 30))
        }

        val engine = engineReport(state.layers.value.flatMap { l -> l.effects.map { it.kindId } })
        val warnings = JSONArray()
        if (!reproducible) {
            warnings.put(
                "At least one frame in this range is not reproducible (two renders of it " +
                    "differ), so identical-looking frames are not evidence that nothing changed.",
            )
        }
        engine.optJSONArray("warnings")?.let { extra ->
            for (i in 0 until extra.length()) warnings.put(extra.optTextOr(i, ""))
        }
        val message = JSONObject()
            .put("ok", true)
            .put("project", state.projectName.value)
            .put("projectFile", state.currentFileName.value ?: JSONObject.NULL)
            .put("from", from)
            .put("to", to)
            .put("frames", shots.size)
            .put("sheet", path ?: "not saved")
            .put("reproducible", reproducible)
            .put("perFrame", stats)
            .put("warnings", warnings)
            .put("engine", engine)
            .put("layers", frameContents())
        clip?.let { message.put("clip", it) }
        return RumiToolOutcome(
            message.toString(),
            png = png,
            path = path,
            summary = "${shots.size} frames ${from}..${to}ms",
        )
    }

    /** A short MP4 of the range, written to Download/Rumo. Blocking. */
    private suspend fun writeClip(from: Long, to: Long, width: Int, fps: Int): String? {
        // Encoders want even dimensions; the scene aspect is 16:9, so rounding
        // the height to even keeps the frame from being resampled.
        val height = ((width.toFloat() * canvasAspect()).toInt() / 2) * 2
        val tmp = java.io.File(host.context.cacheDir, "rumi-clip-${System.currentTimeMillis()}.mp4")
        val bitrate = RumoBridge.bitrateFor(width, height, fps)
        // The exporter walks a timeline that starts at zero, so the range is
        // remapped by adding `from` inside the frame callback rather than
        // teaching the exporter about an offset it has no other use for.
        val ok = Exporter.exportMp4(
            outPath = tmp.absolutePath,
            width = width,
            height = height,
            fps = fps.toFloat(),
            bitrate = bitrate,
            durationMs = to - from,
            frameAt = { t -> state.previewFrameExAt(from + t, width, height) },
            onProgress = {},
        )
        if (!ok) {
            runCatching { tmp.delete() }
            AppLog.error(TAG, "clip export failed: ${Exporter.lastError}")
            return null
        }
        val name = "rumi-clip-${from}-${to}ms.mp4"
        val result = saveBytesToDownloads(host.context, DownloadKind.VIDEO, tmp.readBytes(), name)
        runCatching { tmp.delete() }
        return when (result) {
            is SaveResult.Ok -> result.path
            is SaveResult.Failed -> {
                AppLog.error(TAG, "clip save failed: ${result.reason}")
                null
            }
        }
    }

    private suspend fun save(png: ByteArray, name: String): String? =
        when (val res = saveBytesToDownloads(host.context, DownloadKind.IMAGE, png, name)) {
            is SaveResult.Ok -> res.path
            is SaveResult.Failed -> {
                AppLog.error(TAG, "snapshot save failed: ${res.reason}")
                null
            }
        }

    // --- Small helpers ---

    private fun layerEffects(layerId: String): List<Effect> =
        state.layers.value.firstOrNull { it.id == layerId }?.effects ?: emptyList()

    /** The chain with one more effect appended, or null when the kind is unknown. */
    private fun variant(base: List<Effect>, kind: String, params: List<Float>?): List<Effect>? {
        val fresh = defaultEffectFor(kind, state.customsJson()) ?: return null
        return base + (if (params == null) fresh else fresh.copy(params = params))
    }

    /** A small frame is enough for a diff and keeps a probe cheap. */
    private fun probeSize(): Pair<Int, Int> = PROBE_W to (PROBE_W * canvasAspect()).toInt()

    /** Height per unit of width for the project's canvas. */
    private fun canvasAspect(): Float =
        state.canvasHeight.value.toFloat() / state.canvasWidth.value.coerceAtLeast(1).toFloat()

    private fun toBitmap(px: IntArray, width: Int, height: Int): Bitmap? {
        if (px.size != width * height) return null
        return try {
            Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888).also {
                it.copyPixelsFromBuffer(IntBuffer.wrap(px))
            }
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * The engine's RGBA8 (straight alpha) -> Bitmap (0xAARRGGBB).
     *
     * An explicit assembly, not [toBitmap] with `copyPixelsFromBuffer`: the latter
     * copies the bytes as is, and the RGBA stream would land in an ARGB buffer with
     * R and B swapped. The picture's channels must match what the engine has already
     * drawn and saved.
     */
    private fun rgbaToBitmap(decoded: RumoBridge.DecodedImage): Bitmap? {
        val w = decoded.width
        val h = decoded.height
        if (w <= 0 || h <= 0 || decoded.rgba.size != w * h * 4) return null
        val px = IntArray(w * h)
        var src = 0
        for (i in px.indices) {
            val r = decoded.rgba[src].toInt() and 0xFF
            val g = decoded.rgba[src + 1].toInt() and 0xFF
            val b = decoded.rgba[src + 2].toInt() and 0xFF
            val a = decoded.rgba[src + 3].toInt() and 0xFF
            px[i] = (a shl 24) or (r shl 16) or (g shl 8) or b
            src += 4
        }
        return toBitmap(px, w, h)
    }

    /**
     * Puts the picture on an opaque white background.
     *
     * The engine's raster arrives with a transparent background. For a PNG the model
     * looks at, that means "black text on whatever background the viewer provides",
     * which may be dark too — then the preview looks empty. The white backing
     * removes the question.
     */
    private fun flattenOnWhite(bitmap: Bitmap): Bitmap {
        val out = Bitmap.createBitmap(bitmap.width, bitmap.height, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(out)
        canvas.drawColor(0xFFFFFFFF.toInt())
        canvas.drawBitmap(bitmap, 0f, 0f, null)
        return out
    }

    /**
     * Shrink the picture to a limit on the long side.
     *
     * Needed where the size is chosen not by the engine but by the file itself: for
     * video the frame arrives at the clip's size, and a limit named by the caller
     * would otherwise remain only a promise in the schema.
     */
    private fun fitWithin(bitmap: Bitmap, maxSide: Int): Bitmap {
        val longest = maxOf(bitmap.width, bitmap.height)
        if (longest <= maxSide) return bitmap
        val scale = maxSide.toFloat() / longest
        val w = (bitmap.width * scale).toInt().coerceAtLeast(1)
        val h = (bitmap.height * scale).toInt().coerceAtLeast(1)
        return Bitmap.createScaledBitmap(bitmap, w, h, true)
    }

    private fun encodePng(bitmap: Bitmap): ByteArray? = try {
        ByteArrayOutputStream().use { out ->
            if (bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)) out.toByteArray() else null
        }
    } catch (_: Throwable) {
        null
    }

    /** Lay the frames out in as square a grid as they allow, with thin gutters. */
    private fun contactSheet(shots: List<Bitmap>, cellW: Int, cellH: Int): Bitmap? = try {
        val cols = kotlin.math.ceil(kotlin.math.sqrt(shots.size.toDouble())).toInt().coerceAtLeast(1)
        val rows = (shots.size + cols - 1) / cols
        val gap = 4
        val sheet = Bitmap.createBitmap(
            cols * cellW + (cols + 1) * gap,
            rows * cellH + (rows + 1) * gap,
            Bitmap.Config.ARGB_8888,
        )
        val canvas = Canvas(sheet)
        canvas.drawColor(0xFF000000.toInt())
        val paint = Paint(Paint.FILTER_BITMAP_FLAG)
        shots.forEachIndexed { index, shot ->
            val col = index % cols
            val row = index / cols
            val left = gap + col * (cellW + gap)
            val top = gap + row * (cellH + gap)
            canvas.drawBitmap(shot, Rect(0, 0, shot.width, shot.height), Rect(left, top, left + cellW, top + cellH), paint)
        }
        sheet
    } catch (_: Throwable) {
        null
    }

    /** Mean absolute channel difference and the share of moved pixels. */
    private fun diffStats(a: IntArray, b: IntArray): JSONObject {
        if (a.size != b.size || a.isEmpty()) {
            return JSONObject().put("meanDiff", 0.0).put("changed", 0.0)
        }
        var sum = 0L
        var moved = 0L
        for (i in a.indices) {
            val pa = a[i]
            val pb = b[i]
            val dr = kotlin.math.abs(((pa shr 16) and 0xFF) - ((pb shr 16) and 0xFF))
            val dg = kotlin.math.abs(((pa shr 8) and 0xFF) - ((pb shr 8) and 0xFF))
            val db = kotlin.math.abs((pa and 0xFF) - (pb and 0xFF))
            val da = kotlin.math.abs(((pa ushr 24) and 0xFF) - ((pb ushr 24) and 0xFF))
            sum += dr + dg + db + da
            if (maxOf(dr, dg, db, da) > 8) moved++
        }
        return JSONObject()
            .put("meanDiff", sum.toDouble() / (a.size * 4.0))
            .put("changed", moved.toDouble() / a.size)
    }

    private fun meanColour(px: IntArray): JSONObject {
        if (px.isEmpty()) return JSONObject()
        var r = 0L
        var g = 0L
        var b = 0L
        px.forEach {
            r += (it shr 16) and 0xFF
            g += (it shr 8) and 0xFF
            b += it and 0xFF
        }
        return JSONObject()
            .put("r", r.toDouble() / px.size)
            .put("g", g.toDouble() / px.size)
            .put("b", b.toDouble() / px.size)
    }

    private fun opaqueFraction(px: IntArray): Double {
        if (px.isEmpty()) return 0.0
        var n = 0
        px.forEach { if (((it ushr 24) and 0xFF) > 8) n++ }
        return n.toDouble() / px.size
    }

    /** A 4x4 grid of mean luma, so a text-only model still learns the layout. */
    private fun lumaGrid(px: IntArray, width: Int, height: Int): JSONArray {
        val out = JSONArray()
        if (px.isEmpty() || width <= 0 || height <= 0) return out
        for (row in 0 until 4) {
            for (col in 0 until 4) {
                val x0 = col * width / 4
                val x1 = ((col + 1) * width / 4).coerceAtLeast(x0 + 1)
                val y0 = row * height / 4
                val y1 = ((row + 1) * height / 4).coerceAtLeast(y0 + 1)
                var sum = 0L
                var n = 0
                for (y in y0 until minOf(y1, height)) {
                    for (x in x0 until minOf(x1, width)) {
                        val p = px[y * width + x]
                        val r = (p shr 16) and 0xFF
                        val g = (p shr 8) and 0xFF
                        val b = p and 0xFF
                        sum += (r * 299 + g * 587 + b * 114) / 1000
                        n++
                    }
                }
                out.put(if (n == 0) 0.0 else sum.toDouble() / n / 255.0)
            }
        }
        return out
    }

    /**
     * A colour from what the user or the model wrote.
     *
     * The words `transparent`, `none` and `clear` are not decoration but a necessary
     * case: "make the background transparent" is the most frequent request about the
     * background, and the model almost always writes exactly a word rather than
     * eight zeros. Without them `parseArgb` returned `null`, and the caller
     * **silently** kept the previous colour — the request "make it transparent"
     * looked like "nothing happened", and the model went off to build workarounds.
     * Now it reads as full transparency.
     *
     * `null` remains only for a genuinely incomprehensible string, and the caller
     * must refuse on it: a silent substitution of the value is worse than a refusal.
     */
    private fun parseArgb(raw: String): Long? {
        when (raw.trim().lowercase()) {
            "transparent", "none", "clear" -> return 0L
        }
        val text = raw.trim().removePrefix("#")
        val value = text.toLongOrNull(16) ?: raw.trim().toLongOrNull() ?: return null
        return if (text.length <= 6) 0xFF000000L or (value and 0xFFFFFFL) else value and 0xFFFFFFFFL
    }

    /**
     * `weight` as a face weight, or null when the call does not set one.
     *
     * Both spellings are accepted on purpose: `"bold"` is what a model that
     * knows typography writes, `700` is what a model that knows numbers writes,
     * and the layer stores the number either way.
     */
    private fun textWeightOf(args: JSONObject): Int? {
        if (!args.has("weight") || args.isNull("weight")) return null
        val raw = args.optText("weight").orEmpty().trim()
        if (raw.isEmpty()) return null
        return namedWeight(raw) ?: raw.toIntOrNull()
    }

    private fun namedWeight(raw: String): Int? = when (raw.lowercase()) {
        "thin" -> 100
        "light" -> 300
        "regular", "normal", "book" -> 400
        "medium" -> 500
        "semibold", "demibold" -> 600
        "bold" -> 700
        "extrabold", "heavy" -> 800
        "black" -> 900
        else -> null
    }

    private fun doubles(array: JSONArray?): List<Float>? {
        if (array == null) return null
        val out = ArrayList<Float>(array.length())
        for (i in 0 until array.length()) out += array.optDouble(i, 0.0).toFloat()
        return out
    }

    private fun fail(reason: String, summary: String? = null): RumiToolOutcome =
        RumiToolOutcome(JSONObject().put("ok", false).put("error", reason).toString(), ok = false, summary = summary)

    private companion object {
        const val TAG = "rumi"
        /** Canvas the app starts a project with; the project itself may differ. */
        const val DEFAULT_CANVAS_W = 512
        const val DEFAULT_CANVAS_H = 288
        const val PROBE_W = 320
        const val SNAPSHOT_W = 640
        /** The long side of the picture the model gets in `media(action=preview)`. */
        const val PREVIEW_SIDE = 512
        /**
         * Below this mean per-channel difference two renders of the same state
         * count as the same picture. Well under one 8-bit step, so it separates
         * "identical" from "the frame changed".
         */
        const val STABLE_EPSILON = 0.01
        /** How many beat times to hand the model before summarising instead. */
        const val DEFAULT_MAX_BEATS = 64
        /** There are more pauses in a long recording than bits, so the ceiling is higher. */
        const val DEFAULT_MAX_CUTS = 256
        /** Audio files are read whole for analysis; this is the ceiling. */
        const val AUDIO_BYTE_LIMIT = 256 * 1024 * 1024
        /** SVG handed to the engine in one piece; generous for text art, bounded. */
        const val SVG_MAX_BYTES = 8 * 1024 * 1024
        /**
         * The face weight limit for a preview.
         *
         * Variable fonts are one file with all the axes, and they are several times
         * bigger than a static one: Inter is about 850 KiB, Google Sans Flex is
         * 4.15 MB. The limit is therefore not "reasonable for a preview" but "covers
         * the live families": a refusal on a real font is worse than downloading it
         * in full.
         */
        const val FONT_PREVIEW_LIMIT_BYTES = 16L * 1024 * 1024
    }
}

// --- JSON Schema builders ---
// The shared builders (`str`, `num`, `bool`, `oneOf`, `Prop`, `req`, `opt`, `obj`)
// now live in the module (`com.kerneldroid.aiengines.rumi`), because the
// generative tool schemas use the same ones and a second set here would diverge
// from them at the first format change. Only these array helpers are used by this
// file alone.

private fun numbers(description: String): JSONObject =
    JSONObject().put("type", "array").put("description", description)
        .put("items", JSONObject().put("type", "number"))

private fun strings(description: String): JSONObject =
    JSONObject().put("type", "array").put("description", description)
        .put("items", JSONObject().put("type", "string"))

private fun arrayOfObjects(description: String, item: JSONObject): JSONObject =
    JSONObject().put("type", "array").put("description", description).put("items", item)
