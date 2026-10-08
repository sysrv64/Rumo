// SPDX-License-Identifier: Apache-2.0
use crate::model::Project;

/// Wire tag written by [`encode`].
///
/// * `1` — layers were `{id, kind, name, keyframes}`; later appended
///   `extra`/`visible`/`duration_ms`/`uri` without bumping the tag (the
///   append-only fields were recovered by a fallback decode).
/// * `2` — layers additionally carry an effect chain.
/// * `3` — [`crate::model::LayerExtra`] additionally carries the preview `scale`
///   and the TEXT payload.
/// * `4` — `LayerExtra` additionally carries the cross-fade `transition`.
///   Appending to a struct nested inside `Option` is not detectable by a
///   positional reader, so each tag names its shape outright.
/// * `5` — the project carries its project-defined effects (`Project::custom`),
///   and an effect instance carries the optional id of one
///   (`EffectInstanceCodec::custom`). Both landed together, so a single bump
///   covers them: an instance can only name a definition that the document
///   holds.
/// * `6` — the project carries its [`Canvas`] (size and background). Tag ≤ 5
///   bytes decode with the default frame.
/// * `7` — a text layer carries its weight and its outline (thickness and
///   colour). Tag ≤ 6 bytes decode with the regular face and no contour.
/// * `8` — a layer carries its `start_ms` on the timeline, and `LayerExtra`
///   carries the animated `track_x`/`track_y`/`track_scale`/`track_alpha`.
///   Tag ≤ 7 bytes decode with the layer at 0 ms and no animated track (which
///   means "the property is not animated", not "the property is zero").
///
/// * **Tag 9** — every keyframe gained an [`crate::ease::Ease`] (the curve of the
///   segment that starts at it) and every transition gained one for its ramp.
///   Tag ≤ 8 bytes decode as [`crate::ease::Ease::Linear`] keys and a
///   [`crate::ease::Ease::SMOOTH`] ramp — which are exactly the arithmetic those
///   bytes were written with, so an old project keeps its look to the last bit.
///
/// [`Canvas`]: crate::model::Canvas
pub const CURRENT_TAG: u32 = 9;

/// Oldest wire tag [`decode`] still accepts.
pub const MIN_TAG: u32 = 1;

#[derive(Debug)]
pub enum CodecError {
    UnsupportedTag(u32),
    Postcard(postcard::Error),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedTag(got) => {
                write!(f, "unsupported tag: got {got}, expected {MIN_TAG}..={CURRENT_TAG}")
            }
            Self::Postcard(e) => write!(f, "postcard error: {e}"),
        }
    }
}

impl std::error::Error for CodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnsupportedTag(_) => None,
            Self::Postcard(e) => Some(e),
        }
    }
}

impl From<postcard::Error> for CodecError {
    fn from(e: postcard::Error) -> Self {
        Self::Postcard(e)
    }
}

pub fn encode(project: &Project) -> Vec<u8> {
    let buf = postcard::to_extend(&CURRENT_TAG, Vec::new()).expect("tag encode");
    postcard::to_extend(project, buf).expect("project encode")
}

/// Decode `.rumo` bytes, walking the wire-format history.
///
/// Every tag but the first names its own shape, and is read by its own struct.
/// Tag 1 is the exception: the append-only fields (`extra`/`visible`/
/// `duration_ms`/`uri`) were added to the layer without a tag bump, so that tag
/// covers two shapes and only its oldest member can be attempted
/// unconditionally.
///
/// The ladder is safe for the same reason the tag exists: `postcard` is not
/// self-describing, but `postcard::from_bytes` does not require the input to be
/// fully consumed either — it stops as soon as the type is satisfied. A shape
/// that is too new for the stream therefore asks for fields that are not there,
/// and the last layer's missing fields hit end-of-input. [`encode`] writes
/// exactly `tag ++ Project` with no trailing bytes, so a well-formed stream
/// cannot satisfy an over-long parse.
pub fn decode(bytes: &[u8]) -> Result<Project, CodecError> {
    let (tag, rest) = postcard::take_from_bytes::<u32>(bytes)?;
    if !(MIN_TAG..=CURRENT_TAG).contains(&tag) {
        return Err(CodecError::UnsupportedTag(tag));
    }
    match tag {
        // Current shape: a layer sits on the timeline, its extra carries the
        // animated property tracks, and every key and ramp carries its curve.
        9 => Ok(postcard::from_bytes::<Project>(rest)?),
        // Tag 8: everything tag 9 has except the curves on keys and ramps.
        8 => Ok(postcard::from_bytes::<ProjectTag8>(rest)?.into_current()),
        // Tag 7: everything tag 8 has except the start time and the tracks.
        7 => Ok(postcard::from_bytes::<ProjectTag7>(rest)?.into_current()),
        // Tag 6: everything tag 7 has except the text weight and the outline.
        6 => Ok(postcard::from_bytes::<ProjectTag6>(rest)?.into_current()),
        // Tag 5: everything tag 6 has except the canvas.
        5 => Ok(postcard::from_bytes::<ProjectTag5>(rest)?.into_current()),
        // Tag 4: everything tag 5 has except project-defined effects, so both
        // its layers and its instances are read by the legacy structs.
        4 => Ok(postcard::from_bytes::<ProjectTag4>(rest)?.into_current()),
        // `scale`/`text` landed here, before the cross-fade.
        3 => Ok(postcard::from_bytes::<ProjectTag3>(rest)?.into_current()),
        // Effect chains landed here, before `scale`/`text` existed.
        2 => Ok(postcard::from_bytes::<ProjectTag2>(rest)?.into_current()),
        // Tag 1: `{…, extra, visible, duration_ms, uri}`, else `{id, kind,
        // name, keyframes}`.
        _ => {
            if let Ok(one) = postcard::from_bytes::<ProjectTag1Extra>(rest) {
                return Ok(one.into_current());
            }
            Ok(postcard::from_bytes::<ProjectTag1Old>(rest)?.into_current())
        }
    }
}

/// [`crate::effect::EffectInstance`] as every build before tag 5 wrote it:
/// `{id, kind, enabled, params}`, with no slot for a project-defined target.
/// Written by the tag-2, tag-3 and tag-4 encoders, so one reader serves all
/// three. `Serialize` is derived for the tests, which lay down the bytes those
/// builds wrote.
///
/// No serde defaults on purpose: it must describe the old bytes exactly, so a
/// short read fails rather than inventing a field.
#[derive(serde::Serialize, serde::Deserialize)]
struct EffectInstanceV4 {
    id: uuid::Uuid,
    kind: crate::effect::EffectKind,
    enabled: bool,
    params: Vec<f32>,
}

impl EffectInstanceV4 {
    fn into_current(self) -> crate::effect::EffectInstance {
        let mut inst = crate::effect::EffectInstance::new(self.kind);
        inst.id = self.id;
        inst.enabled = self.enabled;
        // A wrong arity means a damaged chain; the spec defaults keep the rest
        // of the project loadable, exactly as the live decoder does.
        if self.params.len() == inst.params.len() {
            inst.params = self.params;
        }
        inst.normalise();
        inst
    }
}

/// [`crate::model::LayerExtra`] as tags 1 and 2 wrote it: four fields.
///
/// No serde defaults on purpose: it must describe the old bytes exactly, so a
/// short read fails rather than inventing fields.
#[derive(serde::Deserialize)]
struct ExtraTag2 {
    argb: Option<u32>,
    dx: f32,
    dy: f32,
    alpha: f32,
}

impl ExtraTag2 {
    fn into_current(self) -> crate::model::LayerExtra {
        crate::model::LayerExtra {
            argb: self.argb,
            dx: self.dx,
            dy: self.dy,
            alpha: self.alpha,
            ..Default::default()
        }
    }
}

/// [`crate::model::LayerExtra`] as tag 3 wrote it: `scale` and `text`, no
/// `transition`.
#[derive(serde::Deserialize)]
struct ExtraTag3 {
    argb: Option<u32>,
    dx: f32,
    dy: f32,
    alpha: f32,
    scale: f32,
    text: Option<String>,
}

impl ExtraTag3 {
    fn into_current(self) -> crate::model::LayerExtra {
        crate::model::LayerExtra {
            argb: self.argb,
            dx: self.dx,
            dy: self.dy,
            alpha: self.alpha,
            scale: self.scale,
            text: self.text,
            transition: None,
            ..Default::default()
        }
    }
}

/// Layer shape at tag 1, original form: none of the append-only fields yet.
#[derive(serde::Deserialize)]
struct LayerTag1Old {
    id: uuid::Uuid,
    kind: crate::model::LayerKind,
    name: String,
    keyframes: Vec<crate::model::Keyframe>,
}

/// Layer shape at tag 1 with the append-only fields, before effect chains.
#[derive(serde::Deserialize)]
struct LayerTag1Extra {
    id: uuid::Uuid,
    kind: crate::model::LayerKind,
    name: String,
    keyframes: Vec<crate::model::Keyframe>,
    extra: Option<ExtraTag2>,
    visible: bool,
    duration_ms: i64,
    uri: Option<String>,
}

/// Layer shape at tag 2: an effect chain, and an `extra` without `scale`/`text`.
#[derive(serde::Deserialize)]
struct LayerTag2 {
    id: uuid::Uuid,
    kind: crate::model::LayerKind,
    name: String,
    keyframes: Vec<crate::model::Keyframe>,
    extra: Option<ExtraTag2>,
    visible: bool,
    duration_ms: i64,
    uri: Option<String>,
    effects: Vec<EffectInstanceV4>,
}

/// Layer shape at tag 3: an `extra` with `scale`/`text`, no `transition`.
#[derive(serde::Deserialize)]
struct LayerTag3 {
    id: uuid::Uuid,
    kind: crate::model::LayerKind,
    name: String,
    keyframes: Vec<crate::model::Keyframe>,
    extra: Option<ExtraTag3>,
    visible: bool,
    duration_ms: i64,
    uri: Option<String>,
    effects: Vec<EffectInstanceV4>,
}

/// Layer shape at tag 4: the `extra` with `transition` (read as the pre-tag-7
/// shape), and the pre-tag-5 effect shape.
#[derive(serde::Deserialize)]
struct LayerTag4 {
    id: uuid::Uuid,
    kind: crate::model::LayerKind,
    name: String,
    keyframes: Vec<crate::model::Keyframe>,
    extra: Option<LayerExtraV6>,
    visible: bool,
    duration_ms: i64,
    uri: Option<String>,
    effects: Vec<EffectInstanceV4>,
}

#[derive(serde::Deserialize)]
struct ProjectTag1Old {
    schema_tag: u32,
    name: String,
    layers: Vec<LayerTag1Old>,
}

#[derive(serde::Deserialize)]
struct ProjectTag1Extra {
    schema_tag: u32,
    name: String,
    layers: Vec<LayerTag1Extra>,
}

#[derive(serde::Deserialize)]
struct ProjectTag2 {
    schema_tag: u32,
    name: String,
    layers: Vec<LayerTag2>,
}

#[derive(serde::Deserialize)]
struct ProjectTag3 {
    schema_tag: u32,
    name: String,
    layers: Vec<LayerTag3>,
}

#[derive(serde::Deserialize)]
struct ProjectTag4 {
    schema_tag: u32,
    name: String,
    layers: Vec<LayerTag4>,
}

/// [`crate::model::LayerExtra`] as tags 4, 5 and 6 wrote it: the text layer had
/// a string and a colour, but no weight and no outline. Spelled out rather than
/// borrowed from the live struct, so the next field added to `LayerExtra` cannot
/// silently rewrite what those bytes mean.
///
/// No serde defaults on purpose: it must describe the old bytes exactly, so a
/// short read fails rather than inventing a field.
#[derive(serde::Serialize, serde::Deserialize)]
struct LayerExtraV6 {
    argb: Option<u32>,
    dx: f32,
    dy: f32,
    alpha: f32,
    scale: f32,
    text: Option<String>,
    transition: Option<crate::model::Transition>,
}

impl LayerExtraV6 {
    fn into_current(self) -> crate::model::LayerExtra {
        crate::model::LayerExtra {
            argb: self.argb,
            dx: self.dx,
            dy: self.dy,
            alpha: self.alpha,
            scale: self.scale,
            text: self.text,
            transition: self.transition,
            ..Default::default()
        }
    }
}

/// [`crate::model::LayerExtra`] as tag 7 wrote it: the weight and the outline,
/// but no animated property track. Spelled out rather than borrowed from the
/// live struct, so the tracks added afterwards cannot silently rewrite what
/// those bytes mean.
///
/// `Serialize` is derived for the tag-7 test, which lays down the bytes a tag-7
/// build wrote.
#[derive(serde::Serialize, serde::Deserialize)]
struct LayerExtraV7 {
    argb: Option<u32>,
    dx: f32,
    dy: f32,
    alpha: f32,
    scale: f32,
    text: Option<String>,
    text_weight: u16,
    stroke_px: f32,
    stroke_argb: Option<u32>,
    transition: Option<crate::model::Transition>,
}

impl LayerExtraV7 {
    fn into_current(self) -> crate::model::LayerExtra {
        crate::model::LayerExtra {
            argb: self.argb,
            dx: self.dx,
            dy: self.dy,
            alpha: self.alpha,
            scale: self.scale,
            text: self.text,
            text_weight: self.text_weight,
            stroke_px: self.stroke_px,
            stroke_argb: self.stroke_argb,
            transition: self.transition,
            ..Default::default()
        }
    }
}

/// One keyframe as tag 8 wrote it: a time and a value, with the straight line
/// between them implied rather than stored.
///
/// `Serialize` is derived for the tag-8 test, which lays down the bytes a tag-8
/// build wrote.
#[derive(serde::Serialize, serde::Deserialize)]
struct KeyframeV8 {
    time_ms: i64,
    value_f32: f32,
}

impl KeyframeV8 {
    fn into_current(self) -> crate::model::Keyframe {
        crate::model::Keyframe::new(self.time_ms, self.value_f32)
    }
}

/// One cross-fade as tag 8 wrote it: the window and the two flags, with the
/// ramp shape fixed at the smoothstep the editor hard-coded.
#[derive(serde::Serialize, serde::Deserialize)]
struct TransitionV8 {
    start_ms: i64,
    duration_ms: i64,
    with_previous: bool,
    enabled: bool,
}

impl TransitionV8 {
    fn into_current(self) -> crate::model::Transition {
        crate::model::Transition {
            start_ms: self.start_ms,
            duration_ms: self.duration_ms,
            with_previous: self.with_previous,
            enabled: self.enabled,
            // The value that *is* the old hard-coded ramp, so those bytes keep
            // their exact look rather than acquiring a new one.
            ease: crate::ease::Ease::SMOOTH,
        }
    }
}

/// [`crate::model::LayerExtra`] as tag 8 wrote it: the same fields in the same
/// order, with the curves missing from every key and every ramp. Spelled out
/// rather than borrowed from the live struct, so the curve fields added
/// afterwards cannot silently rewrite what those bytes mean.
#[derive(serde::Serialize, serde::Deserialize)]
struct LayerExtraV8 {
    argb: Option<u32>,
    dx: f32,
    dy: f32,
    alpha: f32,
    scale: f32,
    text: Option<String>,
    text_weight: u16,
    stroke_px: f32,
    stroke_argb: Option<u32>,
    transition: Option<TransitionV8>,
    track_x: Vec<KeyframeV8>,
    track_y: Vec<KeyframeV8>,
    track_scale: Vec<KeyframeV8>,
    track_alpha: Vec<KeyframeV8>,
}

impl LayerExtraV8 {
    fn into_current(self) -> crate::model::LayerExtra {
        crate::model::LayerExtra {
            argb: self.argb,
            dx: self.dx,
            dy: self.dy,
            alpha: self.alpha,
            scale: self.scale,
            text: self.text,
            text_weight: self.text_weight,
            stroke_px: self.stroke_px,
            stroke_argb: self.stroke_argb,
            transition: self.transition.map(TransitionV8::into_current),
            track_x: self.track_x.into_iter().map(KeyframeV8::into_current).collect(),
            track_y: self.track_y.into_iter().map(KeyframeV8::into_current).collect(),
            track_scale: self
                .track_scale
                .into_iter()
                .map(KeyframeV8::into_current)
                .collect(),
            track_alpha: self
                .track_alpha
                .into_iter()
                .map(KeyframeV8::into_current)
                .collect(),
        }
    }
}

/// Layer shape at tag 8: the live layer with curves missing from its keys and
/// its ramp, and nothing else different.
#[derive(serde::Serialize, serde::Deserialize)]
struct LayerTag8 {
    id: uuid::Uuid,
    kind: crate::model::LayerKind,
    name: String,
    keyframes: Vec<KeyframeV8>,
    extra: Option<LayerExtraV8>,
    visible: bool,
    duration_ms: i64,
    start_ms: i64,
    uri: Option<String>,
    effects: Vec<crate::effect::EffectInstance>,
}

/// Project shape at tag 8: everything tag 9 has except the curves.
#[derive(serde::Deserialize)]
struct ProjectTag8 {
    schema_tag: u32,
    name: String,
    layers: Vec<LayerTag8>,
    custom: Vec<crate::effect::CustomEffect>,
    canvas: crate::model::Canvas,
}

impl ProjectTag8 {
    fn into_current(self) -> Project {
        Project {
            schema_tag: self.schema_tag,
            name: self.name,
            layers: self
                .layers
                .into_iter()
                .map(|l| crate::model::Layer {
                    id: l.id,
                    kind: l.kind,
                    name: l.name,
                    keyframes: l.keyframes.into_iter().map(KeyframeV8::into_current).collect(),
                    extra: l.extra.map(LayerExtraV8::into_current),
                    visible: l.visible,
                    duration_ms: l.duration_ms,
                    start_ms: l.start_ms,
                    uri: l.uri,
                    effects: l.effects,
                })
                .collect(),
            custom: self.custom,
            canvas: self.canvas,
        }
    }
}

/// Layer shape at tag 5: identical to the live [`crate::model::Layer`] minus
/// everything tag 6, 7 and 8 added, because only the project grew a `canvas`
/// field. Spelled out rather than borrowed so a later change to `Layer` cannot
/// silently rewrite what tag-5 bytes mean; `Serialize` is derived for the tag-5
/// test, which lays down those bytes.
#[derive(serde::Serialize, serde::Deserialize)]
struct LayerTag5 {
    id: uuid::Uuid,
    kind: crate::model::LayerKind,
    name: String,
    keyframes: Vec<crate::model::Keyframe>,
    extra: Option<LayerExtraV6>,
    visible: bool,
    duration_ms: i64,
    uri: Option<String>,
    effects: Vec<crate::effect::EffectInstance>,
}

/// Project shape at tag 5: the pre-canvas project. Its effect instances are the
/// tag-5 shape (they may name a project-defined effect), so they read through
/// the live struct.
#[derive(serde::Deserialize)]
struct ProjectTag5 {
    schema_tag: u32,
    name: String,
    layers: Vec<LayerTag5>,
    custom: Vec<crate::effect::CustomEffect>,
}

/// Layer shape at tag 6: the tag-5 layer, unchanged — only `LayerExtra` grew.
#[derive(serde::Deserialize)]
struct LayerTag6 {
    id: uuid::Uuid,
    kind: crate::model::LayerKind,
    name: String,
    keyframes: Vec<crate::model::Keyframe>,
    extra: Option<LayerExtraV6>,
    visible: bool,
    duration_ms: i64,
    uri: Option<String>,
    effects: Vec<crate::effect::EffectInstance>,
}

/// Project shape at tag 6: everything tag 7 has except the text weight and the
/// outline, which `LayerExtraV6` does not carry.
#[derive(serde::Deserialize)]
struct ProjectTag6 {
    schema_tag: u32,
    name: String,
    layers: Vec<LayerTag6>,
    custom: Vec<crate::effect::CustomEffect>,
    canvas: crate::model::Canvas,
}

/// Layer shape at tag 7: the tag-6 layer with the weight and the outline in its
/// `extra`, and with no place to put a `start_ms` or a property track — which is
/// why those two decode to 0 and empty. `Serialize` is derived for the tag-7
/// test, which lays down the bytes a tag-7 build wrote.
#[derive(serde::Serialize, serde::Deserialize)]
struct LayerTag7 {
    id: uuid::Uuid,
    kind: crate::model::LayerKind,
    name: String,
    keyframes: Vec<crate::model::Keyframe>,
    extra: Option<LayerExtraV7>,
    visible: bool,
    duration_ms: i64,
    uri: Option<String>,
    effects: Vec<crate::effect::EffectInstance>,
}

/// Project shape at tag 7: everything tag 8 has except the layer start time and
/// the four animated property tracks.
#[derive(serde::Deserialize)]
struct ProjectTag7 {
    schema_tag: u32,
    name: String,
    layers: Vec<LayerTag7>,
    custom: Vec<crate::effect::CustomEffect>,
    canvas: crate::model::Canvas,
}

impl ProjectTag1Old {
    fn into_current(self) -> Project {
        Project {
            schema_tag: self.schema_tag,
            name: self.name,
            layers: self
                .layers
                .into_iter()
                .map(|l| crate::model::Layer {
                    id: l.id,
                    kind: l.kind,
                    name: l.name,
                    keyframes: l.keyframes,
                    extra: None,
                    visible: true,
                    duration_ms: crate::model::DEFAULT_LAYER_DURATION_MS,
                    start_ms: 0,
                    uri: None,
                    effects: Vec::new(),
                })
                .collect(),
            custom: Vec::new(),
            canvas: crate::model::Canvas::default(),
        }
    }
}

impl ProjectTag1Extra {
    fn into_current(self) -> Project {
        Project {
            schema_tag: self.schema_tag,
            name: self.name,
            layers: self
                .layers
                .into_iter()
                .map(|l| crate::model::Layer {
                    id: l.id,
                    kind: l.kind,
                    name: l.name,
                    keyframes: l.keyframes,
                    extra: l.extra.map(ExtraTag2::into_current),
                    visible: l.visible,
                    duration_ms: l.duration_ms,
                    // Tags ≤ 7 have no start time: the layer sat at frame 0.
                    start_ms: 0,
                    uri: l.uri,
                    effects: Vec::new(),
                })
                .collect(),
            custom: Vec::new(),
            canvas: crate::model::Canvas::default(),
        }
    }
}

impl ProjectTag2 {
    fn into_current(self) -> Project {
        Project {
            schema_tag: self.schema_tag,
            name: self.name,
            layers: self
                .layers
                .into_iter()
                .map(|l| crate::model::Layer {
                    id: l.id,
                    kind: l.kind,
                    name: l.name,
                    keyframes: l.keyframes,
                    extra: l.extra.map(ExtraTag2::into_current),
                    visible: l.visible,
                    duration_ms: l.duration_ms,
                    // Tags ≤ 7 have no start time: the layer sat at frame 0.
                    start_ms: 0,
                    uri: l.uri,
                    effects: l
                        .effects
                        .into_iter()
                        .map(EffectInstanceV4::into_current)
                        .collect(),
                })
                .collect(),
            custom: Vec::new(),
            canvas: crate::model::Canvas::default(),
        }
    }
}

impl ProjectTag3 {
    fn into_current(self) -> Project {
        Project {
            schema_tag: self.schema_tag,
            name: self.name,
            layers: self
                .layers
                .into_iter()
                .map(|l| crate::model::Layer {
                    id: l.id,
                    kind: l.kind,
                    name: l.name,
                    keyframes: l.keyframes,
                    extra: l.extra.map(ExtraTag3::into_current),
                    visible: l.visible,
                    duration_ms: l.duration_ms,
                    // Tags ≤ 7 have no start time: the layer sat at frame 0.
                    start_ms: 0,
                    uri: l.uri,
                    effects: l
                        .effects
                        .into_iter()
                        .map(EffectInstanceV4::into_current)
                        .collect(),
                })
                .collect(),
            custom: Vec::new(),
            canvas: crate::model::Canvas::default(),
        }
    }
}

impl ProjectTag4 {
    fn into_current(self) -> Project {
        Project {
            schema_tag: self.schema_tag,
            name: self.name,
            layers: self
                .layers
                .into_iter()
                .map(|l| crate::model::Layer {
                    id: l.id,
                    kind: l.kind,
                    name: l.name,
                    keyframes: l.keyframes,
                    extra: l.extra.map(LayerExtraV6::into_current),
                    visible: l.visible,
                    duration_ms: l.duration_ms,
                    // Tags ≤ 7 have no start time: the layer sat at frame 0.
                    start_ms: 0,
                    uri: l.uri,
                    effects: l
                        .effects
                        .into_iter()
                        .map(EffectInstanceV4::into_current)
                        .collect(),
                })
                .collect(),
            // Tag 4 predates project-defined effects entirely.
            custom: Vec::new(),
            canvas: crate::model::Canvas::default(),
        }
    }
}

impl ProjectTag5 {
    fn into_current(self) -> Project {
        Project {
            schema_tag: self.schema_tag,
            name: self.name,
            layers: self
                .layers
                .into_iter()
                .map(|l| crate::model::Layer {
                    id: l.id,
                    kind: l.kind,
                    name: l.name,
                    keyframes: l.keyframes,
                    extra: l.extra.map(LayerExtraV6::into_current),
                    visible: l.visible,
                    duration_ms: l.duration_ms,
                    // Tags ≤ 7 have no start time: the layer sat at frame 0.
                    start_ms: 0,
                    uri: l.uri,
                    effects: l.effects,
                })
                .collect(),
            custom: self.custom,
            // Tag 5 predates the project canvas.
            canvas: crate::model::Canvas::default(),
        }
    }
}

impl ProjectTag6 {
    fn into_current(self) -> Project {
        Project {
            schema_tag: self.schema_tag,
            name: self.name,
            layers: self
                .layers
                .into_iter()
                .map(|l| crate::model::Layer {
                    id: l.id,
                    kind: l.kind,
                    name: l.name,
                    keyframes: l.keyframes,
                    // Tag 6 predates the text weight and the outline; the
                    // legacy extra fills them with the regular face and no
                    // contour.
                    extra: l.extra.map(LayerExtraV6::into_current),
                    visible: l.visible,
                    duration_ms: l.duration_ms,
                    // Tags ≤ 7 have no start time: the layer sat at frame 0.
                    start_ms: 0,
                    uri: l.uri,
                    effects: l.effects,
                })
                .collect(),
            custom: self.custom,
            canvas: self.canvas,
        }
    }
}

impl ProjectTag7 {
    fn into_current(self) -> Project {
        Project {
            schema_tag: self.schema_tag,
            name: self.name,
            layers: self
                .layers
                .into_iter()
                .map(|l| crate::model::Layer {
                    id: l.id,
                    kind: l.kind,
                    name: l.name,
                    keyframes: l.keyframes,
                    // Tag 7 kept the weight and the outline; the legacy extra
                    // fills the animated tracks with "not animated", which is
                    // what an empty track must mean.
                    extra: l.extra.map(LayerExtraV7::into_current),
                    visible: l.visible,
                    duration_ms: l.duration_ms,
                    // Tag 7 predates the timeline position: the layer sat at the
                    // start of the project.
                    start_ms: 0,
                    uri: l.uri,
                    effects: l.effects,
                })
                .collect(),
            custom: self.custom,
            canvas: self.canvas,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{EffectInstance, EffectKind, EffectTarget};
    use crate::model::{Keyframe, Layer, LayerExtra, LayerKind};
    use uuid::Uuid;

    fn layer(name: &str) -> Layer {
        Layer {
            id: Uuid::new_v4(),
            kind: LayerKind::Shape,
            name: name.to_string(),
            keyframes: Vec::new(),
            extra: None,
            visible: true,
            duration_ms: crate::model::DEFAULT_LAYER_DURATION_MS,
            start_ms: 0,
            uri: None,
            effects: Vec::new(),
        }
    }

    /// One effect instance as the pre-tag-5 writers laid it out. Built from the
    /// live defaults so the fixture describes bytes a real build would have
    /// written, not a hand-invented parameter vector.
    fn wire_v4(kind: EffectKind) -> EffectInstanceV4 {
        let inst = EffectInstance::new(kind);
        EffectInstanceV4 {
            id: inst.id,
            kind,
            enabled: inst.enabled,
            params: inst.params,
        }
    }

    /// A minimal structurally-sound project-defined effect, for the codec tests
    /// that carry one in a project document.
    fn custom_effect(id: &str) -> crate::effect::CustomEffect {
        use crate::effect::{CustomParam, CustomPass, EffectSpace, ParamKind};
        crate::effect::CustomEffect {
            id: id.to_string(),
            label: "Warp".to_string(),
            space: EffectSpace::Display,
            passes: vec![CustomPass {
                entry: "fs_main".to_string(),
                shrink: 0,
            }],
            params: vec![CustomParam {
                key: "amount".to_string(),
                label: "Amount".to_string(),
                kind: ParamKind::Float,
                min: 0.0,
                max: 10.0,
                default: vec![1.0],
                unit: String::new(),
                choices: Vec::new(),
            }],
            source: "// wgsl under test; rumo-core never parses it".to_string(),
        }
    }

    #[test]
    fn roundtrip_empty() {
        let project = Project::new("empty");
        let bytes = encode(&project);
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(project, decoded);
    }

    #[test]
    fn roundtrip_layer_keyframes() {
        let mut project = Project::new("one layer");
        let mut l = layer("shape 1");
        l.keyframes = vec![
            Keyframe {
                time_ms: 0,
                value_f32: 0.0,
                ease: crate::ease::Ease::Linear,
            },
            Keyframe {
                time_ms: 500,
                value_f32: 1.0,
                ease: crate::ease::Ease::Linear,
            },
        ];
        project.layers.push(l);
        let bytes = encode(&project);
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(project, decoded);
    }

    #[test]
    fn roundtrip_effect_chain() {
        let mut project = Project::new("fx");
        let mut l = layer("blurred");
        l.effects = vec![
            EffectInstance::new(EffectKind::Blur),
            EffectInstance::new(EffectKind::Glow),
        ];
        l.effects[0].set("radius", &[42.0]);
        l.effects[1].set("intensity", &[2.5]);
        l.extra = Some(LayerExtra {
            argb: Some(0xff00_00ff),
            dx: 3.0,
            dy: -4.0,
            alpha: 0.75,
            scale: 2.5,
            text: Some("hello".to_string()),
            text_weight: 700,
            stroke_px: 3.0,
            stroke_argb: Some(0xff10_1010),
            transition: Some(crate::model::Transition {
                start_ms: 250,
                duration_ms: 750,
                with_previous: false,
                enabled: false,
                ease: crate::ease::Ease::SMOOTH,
            }),
            track_x: vec![
                Keyframe {
                    time_ms: 0,
                    value_f32: 0.0,
                ease: crate::ease::Ease::Linear,
            },
                Keyframe {
                    time_ms: 1000,
                    value_f32: 40.0,
                ease: crate::ease::Ease::Linear,
            },
            ],
            track_scale: vec![Keyframe {
                time_ms: 500,
                value_f32: 2.0,
                ease: crate::ease::Ease::Linear,
            }],
            track_y: Vec::new(),
            track_alpha: Vec::new(),
        });
        l.uri = Some("content://clip".to_string());
        l.start_ms = 750;
        project.layers.push(l);

        let bytes = encode(&project);
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(project, decoded);
        assert_eq!(decoded.layers[0].effects.len(), 2);
        assert_eq!(
            decoded.layers[0].effects[0].target,
            EffectTarget::Builtin(EffectKind::Blur)
        );
        assert_eq!(decoded.layers[0].effects[0].get("radius"), Some(&[42.0f32][..]));
        assert_eq!(decoded.layers[0].start_ms, 750, "the timeline position survives");
        let extra = decoded.layers[0].extra.clone().expect("extra");
        assert_eq!(extra.scale, 2.5, "scale must survive the .rumo round-trip");
        assert_eq!(extra.text.as_deref(), Some("hello"));
        assert_eq!(extra.track_x.len(), 2, "an animated track must survive");
        assert_eq!(extra.track_scale.len(), 1);
        assert!(extra.track_y.is_empty());
        let tr = extra.transition.expect("transition must survive the round-trip");
        assert_eq!(tr.start_ms, 250);
        assert_eq!(tr.duration_ms, 750);
        assert!(!tr.with_previous);
        assert!(!tr.enabled);
    }

    #[test]
    fn reject_unknown_tag() {
        let mut bytes = postcard::to_extend(&999u32, Vec::new()).expect("tag encode");
        bytes = postcard::to_extend(&Project::new("x"), bytes).expect("project encode");
        match decode(&bytes) {
            Err(CodecError::UnsupportedTag(got)) => assert_eq!(got, 999),
            other => panic!("expected UnsupportedTag, got {other:?}"),
        }
        let mut zero = postcard::to_extend(&0u32, Vec::new()).expect("tag encode");
        zero = postcard::to_extend(&Project::new("x"), zero).expect("project encode");
        assert!(matches!(decode(&zero), Err(CodecError::UnsupportedTag(0))));
    }

    #[test]
    fn empty_bytes_err() {
        assert!(decode(&[]).is_err());
    }

    #[test]
    fn pre_effects_bytes_decode_with_empty_chain() {
        #[derive(serde::Serialize)]
        struct LayerV2 {
            id: Uuid,
            kind: LayerKind,
            name: String,
            keyframes: Vec<Keyframe>,
            extra: Option<WireExtraTag2>,
            visible: bool,
            duration_ms: i64,
            uri: Option<String>,
        }
        #[derive(serde::Serialize)]
        struct ProjectTag1ExtraWire {
            schema_tag: u32,
            name: String,
            layers: Vec<LayerV2>,
        }
        let old = ProjectTag1ExtraWire {
            schema_tag: MIN_TAG,
            name: "before effects".to_string(),
            layers: (0..3)
                .map(|i| LayerV2 {
                    id: Uuid::new_v4(),
                    kind: LayerKind::Media,
                    name: format!("clip {i}"),
                    keyframes: vec![Keyframe {
                        time_ms: 10,
                        value_f32: 1.0,
                ease: crate::ease::Ease::Linear,
            }],
                    extra: Some(WireExtraTag2 {
                        argb: Some(0xff11_2233),
                        dx: 1.0,
                        dy: 2.0,
                        alpha: 0.5,
                    }),
                    visible: false,
                    duration_ms: 4321,
                    uri: Some(format!("content://{i}")),
                })
                .collect(),
        };
        let buf = postcard::to_extend(&MIN_TAG, Vec::new()).expect("tag");
        let bytes = postcard::to_extend(&old, buf).expect("v2 body");

        let decoded = decode(&bytes).expect("v2 bytes must decode via the ladder");
        assert_eq!(decoded.layers.len(), 3);
        for (i, l) in decoded.layers.iter().enumerate() {
            assert!(l.effects.is_empty(), "layer {i} must start with no effects");
            assert!(!l.visible);
            assert_eq!(l.duration_ms, 4321);
            assert_eq!(l.uri.as_deref(), Some(format!("content://{i}").as_str()));
            let extra = l.extra.clone().expect("extra");
            assert_eq!(extra.argb, Some(0xff11_2233));
            assert_eq!(extra.scale, 1.0, "tag-1 bytes predate the preview scale");
            assert_eq!(extra.transition, None, "and predate the cross-fade");
        }
    }

    #[test]
    fn tag2_bytes_keep_their_effect_chain() {
        #[derive(serde::Serialize)]
        struct LayerV3 {
            id: Uuid,
            kind: LayerKind,
            name: String,
            keyframes: Vec<Keyframe>,
            extra: Option<WireExtraTag2>,
            visible: bool,
            duration_ms: i64,
            uri: Option<String>,
            effects: Vec<EffectInstanceV4>,
        }
        #[derive(serde::Serialize)]
        struct ProjectTag2Wire {
            schema_tag: u32,
            name: String,
            layers: Vec<LayerV3>,
        }
        let old = ProjectTag2Wire {
            schema_tag: 2,
            name: "effects, no scale".to_string(),
            layers: vec![LayerV3 {
                id: Uuid::new_v4(),
                kind: LayerKind::Media,
                name: "clip".to_string(),
                keyframes: Vec::new(),
                extra: Some(WireExtraTag2 {
                    argb: Some(0xff11_2233),
                    dx: 1.0,
                    dy: 2.0,
                    alpha: 0.5,
                }),
                visible: true,
                duration_ms: 2000,
                uri: Some("content://clip".to_string()),
                effects: vec![wire_v4(EffectKind::Blur)],
            }],
        };
        let buf = postcard::to_extend(&2u32, Vec::new()).expect("tag");
        let bytes = postcard::to_extend(&old, buf).expect("v3 body");

        // Tag 2 is dispatched on, not guessed: this must not fall through to
        // the tag-1 ladder and lose the chain.
        let decoded = decode(&bytes).expect("tag-2 bytes must decode");
        assert_eq!(decoded.layers.len(), 1);
        assert_eq!(decoded.layers[0].effects.len(), 1);
        assert_eq!(
            decoded.layers[0].effects[0].target,
            EffectTarget::Builtin(EffectKind::Blur)
        );
        let extra = decoded.layers[0].extra.clone().expect("extra");
        assert_eq!(extra.dx, 1.0);
        assert_eq!(extra.alpha, 0.5);
        assert_eq!(extra.scale, 1.0);
        assert_eq!(extra.text, None);
        assert_eq!(extra.transition, None);
    }

    #[test]
    fn tag3_bytes_keep_scale_and_text_without_a_transition() {
        #[derive(serde::Serialize)]
        struct LayerTag3Wire {
            id: Uuid,
            kind: LayerKind,
            name: String,
            keyframes: Vec<Keyframe>,
            extra: Option<WireExtraTag3>,
            visible: bool,
            duration_ms: i64,
            uri: Option<String>,
            effects: Vec<EffectInstanceV4>,
        }
        #[derive(serde::Serialize)]
        struct ProjectTag3Wire {
            schema_tag: u32,
            name: String,
            layers: Vec<LayerTag3Wire>,
        }
        let old = ProjectTag3Wire {
            schema_tag: 3,
            name: "scale and text, no cross-fade".to_string(),
            layers: vec![LayerTag3Wire {
                id: Uuid::new_v4(),
                kind: LayerKind::Text,
                name: "Title".to_string(),
                keyframes: Vec::new(),
                extra: Some(WireExtraTag3 {
                    argb: Some(0xff22_3344),
                    dx: -7.0,
                    dy: 8.0,
                    alpha: 0.25,
                    scale: 3.0,
                    text: Some("caption".to_string()),
                }),
                visible: true,
                duration_ms: 1500,
                uri: None,
                effects: vec![wire_v4(EffectKind::Glow)],
            }],
        };
        let buf = postcard::to_extend(&3u32, Vec::new()).expect("tag");
        let bytes = postcard::to_extend(&old, buf).expect("tag-3 body");

        // Tag 3 is dispatched on: reading it with the live struct would eat the
        // `transition` byte out of the next field.
        let decoded = decode(&bytes).expect("tag-3 bytes must decode");
        assert_eq!(decoded.layers.len(), 1);
        let extra = decoded.layers[0].extra.clone().expect("extra");
        assert_eq!(extra.dx, -7.0);
        assert!((extra.alpha - 0.25).abs() < 1e-6);
        assert_eq!(extra.scale, 3.0);
        assert_eq!(extra.text.as_deref(), Some("caption"));
        assert_eq!(extra.transition, None);
        assert_eq!(decoded.layers[0].effects.len(), 1);
        assert_eq!(
            decoded.layers[0].effects[0].target,
            EffectTarget::Builtin(EffectKind::Glow)
        );
    }

    #[test]
    fn tag4_bytes_keep_their_effect_chain() {
        // Tag 4 wrote the `extra` with `transition` but without the text
        // weight and the outline, and the pre-tag-5 effect shape, so this
        // fixture is the pre-tag-7 extra plus a legacy instance — exactly the
        // bytes a tag-4 build emitted.
        #[derive(serde::Serialize)]
        struct LayerTag4Wire {
            id: Uuid,
            kind: LayerKind,
            name: String,
            keyframes: Vec<Keyframe>,
            extra: Option<LayerExtraV6>,
            visible: bool,
            duration_ms: i64,
            uri: Option<String>,
            effects: Vec<EffectInstanceV4>,
        }
        #[derive(serde::Serialize)]
        struct ProjectTag4Wire {
            schema_tag: u32,
            name: String,
            layers: Vec<LayerTag4Wire>,
        }
        let old = ProjectTag4Wire {
            schema_tag: 4,
            name: "cross-fade, no custom effects".to_string(),
            layers: vec![LayerTag4Wire {
                id: Uuid::new_v4(),
                kind: LayerKind::Media,
                name: "clip".to_string(),
                keyframes: Vec::new(),
                extra: Some(LayerExtraV6 {
                    argb: Some(0xff33_4455),
                    dx: 2.0,
                    dy: -2.0,
                    alpha: 0.5,
                    scale: 1.5,
                    text: None,
                    transition: Some(crate::model::Transition {
                        start_ms: 100,
                        duration_ms: 400,
                        with_previous: false,
                        enabled: true,
                ease: crate::ease::Ease::SMOOTH,
            }),
                }),
                visible: true,
                duration_ms: 3000,
                uri: Some("content://clip".to_string()),
                effects: vec![wire_v4(EffectKind::Blur)],
            }],
        };
        let buf = postcard::to_extend(&4u32, Vec::new()).expect("tag");
        let bytes = postcard::to_extend(&old, buf).expect("tag-4 body");

        // Reading a tag-4 blob with the live effect struct would try to consume
        // a `custom` slot that the writer never laid down.
        let decoded = decode(&bytes).expect("tag-4 bytes must decode");
        assert_eq!(decoded.layers.len(), 1);
        assert_eq!(decoded.layers[0].effects.len(), 1);
        assert_eq!(
            decoded.layers[0].effects[0].target,
            EffectTarget::Builtin(EffectKind::Blur)
        );
        assert!(decoded.custom.is_empty(), "tag 4 predates custom effects");
        let extra = decoded.layers[0].extra.clone().expect("extra");
        assert_eq!(extra.scale, 1.5);
        let tr = extra.transition.expect("tag-4 bytes carry the cross-fade");
        assert_eq!(tr.start_ms, 100);
        assert_eq!(tr.duration_ms, 400);
    }

    #[test]
    fn tag6_bytes_decode_with_the_regular_face_and_no_outline() {
        // Tag 6 wrote the canvas but no text weight and no outline, so its
        // bytes are a tag-6 project struct whose layers carry the pre-tag-7
        // `extra` — exactly what a tag-6 build laid down.
        #[derive(serde::Serialize)]
        struct LayerTag6Wire {
            id: Uuid,
            kind: LayerKind,
            name: String,
            keyframes: Vec<Keyframe>,
            extra: Option<LayerExtraV6>,
            visible: bool,
            duration_ms: i64,
            uri: Option<String>,
            effects: Vec<EffectInstance>,
        }
        #[derive(serde::Serialize)]
        struct ProjectTag6Wire {
            schema_tag: u32,
            name: String,
            layers: Vec<LayerTag6Wire>,
            custom: Vec<crate::effect::CustomEffect>,
            canvas: crate::model::Canvas,
        }
        let old = ProjectTag6Wire {
            schema_tag: 6,
            name: "before the weight".to_string(),
            layers: vec![LayerTag6Wire {
                id: Uuid::new_v4(),
                kind: LayerKind::Text,
                name: "Title".to_string(),
                keyframes: Vec::new(),
                extra: Some(LayerExtraV6 {
                    argb: Some(0xffff_ffff),
                    dx: 1.0,
                    dy: 2.0,
                    alpha: 1.0,
                    scale: 1.0,
                    text: Some("Hello".to_string()),
                    transition: None,
                }),
                visible: true,
                duration_ms: 5000,
                uri: None,
                effects: Vec::new(),
            }],
            custom: Vec::new(),
            canvas: crate::model::Canvas {
                width: 1080,
                height: 1920,
                background: 0xff00_0000,
            },
        };
        let buf = postcard::to_extend(&6u32, Vec::new()).expect("tag");
        let bytes = postcard::to_extend(&old, buf).expect("tag-6 body");

        let decoded = decode(&bytes).expect("tag-6 bytes must decode");
        assert_eq!(decoded.canvas.width, 1080, "the canvas survives the ladder");
        assert_eq!(decoded.canvas.height, 1920);
        let extra = decoded.layers[0]
            .extra
            .clone()
            .expect("the layer keeps its props");
        assert_eq!(extra.text.as_deref(), Some("Hello"));
        assert_eq!(extra.text_weight, 400, "an old text layer is regular");
        assert_eq!(extra.stroke_px, 0.0, "and has no outline");
        assert_eq!(extra.stroke_argb, None);
        // Tag 6 has no place to keep a timeline position or an animated track.
        assert_eq!(decoded.layers[0].start_ms, 0, "tag 6 sits at the first frame");
        assert!(extra.track_x.is_empty() && extra.track_alpha.is_empty());
    }

    #[test]
    fn tag7_bytes_decode_with_the_weight_and_no_timeline_position() {
        // Tag 7 wrote the weight and the outline but no `start_ms` and no
        // animated track, so its bytes are a tag-7 project struct whose layers
        // carry the pre-tag-8 `extra` — exactly what a tag-7 build laid down.
        let old = ProjectTag7Wire {
            schema_tag: 7,
            name: "before the timeline".to_string(),
            layers: vec![LayerTag7 {
                id: Uuid::new_v4(),
                kind: LayerKind::Shape,
                name: "moved later".to_string(),
                keyframes: Vec::new(),
                extra: Some(LayerExtraV7 {
                    argb: Some(0xff44_5566),
                    dx: 5.0,
                    dy: 6.0,
                    alpha: 0.5,
                    scale: 1.25,
                    text: None,
                    text_weight: 700,
                    stroke_px: 2.0,
                    stroke_argb: Some(0xff00_00ff),
                    transition: None,
                }),
                visible: true,
                duration_ms: 4000,
                uri: None,
                effects: vec![EffectInstance::new(EffectKind::Blur)],
            }],
            custom: Vec::new(),
            canvas: crate::model::Canvas {
                width: 1080,
                height: 1080,
                background: 0xff11_2233,
            },
        };
        let buf = postcard::to_extend(&7u32, Vec::new()).expect("tag");
        let bytes = postcard::to_extend(&old, buf).expect("tag-7 body");

        // Reading a tag-7 blob with the live struct would ask for a `start_ms`
        // and four tracks the writer never laid down.
        let decoded = decode(&bytes).expect("tag-7 bytes must decode");
        assert_eq!(decoded.canvas.width, 1080);
        let l = &decoded.layers[0];
        assert_eq!(l.start_ms, 0, "a tag-7 layer sat at the start of the project");
        assert_eq!(l.effects.len(), 1, "and keeps its chain");
        assert_eq!(
            l.effects[0].target,
            EffectTarget::Builtin(EffectKind::Blur)
        );
        let extra = l.extra.clone().expect("the layer keeps its props");
        assert_eq!(extra.text_weight, 700, "the weight survives the ladder");
        assert_eq!(extra.stroke_px, 2.0, "and so does the outline");
        assert_eq!(extra.stroke_argb, Some(0xff00_00ff));
        // Empty is the only safe reading here: an empty track means "the
        // property is not animated", not "the property is zero".
        assert!(extra.track_x.is_empty(), "no track_x before tag 8");
        assert!(extra.track_y.is_empty(), "no track_y before tag 8");
        assert!(extra.track_scale.is_empty(), "no track_scale before tag 8");
        assert!(extra.track_alpha.is_empty(), "no track_alpha before tag 8");
        // And the defaults those empties stand for are the layer's own bases.
        assert_eq!(l.sample_scale(1234), 1.25);
        assert_eq!(l.sample_alpha(1234), 0.5);
    }

    /// Bytes a tag-8 build wrote carry no curve anywhere, and they must decode
    /// as the exact arithmetic they were written with: the straight line between
    /// two keys, and the smoothstep ramp the editor hard-coded.
    ///
    /// This is the check that the *reader* is right, not the writer: the bytes
    /// are laid down by the tag-8 structs, which cannot know about curves.
    #[test]
    fn tag8_bytes_decode_with_straight_lines() {
        let old = ProjectTag8Wire {
            schema_tag: 8,
            name: "before the curves".to_string(),
            layers: vec![LayerTag8 {
                id: Uuid::new_v4(),
                kind: LayerKind::Shape,
                name: "animated".to_string(),
                keyframes: vec![KeyframeV8 {
                    time_ms: 0,
                    value_f32: 0.25,
                }],
                extra: Some(LayerExtraV8 {
                    argb: Some(0xff44_5566),
                    dx: 1.0,
                    dy: 2.0,
                    alpha: 1.0,
                    scale: 1.0,
                    text: None,
                    text_weight: 400,
                    stroke_px: 0.0,
                    stroke_argb: None,
                    transition: Some(TransitionV8 {
                        start_ms: 0,
                        duration_ms: 1000,
                        with_previous: true,
                        enabled: true,
                    }),
                    track_x: vec![
                        KeyframeV8 {
                            time_ms: 0,
                            value_f32: 0.0,
                        },
                        KeyframeV8 {
                            time_ms: 1000,
                            value_f32: 100.0,
                        },
                    ],
                    track_y: Vec::new(),
                    track_scale: Vec::new(),
                    track_alpha: Vec::new(),
                }),
                visible: true,
                duration_ms: 4000,
                start_ms: 500,
                uri: None,
                effects: Vec::new(),
            }],
            custom: Vec::new(),
            canvas: crate::model::Canvas {
                width: 1080,
                height: 1080,
                background: 0xff11_2233,
            },
        };
        let buf = postcard::to_extend(&8u32, Vec::new()).expect("tag");
        let bytes = postcard::to_extend(&old, buf).expect("tag-8 body");

        let decoded = decode(&bytes).expect("tag-8 bytes must decode");
        let l = &decoded.layers[0];
        assert_eq!(l.start_ms, 500, "the timeline position survives");
        let extra = l.extra.clone().expect("props survive");
        assert!(
            extra.track_x.iter().all(|k| k.ease == crate::ease::Ease::Linear),
            "a track written before curves must read as the straight line"
        );
        // The point of the default: the sampled values are exactly what a tag-8
        // build computed, not merely close.
        assert_eq!(l.sample_x(500), 50.0, "halfway along a linear track");
        assert_eq!(l.sample_x(1000), 100.0);
        let transition = extra.transition.expect("the cross-fade survives");
        assert_eq!(transition.ease, crate::ease::Ease::SMOOTH);
        // And its ramp is the old smoothstep to the last bit, which is what
        // makes this a backward-compatibility guarantee rather than a hope.
        let p = 0.37f32;
        let legacy = p * p * (3.0 - 2.0 * p);
        assert!((transition.ramp_at(370) - legacy).abs() < 1e-5);
    }

    /// The tag-8 project as a tag-8 build wrote it: the private reader structs
    /// are the serialize side on purpose, so a later change to the live
    /// `Project`/`Layer`/`LayerExtra` cannot rewrite what these bytes mean.
    #[derive(serde::Serialize)]
    struct ProjectTag8Wire {
        schema_tag: u32,
        name: String,
        layers: Vec<LayerTag8>,
        custom: Vec<crate::effect::CustomEffect>,
        canvas: crate::model::Canvas,
    }

    /// The tag-7 project as a tag-7 build wrote it: the private reader structs
    /// are the serialize side on purpose, so a later change to the live
    /// `Project`/`Layer`/`LayerExtra` cannot rewrite what these bytes mean.
    #[derive(serde::Serialize)]
    struct ProjectTag7Wire {
        schema_tag: u32,
        name: String,
        layers: Vec<LayerTag7>,
        custom: Vec<crate::effect::CustomEffect>,
        canvas: crate::model::Canvas,
    }

    #[test]
    fn tag5_bytes_decode_with_the_default_canvas() {
        // Tag 5 wrote the live layer shape (project-defined effects included)
        // and no canvas, so its bytes are a tag-5 project struct with the tag-5
        // layer — which is why the fixture names that struct and not the live one.
        #[derive(serde::Serialize)]
        struct ProjectTag5Wire {
            schema_tag: u32,
            name: String,
            layers: Vec<LayerTag5>,
            custom: Vec<crate::effect::CustomEffect>,
        }
        let effect = custom_effect("warp_x");
        let mut custom = EffectInstance::custom(&effect).expect("custom layout");
        assert!(custom.set("amount", &[3.5]));
        let wire_layer = |name: &str, effects: Vec<EffectInstance>| LayerTag5 {
            id: Uuid::new_v4(),
            kind: LayerKind::Shape,
            name: name.to_string(),
            keyframes: Vec::new(),
            extra: None,
            visible: true,
            duration_ms: crate::model::DEFAULT_LAYER_DURATION_MS,
            uri: None,
            effects,
        };
        let old = ProjectTag5Wire {
            schema_tag: 5,
            name: "before the canvas".to_string(),
            layers: vec![
                wire_layer("blurred", vec![EffectInstance::new(EffectKind::Glow)]),
                wire_layer("warped", vec![custom]),
            ],
            custom: vec![effect],
        };
        let buf = postcard::to_extend(&5u32, Vec::new()).expect("tag");
        let bytes = postcard::to_extend(&old, buf).expect("tag-5 body");

        // Reading a tag-5 blob with the live `Project` would ask for a canvas
        // the writer never laid down.
        let decoded = decode(&bytes).expect("tag-5 bytes must decode");
        assert_eq!(decoded.canvas, crate::model::Canvas::default());
        assert_eq!(decoded.layers[0].start_ms, 0, "tag 5 sat at the first frame");
        assert_eq!(decoded.custom.len(), 1);
        assert_eq!(
            decoded.layers[0].effects[0].target,
            EffectTarget::Builtin(EffectKind::Glow)
        );
        assert_eq!(
            decoded.layers[1].effects[0].target,
            EffectTarget::Custom("warp_x".to_string())
        );
        assert_eq!(decoded.layers[1].effects[0].params, vec![3.5]);
    }

    #[test]
    fn current_tag_roundtrips_a_non_default_canvas() {
        let mut project = Project::new("vertical");
        project.canvas = crate::model::Canvas {
            width: 1080,
            height: 1920,
            background: 0xFF00_FF00,
        };
        let decoded = decode(&encode(&project)).expect("decode");
        assert_eq!(decoded.canvas, project.canvas);
        assert_eq!(project, decoded);
    }

    #[test]
    fn current_tag_roundtrips_a_project_defined_instance() {
        // The definition rides in the project document; the instance carries
        // only its id and its values. Encoding the instance alone would leave
        // the decoder nothing to rebuild the layout from.
        let effect = custom_effect("warp_x");
        let mut inst = EffectInstance::custom(&effect).expect("custom layout");
        assert!(inst.set("amount", &[3.5]), "amount is one slot");
        let mut project = Project::new("custom fx");
        let mut l = layer("warped");
        l.effects = vec![inst];
        project.layers.push(l);
        project.custom = vec![effect];

        let bytes = encode(&project);
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(decoded.custom.len(), 1);
        assert_eq!(decoded.custom[0].id, "warp_x");
        assert_eq!(decoded.custom[0].source, project.custom[0].source);
        assert_eq!(decoded.layers[0].effects.len(), 1);
        let got = &decoded.layers[0].effects[0];
        assert_eq!(got.target, EffectTarget::Custom("warp_x".to_string()));
        assert_eq!(got.params, vec![3.5], "values survive the round-trip");
    }

    #[test]
    fn built_in_instances_still_write_the_plain_shape() {
        // The `custom` slot is present for every instance, so a bug that fills
        // it in for a built-in would turn every chain into a custom one.
        let mut project = Project::new("builtin");
        let mut l = layer("blurred");
        l.effects = vec![EffectInstance::new(EffectKind::Glow)];
        project.layers.push(l);

        let decoded = decode(&encode(&project)).expect("decode");
        assert_eq!(
            decoded.layers[0].effects[0].target,
            EffectTarget::Builtin(EffectKind::Glow)
        );
        assert!(decoded.custom.is_empty());
    }

    /// `LayerExtra` as the tag-1/tag-2 writers laid it out: four fields, no
    /// `scale`/`text`/`transition`. Spelled out per test so a change to the live
    /// struct cannot silently rewrite what "old bytes" mean.
    #[derive(serde::Serialize)]
    struct WireExtraTag2 {
        argb: Option<u32>,
        dx: f32,
        dy: f32,
        alpha: f32,
    }

    /// `LayerExtra` as the tag-3 writer laid it out: `scale` and `text`, no
    /// `transition`.
    #[derive(serde::Serialize)]
    struct WireExtraTag3 {
        argb: Option<u32>,
        dx: f32,
        dy: f32,
        alpha: f32,
        scale: f32,
        text: Option<String>,
    }
}
