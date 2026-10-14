//! Undo diffs are a planner (SPEC §4.2): the patches of typical edits are
//! pinned with snapshots. Each snapshot shows the forward patch, then the
//! inverse, one op per line (`test` lines carry the guarded value).

use json_patch::{Patch, PatchOperation};
use qrec_core::history::{HistoryEntry, commit};
use qrec_core::state::State;
use qrec_core::testkit::{self, ProjectBuilder};
use qrec_core::*;
use serde_json::Value;

/// The SPEC §6.3 example: two layers, two tracks, take 1 on the timeline
/// in two clips, take 2 in the bin.
fn spec_like() -> State {
    testkit::state(
        ProjectBuilder::new()
            .screen_layer("L1", "DP-4")
            .camera_layer("L2", "/dev/video0")
            .mic("A1", "alsa_input.usb-Yeti")
            .system("A2", "alsa_output.monitor")
            .take("take-0001", 912)
            .take("take-0002", 300)
            .clip("c7f3", "take-0001", 0, 410)
            .clip("c8a1", "take-0001", 455, 912)
            .build(),
    )
}

/// Commits `edit` on the SPEC-like state and returns the entry.
fn entry(edit: impl FnOnce(&mut Project)) -> HistoryEntry {
    entry_from(&spec_like(), edit)
}

fn entry_from(state: &State, edit: impl FnOnce(&mut Project)) -> HistoryEntry {
    let ctx = testkit::ctx("2026-10-14T12:30:00Z", 1);
    let mut next = state.clone();
    edit(&mut next.project);
    let c = commit(state, &ctx, "edit".into(), Value::Null, next);
    c.state.history.entries.last().cloned().expect("the edit changed something")
}

fn render(entry: &HistoryEntry) -> String {
    fn lines(patch: &Patch, out: &mut String) {
        for op in &patch.0 {
            let line = match op {
                PatchOperation::Test(t) => format!("test    {} {}", t.path, t.value),
                PatchOperation::Replace(r) => format!("replace {} {}", r.path, r.value),
                PatchOperation::Add(a) => format!("add     {} {}", a.path, a.value),
                PatchOperation::Remove(r) => format!("remove  {}", r.path),
                other => format!("{other}"),
            };
            out.push_str(&line);
            out.push('\n');
        }
    }
    let mut out = String::from("forward:\n");
    lines(&entry.forward, &mut out);
    out.push_str("inverse:\n");
    lines(&entry.inverse, &mut out);
    out
}

fn clip(id: &str, take: &str, in_: u64, out: u64) -> Clip {
    Clip {
        id: id.parse().unwrap(),
        take: take.parse().unwrap(),
        in_,
        out,
    }
}

#[test]
fn split_clip() {
    let e = entry(|p| {
        p.timeline.clips[0].out = 300;
        p.timeline.clips.insert(1, clip("c9b2", "take-0001", 300, 410));
    });
    insta::assert_snapshot!("split_clip", render(&e));
}

#[test]
fn trim_clip() {
    let e = entry(|p| p.timeline.clips[1].in_ = 460);
    insta::assert_snapshot!("trim_clip", render(&e));
}

#[test]
fn move_clip() {
    let e = entry(|p| p.timeline.clips.swap(0, 1));
    insta::assert_snapshot!("move_clip", render(&e));
}

#[test]
fn remove_clip() {
    let e = entry(|p| {
        p.timeline.clips.remove(0);
    });
    insta::assert_snapshot!("remove_clip", render(&e));
}

#[test]
fn add_clip_from_the_bin() {
    let e = entry(|p| p.timeline.clips.push(clip("c0d4", "take-0002", 0, 300)));
    insta::assert_snapshot!("add_clip_from_the_bin", render(&e));
}

#[test]
fn reorder_layers() {
    let e = entry(|p| p.scene.layers.swap(0, 1));
    insta::assert_snapshot!("reorder_layers", render(&e));
}

#[test]
fn edit_a_take_layout() {
    let e = entry(|p| {
        let take = p.takes.get_mut(&TakeId::numbered(1)).unwrap();
        take.scene.layers[1].rect = Rect::new(40, 40, 480, 360);
        take.scene.layers[1].opacity = 0.8;
    });
    insta::assert_snapshot!("edit_a_take_layout", render(&e));
}

#[test]
fn change_a_layer_source() {
    let e = entry(|p| {
        p.scene.layers[0].source = VideoSource::Camera {
            device: "/dev/video2".into(),
            label: "Cam".into(),
        };
    });
    insta::assert_snapshot!("change_a_layer_source", render(&e));
}

#[test]
fn change_the_fps() {
    let e = entry(|p| {
        p.canvas.fps = Fps::new(60, 1);
        qrec_core::requantize::rederive_durations(p);
        for c in &mut p.timeline.clips {
            c.in_ *= 2;
            c.out *= 2;
        }
    });
    insta::assert_snapshot!("change_the_fps", render(&e));
}

#[test]
fn autocut_settings_and_remove_a_track() {
    let e = entry(|p| {
        p.autocut.track = Some("A1".parse().unwrap());
        p.autocut.threshold_db = -40.0;
        p.scene.audio.remove(1);
    });
    insta::assert_snapshot!("autocut_settings_and_remove_a_track", render(&e));
}

/// Facts first: the take enters the pool without an entry, then its clip
/// is placed; the patch never mentions the take itself.
#[test]
fn place_a_new_take() {
    let mut state = spec_like();
    let mut take = state.project.takes[&TakeId::numbered(2)].clone();
    take.streams.iter_mut().for_each(|s| s.file = s.file.replace("take-0002", "take-0003"));
    state.project.takes.insert(TakeId::numbered(3), take);
    state.project.next_take_number = 4;
    let e = entry_from(&state, |p| p.timeline.clips.push(clip("c3e1", "take-0003", 0, 300)));
    let text = render(&e);
    assert!(!text.contains("/takes/"), "{text}");
    insta::assert_snapshot!("place_a_new_take", text);
}

#[test]
fn rename_and_recolor() {
    let e = entry(|p| {
        p.name = "Talk".into();
        p.canvas.background = "#12170F".parse().unwrap();
        p.recording.vaapi_device = Some("/dev/dri/renderD129".into());
    });
    insta::assert_snapshot!("rename_and_recolor", render(&e));
    assert_eq!(e.cmd, Value::Null);
    assert_eq!(e.forward.0.len(), 6, "three guarded replaces");
}
