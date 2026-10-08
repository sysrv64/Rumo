// SPDX-License-Identifier: Apache-2.0
//! JSON ↔ `.rumo` bytes bridge for the Kotlin editor.
//!
//! Wire shape (also produced by `EditorState.toJson()`):
//! `{name, canvasWidth, canvasHeight, backgroundArgb, customEffects:[…],
//! layers:[{id,kind,name,visible,argb,durationMs,startMs,uri,
//! text,textWeight,strokePx,strokeArgb,offsetX,offsetY,scale,alpha,
//! transition:{startMs,durationMs,withPrevious,enabled}|null,
//! keys:[{t,v}], trackX:[{t,v}], trackY:[{t,v}], trackScale:[{t,v}],
//! trackAlpha:[{t,v}], effects:[{id,kind,enabled,params:[f32]}]}]}`.
//! The canvas keys are optional: absent means the built-in 512 × 288 frame with
//! the editor's background, so a file saved before they existed still opens.
//! So are `startMs` and the four property tracks, which means 0 ms and "not
//! animated" respectively — an absent `trackAlpha` is not a layer at alpha 0.
//! `kind` accepts `"SHAPE"` (also `"Shape"`/`"shape"`) or `0..=3`.
//! Effect `kind` is the stable [`crate::effect::EffectKind::id`] string, or the
//! id of an entry in `customEffects`; unknown effect ids are dropped rather than
//! failing the load, so a project written by a newer build still opens.
//! `customEffects` is `parse_chains_json`'s `custom` list in the same shape, and
//! is written back on encode so a project-defined effect survives a save/reopen.

use std::collections::HashSet;

use crate::codec;
use crate::effect::{CustomEffect, EffectInstance, EffectKind};
use crate::ease::Ease;
use crate::model::{
    Canvas, DEFAULT_LAYER_DURATION_MS, Keyframe, Layer, LayerExtra, LayerKind, Project, Transition,
};
use uuid::Uuid;

#[derive(Debug, serde::Deserialize, Default)]
struct ProjectDto {
    #[serde(default)]
    name: String,
    /// Design width in pixels. `i64` on purpose: a nonsense value out of `u16`
    /// range must degrade to the default, not fail the whole load.
    #[serde(default, rename = "canvasWidth")]
    canvas_width: Option<i64>,
    #[serde(default, rename = "canvasHeight")]
    canvas_height: Option<i64>,
    /// Background colour, `0xAARRGGBB`.
    #[serde(default, rename = "backgroundArgb")]
    background_argb: Option<i64>,
    /// Project-defined effects, addressed by id from an effect instance. Keyed
    /// on the wire as `customEffects`, the name `EditorState.toJson` writes.
    #[serde(default, rename = "customEffects")]
    custom_effects: Vec<CustomEffect>,
    #[serde(default)]
    layers: Vec<LayerDto>,
}

#[derive(Debug, serde::Deserialize, Default)]
struct LayerDto {
    #[serde(default)]
    id: Option<String>,
    #[serde(default = "default_shape_kind", deserialize_with = "de_kind")]
    kind: LayerKind,
    #[serde(default)]
    name: String,
    #[serde(default)]
    visible: Option<bool>,
    #[serde(default)]
    argb: Option<u32>,
    #[serde(default, rename = "durationMs")]
    duration_ms: Option<i64>,
    #[serde(default, rename = "startMs")]
    start_ms: Option<i64>,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default, rename = "offsetX")]
    offset_x: Option<f32>,
    #[serde(default, rename = "offsetY")]
    offset_y: Option<f32>,
    #[serde(default)]
    scale: Option<f32>,
    #[serde(default)]
    alpha: Option<f32>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "textWeight")]
    text_weight: Option<u16>,
    #[serde(default, rename = "strokePx")]
    stroke_px: Option<f32>,
    #[serde(default, rename = "strokeArgb")]
    stroke_argb: Option<u32>,
    #[serde(default)]
    transition: Option<TransitionDto>,
    #[serde(default)]
    keys: Vec<KeyDto>,
    /// Animated horizontal offset, the same `[{t,v}]` shape as `keys`.
    #[serde(default, rename = "trackX")]
    track_x: Vec<KeyDto>,
    /// Animated vertical offset.
    #[serde(default, rename = "trackY")]
    track_y: Vec<KeyDto>,
    /// Animated uniform size.
    #[serde(default, rename = "trackScale")]
    track_scale: Vec<KeyDto>,
    /// Animated opacity.
    #[serde(default, rename = "trackAlpha")]
    track_alpha: Vec<KeyDto>,
    #[serde(default)]
    effects: Vec<EffectDto>,
}

/// One keyframe in the editor JSON: `t` is the time in ms, `v` the value, and
/// `ease` the curve of the segment that starts here. Shared by the rotation
/// track and the four property tracks.
#[derive(Debug, serde::Deserialize, Default)]
struct KeyDto {
    #[serde(default)]
    t: i64,
    #[serde(default)]
    v: f32,
    #[serde(default)]
    ease: Option<EaseDto>,
}

impl KeyDto {
    fn into_keyframe(self) -> Keyframe {
        Keyframe {
            time_ms: self.t,
            value_f32: self.v,
            // No curve written means the straight line, which is what a document
            // written before curves existed meant.
            ease: self.ease.map(EaseDto::into_current).unwrap_or(Ease::Linear),
        }
    }
}

/// The curve of one key or ramp, in the editor JSON.
///
/// A **flat** shape (`{"kind":"cubic","x1":…}`) rather than serde's own enum
/// form, because this JSON is written and read by hand in Kotlin: one branch to
/// read instead of two shapes (`"linear"` and `{"cubic":{…}}`). The binary
/// format keeps the compact enum, so nothing is paid for the friendlier file.
#[derive(Debug, serde::Deserialize, Default)]
struct EaseDto {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    x1: f32,
    #[serde(default)]
    y1: f32,
    #[serde(default)]
    x2: f32,
    #[serde(default)]
    y2: f32,
}

impl EaseDto {
    fn into_current(self) -> Ease {
        match self.kind.as_str() {
            "hold" => Ease::Hold,
            "cubic" => Ease::Cubic {
                x1: self.x1,
                y1: self.y1,
                x2: self.x2,
                y2: self.y2,
            },
            // "linear", a missing kind, and anything a newer build wrote that
            // this one does not know: the straight line, never a panic.
            _ => Ease::Linear,
        }
    }
}

/// The same flat shape on the way out, so the file Kotlin writes and the file
/// Kotlin reads are the same shape.
fn ease_json(ease: Ease) -> serde_json::Value {
    match ease {
        Ease::Linear => serde_json::json!({"kind": "linear"}),
        Ease::Hold => serde_json::json!({"kind": "hold"}),
        Ease::Cubic { x1, y1, x2, y2 } => serde_json::json!({
            "kind": "cubic",
            "x1": x1,
            "y1": y1,
            "x2": x2,
            "y2": y2,
        }),
    }
}

/// Cross-fade of the layer against the one under it (see [`Transition`]).
#[derive(Debug, serde::Deserialize, Default)]
struct TransitionDto {
    #[serde(default, rename = "startMs")]
    start_ms: i64,
    #[serde(default = "default_transition_duration_ms", rename = "durationMs")]
    duration_ms: i64,
    #[serde(default = "default_enabled", rename = "withPrevious")]
    with_previous: bool,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    ease: Option<EaseDto>,
}

fn default_transition_duration_ms() -> i64 {
    Transition::default().duration_ms
}

impl TransitionDto {
    fn into_current(self) -> Transition {
        Transition {
            start_ms: self.start_ms,
            // A zero-length ramp would divide by zero in the editor's sampler.
            duration_ms: self.duration_ms.max(1),
            with_previous: self.with_previous,
            enabled: self.enabled,
            // The ramp a document without curves had: the smoothstep this
            // editor hard-coded, which is exactly `Ease::SMOOTH`.
            ease: self.ease.map(EaseDto::into_current).unwrap_or(Ease::SMOOTH),
        }
    }
}

/// One effect in the editor JSON. `params` is the flat slot vector described
/// by [`EffectInstance::params`]; a wrong-length vector is replaced by the
/// spec defaults in [`LayerDto::effects`] conversion.
#[derive(Debug, serde::Deserialize, Default)]
struct EffectDto {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    kind: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    params: Vec<f32>,
}

fn default_enabled() -> bool {
    true
}

fn default_shape_kind() -> LayerKind {
    LayerKind::Shape
}

fn de_kind<'de, D>(d: D) -> Result<LayerKind, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct KindVisitor;

    impl<'de> serde::de::Visitor<'de> for KindVisitor {
        type Value = LayerKind;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a layer kind name (SHAPE/TEXT/MEDIA/AUDIO) or 0..=3")
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<LayerKind, E> {
            // Lenient: unknown future kinds fall back to Shape.
            Ok(match v.to_ascii_uppercase().as_str() {
                "TEXT" => LayerKind::Text,
                "MEDIA" => LayerKind::Media,
                "AUDIO" => LayerKind::Audio,
                _ => LayerKind::Shape,
            })
        }

        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<LayerKind, E> {
            Ok(match v {
                1 => LayerKind::Text,
                2 => LayerKind::Media,
                3 => LayerKind::Audio,
                _ => LayerKind::Shape,
            })
        }

        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<LayerKind, E> {
            self.visit_u64(v.max(0) as u64)
        }
    }

    d.deserialize_any(KindVisitor)
}

fn kind_name(kind: LayerKind) -> &'static str {
    match kind {
        LayerKind::Shape => "SHAPE",
        LayerKind::Text => "TEXT",
        LayerKind::Media => "MEDIA",
        LayerKind::Audio => "AUDIO",
    }
}

/// Build an instance from editor JSON. A built-in id wins; otherwise the id is
/// looked up among the project's `customEffects`, and an id that names neither
/// is dropped. A parameter vector of the wrong arity is replaced by the spec
/// defaults, so a malformed chain degrades instead of failing the whole load.
fn effect_from_dto(dto: EffectDto, customs: &[CustomEffect]) -> Option<EffectInstance> {
    let mut inst = match EffectKind::from_id(&dto.kind) {
        Some(kind) => EffectInstance::new(kind),
        None => {
            // Not a built-in: a project-defined effect referenced by id. An
            // unknown id is dropped, exactly as `parse_chains_json` drops one.
            let effect = customs.iter().find(|c| c.id == dto.kind)?;
            EffectInstance::custom(effect).ok()?
        }
    };
    if dto.params.len() == inst.params.len() {
        inst.params.copy_from_slice(&dto.params);
    }
    inst.enabled = dto.enabled;
    inst.normalise();
    if let Some(raw) = dto.id.as_deref().and_then(|s| Uuid::parse_str(s).ok()) {
        inst.id = raw;
    }
    Some(inst)
}

/// Apply `parse_chains_json`'s policy to the project's `customEffects`: an
/// entry that fails the structural check, or repeats an id, is dropped, first
/// declaration winning. A malformed or future entry therefore costs only
/// itself, and the chains that do not use it still load.
fn usable_customs(raw: Vec<CustomEffect>) -> Vec<CustomEffect> {
    let mut seen = HashSet::new();
    raw.into_iter()
        .filter(|c| c.shape_error().is_none() && seen.insert(c.id.clone()))
        .collect()
}

/// One canvas side from the editor JSON, or `fallback` when the key is absent
/// or the value is nonsense.
///
/// A zero side would make a frame nothing can allocate, so "present but
/// unusable" and "absent" get the same answer rather than a clamped-to-16
/// frame the user never asked for.
fn canvas_side(value: Option<i64>, fallback: u16) -> u16 {
    match value {
        Some(v) if (Canvas::MIN_SIDE as i64..=Canvas::MAX_SIDE as i64).contains(&v) => v as u16,
        _ => fallback,
    }
}

/// Parses editor JSON into `.rumo` codec bytes.
///
/// Empty project names are rejected; layer ids that are not UUIDs get a
/// fresh v4 id so Kotlin row ids (`"1"`, …) never fail the save path.
pub fn project_from_json(json: &str) -> Result<Vec<u8>, String> {
    let dto: ProjectDto =
        serde_json::from_str(json).map_err(|e| format!("bad project json: {e}"))?;
    if dto.name.trim().is_empty() {
        return Err("project name is empty".to_string());
    }
    let ProjectDto {
        name,
        canvas_width,
        canvas_height,
        background_argb,
        custom_effects,
        layers,
    } = dto;
    // The definitions must be usable before the chains that name them are
    // converted, or an instance of a dropped definition would be dropped too.
    let customs = usable_customs(custom_effects);
    let mut project = Project::new(name);
    // An absent or unusable canvas key falls back to the built-in frame: the
    // same answer as a file saved before the keys existed, so nothing on a
    // device is rewritten by opening it.
    let fallback = Canvas::default();
    project.canvas = Canvas {
        width: canvas_side(canvas_width, fallback.width),
        height: canvas_side(canvas_height, fallback.height),
        background: background_argb
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(fallback.background),
    };
    for l in layers {
        let id =
            l.id.as_deref()
                .and_then(|s| Uuid::parse_str(s).ok())
                .unwrap_or_else(Uuid::new_v4);
        // `None` when the editor sent no per-layer props at all, so a project
        // whose layers are bare stays bare instead of gaining an all-default
        // `extra` (which would then encode as `Some` in the codec).
        let extra = if l.argb.is_none()
            && l.offset_x.is_none()
            && l.offset_y.is_none()
            && l.scale.is_none()
            && l.alpha.is_none()
            && l.text.is_none()
            && l.text_weight.is_none()
            && l.stroke_px.is_none()
            && l.stroke_argb.is_none()
            && l.transition.is_none()
            && l.track_x.is_empty()
            && l.track_y.is_empty()
            && l.track_scale.is_empty()
            && l.track_alpha.is_empty()
        {
            None
        } else {
            Some(LayerExtra {
                argb: l.argb,
                dx: l.offset_x.unwrap_or(0.0),
                dy: l.offset_y.unwrap_or(0.0),
                alpha: l.alpha.unwrap_or(1.0),
                scale: l.scale.unwrap_or(1.0),
                // An empty string and "absent" both mean "fall back to `name`",
                // so normalise to `None` instead of round-tripping `""`.
                text: l.text.filter(|t| !t.is_empty()),
                // A weight outside the range real faces live in would make the
                // shaper's query miss every face, so it is clamped where it
                // enters the document rather than at every use.
                text_weight: l.text_weight.unwrap_or(400).clamp(100, 900),
                // A negative or non-finite outline would grow a glyph bitmap by
                // nothing sane; the shaper rounds it to whole pixels anyway.
                stroke_px: l.stroke_px.filter(|v| v.is_finite()).unwrap_or(0.0).max(0.0),
                stroke_argb: l.stroke_argb,
                transition: l.transition.map(TransitionDto::into_current),
                // Empty means "not animated" all the way down, so the sampler
                // falls back to the base value above rather than to zero.
                track_x: l.track_x.into_iter().map(KeyDto::into_keyframe).collect(),
                track_y: l.track_y.into_iter().map(KeyDto::into_keyframe).collect(),
                track_scale: l
                    .track_scale
                    .into_iter()
                    .map(KeyDto::into_keyframe)
                    .collect(),
                track_alpha: l
                    .track_alpha
                    .into_iter()
                    .map(KeyDto::into_keyframe)
                    .collect(),
            })
        };
        project.layers.push(Layer {
            id,
            kind: l.kind,
            name: l.name,
            keyframes: l.keys.into_iter().map(KeyDto::into_keyframe).collect(),
            extra,
            visible: l.visible.unwrap_or(true),
            duration_ms: l.duration_ms.unwrap_or(DEFAULT_LAYER_DURATION_MS),
            // A layer cannot start before the project does: a negative offset
            // would put it outside the timeline rather than at its beginning,
            // and dropping the value would make the layer vanish on load.
            start_ms: l.start_ms.unwrap_or(0).max(0),
            uri: l.uri.filter(|s| !s.is_empty()),
            effects: l
                .effects
                .into_iter()
                .filter_map(|e| effect_from_dto(e, &customs))
                .collect(),
        });
    }
    project.custom = customs;
    Ok(codec::encode(&project))
}

/// One keyframe track as the editor's `[{t,v}]` list, in the same shape for the
/// rotation track and the four property tracks — a caller that does not
/// distinguish them cannot tell them apart on the wire either.
fn keys_json(keys: &[Keyframe]) -> Vec<serde_json::Value> {
    keys.iter()
        .map(|k| {
            serde_json::json!({
                "t": k.time_ms,
                "v": k.value_f32,
                "ease": ease_json(k.ease),
            })
        })
        .collect()
}

/// Decodes `.rumo` bytes back into editor JSON (see module docs).
pub fn project_to_json(bytes: &[u8]) -> Result<String, String> {
    let project = codec::decode(bytes).map_err(|e| format!("bad project bytes: {e}"))?;
    let layers: Vec<serde_json::Value> = project
        .layers
        .iter()
        .map(|l| {
            let extra = l.extra.clone().unwrap_or_default();
            // `text` is emitted as `""` rather than `null` on purpose: Kotlin
            // reads it with `JSONObject.optString`, which turns a JSON null into
            // the literal string "null".
            serde_json::json!({
                "id": l.id.to_string(),
                "kind": kind_name(l.kind),
                "name": l.name,
                "visible": l.visible,
                "argb": extra.argb,
                "durationMs": l.duration_ms,
                "startMs": l.start_ms,
                "uri": l.uri,
                "text": extra.text.clone().unwrap_or_default(),
                "textWeight": extra.text_weight,
                "strokePx": extra.stroke_px,
                "strokeArgb": extra.stroke_argb,
                "offsetX": extra.dx,
                "offsetY": extra.dy,
                "scale": extra.scale,
                "alpha": extra.alpha,
                "transition": extra.transition.map(|t| {
                    serde_json::json!({
                        "startMs": t.start_ms,
                        "durationMs": t.duration_ms,
                        "withPrevious": t.with_previous,
                        "enabled": t.enabled,
                        "ease": ease_json(t.ease),
                    })
                }),
                "keys": l.keyframes.iter().map(|k| {
                    serde_json::json!({
                        "t": k.time_ms,
                        "v": k.value_f32,
                        "ease": ease_json(k.ease),
                    })
                }).collect::<Vec<_>>(),
                "trackX": keys_json(&extra.track_x),
                "trackY": keys_json(&extra.track_y),
                "trackScale": keys_json(&extra.track_scale),
                "trackAlpha": keys_json(&extra.track_alpha),
                "effects": l.effects.iter().map(|e| {
                    serde_json::json!({
                        "id": e.id.to_string(),
                        "kind": e.target.id(),
                        "enabled": e.enabled,
                        "params": e.params,
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::to_string(&serde_json::json!({
        "name": project.name,
        "canvasWidth": project.canvas.width,
        "canvasHeight": project.canvas.height,
        "backgroundArgb": project.canvas.background,
        "customEffects": project.custom,
        "layers": layers,
    }))
    .map_err(|e| format!("project json encode: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One project-defined effect in `EditorState.toJson`'s `customEffects`
    /// shape, the same object `parse_chains_json` accepts under `custom`.
    fn custom_effect_json() -> serde_json::Value {
        serde_json::json!({
            "id": "vignette_x",
            "label": "Vignette X",
            "space": "display",
            "passes": [{"entry": "fs_main", "shrink": 0}],
            "params": [{
                "key": "amount",
                "label": "Amount",
                "kind": "float",
                "min": 0.0,
                "max": 1.0,
                "default": [0.5],
                "unit": "",
                "choices": [],
            }],
            "source": "// wgsl under test",
        })
    }

    fn sample_json() -> String {
        serde_json::json!({
            "name": "promo",
            "layers": [{
                "id": "not-a-uuid-row-id",
                "kind": "SHAPE",
                "name": "Rect",
                "visible": false,
                "argb": 4294923520u32,
                "durationMs": 8000,
                "uri": serde_json::Value::Null,
                "text": "Hello world",
                "offsetX": 12.5,
                "offsetY": -3.25,
                "scale": 2.25,
                "alpha": 0.5,
                "transition": {
                    "startMs": 400,
                    "durationMs": 1200,
                    "withPrevious": true,
                    "enabled": true,
                },
                "keys": [{"t": 0, "v": 0.0}, {"t": 500, "v": 1.0}],
                "effects": [
                    {"id": "11111111-1111-4111-8111-111111111111", "kind": "blur",
                     "enabled": true, "params": [0.0, 30.0, 0.0, 0.0]},
                    {"kind": "no_such_effect", "params": []},
                    {"kind": "pixelate", "enabled": false, "params": [9999.0, 1.0]},
                ],
            }],
        })
        .to_string()
    }

    #[test]
    fn json_roundtrip_keeps_a_project_defined_effect_and_its_instance() {
        let json = serde_json::json!({
            "name": "custom",
            "customEffects": [custom_effect_json()],
            "layers": [{
                "name": "L",
                "kind": "SHAPE",
                "effects": [
                    {"kind": "vignette_x", "enabled": false, "params": [0.75]},
                ],
            }],
        })
        .to_string();
        let bytes = project_from_json(&json).expect("from json");
        let out = project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&out).expect("reparse");

        let customs = v["customEffects"].as_array().expect("customEffects array");
        assert_eq!(customs.len(), 1, "the definition must survive: {out}");
        assert_eq!(customs[0]["id"], "vignette_x");
        assert_eq!(customs[0]["label"], "Vignette X");
        assert_eq!(customs[0]["source"], "// wgsl under test");
        assert_eq!(customs[0]["params"][0]["key"], "amount");

        let fx = v["layers"][0]["effects"].as_array().expect("effects array");
        assert_eq!(fx.len(), 1, "the custom instance must survive: {out}");
        assert_eq!(fx[0]["kind"], "vignette_x");
        assert_eq!(fx[0]["enabled"], false);
        assert_eq!(fx[0]["params"][0], 0.75);

        // Second lap through bytes is stable.
        let bytes2 = project_from_json(&out).expect("from json again");
        let out2 = project_to_json(&bytes2).expect("to json again");
        assert_eq!(out, out2);
    }

    #[test]
    fn json_drops_an_instance_whose_custom_id_is_unknown() {
        let json = serde_json::json!({
            "name": "dangling",
            "customEffects": [custom_effect_json()],
            "layers": [{
                "name": "L",
                "kind": "SHAPE",
                "effects": [
                    {"kind": "blur", "params": [0.0, 30.0, 0.0, 0.0]},
                    {"kind": "nope_x", "params": [1.0]},
                    {"kind": "glow", "params": [0.6]},
                ],
            }],
        })
        .to_string();
        let bytes = project_from_json(&json).expect("from json");
        let out = project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&out).expect("reparse");
        let fx = v["layers"][0]["effects"].as_array().expect("effects array");
        assert_eq!(fx.len(), 2, "only the unknown id is dropped: {out}");
        assert_eq!(fx[0]["kind"], "blur");
        assert_eq!(fx[1]["kind"], "glow");
        // The definition is user data: an instance not using it is no reason to
        // drop it.
        assert_eq!(v["customEffects"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn json_roundtrip_keeps_a_non_default_canvas() {
        let json = serde_json::json!({
            "name": "vertical",
            "canvasWidth": 1080,
            "canvasHeight": 1920,
            "backgroundArgb": 0xFF00FF00u32,
            "layers": [{"name": "L", "kind": "SHAPE"}],
        })
        .to_string();
        let bytes = project_from_json(&json).expect("from json");
        let out = project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&out).expect("reparse");
        assert_eq!(v["canvasWidth"], 1080);
        assert_eq!(v["canvasHeight"], 1920);
        assert_eq!(v["backgroundArgb"], 0xFF00FF00u32);

        // Second lap through bytes is stable.
        let bytes2 = project_from_json(&out).expect("from json again");
        let out2 = project_to_json(&bytes2).expect("to json again");
        assert_eq!(out, out2);
    }

    #[test]
    fn json_defaults_the_canvas_when_the_keys_are_absent() {
        // A file saved before the canvas keys existed must open on the frame
        // the editor always used, not on a zero-sized one.
        let json = serde_json::json!({
            "name": "no canvas",
            "layers": [{"name": "L", "kind": "SHAPE"}],
        })
        .to_string();
        let bytes = project_from_json(&json).expect("from json");
        let out = project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&out).expect("reparse");
        assert_eq!(v["canvasWidth"], 512);
        assert_eq!(v["canvasHeight"], 288);
        assert_eq!(v["backgroundArgb"], Canvas::default().background);
        assert_eq!(Canvas::default().background, 0xFF14_1824);
    }

    #[test]
    fn json_replaces_a_nonsense_canvas_side_with_the_default() {
        // Zero, a negative and an absurd size are all "unusable": the frame
        // falls back whole rather than to a 16px sliver nobody asked for.
        for (width, height) in [(0, 0), (-4, 100_000), (100_000, -1)] {
            let json = serde_json::json!({
                "name": "silly",
                "canvasWidth": width,
                "canvasHeight": height,
                "layers": [],
            })
            .to_string();
            let bytes = project_from_json(&json).expect("from json");
            let out = project_to_json(&bytes).expect("to json");
            let v: serde_json::Value = serde_json::from_str(&out).expect("reparse");
            assert_eq!(v["canvasWidth"], 512, "width from ({width}, {height})");
            assert_eq!(v["canvasHeight"], 288, "height from ({width}, {height})");
        }
    }

    #[test]
    fn json_drops_a_malformed_custom_and_keeps_the_first_of_a_repeated_id() {
        let mut bad = custom_effect_json();
        bad["source"] = serde_json::Value::String(String::new());
        let mut dup = custom_effect_json();
        dup["label"] = serde_json::Value::String("Second".to_string());
        let json = serde_json::json!({
            "name": "picky",
            "customEffects": [bad, custom_effect_json(), dup],
            "layers": [{"name": "L", "kind": "SHAPE", "effects": [
                {"kind": "vignette_x", "params": [0.75]},
            ]}],
        })
        .to_string();
        let bytes = project_from_json(&json).expect("from json");
        let out = project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&out).expect("reparse");
        let customs = v["customEffects"].as_array().expect("customEffects array");
        assert_eq!(
            customs.len(),
            1,
            "malformed and repeated entries drop: {out}"
        );
        assert_eq!(customs[0]["label"], "Vignette X", "first declaration wins");
        assert_eq!(v["layers"][0]["effects"][0]["kind"], "vignette_x");
    }

    #[test]
    fn v1_bytes_decode_with_none_extra() {
        // Wire shape from before `extra` existed.
        #[derive(serde::Serialize)]
        struct LayerV1 {
            id: Uuid,
            kind: LayerKind,
            name: String,
            keyframes: Vec<Keyframe>,
        }
        #[derive(serde::Serialize)]
        struct ProjectV1 {
            schema_tag: u32,
            name: String,
            layers: Vec<LayerV1>,
        }
        let old = ProjectV1 {
            schema_tag: codec::CURRENT_TAG,
            name: "legacy".to_string(),
            layers: vec![LayerV1 {
                id: Uuid::new_v4(),
                kind: LayerKind::Text,
                name: "title".to_string(),
                keyframes: vec![Keyframe {
                    time_ms: 100,
                    value_f32: 2.0,
                ease: crate::ease::Ease::Linear,
            }],
            }],
        };
        let buf = postcard::to_extend(&codec::MIN_TAG, Vec::new()).expect("tag");
        let bytes = postcard::to_extend(&old, buf).expect("v1 body");
        let decoded = codec::decode(&bytes).expect("v1 bytes must decode");
        assert_eq!(decoded.name, "legacy");
        assert_eq!(decoded.layers.len(), 1);
        assert_eq!(decoded.layers[0].extra, None);
        assert!(decoded.layers[0].visible);
        assert_eq!(decoded.layers[0].uri, None);
        assert!(decoded.layers[0].effects.is_empty());
    }

    #[test]
    fn json_roundtrip_keeps_effects_and_drops_unknown_kinds() {
        let bytes = project_from_json(&sample_json()).expect("from json");
        let json = project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&json).expect("reparse");
        let fx = v["layers"][0]["effects"].as_array().expect("effects array");
        assert_eq!(fx.len(), 2, "unknown effect id must be dropped: {json}");
        assert_eq!(fx[0]["kind"], "blur");
        assert_eq!(fx[0]["id"], "11111111-1111-4111-8111-111111111111");
        assert_eq!(fx[0]["enabled"], true);
        assert_eq!(fx[0]["params"][1], 30.0);
        assert_eq!(fx[1]["kind"], "pixelate");
        assert_eq!(fx[1]["enabled"], false);
        assert_eq!(
            fx[1]["params"][0], 128.0,
            "out-of-range size must clamp to the spec max"
        );

        // Second lap through bytes is stable.
        let bytes2 = project_from_json(&json).expect("from json again");
        let json2 = project_to_json(&bytes2).expect("to json again");
        assert_eq!(json, json2);
    }

    #[test]
    fn wrong_arity_effect_params_fall_back_to_defaults() {
        let json = serde_json::json!({
            "name": "p",
            "layers": [{"name": "L", "kind": "SHAPE",
                        "effects": [{"kind": "glow", "params": [1.0]}]}],
        })
        .to_string();
        let bytes = project_from_json(&json).expect("from json");
        let out = project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&out).expect("reparse");
        let params = v["layers"][0]["effects"][0]["params"]
            .as_array()
            .expect("params");
        let glow = crate::effect::EffectKind::Glow.spec();
        assert_eq!(params.len(), glow.slots());
        let threshold = params[0].as_f64().expect("number");
        assert!(
            (threshold - 0.6).abs() < 1e-6,
            "threshold default should survive as f32, got {threshold}"
        );
    }

    #[test]
    fn json_roundtrip_preserves_extra() {
        let bytes = project_from_json(&sample_json()).expect("from json");
        let json = project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&json).expect("reparse");
        assert_eq!(v["name"], "promo");
        let l = &v["layers"][0];
        assert_eq!(l["name"], "Rect");
        assert_eq!(l["visible"], false);
        assert_eq!(l["argb"], 4294923520u32);
        assert_eq!(l["durationMs"], 8000);
        assert!((l["offsetX"].as_f64().unwrap() - 12.5).abs() < 1e-6);
        assert!((l["offsetY"].as_f64().unwrap() + 3.25).abs() < 1e-6);
        assert!((l["alpha"].as_f64().unwrap() - 0.5).abs() < 1e-6);
        assert!((l["scale"].as_f64().unwrap() - 2.25).abs() < 1e-6);
        assert_eq!(l["text"], "Hello world");
        assert_eq!(l["transition"]["startMs"], 400);
        assert_eq!(l["transition"]["durationMs"], 1200);
        assert_eq!(l["transition"]["withPrevious"], true);
        assert_eq!(l["transition"]["enabled"], true);
        assert_eq!(l["keys"].as_array().unwrap().len(), 2);
        // Second lap through bytes is stable.
        let bytes2 = project_from_json(&json).expect("from json again");
        let json2 = project_to_json(&bytes2).expect("to json again");
        assert_eq!(json, json2);
    }

    #[test]
    fn transition_needs_an_explicit_window_and_defaults_on() {
        let json = serde_json::json!({
            "name": "p",
            "layers": [
                {"name": "a", "kind": "SHAPE", "transition": {"durationMs": 0}},
                {"name": "b", "kind": "SHAPE", "transition": serde_json::Value::Null},
                {"name": "c", "kind": "SHAPE"},
            ],
        })
        .to_string();
        let bytes = project_from_json(&json).expect("from json");
        let out = project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&out).expect("reparse");
        // A zero-length ramp would divide by zero in the editor's sampler.
        assert_eq!(v["layers"][0]["transition"]["durationMs"], 1);
        assert_eq!(v["layers"][0]["transition"]["startMs"], 0);
        assert_eq!(v["layers"][0]["transition"]["withPrevious"], true);
        assert_eq!(v["layers"][0]["transition"]["enabled"], true);
        // Absent and `null` both mean "no cross-fade", and a bare layer keeps no
        // `extra` at all.
        assert_eq!(v["layers"][1]["transition"], serde_json::Value::Null);
        assert_eq!(v["layers"][2]["transition"], serde_json::Value::Null);
    }

    #[test]
    fn json_rejects_empty_name() {        assert!(project_from_json(r#"{"name":"","layers":[]}"#).is_err());
        assert!(project_from_json(r#"{"name":"   ","layers":[]}"#).is_err());
        assert!(project_from_json(r#"{"layers":[]}"#).is_err());
        assert!(project_from_json("not json").is_err());
        assert!(project_from_json(&sample_json()).is_ok());
    }

    /// A curve must survive the JSON trip in both directions: it is written by
    /// Kotlin, read by Rust, written back by Rust and read by Kotlin again, and a
    /// curve that only survives one of those legs would be an animation that
    /// changes shape when the project is opened by the assistant.
    #[test]
    fn json_roundtrip_keeps_the_curves() {
        let mut project = Project::new("curves");
        let mut layer = Layer {
            id: Uuid::new_v4(),
            kind: LayerKind::Shape,
            name: "snappy".to_string(),
            keyframes: Vec::new(),
            extra: None,
            visible: true,
            duration_ms: DEFAULT_LAYER_DURATION_MS,
            start_ms: 0,
            uri: None,
            effects: Vec::new(),
        };
        layer.extra = Some(LayerExtra {
            track_x: vec![
                Keyframe::eased(0, 0.0, Ease::SNAP),
                Keyframe::eased(1000, 100.0, Ease::Linear),
            ],
            track_alpha: vec![Keyframe::eased(0, 1.0, Ease::Hold)],
            transition: Some(Transition {
                start_ms: 100,
                duration_ms: 900,
                ease: Ease::EASE_OUT,
                ..Default::default()
            }),
            ..Default::default()
        });
        project.layers.push(layer);

        let bytes = codec::encode(&project);
        let json = project_to_json(&bytes).expect("encode json");
        // One representation in the file, not two: the curve is written as its
        // four numbers even when it is exactly a preset, and the *name* is
        // derived for display (`Ease::preset_name`, the assistant's `easeWire`).
        // Two shapes for the same curve would be two things to keep in sync.
        assert!(json.contains("\"kind\":\"cubic\""), "{json}");
        assert!(json.contains("\"kind\":\"hold\""), "{json}");
        assert!(!json.contains("\"snap\""), "no name in the file: {json}");
        let back = project_from_json(&json).expect("decode json");
        let decoded = codec::decode(&back).expect("decode bytes");
        let extra = decoded.layers[0].extra.clone().expect("props");
        assert_eq!(extra.track_x[0].ease, Ease::SNAP);
        assert_eq!(extra.track_x[1].ease, Ease::Linear);
        assert_eq!(extra.track_alpha[0].ease, Ease::Hold);
        assert_eq!(extra.transition.unwrap().ease, Ease::EASE_OUT);
    }

    #[test]
    fn json_roundtrip_keeps_the_start_time_and_the_property_tracks() {
        let json = serde_json::json!({
            "name": "animated",
            "layers": [{
                "name": "Title",
                "kind": "TEXT",
                "durationMs": 4000,
                "startMs": 1500,
                "offsetX": 10.0,
                "scale": 1.0,
                "alpha": 1.0,
                "trackX": [{"t": 1500, "v": 0.0}, {"t": 2500, "v": 80.0}],
                "trackY": [{"t": 1500, "v": -20.0}],
                "trackScale": [{"t": 1500, "v": 1.0}, {"t": 3500, "v": 2.5}],
                "trackAlpha": [{"t": 1500, "v": 0.0}, {"t": 1900, "v": 1.0}],
            }],
        })
        .to_string();
        let bytes = project_from_json(&json).expect("from json");
        let out = project_to_json(&bytes).expect("to json");
        let v: serde_json::Value = serde_json::from_str(&out).expect("reparse");
        let l = &v["layers"][0];
        assert_eq!(l["startMs"], 1500, "the timeline position survives");
        assert_eq!(l["trackX"].as_array().expect("trackX").len(), 2);
        assert_eq!(l["trackX"][1]["t"], 2500);
        assert_eq!(l["trackX"][1]["v"], 80.0);
        assert_eq!(l["trackY"][0]["v"], -20.0);
        assert_eq!(l["trackScale"][1]["v"], 2.5);
        assert_eq!(l["trackAlpha"][0]["v"], 0.0);
        assert_eq!(l["trackAlpha"][1]["t"], 1900);

        // The samplers read the tracks back: an empty one keeps its base, which
        // is the whole point of "not animated".
        let project = codec::decode(&bytes).expect("decode");
        let layer = &project.layers[0];
        assert_eq!(layer.start_ms, 1500);
        assert_eq!(layer.sample_x(1500), 0.0);
        assert_eq!(layer.sample_x(2000), 40.0);
        assert_eq!(layer.sample_scale(3500), 2.5);
        assert_eq!(layer.sample_alpha(1500), 0.0);
        assert_eq!(layer.sample_alpha(1900), 1.0);
        assert_eq!(layer.sample_y(900), -20.0, "one key holds everywhere");

        // Second lap through bytes is stable.
        let bytes2 = project_from_json(&out).expect("from json again");
        assert_eq!(project_to_json(&bytes2).expect("to json again"), out);
    }

    #[test]
    fn json_defaults_the_start_time_and_animates_nothing_when_absent() {
        // A document saved before either field existed must still open, at the
        // start of the project, with every property at its base value.
        let json = serde_json::json!({
            "name": "plain",
            "layers": [
                {"name": "bare", "kind": "SHAPE"},
                {"name": "props", "kind": "SHAPE", "offsetX": 4.0, "alpha": 0.5,
                 "scale": 2.0, "durationMs": 2500},
            ],
        })
        .to_string();
        let bytes = project_from_json(&json).expect("from json");
        let project = codec::decode(&bytes).expect("decode");
        assert_eq!(project.layers[0].start_ms, 0);
        assert_eq!(project.layers[0].extra, None, "a bare layer stays bare");
        assert_eq!(project.layers[0].sample_alpha(0), 1.0);
        assert_eq!(project.layers[0].sample_scale(0), 1.0);
        let props = &project.layers[1];
        assert_eq!(props.start_ms, 0);
        assert_eq!(props.sample_x(0), 4.0);
        assert_eq!(props.sample_alpha(0), 0.5);
        assert_eq!(props.sample_scale(0), 2.0);

        let v: serde_json::Value =
            serde_json::from_str(&project_to_json(&bytes).expect("to json")).expect("reparse");
        assert_eq!(v["layers"][0]["startMs"], 0);
        assert_eq!(v["layers"][0]["trackX"].as_array().map(Vec::len), Some(0));
        assert_eq!(v["layers"][1]["trackAlpha"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn a_negative_start_time_is_clamped_to_the_project_beginning() {
        // Clamped, not rejected: dropping it would make the layer vanish.
        let json = serde_json::json!({
            "name": "clamped",
            "layers": [{"name": "L", "kind": "SHAPE", "startMs": -500}],
        })
        .to_string();
        let bytes = project_from_json(&json).expect("from json");
        let v: serde_json::Value =
            serde_json::from_str(&project_to_json(&bytes).expect("to json")).expect("reparse");
        assert_eq!(v["layers"][0]["startMs"], 0);
    }
}
