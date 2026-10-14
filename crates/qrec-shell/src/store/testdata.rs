//! Projects and history entries for the store and session tests, built
//! from JSON with the SPEC §6.3 field names (no `qrec-core/testkit`
//! needed, so `cargo test -p qrec-shell` runs them too).

use qrec_core::history::HistoryEntry;
use qrec_core::model::Project;
use serde_json::{Value, json};

pub const CREATED_AT: &str = "2026-10-14T12:00:00Z";

fn screen() -> Value {
    json!({
        "id": "L1",
        "name": "Screen FAKE-1",
        "visible": true,
        "opacity": 1.0,
        "source": { "kind": "screen", "output": "FAKE-1", "label": "Fake screen 1" },
        "rect": { "x": 0, "y": 0, "w": 320, "h": 180 },
        "crop": { "top": 0, "right": 0, "bottom": 0, "left": 0 },
        "fit": "contain"
    })
}

fn mic() -> Value {
    json!({
        "id": "A1",
        "role": "mic",
        "source": "alsa_input.fake",
        "label": "Fake mic",
        "gainDb": 0.0,
        "muted": false,
        "syncOffsetMs": 0
    })
}

/// A take of `frames` frames at 30 fps with a screen and a mic stream.
pub fn take_json(id: &str, frames: u64) -> Value {
    json!({
        "status": "ok",
        "recordedAt": CREATED_AT,
        "durationFrames": frames,
        "scene": { "layers": [screen()], "audio": [mic()] },
        "streams": [
            {
                "id": "L1", "kind": "video", "file": format!("takes/{id}/L1-screen-FAKE-1.mkv"),
                "width": 320, "height": 180, "startOffsetNs": 0,
                "durationNs": frames as i64 * 1_000_000_000 / 30, "frames": frames,
                "fps": { "num": 30, "den": 1 }
            },
            {
                "id": "A1", "kind": "audio", "file": format!("takes/{id}/A1-mic.mka"),
                "sampleRate": 48000, "channels": 1, "startOffsetNs": 0,
                "durationNs": frames as i64 * 1_000_000_000 / 30
            }
        ],
        "placed": true
    })
}

/// 320×180 @ 30 with a screen layer and a mic, take-0001 (300 frames) and
/// two clips: `c001` = [0, 100) and `c002` = [150, 300).
pub fn sample_json() -> Value {
    json!({
        "version": 1,
        "name": "swamp",
        "createdAt": CREATED_AT,
        "canvas": { "width": 320, "height": 180, "fps": { "num": 30, "den": 1 }, "background": "#000000" },
        "scene": { "layers": [screen()], "audio": [mic()] },
        "recording": { "countdownSecs": 0, "encoder": "auto", "vaapiDevice": null },
        "autocut": {
            "autoRunAfterTake": false, "track": "A1", "thresholdDb": -35.0,
            "minSilenceMs": 400, "paddingMs": 120, "minKeepMs": 200
        },
        "takes": { "take-0001": take_json("take-0001", 300) },
        "nextTakeNumber": 2,
        "timeline": { "clips": [
            { "id": "c001", "take": "take-0001", "in": 0, "out": 100 },
            { "id": "c002", "take": "take-0001", "in": 150, "out": 300 }
        ] }
    })
}

pub fn sample_project() -> Project {
    serde_json::from_value(sample_json()).expect("the sample project matches the model")
}

/// A history entry with empty patches.
pub fn entry(seq: u64) -> HistoryEntry {
    serde_json::from_value(json!({
        "seq": seq,
        "ts": CREATED_AT,
        "label": format!("Edit {seq}"),
        "cmd": { "cmd": "clipRemove" },
        "forward": [],
        "inverse": [],
        "afterHash": ""
    }))
    .expect("entries deserialize")
}
