//! The project model: everything stored in `qrec.json` (schema v1, SPEC §6.3).
//!
//! Times are integer frames at the canvas frame rate; clips use half-open
//! ranges `[in, out)`. UI state (selection, playhead, zoom) is not part of
//! the project; it lives in `.qrec/ui.json` (see `state::UiState`).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::CoreError;
use crate::ids::{ClipId, Frame, LayerId, TakeId, TrackId};
use crate::validate::MAX_FPS;

/// The schema version this build reads and writes.
pub const CURRENT_VERSION: u32 = 1;

/// The project file name inside a project folder.
pub const PROJECT_FILE: &str = "qrec.json";

fn is_false(b: &bool) -> bool {
    !*b
}

/// A project: one folder with `qrec.json` and its media.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Project {
    pub version: u32,
    /// Defaults to the folder name.
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub canvas: Canvas,
    /// The project scene, used for new takes.
    pub scene: Scene,
    pub recording: RecordingSettings,
    pub autocut: AutocutSettings,
    /// The media pool. Append-only facts about the disk; not undoable (SPEC §8.2).
    pub takes: BTreeMap<TakeId, Take>,
    /// The next take number. Never reused; not undoable.
    pub next_take_number: u32,
    pub timeline: Timeline,
}

/// Values a new project starts from (user config, SPEC §6.4).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectDefaults {
    pub canvas: Canvas,
    pub recording: RecordingSettings,
    pub autocut: AutocutSettings,
}

impl Project {
    /// A new, empty project. `created_at` is truncated to whole seconds.
    pub fn new(name: impl Into<String>, created_at: DateTime<Utc>, defaults: &ProjectDefaults) -> Self {
        use chrono::SubsecRound;
        Self {
            version: CURRENT_VERSION,
            name: name.into(),
            created_at: created_at.trunc_subsecs(0),
            canvas: defaults.canvas.clone(),
            scene: Scene::default(),
            recording: defaults.recording.clone(),
            autocut: defaults.autocut.clone(),
            takes: BTreeMap::new(),
            next_take_number: 1,
            timeline: Timeline::default(),
        }
    }

    pub fn take(&self, id: &TakeId) -> Option<&Take> {
        self.takes.get(id)
    }

    pub fn clip_index(&self, id: &ClipId) -> Option<usize> {
        self.timeline.clips.iter().position(|c| &c.id == id)
    }

    pub fn clip(&self, id: &ClipId) -> Option<&Clip> {
        self.timeline.clips.iter().find(|c| &c.id == id)
    }
}

// ---------------------------------------------------------------- canvas

/// The output frame: size, frame rate and background.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub fps: Fps,
    pub background: Color,
}

impl Default for Canvas {
    fn default() -> Self {
        Self { width: 1920, height: 1080, fps: Fps::new(30, 1), background: Color::black() }
    }
}

/// A frame rate as a rational number (`30/1`, `30000/1001`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fps {
    pub num: u32,
    pub den: u32,
}

impl Fps {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// The nominal integer rate used for timecode frame numbers: `ceil(num/den)`.
    pub fn nominal(self) -> u32 {
        self.num.div_ceil(self.den.max(1))
    }
}

impl<'de> Deserialize<'de> for Fps {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            num: u32,
            den: u32,
        }
        let raw = Raw::deserialize(d)?;
        if raw.num == 0 || raw.den == 0 {
            return Err(serde::de::Error::custom("fps num and den must be positive"));
        }
        Ok(Fps::new(raw.num, raw.den))
    }
}

impl fmt::Display for Fps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 { write!(f, "{}", self.num) } else { write!(f, "{}/{}", self.num, self.den) }
    }
}

impl FromStr for Fps {
    type Err = CoreError;

    /// Accepts `30`, `30/1`, `30000/1001` and decimals. The NTSC decimals
    /// (`23.976`, `29.97`, `47.952`, `59.94`, `119.88`) map to `N·1000/1001`.
    /// Anything that isn't a rate above 0 and at most [`MAX_FPS`] is `Usage`;
    /// a decimal is range-checked before its digits become a fraction, so
    /// no input can overflow (or, in release builds, wrap into a valid rate).
    fn from_str(s: &str) -> Result<Self, CoreError> {
        let bad = || CoreError::usage(format!("invalid frame rate {s:?} (examples: 30, 29.97, 30000/1001)"));
        let s = s.trim();
        let fps = if let Some((n, d)) = s.split_once('/') {
            Fps::new(n.trim().parse().map_err(|_| bad())?, d.trim().parse().map_err(|_| bad())?)
        } else if let Some((int, frac)) = s.split_once('.') {
            if int.is_empty() || frac.is_empty() || frac.len() > 6 || !(int.bytes().chain(frac.bytes())).all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            let value: f64 = s.parse().map_err(|_| bad())?;
            if !(value > 0.0 && value <= MAX_FPS) {
                return Err(bad());
            }
            let ntsc = (value * 1001.0 / 1000.0).round();
            if ntsc >= 1.0 && (ntsc * 1000.0 / 1001.0 - value).abs() < 0.0015 && (value - value.round()).abs() > 0.001 {
                // `ntsc` ≤ 241 here, so the cast and the product are exact.
                Fps::new((ntsc as u32).checked_mul(1000).ok_or_else(bad)?, 1001)
            } else {
                // `int` ≤ 240 and `frac` < 10⁶ here; checked all the same.
                let den = 10u32.pow(frac.len() as u32);
                let frac = frac.parse::<u32>().map_err(|_| bad())?;
                let num = int.parse::<u32>().ok().and_then(|int| int.checked_mul(den)?.checked_add(frac)).ok_or_else(bad)?;
                let g = gcd(num, den);
                Fps::new(num / g, den / g)
            }
        } else {
            Fps::new(s.parse().map_err(|_| bad())?, 1)
        };
        if fps.num == 0 || fps.den == 0 || fps.as_f64() > MAX_FPS {
            return Err(bad());
        }
        Ok(fps)
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// A `#RRGGBB` color.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct Color(String);

impl Color {
    pub fn black() -> Self {
        Self("#000000".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The color as `0xRRGGBB` for ffmpeg's `color=c=…` source.
    pub fn ffmpeg_hex(&self) -> String {
        format!("0x{}", &self.0[1..])
    }
}

impl FromStr for Color {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, CoreError> {
        let ok = s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit());
        if ok {
            Ok(Self(s.to_ascii_uppercase()))
        } else {
            Err(CoreError::usage(format!("invalid color {s:?} (expected #RRGGBB)")))
        }
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(|e: CoreError| serde::de::Error::custom(e.message))
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ----------------------------------------------------------------- scene

/// Layers and audio tracks. The project scene is used for new takes; each
/// take keeps its own snapshot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scene {
    /// Array order is z-order: the last layer is on top.
    pub layers: Vec<Layer>,
    pub audio: Vec<AudioTrack>,
}

impl Scene {
    pub fn layer(&self, id: &LayerId) -> Option<&Layer> {
        self.layers.iter().find(|l| &l.id == id)
    }

    pub fn layer_mut(&mut self, id: &LayerId) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| &l.id == id)
    }

    pub fn layer_index(&self, id: &LayerId) -> Option<usize> {
        self.layers.iter().position(|l| &l.id == id)
    }

    pub fn track(&self, id: &TrackId) -> Option<&AudioTrack> {
        self.audio.iter().find(|t| &t.id == id)
    }

    pub fn track_mut(&mut self, id: &TrackId) -> Option<&mut AudioTrack> {
        self.audio.iter_mut().find(|t| &t.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty() && self.audio.is_empty()
    }
}

/// A video source placed on the canvas ("Onion layer" in the UI).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    /// 0.0 to 1.0.
    pub opacity: f64,
    pub source: VideoSource,
    /// Placement on the canvas, in canvas pixels. May extend past the canvas.
    pub rect: Rect,
    /// Trim of the source, in source pixels.
    pub crop: Crop,
    pub fit: Fit,
    /// Locked layers can't be moved on the canvas (UI aid).
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    /// Take scenes only: the source disappeared mid-take or its file is unreadable.
    #[serde(default, skip_serializing_if = "is_false")]
    pub missing: bool,
}

/// What a layer captures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum VideoSource {
    /// A monitor, captured through the ScreenCast portal. `output` is the bare
    /// output name (`DP-4`), never the description.
    Screen { output: String, label: String },
    /// A V4L2 camera (`/dev/video0`).
    Camera { device: String, label: String },
}

impl VideoSource {
    pub fn label(&self) -> &str {
        match self {
            VideoSource::Screen { label, .. } | VideoSource::Camera { label, .. } => label,
        }
    }

    /// `screen` or `camera`.
    pub fn kind_str(&self) -> &'static str {
        match self {
            VideoSource::Screen { .. } => "screen",
            VideoSource::Camera { .. } => "camera",
        }
    }
}

/// A rectangle in canvas pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
}

/// Pixels trimmed from each side of a source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Crop {
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    pub left: u32,
}

impl Crop {
    pub fn is_zero(&self) -> bool {
        *self == Crop::default()
    }
}

/// How a (cropped) source fills its rect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Fit {
    /// Scale to fit inside the rect, keeping the aspect ratio (letterboxed).
    #[default]
    Contain,
    /// Scale to cover the rect, keeping the aspect ratio (overflow cropped).
    Cover,
}

/// An audio source recorded to its own file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioTrack {
    pub id: TrackId,
    pub role: AudioRole,
    /// PulseAudio/PipeWire source name (`alsa_input.…` or `….monitor`).
    pub source: String,
    pub label: String,
    pub gain_db: f64,
    pub muted: bool,
    /// Manual sync correction. Positive values place the audio later.
    pub sync_offset_ms: i32,
    /// Take scenes only: the source disappeared mid-take or its file is unreadable.
    #[serde(default, skip_serializing_if = "is_false")]
    pub missing: bool,
}

/// What an audio track records. Labels only; any number of each is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AudioRole {
    Mic,
    System,
}

impl AudioRole {
    pub fn as_str(self) -> &'static str {
        match self {
            AudioRole::Mic => "mic",
            AudioRole::System => "system",
        }
    }
}

// -------------------------------------------------------------- settings

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingSettings {
    /// 0 to 10 seconds.
    pub countdown_secs: u32,
    pub encoder: EncoderPref,
    /// The VA-API render node, e.g. `/dev/dri/renderD129`. `null` lets Qrec
    /// pick the first Intel or AMD node (never NVIDIA).
    pub vaapi_device: Option<String>,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self { countdown_secs: 0, encoder: EncoderPref::Auto, vaapi_device: None }
    }
}

/// Which H.264 encoder to use for recording and MP4 export.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EncoderPref {
    /// VA-API when it is verified on the chosen node, x264 otherwise.
    #[default]
    Auto,
    Vaapi,
    X264,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutocutSettings {
    pub auto_run_after_take: bool,
    /// The audio track used for silence detection. `null` = the take's first mic track.
    pub track: Option<TrackId>,
    pub threshold_db: f64,
    /// Only silences at least this long are removed.
    pub min_silence_ms: u32,
    /// Kept on each side of speech.
    pub padding_ms: u32,
    /// Leftover fragments shorter than this are dropped.
    pub min_keep_ms: u32,
}

impl Default for AutocutSettings {
    fn default() -> Self {
        Self {
            auto_run_after_take: false,
            track: None,
            threshold_db: -35.0,
            min_silence_ms: 400,
            padding_ms: 120,
            min_keep_ms: 200,
        }
    }
}

// ----------------------------------------------------------------- takes

/// One recording session: a folder with one file per source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Take {
    pub status: TakeStatus,
    pub recorded_at: DateTime<Utc>,
    /// 0 while recording. Derived from the streams (ARCHITECTURE §2.2).
    pub duration_frames: Frame,
    /// Snapshot of the project scene at record time. Editable and undoable.
    pub scene: Scene,
    pub streams: Vec<Stream>,
    /// Why the take is broken, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Finalization placed clips for this take (drives discard-last, A§5.5).
    #[serde(default, skip_serializing_if = "is_false")]
    pub placed: bool,
    /// Clips a record-over replaced; discarding the take puts them back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaced: Vec<Clip>,
}

impl Take {
    pub fn stream(&self, id: &str) -> Option<&Stream> {
        self.streams.iter().find(|s| s.id == id)
    }

    pub fn stream_mut(&mut self, id: &str) -> Option<&mut Stream> {
        self.streams.iter_mut().find(|s| s.id == id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TakeStatus {
    Recording,
    Ok,
    Broken,
    TooShort,
}

impl TakeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TakeStatus::Recording => "recording",
            TakeStatus::Ok => "ok",
            TakeStatus::Broken => "broken",
            TakeStatus::TooShort => "tooShort",
        }
    }
}

/// One recorded file. `id` is the layer or track id it was recorded for.
///
/// While the take is recording, media fields are absent; they are filled
/// from `ffprobe` when the take is finalized.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stream {
    pub id: String,
    pub kind: StreamKind,
    /// Project-relative, `/`-separated.
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    /// Running time of the file's first buffer minus the take's start `T0`.
    pub start_offset_ns: i64,
    /// Probed duration of the file. Absent while recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ns: Option<i64>,
    /// Video only: packet count from `ffprobe -count_packets` (A§2.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
    /// Video only: the recorded frame rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<Fps>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StreamKind {
    Video,
    Audio,
}

// -------------------------------------------------------------- timeline

/// One magnetic track: clips sit back to back, positions are prefix sums.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Timeline {
    pub clips: Vec<Clip>,
}

/// A range `[in, out)` of a take, in frames.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Clip {
    pub id: ClipId,
    pub take: TakeId,
    #[serde(rename = "in")]
    pub in_: Frame,
    pub out: Frame,
}

impl Clip {
    pub fn len(&self) -> Frame {
        self.out.saturating_sub(self.in_)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_parsing() {
        assert_eq!("30".parse::<Fps>().unwrap(), Fps::new(30, 1));
        assert_eq!("30000/1001".parse::<Fps>().unwrap(), Fps::new(30000, 1001));
        assert_eq!("29.97".parse::<Fps>().unwrap(), Fps::new(30000, 1001));
        assert_eq!("23.976".parse::<Fps>().unwrap(), Fps::new(24000, 1001));
        assert_eq!("59.94".parse::<Fps>().unwrap(), Fps::new(60000, 1001));
        assert_eq!("25.0".parse::<Fps>().unwrap(), Fps::new(25, 1));
        assert_eq!("12.5".parse::<Fps>().unwrap(), Fps::new(25, 2));
        assert!("0".parse::<Fps>().is_err());
        assert!("abc".parse::<Fps>().is_err());
        assert!("1000".parse::<Fps>().is_err());
        assert_eq!(Fps::new(30000, 1001).nominal(), 30);
        assert_eq!(Fps::new(30000, 1001).to_string(), "30000/1001");
    }

    #[test]
    fn huge_decimal_rates_are_usage_errors() {
        // These overflowed u32 while becoming a fraction: a panic in debug
        // builds, and in release builds a wrapped value that passed the
        // range check ("4324.967296" became 30/1, "4295.000000" 511/15625).
        for s in [
            "500000000.5",
            "4995004.995005",
            "4324.967296",
            "4295.000000",
            "4290677.322677",
            "5000.123456",
            "9999999999.999999",
            "240.000001",
        ] {
            let err = s.parse::<Fps>().unwrap_err();
            assert_eq!(err.code, crate::error::ErrorCode::Usage, "{s}");
        }
        assert_eq!("240.000000".parse::<Fps>().unwrap(), Fps::new(240, 1));
        assert_eq!("0.5".parse::<Fps>().unwrap(), Fps::new(1, 2));
    }

    proptest::proptest! {
        /// A decimal parses iff it is a rate above 0 and at most 240, to
        /// itself or to the NTSC rate it names; nothing panics.
        #[test]
        fn decimal_rates_parse_to_their_value(int in "[0-9]{1,10}", frac in "[0-9]{1,6}") {
            let s = format!("{int}.{frac}");
            let value: f64 = s.parse().unwrap();
            let in_range = value > 0.0 && value <= MAX_FPS;
            match s.parse::<Fps>() {
                Ok(fps) => {
                    proptest::prop_assert!(in_range, "{} parsed as {}", s, fps);
                    proptest::prop_assert!((fps.as_f64() - value).abs() < 0.0015, "{} parsed as {}", s, fps);
                }
                Err(e) => {
                    proptest::prop_assert!(!in_range, "{} rejected: {}", s, e);
                    proptest::prop_assert_eq!(e.code, crate::error::ErrorCode::Usage);
                }
            }
        }

        /// Any text is a rate or a usage error, never a panic.
        #[test]
        fn any_text_parses_or_is_a_usage_error(s in "[0-9./ ]{0,24}|\\PC{0,24}") {
            if let Err(e) = s.parse::<Fps>() {
                proptest::prop_assert_eq!(e.code, crate::error::ErrorCode::Usage);
            }
        }
    }

    #[test]
    fn colors() {
        assert_eq!("#12170f".parse::<Color>().unwrap().as_str(), "#12170F");
        assert!("12170F".parse::<Color>().is_err());
        assert!("#12170".parse::<Color>().is_err());
        assert_eq!(Color::black().ffmpeg_hex(), "0x000000");
    }

    #[test]
    fn stream_shape_matches_spec() {
        let s = Stream {
            id: "L1".into(),
            kind: StreamKind::Video,
            file: "takes/take-0001/L1-screen-DP-4.mkv".into(),
            width: Some(2560),
            height: Some(1440),
            sample_rate: None,
            channels: None,
            start_offset_ns: 0,
            duration_ns: None,
            frames: None,
            fps: None,
        };
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"id":"L1","kind":"video","file":"takes/take-0001/L1-screen-DP-4.mkv","width":2560,"height":1440,"startOffsetNs":0}"#
        );
    }
}
