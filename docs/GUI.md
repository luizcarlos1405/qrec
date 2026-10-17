# Qrec — GUI implementation plan (M4, M5)

`SPEC.md` is the product contract and `docs/ARCHITECTURE.md` the engineering
contract. This file is the plan and the contract for the GUI: the
`qrec-gui` crate and everything it needs from the other crates. It
replaces A§12. Where it disagrees with SPEC, SPEC wins and this file gets
fixed.

**References.** `SPEC §N`, `A§N` (ARCHITECTURE), `G§N` (this file),
`DESIGN` (DESIGN.md), `BRIEF` (design/BRIEF.md), `README`
(design/README.md, the product owner's decisions of 2026-10-15). Screens
are `S01a`…`S25` and `X1`…`X3`, components `C01`…`C34` (BRIEF §4–§5; frame
and node ids in README and G§5). *(spike)* marks a measurement from the
feasibility spike in
`/home/luizcarlos1405/RSensato/Qrec/qrec/.claude/worktrees/qrec-v1/tmp/gui-spike`
(gitignored: read it by that absolute path from any worktree; WP0b copies
the scripts the harness needs into the repo, G§8.4).

**Status.** M1 (core + CLI) and M2 (capture engine, hosts, IPC) are done.
The Swamp Night slice (S01a–S18b, 26 frames) was approved on 2026-10-15.
S19–S25 and X1–X3 are drafts (README "Later, not in this review").
Nothing in `qrec-gui` exists yet.

---

## G§0 Gates

The integrator asks the product owner (PO) on day 1. Nothing waits for an
answer unless the table says it blocks.

**Answers (2026-10-15):** 0.1 approved (and `gstreamer-allocators` 0.25
after the fact); 0.2 approved as drawn, so WP8, WP9 and the S24 overlay
build the drawn frames, and the interim pages only stand in until they
merge; 0.6: the PO keeps two pen.dev documents open and another one is
often active, so it doesn't block: agents read `design/qrec.pen` through
the MCP only while `get_app_state` shows it active, and never switch the
active document; the art export asks the PO for a short window with it
active. 0.3–0.5: the defaults.

| # | Ask | Blocks | Default until answered |
| --- | --- | --- | --- |
| 0.1 | A§1.2: add `gtk4` 0.11.5, `libadwaita` 0.9.2, `pango` 0.22.9 (G§6.1) and raise `rust-version` from 1.85 to 1.92 | WP0b and everything after it | WP0a runs (it adds no dependency) |
| 0.2 | Review the draft frames S19, S19b, S20, S20b, S21, S22, S23, S24, S25, X1, X2, X3 (node ids in README) and the interim pages of G§1.1 #2: approve, list changes, or write a waiver into `design/README.md` | WP8, WP9, the S24 overlay (WP4), D-21, D-22, D-29 (drafted part), D-31 | interim pages; the core's toast copy; no palette, cheat sheet or Settings pages besides Project |
| 0.3 | DESIGN Layout's adaptive rules have no frames: sign them off, or ask for frames at 1300×1000 and 1000×800 | nothing | build DESIGN's rules (G§3.15); 1300- and 1000-wide screenshots go to the M4 review |
| 0.4 | If Program playback can't hold canvas fps (WP1 measures it, G§3.10): is a preview-size player with the `PROXY` badge enough, or are proxy files wanted? | nothing | preview-size player, no proxy files (H-9) |
| 0.5 | The departures marked **PO** in G§2 | nothing | as written |
| 0.6 | Keep `design/qrec.pen` the active document in pen.dev while agents read it: the pencil MCP reads the active document and ignores `filePath` (another document is active today) | the art export in WP0b | agents call `get_app_state` before reading; when another document is active they use the PNGs and BRIEF |

---

## G§1 Scope

### G§1.1 What gets built, in order

1. **The approved slice (M4).** S01a, S01b, S02, S03, S04 (with the S04
   notes), S04b, S05, S05b, S06, S07, S08, S09, S10, S11, S12, S13, S13b,
   S14, S15a, S15b, S15c, S16, S16b, S17, S18 and S18b. Swamp Night,
   1600×1000, built from C01–C34.
   - It includes the Settings dialog and its Project page as drawn behind
     S18b (WP6), so the fps-change confirmation is reachable in M4.
   - SPEC §11 confirmations in M4: purge (S18, from the bin), fps change
     (S18b, Settings › Project), and export to an existing hand-picked
     file (BRIEF S18 copy note, from Export "Change…"). Clear history
     needs the palette (WP9, gated): until then it is CLI only (`qrec
     history clear --yes`).
2. **Draft screens, only after G§0.2.** WP8 (S23, X1–X3 and the D-29
   variants as drawn), WP9 (S19–S22, the S25 copy), the S24 canvas
   overlay (WP4).
   - SPEC requires some of their behavior whatever the design (SPEC §3.3,
     §4.3, §4.4, §16), so M4 ships it as **interim pages** (WP0b): the C31
     Blocking block with SPEC's copy and the buttons SPEC names, without
     art or per-row layout. They cover S23 (the doctor report as text, Check again,
     Quit), X1 ("A take is being recorded from the CLI.", Stop & open,
     Quit), the export wait, X2, X3, a project held by another window, a
     holder that doesn't respond, and GStreamer stuck. They are part of
     the G§0.2 review.
   - Until S24 is approved, a refused screen shows the approved S13b row
     error ("Access denied by the desktop portal"), the core toast, and
     the Live picture without that layer.
   - Until S19 and S20 are approved, `Ctrl+K`, `:` and `?` (and the header
     Commands button) show the info toast "Not built yet" with "The command
     palette comes with milestone M5." or "The cheat sheet comes with
     milestone M5." Their hints are drawn as in the approved frames.
3. **The other three themes** (Swamp Day, Dragon's Keep, Ogre Mode) are
   token swaps (SPEC §15.2, README): one token CSS file and one Rust palette
   per theme, generated from `design/tokens/qrec-tokens.json`; no
   per-theme widget code or content. Proofs per BRIEF §7 (WP10). The
   picker is S21 (WP9, gated); until then `qrec config set theme <id>`
   switches live (WP10).
4. **Adaptive layout** (DESIGN Layout): the panel docked at 384 px at
   ≥ 1400 px wide, at 320 px with the inspector 2-up at 1100–1399, an
   overlay sheet below 1100; the compact timeline under 820 px of height.
   The window skeleton has the modes from WP0b and each region's WP builds
   its own (G§3.15).

### G§1.2 Out of scope, deferred

- SPEC §1.2 non-goals.
- Deferred, each with its question and default in G§9:
  - proxy files (G§0.4);
  - the squelch sound (no asset);
  - following GNOME light/dark (Q9);
  - raising the existing window when a second GUI starts;
  - attaching the portal picker to the window;
  - renaming the project in the GUI (`qrec project set --name`; S18b's
    Project page has no Name field);
  - in-app `history restore`;
  - panel splitters (`ui.panels` stays unused);
  - Flatpak.

### G§1.3 Precedence for visual values

1. The approved pen frame or component (CLAUDE.md: `qrec-gui` follows
   `design/qrec.pen`). Read it with the pencil MCP, read-only: `Get`,
   `GetVariables`, `Print` with `filePath
   /home/luizcarlos1405/RSensato/Qrec/qrec/design/qrec.pen`, and only
   after `get_app_state` shows that file as the active document (G§0.6).
   Never call SetVariables, Insert, Update, Delete, Replace, Move,
   Generate or Export. Otherwise use the PNG in `design/screens/`.
2. DESIGN.md for what pen can't express: font axes, motion, real link
   underlines, contrast and focus rules, reduced motion.
3. BRIEF for copy and sample data, except where an approved PNG differs:
   the PNG wins (S12's summary, S16b's toast, S14's take 7 line).

Resolved conflicts (don't re-decide):

| Topic | Build |
| --- | --- |
| Fit-zoom end inset (README known gap) | `(W − 8) / total` (G§4.5); never measure clip x off a fit-zoom mock |
| Zoom to selection | the selection fills the time area exactly, no margin: S12 is 1040 / 3105 = 0.335 px/frame |
| Transport row | anchored to the stage bottom: top = stage height − 44 (window y 612 in S02 and in S14) |
| Fraunces axes | DESIGN: `SOFT` 50 (display, display-sm), 100 (wordmark); `WONK` 1; optical size = font size. The PNGs show default axes; the PO checks it (G§8.7) |
| Countdown dim | `video-scrim` at 65 % opacity = black at 40 % (S03) |
| Trim ghost outline | 1.5 px dashed 6/4 `accent` (C16 `y1M5t`, S10) |
| Header project name | `title` 16/22 600 |
| Destructive-flat pressed | `ghost-fill` background (README known gap) |
| Links (Open log, Open full log) | real underline |
| Export slot while exporting | "42%" + 40×4 mini mud bar; "Exporting 42%" in the tooltip (README polishing call) |
| Scrims | the modal and Settings scrims cover 1600×968, so the hint bar stays live (S18, S18b; they stack when a modal opens over Settings). The cheat sheet (draft S20) covers the whole window |
| S12 summary | label/value rows: "Take 8" │ "00:01:43:15 → 00:01:37:09", "Timeline" │ "00:04:06:11 → 00:04:00:05" (PNG) |
| S16b toast | body "<path> · 88.4 MB"; actions Open file, Open folder (PNG; DESIGN allows two actions) |
| S14 take 9 | "Files didn't close within 10 s." |
| DESIGN icon names that don't exist | `emblem-ok-symbolic` → custom `qrec-success-symbolic`; `emblem-system-symbolic` → `applications-system-symbolic` (the gear the frames draw; Adwaita 50's `preferences-system-symbolic` is tools; WP6) |
| Ghost settle S05 → S05b | 180 ms state crossfade; instant under reduced motion |
| Spinner | one 270° arc, one turn per second; keeps spinning under reduced motion |
| `N` during drags | live while trimming, disabled while moving (README polishing call) |

---

## G§2 Behavior decisions

Defaults the build follows where SPEC, the core and the approved frames
leave a choice or disagree. **PO** marks a departure from a drawn frame;
G§8.7 shows each one to the product owner. D-01…D-36 follow the
requirement review's numbering; H-n are host and engine calls.

| ID | Decision | WP |
| --- | --- | --- |
| D-01 | `Space`, `J`, `L` do nothing in Live mode: `keymap::action_for` returns `Unavailable("Switch to Program (Tab) to play the timeline")` for `play.toggle`, `shuttle.reverse` and `shuttle.forward` when `ui.canvas_mode == Live`, so the hint shows disabled (S02) and the palette says why. CLI `qrec play` still switches to Program (A§5.10 unchanged). | WP0a |
| D-02 | The hint bar comes from a new pure `view::hints` (G§4.3), with `hint_enabled` for ids the GUI handles itself. `keymap::hints` and `HintContext` stay for `qrec keys` and the agent guide. | WP6 |
| D-03 | Meters stay live in Program mode (S06 draws them). With no capture pipeline (Program, Idle) the GUI runs an audio-only `MeterMonitor` (G§3.9). The capture pipeline is unchanged (SPEC §5.2). | WP1, WP0b |
| D-04 | **PO** Starting phase: the header shows `■ Stop [R]` (a stop is valid then); the canvas shows the C06 Saving pill reading "Starting take 10…"; no REC badge, timer or ghost until `Recording`. Never a fake "Recording…" (SPEC §5.3.2). | WP4, WP6 |
| D-05 | A record started from the GUI in Program mode first dispatches `SetCanvasMode{Live}`, then the record command (one FIFO). A take started elsewhere while the GUI shows Program keeps the Program picture under the REC overlays. Switching to Live during such a take shows the plate "Live preview starts after this take" (the engine adds no preview branch mid-take, A§8.11). | WP0b, WP4 |
| D-06 | Program mode with no clip selected is read-only. New core `ui_normalize::edit_target(&Project, &UiState) -> Option<SceneTarget>`: Live → `Project`; Program with a selection → the first selected clip's take; otherwise `None`. `keymap` layer nudges (and so their palette rows) are `Unavailable("Select a clip to edit its take layout")` on `None`. The panel subtitle reads that sentence (S07); rows and inspector show `canvas_scene` but are disabled; canvas drags are off. | WP0a, WP4, WP5 |
| D-07 | **PO** In a take layout, "+ Add layer" and "+ Add audio track" are disabled (0.45) with the tooltip "Add sources in Live mode (Tab): they go into the project scene" (`LayerAdd`/`AudioAdd` accept only the project, A§4.4). S06 draws them enabled. | WP5 |
| D-08 | **PO** The lane-header `M` mutes playback only, like solo: new `UiState.lane_mute: BTreeSet<TrackId>` (`laneMute`, set whole with `SetUi{lane_mute}`). The player drops those tracks; the export is unaffected; tooltip "Mute A1 while playing (export unaffected)". The panel's `M` stays `AudioSet{muted}` on the edited scene. | WP0a, WP1, WP3 |
| D-09 | `n` / `ui.snapping` also governs canvas snapping. No bypass modifier: nudge with the arrows instead. | WP4 |
| D-10 | `Tab` switches Live/Program in the main window; inside overlays and text fields it moves focus (GTK). Region focus cycling is the new keymap bindings `region.next` (`F6`) and `region.prev` (`Shift+F6`), group View, rebindable: header → transport → timeline toolbar → timeline → panel → bin when open (BRIEF §6). Quit is the binding `app.quit` (`Ctrl+Q`). | WP0a, WP0b |
| D-11 | Key precedence is the algorithm in G§3.8. Chrome buttons are `focus-on-click = false`, so a click never makes `Space` or `Enter` activate a button. The bin list's `↑` `↓` `Enter` `Space` (Q6) and the keys inside overlays (`Tab`, `Enter`, `↑` `↓` in a list) are widget keys, not bindings, like typing in a field. | WP0b |
| D-12 | Window close and `app.quit`. Countdown: `RecordCancel`. Starting or Recording: `RecordStop`; the window stays (finalizing) and closes once Idle. Export running: modal "Quit while exporting?" with Keep exporting (focused) and Cancel export and quit. Autocut analysis: dropped (the closing host doesn't wait for it and dropping its Session cancels it: nothing is cut, also for the "autocut after every take" analysis of the take just stopped). Recovery probe: wait (≤ 60 s; then the host stops waiting and the next open recovers the take). | WP0b, WP6 |
| D-13 | A second GUI on a project a GUI holds gets the page "This project is open in another Qrec window" (pid) and Quit (exit 6). GUI hosts refuse the wire `shutdown` with `busy` "the Qrec window holds this project; close it there". | WP0a, WP0b |
| D-14 | The GUI never adopts another folder's project: no live-host fallback and no ancestor project. A folder inside a project gets X2 with the extra line "It's inside the project <dir>; run qrec there." | WP0b |
| D-15 | Autocut page (S12). The sliders are a draft: tuning writes no history. At most one dry run is in flight: a change made while one runs is kept and sent when its reply arrives (newest wins, 250 ms debounce). While a dry run runs, the summary title shows a 16 px spinner and "Analyzing…"; clip plates show the analyzing spinner only for `Autocut` jobs with `dry_run: false`. Apply = `AutocutSettings{patch}` with the changed fields (only if any changed), then `Autocut{scope, dry_run: false}`: two entries, the cut undoes first. Cancel and `Esc` dispatch nothing. "Run autocut after every take" dispatches `AutocutSettings{auto_run_after_take}` at once. "Detect silence on" is part of the draft (`track`). | WP5 |
| D-16 | First run "Add N sources": N commands in one FIFO, screens first, then cameras, then the mic, then system audio (so the camera lands on top); N history entries. Checked by default: the first screen, the first camera, the default mic, the default monitor. | WP4 |
| D-17 | **PO** Bin Preview plays the whole take (`plan_ges_take`) in the Program picture with the badge `TAKE 4` (C06 pill, film icon). In Live mode it first dispatches `SetCanvasMode{Program}`, and stays in Program afterwards. `Space` toggles it while `LocalUi.focus == Bin`; `Esc`, closing the bin or any timeline action ends it. GUI-local, no command. | WP4, WP7 |
| D-18 | Bin secondary lines need one new fact, `Take.recovered` (set by startup recovery, G§4.8); the rest is derived. tooShort → "7 frames; takes need 10."; broken → mapped from `Take.error` (the EOS timeout reason → "Files didn't close within 10 s."); recovered → chip Recovered and "Recovered after Qrec closed mid-take."; status ok and never placed → "Cancelled with Esc."; another take's `replaced` holds its clip → "Replaced by take 5."; a history entry "Discard take N" at or before the cursor → "Discarded with D."; otherwise "Removed from the timeline." | WP0a, WP7 |
| D-19 | Clear history: the palette row "Clear undo history" (WP9, gated); CLI only until then. No project rename in the GUI (S18b's Project page has no Name field). No in-app history restore. | WP9 |
| D-20 | Settings › Project follows S18b: the "Canvas" group (Size dropdown "1920×1080 (16:9)", Frame rate 24 / 25 / 30 / 50 / 60, Background) and the footer "Saved in this project's <dir>/qrec.json." (`~` for the home dir). Settings › Recording and › Autocut aren't drawn: built only with frames or a waiver (G§0.2), in WP9. | WP6, WP9 |
| D-21 | Toast copy follows S25 once approved (WP9): "Recording didn't start"; "Camera disconnected" / "Screen disconnected" / "Mic disconnected" / "System audio disconnected" with "<label> stopped sending frames. Take N keeps recording without <id> <name>."; "qrec.json changed on disk but isn't valid". The core's current copy stays until then. | WP9 |
| D-22 | The cheat sheet keeps the `Canvas` group (layer nudges) at the bottom of column 3 (gated). | WP9 |
| D-23 | `N` during drags: G§1.3, D-42. | WP0b, WP6 |
| D-24 | Export "Change…" opens `gtk::FileDialog::save` (portal) on `exports/` with the auto name. The GUI still dispatches `confirmed: false` and shows the "Replace file" modal on `confirmation_required` (SPEC §11 binds, so an existing file is confirmed twice). A forbidden path shows the core message under the Output field (C29 invalid state). A picked file follows the Format switch: a name in the old format's extension gets the new one's (`talk.mp4` → `talk.webm`; review fix: WebM was written into talk.mp4), another name stays as picked. | WP7 |
| D-25 | More lanes than fit: rows scroll vertically under the fixed toolbar and ruler (`Shift`+wheel or the scrollbar). The panel is a ScrolledWindow. | WP3, WP5 |
| D-26 | Shuttle speed shows after the transport total: `· 2×`, `· −4×` (`mono` `muted`, true minus). | WP4 |
| D-27 | Program canvas on an empty timeline: the canvas background and the PROGRAM badge only. S01a comes back whenever the project scene is empty (Live mode). | WP4 |
| D-28 | No splitters: fixed regions and breakpoints (DESIGN Layout). | WP0b |
| D-29 | GUI-start variants not drawn. Export running: "An export from the CLI is running." with "exports/<name> · 43 %", Wait (primary) and Quit. Idle host: silent `shutdown`; the caption "Opening…" after 1 s. Stop & open: the button's loading state "Stopping…". Interim pages in WP0b; drafted look in WP8 (gated). | WP0b, WP8 |
| D-30 | **PO** Reduced motion = config `ui.reduced_motion` OR the system (`gtk-interface-reduced-motion = reduce`, or `gtk-enable-animations = false`). Under `QREC_GUI_TEST` only the config counts (G§8.4). S21's caption (gated) becomes "Also on when your system asks for less motion." | WP0b, WP9 |
| D-31 | Palette labels and CLI forms follow S19/S20/S22 only after G§0.2. Until then the core keymap labels stay, so `qrec keys` and the agent guide don't change. | WP9 |
| D-32 | Layer drags in Program mode move the selection box and the readout; the video updates on release (one `LayerSet`). Live drags move the video too, through `LiveControl::transient_layout` (G§3.9). | WP4 |
| D-33 | An arrow-key nudge burst is one gesture: the first press starts a transient preview (box, readout, inspector; in Live the video through `transient_layout`), repeats add to it, and one `LayerSet` goes out on key release, or 300 ms after the last repeat when no release arrives (1 s after a lone press: a held key waits out the compositor's repeat delay, 500–600 ms, before its first repeat). The stage watches the releases on the window in the capture phase: the key router takes the presses there, so GTK gives it their releases too (R19). | WP4 |
| D-34 | A key whose action is unavailable toasts the reason (info, 5 s), except `Esc` with nothing to cancel. `r` during an export gives "Can't record while exporting" / "Recording works again once the export finishes or you cancel it." / Show export. | WP0b |
| D-35 | Empty folder: eager init. `GuiHostSpec.first = Init{canvas: None, name: None}`, queued ahead of every IPC request (SPEC §4.4), then S01a shows. | WP0a, WP0b |
| D-36 | Clips get no right-click menu and double-click does nothing (not designed); keys cover every action. | WP3 |
| D-37 | Selection gestures (SPEC §7.1). Timeline: press on an unselected clip → `Select{Set([c])}`; press on a selected clip → nothing, and a release without a drag → `Select{Set([c])}`; `Shift`+click → `Select{RangeTo(c)}`; `Ctrl`+click → `Select{Toggle(c)}` (no drag starts with a modifier held); click on an empty row → `Select{Clear}`. Bin (GUI-local `LocalUi.bin_selection`): click selects one, `Ctrl`+click toggles, `Shift`+click extends, `↑` `↓` move it, `Shift`+`↑` `↓` extend it. Two or more selected show the S18 footer and no row actions. A change of the bin's takes keeps the rows of the takes still listed (and their focus); when the row with the keyboard focus leaves (`Enter` inserted its take), the row now at its place takes the focus, and the selection when that take was the one selected (review fix: every row was made again, the focus left the bin and `↓` seeked the timeline). | WP3, WP7 |
| D-38 | Insert from the bin by drag (SPEC §7.1): bin rows of ok takes are `gtk::DragSource`s with the string `qrec-take:<id>`; the timeline's time area (`time-area`: the canvas, or the empty timeline's S01b page that replaces it) is a `gtk::DropTarget`. Over the canvas the C18 caret shows at the nearest clip boundary (slot k of n + 1) with the tooltip "Insert take 4 · slot k of n + 1"; over the empty page, "slot 1 of 1" by the pointer (review fix: the target was on the canvas alone, and an empty timeline refused every drop); drop → `ClipAdd{take, at: Index(k − 1)}`; `Esc` cancels (GTK). Not while recording. While an accepted take hovers the canvas, the drop target sets `LocalUi.drag = BinInsert` (the hint bar's BinDrag, D-42's swallow) unless a drag is already set, and clears only the one it set (round 3; WP7's drag source may set its own from the bin). | WP3, WP7 |
| D-39 | Wheel and pan (SPEC §7.2): plain wheel (`dy`, and touchpad `dx` and `dy`) scrolls horizontally, 64 px per notch; `Shift`+wheel scrolls the lanes vertically; `Ctrl`+wheel zooms ×1.25 per notch at the pointer; middle-drag pans both axes. The view follows at once; `SetUi{scroll_px}` goes out 300 ms after the last event (echo rule, G§3.6). | WP3 |
| D-40 | Ruler (SPEC §7.2): press → `Seek{To(f)}`; dragging scrubs: the drawn playhead and the player's scrub follow every motion event, and `Seek{To(f)}` goes out at most every 100 ms (10 per second), plus one on release. The timeline scrubs through the stage's sync (`media::program::scrub`, G§3.10 step 9), which owns the player's commands. | WP3, WP4 |
| D-41 | "One command per gesture" means one undoable command per gesture. The press may also dispatch `Select{Set}` (UI state, no history). | WP3, WP4, WP5 |
| D-42 | Keys during a drag: `Esc` cancels it; `n` toggles snapping during a trim only (S10, S11); `Shift`, `Alt` and `Ctrl` change the drag's constraints; every other key is swallowed and dispatches nothing (`s` mid-trim does nothing). | WP0b |
| D-43 | Enablement while not Idle: a control is enabled when its command passes `view::enabled(&State, &Command)` (the `keymap` gate: `allowed_while_recording` for the phase, GUI-only commands, `disk_invalid`) and, for scene edits, `edit_target` isn't `None`. So gesture starts, layer rows, audio-row controls (M, gain, sync, overflow) and the inspector are disabled; meters stay live; lane `M`/`S` (`SetUi`), selection, seeks and the mode switch stay enabled. Dimming per S03/S04: timeline clips at 0.7 during the countdown and 0.6 while recording or finalizing; disabled controls at 0.45. | WP0a, WP3, WP4, WP5 |
| D-44 | `LocalUi.playing_rate` resets to 1 when `runtime.playing` turns false, and when playback starts by anything but the GUI's own `Shuttle` (Space, CLI `qrec play`). The core keeps no rate or direction (`runtime.playing` only), so a `Shuttle` while already playing produces no snapshot that could tell the player: the KeyRouter pre-step sets the rate and calls `ProgramPlayer::play(rate)` itself (G§3.12 R8), then dispatches the `Shuttle`. | WP0b, WP4 |
| D-45 | Trim zones: the 4 px inside each clip edge. At a cut the left clip owns its end zone and the right clip its start zone; when one of the two is selected, its edge takes both (8 px). Clips under 6 px wide have no zones (body only; `[` `]` and `Alt+[` `]` trim them). | WP3 |
| D-46 | Units: `ui.zoom` (px per frame) and `ui.scroll_px` are logical px at 100 % UI scale. View models compute at 100 %; widgets multiply by `ui.scale` when drawing and divide pointer positions by it. | WP0b, WP3 |
| D-47 | Recovery hold (A§5.3): UI-state commands (new `Command::ui_only()`: `Select`, `Seek`, `SetUi`, `SetCanvasMode`, `Play`, `Pause`, `Shuttle`) are never parked. Parked commands keep their order among themselves and resolve the selection and playhead when they run. The header shows "Recovering take 7…" meanwhile. | WP0a |
| D-48 | **PO** Settings in M4: the dialog with its five tabs as drawn (S18b); only Project is enabled; the others are disabled with the tooltip "Not built yet" until WP9. | WP6 |
| D-49 | Export panel: Setup, Done and Failed trap focus (Overlay keys; an outside click closes them). Running is passive: no focus trap and no click-catcher, keys and clicks reach the window (S15b: the S06 hints stay live), and `Esc`, `Ctrl+E` or the header slot close it. | WP0b, WP7 |
| D-50 | Retrying the live preview (S24 "Try again", gated) goes through the Session: `GuiHost::retry_live()` posts `Msg::RetryLive`, which the session thread turns into `CaptureEngine::retry_live`, ordered with `SetLive` and `StartCapture` and logged. | WP0a, WP1 |
| D-51 | **PO** A step while the program plays (`←` `→` a frame, `Shift+←` `Shift+→` 1 s, `↑` `↓` an edit point; a key or the transport's button) pauses it first and steps from the frame the picture shows, as an editor's frame step does (Resolve, Premiere): `Pause`, then one `Seek{To(f)}` to where the step lands from the player's frame (`apply::ui::seek_frame`). SPEC §10.2, DESIGN and BRIEF don't say what a step does while playing; before, a frame step barely moved (its seek landed within the sync's 2-frame tolerance of the playing player, which played on; R18's audit), and ±1 s and the edit jumps played on from where they landed (keeping that for them is the alternative). `Home`, `End`, a ruler click and the CLI's `qrec seek` still move a playing program on (the sync's once-a-second seek waits for the window's, G§3.10 step 3). The sync leaves the playhead to the step's seek and holds the paused picture until it lands (G§3.10 step 2). | WP4 |
| H-1 | Live preview: a `gtk4paintablesink` created on the GTK thread and handed to the engine (SPEC §5.2), composited at the preview size (≤ 960×540 × the device scale), BGRx. The RGBA appsink path stays for tests and as the picture before WP1 merges. | WP1 |
| H-2 | Program playback: one GES timeline and pipeline per session, edited in place and committed; a second `gtk4paintablesink`. | WP1 |
| H-3 | The GUI renders with GSK GL: `GDK_DISABLE=vulkan` is set unless the user set `GDK_DISABLE` or `GSK_RENDERER`, in which case both are left alone and doctor notes it (G§3.13). | WP0a |
| H-4 | A GUI host keeps portal casts open for 60 s after the live pipeline stops (`EngineOptions.cast_grace`), so `Tab` back to Live doesn't re-open screencasts. | WP1 |
| H-5 | Device facts come from a cache the GUI refreshes on a worker (`facts::SharedSources`), so `LayerAdd` and a take start never block the session thread on discovery. | WP0a |
| H-6 | The Session calls its `StateObserver` after every assignment of its state, durable or not (`RecordClock`, export progress, `runtime.playing`, recovery, jobs), before that outcome's toasts, progress and replies (G§3.5). | WP0a |
| H-7 | `Responder::reply_late` tells immediate errors (the GUI toasts them) from late ones (the core's `Notify` already toasted them; a property test holds the core to that). | WP0a |
| H-8 | No proxy files in M4/M5. The player logs QoS drops. | — |
| H-9 | If WP1's playback measurement fails, the player composites at the preview size (`PlayerOptions.out_size`, `ges_plan::scaled`) and the stage shows the `PROXY` badge; parity tests and frame renders stay at canvas size (G§0.4). | WP1, WP4 |

---

## G§3 Architecture

### G§3.1 Crates

```
qrec (bin) ──► qrec-cli ──────────────────────► qrec-core
     ├───────► qrec-shell ────────────────────► qrec-core
     └───────► qrec-gui ──► qrec-shell (gst) ─► qrec-core
```

- `qrec-gui` owns every GTK, libadwaita and Pango call. It never reads the
  project files (R7) and changes State only with Commands (R8).
- Pure view models live in `qrec-core/src/view/` (G§4): everything
  computed from `State` plus a few numbers (widths, zoom, GUI facts) into
  strings, flags and geometry. Snapshot-tested with insta; no display.
- The GUI host (lock, socket, IPC server, Session thread, exit rules) lives
  in `qrec-shell/src/host/gui.rs`: GTK-free and tested with a fake port.
- `qrec-cli` gains `Backend::gui` and stays GTK-free (A§1.1).

### G§3.2 `qrec` with no command

1. `main()` keeps its order (A§8.10): GPU probe child → `is_gui` (no
   command) → `gpu::prepare_gpu_env(for_gui)` → `qrec_cli::run_process`.
2. `exec::execute`: `Plan::Gui` → `project_dir(cli, cwd)` →
   `backend.gui(&dir, explicit)` → that exit code. `--json` with no command
   is `usage` "the GUI has no --json output; give a command" (exit 2).
   `Backend::gui` defaults to `unavailable` with `GUI_NOT_BUILT`, for the
   `Standalone` build.
3. `Shell::gui` (binary, `crates/qrec/src/shell.rs`):
   - logging `Role::Gui` to `UserDirs::gui_log(&dir)` =
     `$XDG_STATE_HOME/qrec/gui-<first 12 hex of sha256(canonical dir)>.log`,
     opened for append, with a header line `=== qrec <version> pid <pid>
     <RFC 3339> <dir> ===`, rotated to `.log.1` past 8 MiB and never
     truncated: a second GUI on the same project (which exits 6) doesn't
     erase or move the first one's log (every GUI holds a shared `flock` on
     its log; a start rotates only under the exclusive one), and GUIs on two
     projects never share one;
   - `info`, `debug` with `-v`; warnings also on stderr; `OWN_CRATES`
     includes `qrec_gui`;
   - `host_log` (`cli_host.rs`, the CLI's "host disconnected" hint) names
     this file for GUI hosts;
   - then `qrec_gui::run(GuiArgs { project, explicit, verbose })`.
4. `qrec_gui::run`: `host::signals::install(&HOST_SIGNALS)`; G§3.3 step
   1; `adw::Application` with id `com.rsensato.qrec` and
   `ApplicationFlags::NON_UNIQUE`; `app.run_with_args::<&str>(&[])`;
   returns the exit code the app stored in a thread-local before quitting.

Exit codes (A§4.8):

| Code | When |
| --- | --- |
| 0 | the window closed |
| 130 | a signal |
| 3 | X2 (not my swamp, or inside a project) |
| 5 | S23 (a required dependency is missing) or GStreamer stuck |
| 6 | the project is held: Quit on X1 or the export wait, already open in a GUI, holder not responding |
| 1 | X3, the folder can't be opened, the host can't start, the session thread crashed |

### G§3.3 Startup

The host publishes its socket before GStreamer, doctor and the Session
(A§8.3 step 1): a niri `qrec record toggle` sent while the window opens
queues in the GUI instead of spawning a headless recorder.

| # | Thread | Work | On failure |
| --- | --- | --- | --- |
| 1 | main, before GTK | canonicalize `--project` or the cwd; `ipc::resolve::classify(dir, true)`; for `Other`, look for an ancestor with `qrec.json` (D-14); read `config.toml` and `keys.toml` (`qrec_shell::{config_file, keys_file}::read`, ≤ 1 MiB each); `keymap::resolve` | `Other` → X2 at step 2 (exit 3); unreadable folder → "Can't open this folder" page with the OS error (exit 1) |
| 2 | GTK `startup`, `activate` | fonts; CSS providers with the theme, scale and motion from step 1; icon theme; the window (main layout, empty and disabled, title = folder name) or the X2 page; `keymap::problems` → one warning toast each, shown once the window maps; `SharedSources::refresh` spawned on `qrec-gui-io`, so the cache is warm before the Session needs it | — |
| 3 | `qrec-gst`, spawned in `activate` | `gst_init::init()` (≤ 30 s), `ges_render::init()` | stuck → stuck page (exit 5) |
| 4 | `qrec-startup`, spawned in `activate` (not for X2) | `ProjectLock::try_acquire`: free → `GuiHost::start` (lock record `starting`, `first` queued, socket published, `ready`; IPC requests queue); held → takeover (G§3.4.3), then `GuiHost::start` | takeover pages (exit 6); the host can't start → page with the error (exit 1) |
| 5 | GTK, after 3 | create the live and the program `gtk4paintablesink`, read each `paintable` (main thread only: a sink created on a worker deadlocks *(spike)*); `MediaHub` keeps both elements until exit | — |
| 6 | `qrec-doctor`, after 3 | `doctor::run(DoctorOptions { fake_backend, project/config VA device, probe_vaapi: false })` | a required check failed → `GuiHost::fail(missing_dependency)` and interim S23 (exit 5) |
| 7 | `qrec-startup`, once 4, 5 and 6 succeeded | `GuiHost::open(GuiOpener)`: `qrec-session` opens the Session, attaches it (the `first` request runs, then the queued ones), posts the first snapshot and `HostEvent::Opened` | `Invalid` with the restore hint → interim X3 (exit 1) |
| 8 | GTK, on `Opened` | spawn `ProgramPlayer`, `PreviewCache`; `MediaHub` starts toggling `MeterMonitor`; later `SharedSources` refreshes when an Add menu opens (no hotplug watcher feeds it yet: a device plugged in shows at the next Add menu; the first run's rows keep the person's picks across a refresh, R25) | — |

- Steps 3 and 4 start together: publishing never waits for GStreamer or
  doctor. The stage shows the caption "Opening <name>…" if step 7 hasn't
  finished after 1 s. Nothing animates on start (DESIGN Motion): a region
  that follows the first snapshot out of the placeholder state jumps to it
  while `LocalUi.startup` isn't `Open` yet (the first snapshot comes before
  `Opened`); the bin drawer left open in ui.json slid in until the review
  fix.
- `GuiHost::fail` (GStreamer stuck, S23) and a failed open (X3) answer
  every queued and later request with that error, stop accepting and
  release the lock at once. The page stays up while the CLI works
  in-process, so `qrec history restore <seq> --yes` can repair an X3
  project. "Check again" on S23 re-runs step 6 and, when it passes, steps
  4 and 7.
- A signal before step 4 quits with 130 (the workers check `Interrupts`
  between steps). From step 4 on, the host handles signals (A§8.6), also
  before the Session is open.
- Until the Session is open the header's buttons do what their keys do:
  nothing but Quit (`router::decide_button`). A blocking page (startup's
  or a session that crashed) clears `LocalUi.overlays` in the update that
  shows it, so no dialog stays over the page (review fix R36: Settings
  opened from the gear while the window waited, then covered X1 with its
  scrim, and `Esc` couldn't close it).

### G§3.4 GUI host (shell)

#### G§3.4.1 `qrec-shell/src/host/gui.rs` (WP0a)

```rust
pub trait GuiPort: Send + Sync + 'static {            // implemented by qrec-gui; O(1), never blocks
    fn snapshot(&self, rev: u64, state: Arc<State>);   // after every state assignment (H-6)
    fn toast(&self, toast: Toast, recovery: bool);
    fn host(&self, event: HostEvent);
}
pub enum HostEvent { Ready, Opened, OpenFailed(CoreError), Failed(CoreError), Stopped { end: HostEnd }, Crashed(String) }
pub struct GuiHostSpec {
    pub dir: PathBuf,                                  // canonical
    pub lock: ProjectLock,
    pub env: Env,
    pub interrupts: Option<Interrupts>,
    pub rules: ExitRules,
    pub first: Option<Request>,                        // D-35: queued before the socket is published
}
pub struct GuiHost { /* ServerSender, Arc<HostCtl>, JoinHandle */ }
impl GuiHost {
    pub fn start(spec: GuiHostSpec, port: Arc<dyn GuiPort>) -> Result<GuiHost, CoreError>; // worker: lock record, socket, IPC, signals; requests queue
    pub fn open(&self, opener: Box<dyn Opener + Send>);        // spawns qrec-session; Opened or OpenFailed
    pub fn fail(&self, err: CoreError);                        // every queued and later request gets `err`; stop; release the lock; Failed, then Stopped
    pub fn submit(&self, req: Request, to: Box<dyn Responder>) -> bool; // the IPC FIFO; parked until attach; exactly one answer
    pub fn retry_live(&self);                                  // posts Msg::RetryLive (D-50)
    pub fn close(&self);                                       // behind earlier submits: stop accepting; exit once quiet (≤ 60 s); then Stopped
    pub fn is_finished(&self) -> bool;
}
```

- `close` takes the window's FIFO (a status query whose answer asks the
  host to close), so everything the window submitted before runs first: a
  debounced `SetUi` or `Seek` flushed as it closes (`Store::before_close`,
  G§3.6 Debounces), D-12's stop, D-16's N commands. `submit` wraps `to`
  (`AnswerOnDrop`): a request the Session
  never handled because the host stopped first (still queued, parked or
  waiting for a late reply) answers with why, so a `Store::dispatch_with`
  `done` (G§3.7) always runs. Once the host finishes, requests are refused
  at once with that reason. (WP0a review.)
- The `first` request's error is toasted only while the host serves its
  Session: not before one is attached (a start that failed after the
  server started, a close, a failed open), not for the refusal of a
  Session a signal interrupted, not once stopping (`Stopped` and the other
  host events say why). (WP0a review.)

- `start` runs `Host::start(HostKind::Gui, …)` (A§8.3 step 1), with
  `first` pushed to the queue before the socket is renamed into place.
- `open` spawns `qrec-session`, which:
  1. opens the Session: `opener.open(dir, OpenOpts { host: Gui, mutating:
     true, recovery: false }, Some(host.session_lock()?))`;
  2. attaches it, posts the first snapshot and `Opened`;
  3. runs `host.serve(...)`, then `host.finish(...)`, then posts
     `Stopped`.

  A failed open answers the queue with its error, finishes (the lock is
  released) and posts `OpenFailed`. A `catch_unwind` around open and
  serve turns a panic into `server.fail` plus `Crashed`.
- **Exit rules.** `ExitInputs` gains `gui: bool` and `close_requested:
  bool`. `decide` for a GUI host:
  - never `Unused` and never `Idle`;
  - `close_requested && quiet` → `Exit(HostEnd::Closed)` (new variant,
    exit 0); autocut analyses don't count once the close came (D-12:
    dropped; dropping the Session cancels them, so nothing is cut);
  - `close_requested` for 60 s → `Exit(HostEnd::Closed)` anyway: the
    Session stops waiting for a recovery probe too (`Msg::Abandon`; the
    next open recovers that take), and dropping it cancels jobs and
    finalizes a take within its own deadlines;
  - signals as for every host: 15 s grace, the second signal `hard_exit`s
    (A§8.6).
- **Wire `shutdown`** to a GUI host gets `busy` (D-13). The IPC server
  checks its host kind.
- **CLI routing** is unchanged (A§8.3–A§8.5): every `qrec …` for the
  project reaches this host, including niri binds run from `$HOME` (A§8.4
  live-host step). The GUI host takes everything (no bounce); `seek`,
  `play` and `pause` work only here; a CLI that disconnects never cancels
  its export (`keeps_orphaned_jobs`).

#### G§3.4.2 The GUI's Session (`qrec-gui/src/host/opener.rs`, WP0b)

`GuiOpener: Opener` builds:

- `Deps.capture`: `GstCaptureEngine::spawn(EngineOptions { side:
  Some(side_tx), devices: Some(Arc::new(LiveDevices)), preview,
  cast_grace: 60 s, fake: FakeHooks::from_env() (the fake backend; G§8.4's
  hooks: `EngineOptions::new` takes none, so in-process tests never see an
  exported one), ..EngineOptions::new(dir, Backend::from_env(), &dirs)
  })` with `preview = capture::PREVIEW_SINK_SUPPORTED.then(||
  PreviewOutput { sink: live_sink, max_size })`. Before WP1 sets that
  constant, the engine's RGBA appsink feeds `SideEvent::Preview` and
  `MediaHub` shows those frames as `gdk::MemoryTexture`s. The opener takes
  `control()` (`LiveControl`) before boxing the engine and hands it to
  `MediaHub`.
- `Deps.media`: `FfmpegRunner::new(dir)`; clock `system_clock`; entropy
  `os_entropy`.
- `SessionConfig`:
  - `lock`;
  - `env`: `SessionEnv::from_process()` with `caps.gst_factory =
    Some(caps::gst_factory_probe)` unless the fake backend, and the
    `ConfigFile` read at step 1 (a broken `config.toml` is reported once,
    A§4.2);
  - `notifier` and `observer`: the port;
  - `sources`: `SharedSources` (H-5).

`max_size` = (960, 540) × the window's scale factor at step 5.

#### G§3.4.3 Takeover at start (`qrec-shell/src/host/takeover.rs`, WP0a; A§8.6)

```rust
pub enum Holder { Free(ProjectLock), Host { hello: HelloInfo, socket: PathBuf }, Starting, Unresponsive(Option<LockRecord>) }
pub fn find_holder(dir: &Path, env: &Env) -> Result<Holder, CoreError>;               // try_acquire, else hello (2 s)
pub enum Stopped { Finalized(TakeFinalizedView), Cancelled(String) }
pub fn stop_recording(socket: &Path, timeout: Duration) -> Result<Stopped, CoreError>; // RecordStop; the finalized take, or no take
pub fn request_shutdown(socket: &Path) -> Result<(), CoreError>;
pub fn wait_for_lock(dir: &Path, timeout: Duration) -> Result<ProjectLock, CoreError>; // try_acquire every 50 ms
```

`stop_recording` answers `Finalized` once the take is finalized, or at once
`Cancelled("Countdown cancelled")` when the holder was still counting down
(or "Recording cancelled before it started" for a start that waited for a
recovery): no take was made, and Stop & open goes on with
`request_shutdown` and `wait_for_lock` as usual. (Changed in the WP0a
review: it returned the raw `CmdResult`, documented as always a finalized
take.)

| Holder | GUI |
| --- | --- |
| Free | `GuiHost::start` |
| a GUI host | D-13 page, exit 6 |
| `hello.record` Countdown | no take yet: like starting, `hello` every 50 ms (for up to `remaining` + 3 s) until it records (the next row) or is idle |
| `hello.record` Starting, Recording or Finalizing | X1: elapsed from `Recording.elapsed_ns`, `hello` every 1 s. Stop & open → `stop_recording` (≤ 60 s) → `request_shutdown` → `wait_for_lock` (≤ 20 s). Quit → exit 6, the recording continues |
| an export job | D-29 page; percent from `export_progress_view` over `hello.jobs`, polled every 500 ms. Wait → until no jobs, then `request_shutdown` (headless) or wait for the CLI host to exit, then `wait_for_lock`. Quit → exit 6 |
| idle | `request_shutdown` → `wait_for_lock` |
| starting or stopping | retry every 50 ms for 3 s, then not responding |
| unresponsive | "The Qrec process holding this project isn't responding" (pid, kind, since) + Quit, exit 6 |

WP0a implements the four functions completely; WP0b's interim pages drive
every row; WP8 restyles the pages per X1 and D-29 once approved.

### G§3.5 State delivery to GTK

```
session thread ── StateObserver ──► GuiPort ──┐
session notifier ─────────────────► GuiPort ──┤   Mutex<VecDeque<Inbound>>
GUI Responder (accepted/progress/reply) ──────┤   + one pending MainContext::invoke
startup workers, player, caches, testctl ─────┘          │
capture side channel ─► qrec-side pump ─► latest-wins slots ─► GTK main: drain in order
```

- **Observer.** `session::StateObserver { fn changed(&mut self, rev: u64,
  state: &State) }` in `SessionConfig.observer`. The Session calls it
  right after every assignment of `self.state`: the `self.state = next`
  in `run_outcome` (`session/effects.rs`) that every outcome reaches,
  durable or not; `keep_in_memory`; and the opened state. It runs before
  that outcome's `Notify`, `Reply` and `Progress` effects, so a reply
  never reaches the GUI before the state it describes, and runtime-only
  changes reach it too: `RecordClock` (~4 Hz `elapsed_ns`), export
  `seg_done`, `runtime.playing`, `recovering`, autocut `pending`. An
  ignored event may hand over an equal state; the GUI re-renders from the
  newest without diffing.
- The GUI's observer sends `Arc::new(state.clone())`. A clone per change
  is cheap at Qrec's sizes (hundreds of clips; history entries are
  already `Arc`). Switch `Session.state` to `Arc<State>` only if WP0a's
  debug timing shows more than 1 ms per clone (G§9 R14).
- **Late replies.** `Responder::reply_late(&mut self, r)` (default: calls
  `reply`) is called for `Effect::Reply`; immediate replies keep `reply`.
- **Queue** (`qrec-gui/src/host/queue.rs`):
  `Inbound::{Snapshot(rev, Arc<State>), Accepted(ticket), Progress(ticket,
  Progress), Reply { ticket, result, late }, Toast { toast, recovery },
  Host(HostEvent), Startup(StartupEvent), Player(PlayerEvent),
  Preview(PreviewData), Sources(SourceInventory), Test(TestRequest)}`.
  - `push` works from any thread.
  - A snapshot replaces a snapshot at the tail only (adjacent ones
    coalesce), so a reply is never reordered against its state.
  - At most one `glib::MainContext::default().invoke(drain)` is pending
    (`AtomicBool`); `drain` handles everything queued, in order, on the
    main thread (a thread-local holds the app).
- **High-rate data never enters the queue.**
  - `SideEvent::Level` (20 Hz per device): the latest reading per device
    plus a ring buffer (the last 15 min at 20 Hz, for the ghost waveform)
    in `Arc<Mutex<SideSlots>>`, one wake per batch.
  - `SideEvent::Preview` (before WP1, G§3.4.2): the latest frame.
  - `SideEvent::LiveStatus`: a slot. `SideEvent::LiveError`: a warning
    toast "Live preview problem" with the message.
  - The player position: an `AtomicU64` read on the frame clock.

### G§3.6 GTK-side model

```rust
#[derive(Clone)]
pub struct Ctx {                     // passed to every region constructor
    pub store: Store,
    pub keys: KeyRouter,
    pub actions: ActionRouter,
    pub media: MediaHub,
    pub theme: Theme,
    pub window: WindowLayers,
}

impl Store {
    pub fn state(&self) -> Arc<State>;
    pub fn local(&self) -> std::cell::Ref<'_, LocalUi>;
    pub fn update_local(&self, f: impl FnOnce(&mut LocalUi));
    pub fn subscribe(&self, f: impl Fn(&Change) + 'static) -> Subscription;
    pub fn subscribe_local(&self, f: impl Fn(&LocalUi) + 'static) -> Subscription;
    pub fn dispatch(&self, cmd: Command);                          // default reply policy (G§3.7)
    pub fn dispatch_with(&self, cmd: Command, confirmed: bool, done: impl FnOnce(Reply) + 'static);
    pub fn dispatch_then(&self, cmd: Command, after: impl FnOnce(&Reply) + 'static); // default policy, then `after` (a refused pick shows the project again)
    pub fn apply_default(&self, cmd: &Command, reply: Reply);     // the default policy, for a `dispatch_with` handler's own first step (R46)
    pub fn subscribe_close(&self, f: impl Fn(&()) + 'static) -> Subscription; // a debounce sent before the host closes (G§3.4.1, R66)
    pub fn before_close(&self);                                    // runs them (`close_host_then_quit`)
    pub fn notify(&self, notice: Notice);                          // a toast made by the GUI
    pub fn keymap(&self) -> Rc<Keymap>;
    pub fn sent(&self) -> std::cell::Ref<'_, SentValues>;          // echo rule
}
pub struct Change { pub prev: Arc<State>, pub next: Arc<State> }   // helpers: timeline(), scene(), ui(), record(), jobs(), takes(), history()
pub struct Reply { pub result: Result<serde_json::Value, CoreError>, pub late: bool }
```

`LocalUi` (`store/local.rs`, WP0b, frozen) holds the GUI-only state that
more than one region reads. Fields and their writers:

```rust
pub struct LocalUi {
    pub startup: StartupPhase,                   // WP0b
    pub doctor: Option<Arc<DoctorReport>>,       // WP0b (S23, wl-copy availability)
    pub focus: FocusRegion,                      // WP0b, from the window's focus widget
    pub drag: Option<DragKind>,                  // WP3 trim/move/scrub, WP4 layer drags and nudges, WP5 row reorder, WP7 bin → timeline
    pub overlays: Vec<Overlay>,                  // a stack, top last: WP0b, WP6, WP7, WP9
    pub panel_page: PanelPageKind,               // WP5
    pub autocut_preview: Option<Arc<AutocutPreview>>, // WP5 → WP3 draws
    pub take_preview: Option<TakePreview>,       // WP7 → WP4 plays (D-17)
    pub trim_frame: Option<TrimFrame>,           // WP3 → WP4 shows (S10)
    pub layer_drag: Option<LayerDragPreview>,    // WP4 → WP5 inspector
    pub viewport: Viewport,                      // WP3 (frame zoom → hints)
    pub hover_clip: Option<ClipId>,              // WP3
    pub bin_selection: Vec<TakeId>,              // WP7 (D-37)
    pub first_run_checked: Option<usize>,        // WP4 → WP6 hint "Add N sources"
    pub live_status: Arc<Vec<LiveSourceStatus>>, // WP0b side pump
    pub sources: Option<Arc<SourceInventory>>,   // WP0b refresh (S01a, Add menus)
    pub playing: bool,                           // WP4 player sync: the player plays, the program or a take
    pub program_playing: bool,                   // WP4 player sync: the program plays (G§3.10 step 3)
    pub playing_rate: f64,                       // WP4 player sync (D-44)
    pub export_view: ExportPanelState,           // WP7
    pub layout: LayoutClass,                     // WP0b: Wide | Medium | Narrow, and compact (G§3.15)
    pub keymap_problems: Vec<String>,            // WP0b (step 1 and reloads)
}
pub enum Overlay {
    Confirm { cmd: Command, error: CoreError },  // view WP6 (S18, S18b, overwrite)
    QuitWhileExporting { percent: u8 },          // view WP6 (D-12)
    ExportPanel(ExportPanelMode),                // view WP7; Running is passive (D-49)
    Settings(SettingsPage),                      // view WP6 (shell, Project), WP9 (other pages)
    Palette,                                     // view WP9
    CheatSheet,                                  // view WP9
    LayersSheet,                                 // narrow windows: layer WP0b, content WP5
    Menu,                                        // a PopoverMenu or DropDown is open
}
pub enum ExportPanelMode { Setup, Running, Done, Failed }
pub enum SettingsPage { Appearance, Recording, Project, Autocut, Keyboard }
```

- WP0b's `ModalHost` stub renders `Confirm` and `QuitWhileExporting` as a
  plain C24 text modal until WP6 replaces the view, so the close sequence
  (D-12) works from WP0b on.
- `LocalUi::facts() -> view::GuiFacts` feeds the hint bar and the header.
- **Echo rule.** A widget that sent a UI value (`SetUi` zoom or scroll,
  `Seek`) keeps its local value until a snapshot shows that value, or a
  value it didn't send. Values it didn't send (CLI `qrec seek`, another
  widget) are adopted at once. `Store::sent()` keeps the last value sent
  per `UiPatch` field and per `Seek`. Once a snapshot showed the sent
  value, the timeline forgets it: a later snapshot back at it is someone
  else's (review: the CLI's fit, while a local zoom waited, passed for the
  timeline's own old fit).
- **Debounces.** `SetUi{zoom, scroll_px}` 300 ms after the gesture's last
  event; `Seek` while playing at most once per second (G§3.10), while
  scrubbing at most 10 per second (D-40); player reload 100 ms; autocut
  dry run 250 ms, one in flight (D-15); bin sizes 500 ms; inspector
  fields on commit. A debounce still waiting when the window closes goes
  out right before its host closes: `Store::before_close` (in
  `close_host_then_quit`, before the media stop and `GuiHost::close`) runs
  each `Store::subscribe_close` callback, so the command enters the FIFO
  ahead of the close (G§3.4.1). The timeline's `SetUi` does (review R66: a
  zoom closed within its 300 ms was sent when its timer fired, to a
  closing host that refused it, and never reached `ui.json`), and so do
  the playing program's playhead (`SyncMachine::on_close`: `Seek{To(the
  player's frame)}`, as a stop parks it; the next once-a-second seek came
  after the close) and a nudge burst still waiting for its key's release
  (its one `LayerSet`; A1's audit of R66:
  `a_playing_programs_playhead_is_saved_at_the_close`,
  `a_nudge_burst_is_saved_at_the_close`). The others send nothing that
  stays: the dry run, the bin's sizes and the player's reload; a scrub or
  a drag still held ends with the window, unsent, as an `Esc` would end
  it.

### G§3.7 Dispatch, gestures, confirmations, toasts

- **One path.** A button or menu item with a binding calls
  `ctx.keys.run_binding(id)`, so it gets the key's pre-steps (G§3.8) and,
  like the key, runs nothing but `app.quit` before the project opens.
  Others build a `Command` and call `ctx.store.dispatch`. Nothing else
  submits requests (R8).
- **Default reply policy** (`store/replies.rs`):
  - `Ok` → nothing (the snapshot shows the change).
  - Immediate `Err` with `confirmation_required` → push
    `Overlay::Confirm { cmd, error }`. WP6 renders it from `view::confirm`;
    confirming re-dispatches with `confirmed: true`. A confirmation isn't
    a lock: the core checks again.
  - Other immediate `Err` → `view::notices::error_notice(&err, &cmd,
    &state)` → a toast, or nothing.
  - Late `Err` → log only: the core's `Notify` already toasted it (H-7;
    `crates/qrec-core/tests/props_late_errors.rs` holds every `on_event`
    path to that).
  - Widgets that own a flow (export panel, autocut page, purge) pass their
    own `done`. One whose command comes back `confirmation_required` opens
    the modal with `store.ask_confirmation(cmd, error, confirmed)`:
    confirming it hands `cmd` to `confirmed` (`Store::dispatch_confirmed`
    finds the hook), which sends it again with `confirmed: true` and its
    own `done`, instead of the default policy; the hook goes when the
    overlay closes unconfirmed. The export panel's Replace modal (D-24) is
    one: an export confirmed there shows Running, then Done or Failed, like
    any of the panel's own (WP7 integration; it ended in the core's toasts
    only). `modal.rs` sends the confirmed command before it closes the
    overlay (the hook goes with it).
- **Gestures** (A§4.1, SPEC §7.1).
  - During a drag the widget computes its preview with `view::*`. When it
    needs the resulting layout it may call `qrec_core::apply` on a clone,
    with `Ctx::preview(now, config)` (new pure helper), and discard the
    effects.
  - On release: one undoable Command (D-41). `Esc` cancels (Q3); other
    keys follow D-42.
  - A snapshot that arrives mid-drag recomputes the preview against it. If
    the dragged clip or layer is gone, the drag is cancelled; the
    timeline's ruler scrub and middle-drag pan are cancelled once it has no
    view (it went empty: the ruler and the canvas go, and their release
    never comes; review: the scrub kept `LocalUi.drag`, so the router
    swallowed every key but `Esc`), and so is a press GTK cancels.
  - A release Command that fails snaps back and toasts.
- **Toasts.** Core toasts come through the port; GUI toasts through
  `store.notify(Notice)`:

  ```rust
  pub struct Notice { pub level: ToastLevel, pub title: String, pub body: Option<String>, pub actions: Vec<NoticeAction> }
  pub enum NoticeAction { Core(UiAction), ShowExport, OpenBin, OpenSettings(SettingsPage), RetrySource(String) }
  ```

  ToastHost (WP6, C23): one toast at a time with a "+N" queue chip; info
  and success 5 s (8 s with actions), warning 8 s, error until dismissed;
  at most two actions are shown (DESIGN). `Dispatch(Undo{..})` stays
  enabled only while `history.cursor` equals its value when the toast
  arrived. A toast equal to the newest one (shown, or the last waiting) is
  that toast again: it takes the newest arrival's cursor and the shown
  one's time starts over (`ToastQueue::push`, `Pushed::Coalesced`; review
  fix: a held key whose action is unavailable queued one toast per
  auto-repeat, "+36" after 2 s). Since R30 the key router swallows such a
  key's repeats itself (`router::on_repeat`), so the coalescing is for the
  same notice sent again by any other path: a button or menu item pressed
  again, the same action run repeatedly, the core's same toast twice
  (`the_same_toast_thirty_times_is_one`: `keys.help` 30 times through the
  test channel, one toast; R29's audit). One between others still counts
  (A, B, A is "+2"), and one arriving while the shown one fades out waits
  after it. A done-export toast is dropped while the export panel shows
  that export's Done state, and the export-failed one while it shows its
  Failed state (`toasts::shown_by_the_panel`; WP7 integration: S17 draws
  no toast over the panel). The panel answers first: the core sends the
  reply before the toast, through the same queue. Only that export's: its
  answer marks the next export toast as the one its page shows
  (`ExportPanelState::toast_due`, which that toast takes back), so another
  export's toast shows whatever the panel shows (review fix: a CLI export
  the window never saw run, its snapshots coalesced in the queue, lost its
  toast to the panel's own last Done page). `toasts::panel_shows` makes
  that decision and the take-back in one step, on the Store, and a unit
  test drives the sequence (R37's audit: the host's own take-back could go
  unnoticed). The page and its mark are one private field of
  `ExportPanelState`: `show_end`, the only way to a Done or Failed page,
  sets the mark with it, and the page going (`clear_end`) takes the mark
  along, so no path to the page can forget it (only `Panel::answered` set
  it, by hand; `a_done_or_failed_page_comes_with_its_toasts_mark`).
- **`UiAction`** (`desktop.rs`, WP0b):
  - `OpenPath` → `gtk::FileLauncher::launch`;
  - `RevealPath` → `gtk::FileLauncher::open_containing_folder`;
  - `CopyText` → `gdk::Clipboard::set_text`;
  - `CopyImage` → `wl-copy --type image/png` with the PNG on stdin, through
    the shell's `proc` runner on a worker (`WL_COPY_TIMEOUT`, stderr to
    `.qrec/logs/…-wl-copy.log`); hidden when doctor's `wl-copy` check
    failed;
  - `Dispatch(cmd)` → `store.dispatch`.

  A failure toasts ("Couldn't open <path>").

### G§3.8 Keyboard (A§9.7)

`KeyRouter` (`keys/mod.rs` glue, WP0b) holds one `gtk::EventControllerKey`
on the window in `PropagationPhase::Capture` and asks the GTK-free
`keys::router::decide(&KeyInput, &RouterState) -> KeyDecision` (WP0b). For
each key press, first match:

1. Chord: `KeyChord::from_gdk(keyval.name(), ctrl, alt, shift, super)`;
   `None` → let it propagate.
2. Focus in an editable (`gtk::Text`, `Entry`, `SearchEntry`,
   `EditableLabel` while editing, `NumericField`) → propagate. The field
   handles `Enter`, `Esc` and `Tab`, and a held key's repeats.

   Auto-repeat (review fix R30): Wayland clients repeat a held key as
   presses with no release between, and GTK has no repeat flag, so the
   controller follows the releases too and keeps the hardware keycodes
   held (`router::HeldKeys`, forgotten when the window stops being
   active): a press of one already down is a repeat (`KeyInput.repeat`).
   A repeat goes on with the steps below only when they run one of
   `REPEATING_BINDINGS` (`seek.*`, `select.prev`/`next`, `clip.move-*`,
   `clip.nudge-*`, `layer.nudge-*`, `zoom.in`/`out`,
   `history.undo`/`redo`), or propagate a key that isn't `Enter` or
   `Space` (a focused widget's, a list's or an overlay's navigation);
   anything else is swallowed: toggles, `record.*`, `cancel` and step 5's
   chain, a button's activation, a reason already said. (Holding `R` past
   the repeat delay started a take and stopped it 600 ms later; held
   `Space`, `Tab`, `b` and `n` flapped.)
3. A drag in progress (`LocalUi.drag`) → D-42: `Esc` cancels it; `n`
   toggles snapping during a trim; modifiers go to the gesture; during a
   `LayerNudge` drag the `layer.nudge-*` keys go on with the burst (D-33,
   `NUDGE_BINDINGS`; WP4); anything else is swallowed.
4. The top overlay traps focus (Modal, Settings, Palette, CheatSheet, Menu,
   LayersSheet, or the export panel in Setup, Done or Failed): `Tab`,
   `Enter`, `Space` and the arrows propagate to the overlay's widgets;
   `keymap::lookup` in context `Overlay` (`cancel` closes the top overlay,
   except on the autocut page in the Narrow layers sheet, where it is the
   page's Cancel, GUI action `autocut.page`, as step 5 docked; `app.quit`);
   nothing else.
5. `Esc`, before the lookup, first that applies: close the passive export
   panel (D-49) → close the autocut page (its Cancel) → end a bin preview
   → `action_for("cancel")` (`RecordCancel`, clear the clip selection,
   clear the layer selection). "Nothing to cancel" is silent.
6. `LocalUi.focus == Bin` and the chord is `↑` or `↓` (± `Shift`),
   `Return`, `KP_Enter` or `space` → the bin's widget keys (D-11, D-37):
   move or extend the bin selection, insert at end, toggle the preview.
7. A widget with visible keyboard focus that is activatable or a list
   (button, toggle, check, switch, scale, dropdown, list row) and a
   navigation chord (`space`, `Return`, `KP_Enter`, arrows, `Home`, `End`,
   `Page_Up`, `Page_Down`) → propagate.
8. Context: record phase not Idle → `Recording`; focus in the stage canvas
   with `edit_target` and `ui.selected_layer` set → `Canvas`; otherwise
   `Timeline` (bindings active in `Global` are active there too,
   `Binding::active_in`).
9. `keymap::lookup(keymap, chord, ctx)`; none → propagate.
10. GUI overrides: `export` → GUI action `export.panel`; `autocut.preview`
    → GUI action `autocut.page`; a runnable `layer.nudge-*` → the stage's
    GUI action of that id (D-33's burst, the stage registers them; WP4);
    `zoom.fit` → the timeline's GUI action of that id, which fits its own
    view (review: from fit the core's `SetUi{zoom: Clear}` changes nothing
    a snapshot shows, so a local zoom still waiting for its `SetUi` went out
    after it and won; the CLI and the palette's `command_for` keep the
    command).
11. `keymap::action_for(id, &effective_state)`, where `effective_state`
    is the snapshot with `ui.playhead` = the player's frame while the
    program plays (`LocalUi.program_playing`, G§3.10 step 3):
    - `Run(cmd)` → pre-steps, then dispatch; a step while the program
      plays → `Pause`, then `Seek{To(where it lands)}` (D-51);
    - `Gui` → `ActionRouter::run(id)`;
    - `Unavailable(reason)` → D-34.

    Stop propagation.

Pre-steps:
- While the program plays, every command but an absolute seek
  (`Seek{To(f)}`, `Start`, `End`), when the player's frame isn't the
  core's playhead → `Seek{To(player_frame)}` first. The core resolves
  splits, trims, nudges, `frame.save`, frame steps, ±1 s, edit jumps, a
  shuttle's start (the forward reset at the end, the reverse stop at 0)
  and a selection by the playhead from `ui.playhead`, which lags the
  player by up to a second while playing (R18; it was only the first
  four).
- A step (`seek.prev-frame`, `seek.next-frame`, `seek.back`,
  `seek.forward`, `seek.prev-edit`, `seek.next-edit`; `STEP_BINDINGS`)
  while the program plays → `PreStep::Pause{park}` instead: the sync is
  told the step parks the playhead itself (`media::program::key_parks`),
  then `Pause` goes out, and the command is the absolute
  `Seek{To(park)}`, `park` being where the step lands from the player's
  frame (D-51; R18's audit: the pre-step seek plus the relative step
  landed within the sync's tolerance of the playing player, which
  played on).
- `record.toggle` and `record.over` from Idle in Program mode →
  `SetCanvasMode{Live}` first (D-05).
- `shuttle.*` → set `LocalUi.playing_rate` first; while already playing,
  also `ProgramPlayer::play(rate)` (D-44).

`ActionRouter` (`keys/actions.rs`, WP0b): `register(id, Rc<dyn Fn()>)`,
`run(id)`; each region registers its ids in its own constructor.
Registered by: `zoom.in`, `zoom.out`, `zoom.fit`, `zoom.selection` (WP3);
`settings.open` (WP6); `export.panel`, `export.show`, `bin.preview` (WP7;
`export.show`, a toast's Show export, only opens the panel: the toggle
closed the passive panel the D-34 toast shows under);
`autocut.page` (WP5); `region.next`, `region.prev`, `app.quit`, and
`keys.help` and `palette.open` as the "Not built yet" toast (WP0b; WP9
re-registers them). An id nobody registered logs at debug level.

Focus: chrome buttons are `focus-on-click = false`; the stage canvas, the
timeline canvas and bin rows are focusable and take focus on click;
`LocalUi.focus` follows `window.focus-widget` (each region's root widget is
named `region-<name>`; the focused widget's nearest such ancestor). A
press outside an edit in progress ends it before the press acts (G§3.12
R12), so a click on a chrome button never leaves a field focused under
its action.

### G§3.9 Live preview, meters and live status (shell WP1, GTK WP4)

- **Preview sink.** `EngineOptions.preview: Option<PreviewOutput {
  sink: gst::Element, max_size: (u32, u32) }>` (feature `gst`; type from
  WP0a, behavior WP1).
  - With it, the preview branch is `compositor !
    video/x-raw,format=BGRx,width=pw,height=ph,framerate=fps ! queue
    leaky=downstream max-size-buffers=1 ! <sink, sync=false>`.
  - (pw, ph) = the canvas fitted into `max_size`, even. Pad geometry =
    `layout::place` dst × (pw / canvas.w).
  - BGRx in system memory, drawn by GSK GL, was the cheapest
    system-memory path for a 1080p30 test stream: 29 % process CPU and
    4.3 ms main-thread paint (p50) *(spike)*; NV12/I420 into the sink cost
    about 59 %. Compositing two sources at canvas size was the larger cost
    (46–65 % in the spike's gst-launch runs), so compositing at 960×540
    matters more than the sink format.
  - The sink is reused across rebuilds: the old pipeline goes to NULL
    (≤ 5 s), the sink is removed from it and added to the new one.
  - Without `preview`, the RGBA appsink → `SideEvent::Preview` path stays
    (tests, and the GUI before WP1 merges).
  - `PREVIEW_SINK_SUPPORTED = true` since WP1 part 2 merged (2026-10-16):
    the opener passes the live sink and `MediaHub`'s live picture follows
    its paintable (`FramePaintable::set_source`), so WP0b's interim stage
    already shows it; checked in nested sway through Program and back and
    a take. The engine lays a rebuilt pipeline out before it starts (A§8.11):
    with the sink's `async=false`, a crop changed after the start killed a
    `cover`-cropped camera "not-negotiated" in about half the round trips.
- The GTK thread creates the sink (G§3.3 step 5) and holds a reference
  until exit, so the paintable is always finalized on the main thread. It
  never waits on the engine (R4).
- **Live status.** New `SideEvent::LiveStatus(Vec<LiveSourceStatus>)`,
  sent whenever it changes (after builds, portal answers, deaths and
  rebuilds that change a source's state or size; a rebuild that changes
  nothing sends nothing), in the scene's order (layers in array order,
  bottom first, then audio; consumers look a source up by name,
  `live_sources_view`):

  ```rust
  pub struct LiveSourceStatus { pub source: LiveSource, pub state: LiveSourceState, pub size: Option<(u32, u32)> }
  pub enum LiveSource { Screen { output: String }, Camera { device: String }, Audio { device: String } }
  pub enum LiveSourceState { AwaitingPortal, Live, Refused { cancelled: bool, message: String }, Dead { message: String } }
  ```

  `size` feeds the canvas layer boxes in Live mode. `Refused { cancelled:
  true }` is a closed picker (S24 variant); `false` is a denial. A screen
  asked of the portal again (a retry, a take's dialog) is `AwaitingPortal`
  while the dialog is open, before its old `Dead` branch or `Refused`.
- **`LiveControl`** (`GstCaptureEngine::control()`, `Clone`):
  `transient_layout(Option<Scene>)`, and `progress()` (round 3
  integration fixes): `LiveProgress { settled, generation }`, where the
  engine is with the live scenes it was given, for `waitIdle` (G§3.18):
  `settled` once every `set_live`/`retry_live` so far was handled and each
  video source of the live pipeline negotiated in it (or died, or there is
  no pipeline); `generation` is the pipeline the preview comes from
  (`capture::LiveTracker`, counted on the caller's thread). It lays the
  preview out until the next `SetLive` (any) or `None`, and never touches
  State: the
  compositor's pads move at once; a changed `videocrop` crop goes with its
  pad to the layer's streaming thread, which applies both before the next
  caps event or buffer (so a Live crop shows at the source's next frame; a
  still screen's comes with its 500 ms keepalive). A `videoscale` before
  each `videocrop` answers the source's caps queries for any size, only
  the crop sides that change are written, and record bins join their tee
  without RECONFIGURE, so no crop can leave a source without caps and the
  layout never reaches a take (A§8.11; WP4 closed the race, the stress
  tests `transient_crops_never_lose_a_source` and
  `takes_keep_every_stream_under_transient_crops` hammer it). The GUI sends
  it during a Live layer drag or nudge burst (D-32, D-33) and sends `None`
  on cancel or after a failed release.
- **Retry** (D-50): `Msg::RetryLive` → `CaptureEngine::retry_live(out)`:
  forget refused and dead sources, rebuild. It runs on the session thread,
  ordered with `SetLive` and `StartCapture`, and is logged.
- **Cast grace** (H-4): after `SetLive(None)` with no take, the casts stay
  open for `cast_grace`, then close.
- **`MeterMonitor`** (`capture/meters.rs`, D-03):
  - `MeterMonitor::start(devices: Vec<String>, backend, side_tx)` and
    `stop()` (asynchronous, ≤ 5 s);
  - one pipeline per device: `pulsesrc device=X ! audioconvert ! level
    interval=50ms ! fakesink sync=false` (fake backend: the engine's
    `audiotestsrc` tone with silences);
  - readings as `SideEvent::Level`, like the engine's.

  `MediaHub` (WP0b) runs it exactly when the Session is open, `record` is
  Idle and `canvas_mode == Program` (the capture pipeline is down then),
  with the project scene's tracks. PipeWire allows a second stream on the
  same source.
- Meters (WP5, C11) read `MediaHub::level(device)` on each frame tick;
  `device` = `AudioTrack.source`.

### G§3.10 Program player and sync (shell WP1, GTK WP4)

```rust
// qrec-shell/src/player.rs (feature gst)
pub struct PlayerOptions {
    pub project_dir: PathBuf,
    pub video_sink: gst::Element,                    // the program gtk4paintablesink (GTK thread)
    pub audio: AudioOut,
    pub out_size: Option<(u32, u32)>,                // None = canvas size; Some = composite at the canvas fitted into this bound (ges_plan::fit_size: aspect kept, even sides, never larger than the canvas; H-9)
    pub events: Box<dyn Fn(PlayerEvent) + Send + Sync>,
}
pub enum AudioOut { System /* pulsesink client-name=Qrec; fakesink + warning if it can't start */, Silent /* fakesink sync=true */ }
pub enum PlayerEvent { Loaded { duration_ns: u64 }, Ended { reverse: bool }, Error { message: String }, Late { dropped: u64 }, Stopped }
impl ProgramPlayer {
    pub fn spawn(opts: PlayerOptions) -> std::io::Result<Self>; // thread qrec-player
    pub fn load(&self, plan: GesTimeline, at_ns: u64);          // newest wins; keeps play/pause
    pub fn seek(&self, ns: u64);                                // FLUSH|ACCURATE; one in flight, newest wins
    pub fn play(&self, rate: f64);                              // 1, 2, 4; negative = reverse stepping; also changes the rate while playing
    pub fn pause(&self);
    pub fn scrub(&self, ns: u64);                               // seek + 60 ms of audio
    pub fn position_ns(&self) -> u64;                           // AtomicU64: read about 125 times a second while playing forward; in reverse it moves once per step (≤ 15 a second)
    pub fn idle(&self) -> bool;                                 // nothing to load, seek or scrub, queued or in flight (waitIdle, G§3.18)
    pub fn shutdown(&self);                                     // asynchronous; Stopped once NULL (≤ 5 s), at once when there is no pipeline
}
pub const STUCK: &str = "the Program picture is stuck; restart Qrec";
```

- `AudioOut::Silent` under the fake backend and in tests.
- One `ges::Timeline` and one `ges::Pipeline` (`FULL_PREVIEW`,
  `ges_render::preview_pipeline`, `bilinear_compositors`) for the player's
  life. `load`:
  1. pauses if playing;
  2. clears the layers and rebuilds from the plan (new
     `ges_render::rebuild_into(&timeline, &plan, dir, &mut retired)`),
     updating the restriction caps if the canvas or `out_size` changed;
  3. commits and waits for "commited" (a stopped pipeline prerolls first:
     it applies a commit only as it starts), an accurate seek to `at_ns`,
     then resumes.

  The old layers stay in `retired` (`ges_render::Retired`) until that
  commit applied or the pipeline is READY or NULL: GES frees a removed
  clip's sources with the clip while their `uridecodebin`s still run in
  the composition, and one still starting calls into the freed source
  (SIGSEGV in GES's `_set_ghost_pad_target`; found merging WP0b part 1).

  Asset discovery runs on the player thread only. The bus uses a sync
  handler, never a watch on the default MainContext (R5).
- **Errors and wedges.** An error is reported (`Error`) before the
  pipeline stops. Stopping, READY after an error or for an empty plan and
  NULL at `shutdown`, runs on a helper thread for at most 5 s
  (`STOP_TIMEOUT`): READY waits for every streaming thread to leave its
  pad, and one stuck where flushing doesn't reach (a decoder or GES
  deadlock) never does. A pipeline wedged past that is abandoned (leaked,
  never freed under its threads; the Program sink is inside it, so no new
  pipeline can take over): every later load reports `STUCK`, seeks do
  nothing, and `shutdown` says `Stopped` at once. While shutting down, an
  error never takes the pipeline to READY: `shutdown`'s bounded NULL does
  it. A bus error while a load or seek is in flight fails it at once;
  only the follow-on errors of a failure already reported are logged.
- Forward rates are seeks with `rate`. Reverse is stepping: accurate
  seeks at most 15 a second (one per canvas frame period, never more
  often than every 67 ms; G§9 R4) to where |rate| × the time since
  reverse play started (or since the last seek) puts the playhead,
  frame-aligned, with the pipeline paused and video only (GES reverse
  decoding isn't relied on); `position_ns` moves at each step.
- **Speed** (SPEC §12: canvas fps with 2 screens + 1 camera). GES decodes
  in software (`ges_render::init` demotes hardware decoders for the
  process) and composites at the restriction size, the dominant cost in
  the spike. WP1 measures it on the dev machine before anything else
  (WP1 acceptance); if it fails, `out_size = Some(max preview size)` with
  `ges_plan::scaled` (destination positions and sizes × out/canvas, edge
  by edge to even pixels; source crops stay in file pixels, since GES
  crops before it scales) and the `PROXY` badge (H-9), shown when
  `fit_size(canvas, out_size)` isn't the canvas (not merely when
  `out_size` is set). Parity tests and `FrameRenderer` stay at canvas
  size.
- **Core additions** (WP1): `ges_plan::plan_ges_take(&Project, &TakeId) ->
  GesTimeline` (one clip `[0, durationFrames)` in the take scene: S10
  scrub, D-17); `ges_plan::without_tracks(plan, solo: Option<&TrackId>,
  muted: &BTreeSet<TrackId>) -> GesTimeline` (solo, D-08);
  `ges_plan::scaled(plan, w, h) -> GesTimeline`. `GesVideoClip` gains
  `take` and `layer`, `GesAudioClip` `take` and `track` (WP0a,
  `plans.rs`).
- **Sync protocol** (`qrec-gui/src/media/program.rs`, WP4): a pure state
  machine plus thin glue, unit-tested with a fake player.
  1. Plan = `without_tracks(plan_ges(project), ui.solo, ui.lane_mute)`. On
     a change (`PartialEq`) → `load(plan, current position)` after 100 ms.
     While the stage shows Live, only mark it dirty.
  2. `runtime.playing` false → true: `play(LocalUi.playing_rate)` from
     `ui.playhead` (the rate is 1 unless the GUI sent the Shuttle, D-44).
     true → false: `pause()`, then `Seek{To(player_frame)}`; the rate
     resets to 1. Leaving Program while playing (`Tab`, D-05's record
     start, CLI `SetCanvasMode`) is the same stop: the core turns
     `playing` false in the snapshot that switches to Live (R21). A
     step's stop (D-51) parks nothing: the step's own `Seek{To(f)}` comes
     after its `Pause` and puts the playhead on `f`, so the sync
     (`SyncMachine::note_key_park`) sends no once-a-second seek meanwhile
     (it would come after the step's and undo it), only pauses, then holds
     the paused player until the snapshots reach `f` (at most a second;
     shown at once when the stop's snapshot has it) instead of showing the
     lagging playhead between.
  3. While playing, the drawn playhead is `position_ns`;
     `Seek{To(player_frame)}` goes out every second (an own seek); the
     timeline auto-pages. The own seek waits (polling every 20 ms) while a
     `Seek` the window sent is unanswered (`Store::seek_unanswered`: Home,
     End, a click on the ruler, a key's pre-step; a step's park, D-51):
     sent from a snapshot without that seek, it came after it and undid
     it, and the player went back with the playhead (at full speed the
     window is the 20–30 ms a seek takes to come back, each second;
     `a_seek_while_playing_lands_and_plays_on` holds every command 400 ms;
     found verifying D-51). A seek from outside the window (the CLI's, an
     agent's) can still meet it. "Playing" is `LocalUi.program_playing`: the
     program plays in the player, not a bin preview's take (R00, R16), and
     only once the program's load after a take landed (the player idle),
     since until then `position_ns` is the take's. The timeline's playhead,
     auto-page and corner, the transport, the key router's effective
     playhead and `GuiFacts::playing` (the hint bar's Play/Pause) follow
     it; `LocalUi.playing` says the player plays anything.
  4. Paused: a snapshot playhead the player isn't on → `seek()`, except
     while a ruler scrub holds it (step 9). While playing, a playhead more
     than 2 frames off the player's moves it (WP4), unless the GUI set it
     itself: by its once-a-second seek, or by a key's pre-step seek to the
     player's frame (G§3.8; the key router tells the sync the frame it
     took, `media::program::key_took_player_frame`, and a playhead at it
     within a second is own: at 2×/4× or stepping in reverse the player is
     past it by the time its snapshot comes, R22;
     `a_keys_seek_while_playing_never_pulls_the_player_back` sees the
     wiring, with every command held 200 ms, R22's audit).
  5. `Ended` → `Pause`, then `Seek{End}` (reverse: `Seek{Start}`).
  6. `Error` → error toast "Playback stopped" with the message; stays
     paused. A bin preview's take that fails gets `take_preview.playing =
     false` too, so the preview doesn't play straight into the failure
     again (a toast each round, R17); the next `Space` plays it from 0.
  7. Shuttle: `L` → rate 1 → 2 → 4; `J` → −1 → −2 → −4; `K` stops. Reverse
     at 0 stops (core).
  8. Trim scrub (`LocalUi.trim_frame`) and bin preview
     (`LocalUi.take_preview`): `load(plan_ges_take(..), frame)`; when they
     end, `load(program plan, ui.playhead)`. When a bin preview plays to
     its end (or fails, step 6) the sync sets `take_preview.playing =
     false` (WP7 owns the field). A bin preview that takes over a playing
     program pauses it, so the core doesn't play under the take (R16), and
     parks its playhead at the frame the player showed, as a stop does
     (step 2): `Pause`, then `Seek{To(f)}`, f read before the take loads
     (R16's follow-up: it stayed at the last once-a-second seek, up to a
     second behind the picture, and the program came back there). A
     playhead change ends a preview (D-17), so the park is the preview's
     own: `TakePreview.parking` holds from the takeover until the park's
     reply, and meanwhile a playhead change (the park's, a once-a-second
     seek still on its way) doesn't end it
     (`a_preview_parks_a_playing_programs_playhead`,
     `gui_bin_export::a_bin_preview_parks_the_playing_program_where_it_showed`).
  9. Ruler scrub (D-40, round 3): the timeline calls
     `media::program::scrub(frame)` on each motion of a ruler drag and
     dispatches the throttled `Seek`s itself. The sync scrubs a paused
     player that holds the program in Program mode (not while playing,
     in Live, or with a take in the player: the drag's seeks move it
     then), and holds it there: no seek while the drag runs (its seeks'
     snapshots lag behind the scrubs) or after it until the snapshots
     reach the last scrubbed frame (the GUI's last `Seek` being that
     frame's), at most `SCRUB_ECHO_MS` (1 s, a refused seek never echoes).
     A start, a load or a take in the player ends the hold. Before, the
     timeline called `ProgramPlayer::scrub` itself and the sync sought
     the player again after every release.
- Audio scrub: a ruler drag scrubs on every motion event through the
  sync (step 9), one in flight (D-40). A scrub that comes while a load
  waits is a plain seek (that load's seek, so the load still says
  `Loaded`).

### G§3.11 Thumbnails, waveforms, silence overlay, bin sizes (WP2)

Derived data, never State (SPEC §6.1: `.qrec/cache/` can be deleted at any
time; A§8.7 rules for every child).

- **Core argv** (`media_argv.rs`, pure, insta):
  - `strip_argv(file, chunk, height)`: `ffmpeg … -ss <60·chunk> -t 60
    -skip_frame nokey -i file:<f> -an -vf fps=1,scale=-2:<h> -f rawvideo
    -pix_fmt rgba pipe:1`: keyframes only (one per second in recordings,
    A§8.11), image i ↔ file time 60·chunk + i s. A chunk that reaches the
    end of the file has one image less than its whole seconds (ffmpeg's
    `fps=1` at EOF: 34 for a 95 s file's chunk 1); one past the end has
    none.
  - `frames_argv(file, start_ns, count, height)`: `[-ss <start − 2 ms>] -i
    … -an -frames:v <count> -vf scale=-2:<h> -f rawvideo -pix_fmt rgba
    pipe:1`: consecutive recorded frames from the one at `start_ns` (frame
    zoom; timeline frames map to recorded ones with `shown_frame`, A§9.3);
    `count` ≤ 64 (`MAX_PREVIEW_FRAMES`). The seek backs off 2 ms like a
    layer input's and is left out when that is ≤ 0.
  - `peaks_argv(file)`: `-map 0:a:0 -ac 1 -ar 48000 -f s16le pipe:1`;
    `bucket_peaks(samples, 48) -> Vec<(i16, i16)>` (1 ms buckets) and
    `peak_levels(per_ms) -> PeakLevels` (1, 16 and 256 ms buckets, so a
    lane draws from the coarsest level with at least one bucket per px:
    `PeakLevels::level(ms_per_px)`). `PeakLevels` lives here and is
    re-exported as `media::preview::PeakLevels`.
  - `preview_probe_argv(file)`: `ffprobe -v error -select_streams v:0
    -show_entries stream=width,height,r_frame_rate -of json file:<f>`, a
    video's size and rate from its header alone (no `-count_packets`, so
    quick on an hour-long file). The cache runs it once per file version
    before a strip or frames run (logged as kind `probe`, in the job's
    child slot; one at a time per version, and a failed one answers from
    memory for 30 s, `PROBE_RETRY_AFTER`, so the chunks of an unreadable
    file share one probe): `Frames` names a recorded frame while `frames_argv` takes
    `start_ns` (the recorded rate is needed), and raw RGBA can't be cut
    into images without the width. `preview_width(src_w, src_h, h)` is
    `scale=-2:<h>`'s width, rounded as ffmpeg does (checked against it for
    odd sizes).
  - `preview_key(kind, file, size, mtime_ns, params) -> String` (sha256,
    like `SilenceKey`).
- **Shell** `media::preview::PreviewCache` (`Clone`):
  `new(project_dir, on_data: Box<dyn Fn(PreviewData) + Send + Sync>)`,
  `request(PreviewRequest)` (deduplicated by key: a request already queued
  or running is joined, one answer for all; one still queued that is asked
  for again moves up to the newest: the GTK getters ask on every draw, so
  what is on screen now runs before what scrolled by), `prefetch(PreviewRequest)`
  (added in the rounds 1–2 review: queued after every `request`, at the
  oldest end; a queued one keeps its place), `set_exporting(bool)`,
  `shutdown()`, `in_flight()` (requests queued or running, an answer being
  handed to `on_data` included: `waitIdle`, G§3.18). Two workers
  (`qrec-preview-<n>`) take the newest request first; at most 2 ffmpeg at
  once, 1 while an export runs (`MediaHub::follow` sets it while an export
  job runs). Also public: `PreviewRequest::{file, what}`,
  `PreviewData::{file, image_count}`, `WHAT_*` (a `Failed`'s `what`),
  `image_bytes`, `trim_caches`, `trim_dir`.

  ```rust
  pub enum PreviewRequest {
      Strip { file: String, chunk: u32, height: u32 },             // 60 s of one-per-second images
      Frames { file: String, first: u64, count: u32, height: u32 }, // ≤ 64 recorded frames
      Peaks { file: String },
      CachedSilence { file: String, threshold_db: f64, min_silence_ms: u32 },
  }
  pub enum PreviewData {
      Strip { file: String, chunk: u32, height: u32, width: u32, images: Arc<[u8]> }, // RGBA, image after image
      Frames { file: String, first: u64, width: u32, height: u32, images: Arc<[u8]> },
      Peaks { file: String, levels: Arc<PeakLevels> },
      Silence { file: String, key: SilenceKey, intervals: Option<Vec<Interval>> },     // None: never analyzed
      Failed { file: String, what: &'static str, message: String },
  }
  pub struct PeakLevels { pub per_ms: Arc<[(i16, i16)]>, pub per_16ms: Arc<[(i16, i16)]>, pub per_256ms: Arc<[(i16, i16)]> }
  ```

  - Disk: `.qrec/cache/thumbnails/<key>.rgba` + `<key>.json` (`{width,
    height, count}`) per strip chunk, and `.qrec/cache/waveforms/<key>.peaks`
    (the 1 ms level; the others are recomputed on load), via
    `ProjectPaths::{thumbnails_dir, waveforms_dir}`; tmp + rename. A short,
    foreign or mismatched file is a miss and is made again; a hit refreshes
    the file's mtime (the trim goes least recently used first). `Frames`
    stay in memory only.
  - Disk caps, enforced oldest first when a cache starts
    (`PreviewCache::new`, startup step 8, once the host opened the
    project), on a short thread of its own (`qrec-preview-trim`), which
    also sweeps temp files older than an hour: thumbnails 2 GiB, waveforms
    512 MiB.
  - `CachedSilence` only reads the analysis cache
    (`.qrec/cache/silence/<key>.json`, A§8.7) and never runs ffmpeg.
  - Children: stdout drained on its own thread; stderr drained by a thread
    that appends each line, prefixed `[<pid> <kind>]`, to one rolling
    `.qrec/logs/preview.log` (rotated to `preview.1.log` past 8 MiB); the
    first failed run of a file version and kind (per cache) also writes its
    last 30 lines to its own `YYYYMMDD-HHMMSS-<pid>-preview-<kind>.log`, and
    later failures of it add a line naming that log to `preview.log`. Both
    A§8.7 prunes (`proc::prune_logs`, which the Session's open runs, and
    `media::prune_logs`) keep the newest 200 timestamp-named logs
    (`media::is_stamped_log_name`) and ignore `preview*.log`: preview runs
    that work never evict export and recorder logs, and a failed run's own
    log counts like any job log (one per broken file version and kind).
    Timeout 20 s + 30 s/GiB, own process group, killed on shutdown. A
    captured or streamed stdout that isn't read to the end within 2 s of a
    clean exit fails the run (`child::Finished::stdout_complete`): nothing
    partial is cached.
- `media::take_sizes(project_dir, &[TakeId]) -> BTreeMap<TakeId, u64>`:
  blocking (from `take_contents`), called on a worker. A take whose folder
  is gone or wasn't looked into (a symlink, unreadable) is left out; an
  empty folder is 0.
- **GTK caches** (`qrec-gui/src/media/{thumbs,peaks}.rs`; API frozen by
  WP0b, implemented by WP2). Getters never block; a miss requests the
  containing chunk or block and returns `None`, and a getter on one asked
  for and not answered asks again (the cache moves it up); `request_strip`,
  `request_frames` and `PeakCache::request` are prefetches
  (`PreviewCache::prefetch`: after every getter's request); subscribers get
  the file whose data arrived and redraw.

  ```rust
  impl ThumbCache {
      pub fn new(cache: PreviewCache, budget_bytes: usize) -> Self;   // 256 MiB of RGBA, LRU by bytes
      pub fn strip_image(&self, file: &str, file_time_s: u32, height: u32) -> Option<gdk::Texture>;
      pub fn frame(&self, file: &str, recorded_frame: u64, height: u32) -> Option<gdk::Texture>;
      pub fn request_strip(&self, file: &str, chunk: u32, height: u32);  // prefetch; idempotent
      pub fn request_frames(&self, file: &str, first: u64, count: u32, height: u32);
      pub fn subscribe(&self, f: impl Fn(&str) + 'static) -> Subscription;
      pub fn bytes(&self) -> usize;
  }
  impl PeakCache {
      pub fn new(cache: PreviewCache, budget_bytes: usize) -> Self;   // 128 MiB, LRU by bytes
      pub fn peaks(&self, file: &str) -> Option<Arc<PeakLevels>>;
      pub fn request(&self, file: &str);
      pub fn subscribe(&self, f: impl Fn(&str) + 'static) -> Subscription;
      pub fn bytes(&self) -> usize;                                    // G§8.6
  }
  ```

  Both also have `on_data(&PreviewData)`, which `MediaHub::on_preview`
  calls with every answer from the queue before its own subscribers. Each
  sits over a GTK-free store (`ThumbStore`, `PeakStore`, on one byte-bounded
  LRU): strips per 60 s chunk (a time past a chunk's last image shows that
  image), recorded frames in aligned blocks of 64 (`FRAME_BLOCK`); an
  entry larger than the whole budget is never kept. `PreviewData::Failed`
  names no chunk or block, so a failure counts for the file (and that
  kind), asked again after 30 s (`RETRY_AFTER`), for one entry at a time
  until one works (then the rest as they are drawn).

  Textures are `gdk::MemoryTexture` (R8G8B8A8) built on the main thread
  from the shared bytes (R22), once per image, from a zero-copy slice of
  the chunk's `Arc<[u8]>`, and dropped with their chunk. Strip height =
  72 × `ui.scale` × the window's scale factor, rounded up, at most 1080
  (`timeline::control::thumb_px`: 72, 90 and 108 at 100, 125 and 150 % on
  a 1× output, twice that on a 2× one; the canvas paints scaled by the UI
  scale, so a texture is drawn at its own size; review: the UI scale was
  left out and the strips upscaled). A one-hour take is 60 chunks of about
  2.2 MB at 72 px, loaded only for what is on screen. The canvas's cached
  node is kept per generation, size and strip height, and a theme change
  (colors, scale) drops it.
- **Silence overlay** (WP3): `view::timeline::silence_spans(state, files:
  &BTreeMap<String, FileFact>, cached: &BTreeMap<SilenceKey,
  Vec<Interval>>)` → hatches on the autocut track's lane, at the
  project's current threshold and min silence, from `runtime.silence` plus
  the `CachedSilence` answers. The file facts (`facts::file_fact`) are
  stat'ed on a worker. The timeline asks for each cached analysis once per
  `SilenceKey` its answer carries (`timeline::control::silence_requests`:
  the file's version, the threshold and the min silence), again whenever
  the autocut settings change, from the CLI, IPC or an undo too (review:
  the dedupe left the min silence out, so a new min silence never fetched
  its analysis, and a settings change waited for another one).

### G§3.12 Threads and rules

| Thread | Owns |
| --- | --- |
| GTK main | widgets, Store, pure core calls |
| `qrec-gst`, `qrec-startup`, `qrec-doctor` | gst init; lock, takeover, host start and open; doctor |
| `qrec-session` (+ IPC, waker and signal threads) | the Session (A§8.1) |
| `qrec-capture` + GStreamer streaming threads | the capture pipeline (A§8.11) |
| `qrec-side` | side-channel pump |
| `qrec-player` | the GES pipeline |
| `qrec-meters` | meter monitor pipelines |
| `qrec-preview-<n>` | ffmpeg for thumbnails and peaks |
| `qrec-preview-trim` | the preview caches' disk-cap trim when a cache starts (G§3.11) |
| `qrec-gui-io` | bin sizes, silence keys, wl-copy, source discovery |
| `qrec-testctl` | the test control socket (G§3.18), test mode only |

- **R1.** The GTK thread never blocks: no `recv`, `join`, Session call,
  `gst::init`, GES build, discovery, doctor or file I/O. Exceptions, all
  before the first window maps: `qrec_gui::run` reads `config.toml` and
  `keys.toml` (≤ 1 MiB each, G§3.3 step 1), and `startup` registers the
  three bundled font files with Pango and loads the GResource. The bin's
  and the export panel's modules are held to it by a source scan
  (`export::tests`, a worker-side line marked `// R1: qrec-gui-io`):
  Change… stat'ed `exports/` on the GTK thread (review fix; the panel asks
  `qrec-gui-io` when it opens, `export::flow::save_folder`).
- **R2.** The session thread never waits on GTK: port calls push to a
  queue under a short mutex.
- **R3.** One ordered queue; adjacent snapshots coalesce; one pending
  wake; high-rate data in latest-wins slots.
- **R4.** The main loop runs (`app.hold()` from close until `Stopped`)
  until the engine, the player and the meter monitor reached NULL:
  `gtk4paintablesink` calls into the main context synchronously. The GTK
  thread never waits for those threads, and it holds both sinks until exit.
  As built (rounds 1–2 review): `App::quit` destroys the window and polls
  every 50 ms until `MediaHub::stopped()` (the player's `Stopped`, every
  stopped `MeterMonitor::is_finished()`), at most 6 s
  (`app::MEDIA_STOP_WAIT`), then quits; the exit code is the blocking
  page's when one is up at the host's `Stopped` (a session crash during
  the close: `HostFailed`, 1), else the close's. A panic in `activate`
  quits with 1 (through the close when the app is up).
- **R5.** No GStreamer call on the GTK thread before `gst_init::init()`
  returned Ok on a worker. Never `GtkMediaFile` or `GtkVideo` (libgtk-4
  links GStreamer and would initialize it on the main thread *(spike)*).
  No bus watch on the default MainContext.
- **R6.** Every wait has a deadline:

  | Wait | Limit | Wait | Limit |
  | --- | --- | --- | --- |
  | `hello` | 2 s | lock after `shutdown` | 20 s |
  | gst init | 30 s | doctor portal / pactl | 5 s / 3 s |
  | live pipeline start | 5 s | EOS / teardown | 10 s / 5 s |
  | engine shutdown | 16 s | IPC flush | 10 s |
  | GUI close until quiet | 60 s | player, meters to NULL | 5 s |
  | preview ffmpeg | 20 s + 30 s/GiB | wl-copy | 5 s |
  | signal grace | 15 s | X1 stop reply | 60 s |
  | test control request | its `timeoutMs` (≤ 60 s) | | |

- **R7.** Transient state never enters State and the GUI never mutates
  State optimistically: drag and nudge previews are GUI-local (A§4.1).
  The GUI never reads `qrec.json`, `ui.json` or the history; derived
  caches only through `PreviewCache`.
- **R8.** Every change to State or the project is a Command through
  `GuiHost::submit` → `Session::dispatch` (CLAUDE.md, SPEC §4.2). Allowed
  calls that aren't Commands (none touches State or the project files):
  1. `LiveControl::transient_layout` (preview geometry, G§3.9);
  2. `MeterMonitor` start and stop;
  3. the `ProgramPlayer` (load, seek, play with a rate, pause, scrub);
  4. `PreviewCache` requests and the GTK caches;
  5. `config.toml` and `keys.toml` writes through
     `qrec_shell::{config_file, keys_file}` (WP9), as `qrec config set`
     does;
  6. desktop actions (`UiAction`);
  7. `GuiHost::retry_live` (a `Msg` the session thread orders, D-50);
  8. the test control channel's reads (G§3.18).

  Only `store/`, `host/` and `startup/` touch `GuiHost`; a test greps the
  crate for it.
- **R9.** Children the GUI starts (ffmpeg, wl-copy) follow A§8.7: a log
  file, a timeout, no unread pipe.
- **R10.** No panic crosses into C. Unwinding into GLib's C frames aborts
  the process (`panic_cannot_unwind` in glib's source trampoline: a
  `RefCell` borrowed twice in startup's `fail_host` aborted a WP0b run
  that way), so every closure GLib or GTK calls goes through `src/guard.rs`
  (WP0b):
  - signal handlers and async callbacks are wrapped in `guarded!("what",
    move |…| …)` (`after_panic = expr` picks the return value after a
    panic);
  - sources go through `guard::{idle, idle_local_once, timeout_local,
    timeout_local_once, invoke, tick}`, the only callers of GLib's own;
    a `guard::Slot` holds every one an owner keeps to stop it or to see
    whether it runs (the timeline's frame clock, `SetUi` and tooltip
    timers, the meters' refresh, the export panel's clock, the toasts'
    timer, the program sync's, the stage's ticks and nudge timer, the
    autocut debounce, the bin's sizes debounce, the spinner's, the mud
    bar's and the countdown's ticks, the startup signal watch): it empties
    itself however the callback ends (`Break`, fired, a caught panic,
    removed, its widget gone), so a check of it never waits on a dead id,
    its `stop` never removes a source GLib destroyed (`SourceId::remove`
    panics on one), and its owner starts it again with the next change
    (a meter on the panel's next refresh, the export clock on the next
    snapshot; review R09, R65: the timeline's tick never came back after a
    caught panic, and the meters' and the export clock's raw ids stayed
    dead, then panicked when removed). A test finds any
    `glib::SourceId` or `gtk::TickCallbackId` named outside `guard.rs`
    (`every_kept_source_is_a_slot`; R65's audit);
  - subclass virtual methods and other entry points use `guard::run` or
    `guard::catch`;
  - the queue's drain, `Subscribers::emit` and `ActionRouter::run` contain
    a panic to its item.

  A caught panic is logged as an error with what was running, counted
  (the test channel's `local` reports `panics`) and told as an error toast
  at most every 5 s; the closure returns its `AfterPanic` value (nothing,
  `false`, `Propagation::Proceed`, `ControlFlow::Break`: a repeating
  source stops) and the window carries on. A test greps the crate's
  production code (each `#[cfg(test)]` item taken out, wherever it is): GLib's source functions only in `guard.rs`, and every
  `.connect_…(`, `.launch(` and `.open_containing_folder(` call, every
  call that takes a `Cancellable` before its last argument (GIO's async
  calls: `FileDialog::save`, `AlertDialog::choose`, any `*_async`; a sync
  one takes it last) and every `adw::CallbackAnimationTarget::new(` whose
  last argument is a `guarded!(…)` (a fn path, a variable or a closure of
  its own is found; an animation target's closure may instead be one
  `guard::run(…)` call; review R73: WP7's save dialog and the toasts' and
  the bin slide's targets were guarded by hand, unchecked). Virtual methods
  aren't grepped: each work package wraps its own (`snapshot`, `measure`,
  `dispose`, …) in `guard::run`.
  `qrec-gui-io` (`media::io`) runs each job under `catch_unwind`: a panic
  ends that job, and is told on the main thread like the others; a
  `run_io_then` continuation stays on the main thread, and the work's
  result comes back through the queue (`Inbound::Io`), so nothing of the
  main thread is ever dropped on the worker.
- **R11.** A widget's own callbacks never hold it, or a widget inside it,
  strongly: a gesture's or a drag source's closure that keeps the widget
  it hangs on is a cycle GTK 4 never breaks (no `destroy` for a child), so
  the widget outlives its removal with everything it holds. The handler
  takes its widget from the controller (`g.widget()`) or holds a
  `downgrade()`. Widgets made again on a rebuild must go: the bin's rows
  are counted (`local`'s `binRowsAlive`; WP7 integration audit: a row's
  click gesture held its row, so every rebuild kept the rows it dropped),
  and so are the panel's (`layerRowsAlive`, `audioRowsAlive`; review R43:
  a layer row's drag gesture and its name's rename handler held the row).
  Their subscriptions go with them: a widget made per row keeps its
  `Subscription` (the meters' and the bin thumbnails' theme ones) and
  drops it on `dispose`, never `detach()`es it (`local`'s
  `themeSubscribers`; WP7 integration audit: each rebuild left its
  thumbnails' callbacks in the theme's list).
- **R12.** Leaving an edit commits it, whatever leaves it. Enter, Tab and
  a blur commit a field (G§4.7), Enter saves a rename; and a press anywhere
  outside the field or rename being edited ends the edit before the press
  acts: the window's capture-phase press handler (`keys/mod.rs`,
  `end_edits_on_press`) takes the focus out of the field, whose
  leave-commit runs then, or saves the rename as its Enter does (an
  `EditableLabel` that loses the focus stays in its edit). The edit's
  command is built against the scene it was typed for and enters the FIFO
  ahead of whatever the press starts, so a button, a toggle, a menu or a
  click on the canvas sees the committed value. A press inside the edit
  (its text, its unit, its own menu) leaves it alone; keys need no such
  step, since an editable takes them all (G§3.8 step 2). Review R38: chrome
  buttons are `focus-on-click = false` (D-11), so a field kept the focus
  through a button's action, and the mode switch's change dropped the edit
  (R38's reset) and Play left it unsent
  (`gui_panel::a_press_outside_an_edit_commits_it_first`). A change from
  outside the window still drops it (G§4.7).

### G§3.13 GPU environment (A§8.10)

- `plan_gpu_env` (WP0a), for `for_gui`: when neither `GDK_DISABLE` nor
  `GSK_RENDERER` is set, set `GDK_DISABLE=vulkan`; when either is set,
  leave both alone (logged; doctor prints a note) (H-3). The reasons
  *(spike)*:
  - Mesa's Vulkan driver opens `/dev/dri/renderD128` (NVIDIA) while it
    enumerates devices, even with the ICD pinned; only `GDK_DISABLE=vulkan`
    stopped it;
  - GSK GL used half the main-thread time and 13–20 points less CPU than
    Vulkan on the Intel iGPU;
  - compositor screenshots of a Vulkan-rendered window tore.
- The existing EGL and Vulkan pins stay; `doctor` prints the note.
- Nothing may start a thread before `prepare_gpu_env`, which runs first in
  `main`.
- Check with an strace of the GUI start
  (`crates/qrec/tests/gui_support/scripts/gpu-opens.sh`) that only the
  chosen render node is opened (G§8.6).

### G§3.14 Theme, scale, motion, fonts, icons, art

- **Generated tokens.** From `design/tokens/qrec-tokens.json`, committed
  (the Nix build source excludes `design/`):
  - `resources/theme/tokens/{swamp-night,swamp-day,dragons-keep,ogre-mode}.css`:
    `:root { --qrec-<token>: …; }` with 75 colors, 15 numbers in px and 3
    font names;
  - `src/theme/palette.rs`: `Palette` with one `gdk::RGBA` field per color
    token plus `take: [RGBA; 8]` and `take_tint: [RGBA; 8]`, one per theme.
    Custom-drawn widgets read colors from it: GTK can't look up a CSS
    custom property from code.

  The generator is the test `crates/qrec-gui/tests/tokens.rs`: it
  regenerates in memory and fails on drift; `QREC_UPDATE_TOKENS=1` writes
  the files; it is skipped when `design/` is absent. No hex value appears
  in widget code or component CSS (the same test greps).
- **CSS files** (`resources/theme/`):
  - `adwaita.css`: libadwaita variables mapped to tokens (below);
  - `base.css`: type role classes `.t-<role>`, the focus ring, `:disabled
    { opacity: .45 }`, hover/pressed overlays, R1/R2 selection, `.e1`,
    `.e2`, `.e3`, `.take-1` … `.take-8`. The focus ring is
    `:focus:focus-visible`: GTK sets `:focus-visible` on the whole focus
    chain, so `:focus-visible` alone ringed every ancestor of the focused
    widget (WP6 undid it in its own CSS; fixed at the source in round 3).
    Overrides say `:focus:focus-visible` too, to stay as specific (a
    field's text, a menu item, a scale); fields ring themselves with
    `:focus-within`. The test `focus_rings_go_on_the_focused_widget_only`
    checks every stylesheet;
  - `components/<area>.css`, one per owner (G§3.16).

  Load order: the tokens provider (priority APPLICATION), then one provider
  with adwaita + base + every `components/*.css` of the GResource,
  `common.css` first, the others by name (APPLICATION + 1), all through
  `css::scale_px`; CSS parse errors are logged with the file and line.
  `adwaita.css` also zeroes the header bar's side margins (libadwaita's
  −1 px made the header region (−1, 0, 1602, 48)).
- **Box spacing is CSS.** GtkBox adds its CSS `border-spacing` to its own
  `spacing`, so a box the CSS spaces (by its class, or as a `.qrec-btn`'s
  content, `button.qrec-btn > box`) is made with spacing 0: the gaps
  scale with the UI then (D-46). `button::labeled`, `keycap::chord`,
  `Chip`, the tooltips and the interim pages had their own spacing too,
  which doubled the CSS's (16 px in buttons, 4 px between a chord's caps;
  WP6 worked around it in its CSS); fixed at the source in round 3, and
  `labeled` centers its key cap vertically, as the timeline toolbar's
  toggles (Bin, Snap) center theirs and the bin's count chip (round 3
  integration fixes: they filled the 28 px buttons; `s07_fit` checks them
  16 px tall on the toolbar's middle). The test
  `boxes_the_css_spaces_have_no_spacing_of_their_own` greps for it.
- **libadwaita bridge** (`adwaita.css`):
  `--window-bg-color: var(--qrec-bg)`, `--window-fg-color:
  var(--qrec-text)`, `--view-bg-color: var(--qrec-field)`,
  `--view-fg-color: var(--qrec-text)`, `--headerbar-bg-color` and
  `--headerbar-backdrop-color: var(--qrec-surface)`,
  `--headerbar-fg-color: var(--qrec-text)`, `--headerbar-border-color:
  var(--qrec-border)`, `--headerbar-shade-color: transparent`,
  `--popover-bg-color`, `--dialog-bg-color` and `--card-bg-color:
  var(--qrec-raised)` (each `--*-fg-color: var(--qrec-text)`),
  `--sidebar-bg-color: var(--qrec-surface)`, `--accent-bg-color:
  var(--qrec-accent)`, `--accent-fg-color: var(--qrec-text-on-accent)`,
  `--accent-color: var(--qrec-accent-text)` (overrides the system
  accent), `--destructive-bg-color: var(--qrec-record)`,
  `--destructive-fg-color: var(--qrec-text-on-record)`,
  `--destructive-color: var(--qrec-record-text)`, `--success-color:
  var(--qrec-success)`, `--warning-color: var(--qrec-warning-text)`,
  `--error-color: var(--qrec-record-text)`, `--border-color:
  var(--qrec-border)`, `--disabled-opacity: 45%`.
- **Theme, scale and motion before the first frame.** G§3.3 step 1 reads
  `config.toml`; `startup` loads that theme's tokens, scales the CSS and
  sets the motion policy before the window maps.
- **Theme switch** (WP10, also used by S21 later): reload the tokens
  provider; swap the `Palette`; `adw::StyleManager::set_color_scheme`
  (`ForceLight` for Swamp Day, `ForceDark` otherwise); `queue_draw` every
  custom widget through `Theme::subscribe` (gtk4-rs doesn't expose
  `css_changed`).
- **UI scale** (100/125/150 %, `ui.scale`): every `<n>px` in the CSS text
  is multiplied by the scale before loading, and so is Fraunces' optical
  size (`"opsz" <n>`, which follows the font size, within 9–144); custom
  widgets multiply their metrics by `theme.scale()` (D-46): the C01
  spinner's sizes are CSS (`qrec-spinner`, `.s14`, `.s12`, so they scale
  with the icons), and `widgets::draw::hatch` takes the scale (a `scale`
  parameter, added in the rounds 1–2 review: its stripes, pitch and edges);
  the preview's `max_size` is unaffected.
- **Motion**: `theme::motion::MotionPolicy { reduced }` per D-30, with the
  window class `.reduced-motion`, which follows the policy live
  (`Theme::follow_motion`).
  - Curves: `EASE_STD` (0.4, 0, 0.2, 1), `EASE_OUT_EXPO` (0.16, 1, 0.3, 1),
    `EASE_IN` (0.4, 0, 1, 1).
  - Every Qrec animation is driven from code, never CSS `@keyframes`: tick
    callbacks reading `motion::now()` for the torch flicker (DESIGN
    keyframes, linear between them; widget opacity also scales its
    `record-glow` shadow), the mud bar, the countdown, spinners and the
    ghost stripes; `adw::TimedAnimation` for zoom, the bin drawer, the
    panel sheet and toasts. CSS transitions only for hover and state
    colors. So test mode can freeze every animation (G§8.4). A tick
    callback runs only while its widget visibly moves: the mud bar's only
    while an export runs with motion allowed and the clock moving
    (`mud_bar::animates`; done and failed bars, reduced motion and test
    mode's frozen clock, `motion::frozen()`, are still pictures; WP7
    integration), like the meters' 30 Hz timer (G§8.6). The test channel's
    `local` counts the bars ticking (`mudBarsTicking`): S15b and S15c's
    running pages check none does under the frozen clock (WP7 integration
    audit: no test held the merge to this before).
- **Fonts** (SPEC §15.3, Q11):
  - bundled files `Fraunces[SOFT,WONK,opsz,wght].ttf`,
    `AtkinsonHyperlegibleNext[wght].ttf`, `JetBrainsMono[wght].ttf` from
    `$QREC_FONT_DIR`, else `<exe dir>/../share/qrec/fonts`;
  - registered in `startup`, before any widget:
    `gtk::Label::new(None).pango_context().font_map()?.add_font_file(path)`
    (Pango ≥ 1.56) *(spike)*. Missing files → a warning and the DESIGN
    fallback stacks;
  - letter-spacing in px: badge 0.72, rec 0.78, display −0.32,
    display-sm −0.12, headline −0.1;
  - mono roles: `font-feature-settings: "tnum" 1`;
  - a line that mixes families (header subtitle, summaries) is two
    baseline-aligned labels;
  - `theme::fonts` checks through Pango that each bundled family resolves
    to itself (the test channel's `fonts`);
  - **manual font rendering**: startup sets `gtk-font-rendering = manual`
    with GTK's default `gtk-hint-font-metrics` (on) and the desktop's
    antialiasing and hinting (`theme::fonts::configure_rendering`,
    `Rendering::QREC`), applied alike to layout and drawing. GTK 4.22's
    automatic mode lays a label's text out a pixel shorter at the top than
    it draws it, so a partial redraw of a changed label (the header title:
    the folder's name, then the project's) left the old text's ascender
    tops above the new one under the GL renderer; `GSK_DEBUG=full-redraw`
    or the manual mode removes it (`gui_text`'s
    `the_header_title_leaves_no_trace_on_gl`, below; `gui_shell`'s
    `the_header_title_leaves_no_trace_of_the_old_one`, which failed by 8 px
    on gles2 without it at WP0b, no longer can in today's header layout
    (WP6's): its all-ascender title shows no mark on GL in automatic mode
    or with metrics off, only with `hintstyle=none`, 4 px). The cost: Cairo
    hints the metrics and Pango rounds the positions, so every glyph
    advances a whole pixel: JetBrains Mono 7, 7 and 8 px at 11, 12 and
    13 px where the frames advance 6.6, 7.2 and 7.8, and Atkinson's lines
    drift a few px with their letters (G§7.2's departures).
  - **The text-rendering experiment** (WP10, pixman; the crops and numbers
    in `tmp/text-rendering/`, the PO's call, G§8.7): test mode's
    `QREC_GUI_TEST_FONT_RENDERING` names another setup
    (`theme::fonts::Rendering`: `mode=manual|automatic`, `metrics=on|off`,
    `hintstyle=desktop|none|slight|medium|full`; the harness hands the test
    run's value to every window, a case's own wins; the `fonts` op reports
    the settings as they are). GTK 4.22 has no other text setting worth a
    candidate: `gtk-xft-rgba` is ignored (GTK 4 antialiases in grayscale
    only), `gtk-xft-antialias = 0` is aliased text, and per-widget font
    options can't unhint the metrics (`gtk-hint-font-metrics` wins). 69
    single-label texts of S02, S06, S12, S14, S16 and S17 measured against
    the frames (the ink between its 50 % crossings, `tmp/text-rendering/
    tools/measure.py`; the 21 mono and 48 UI texts):

    | Setup | Mono within 1 px (mean \|Δ\|) | UI within 1 px (mean \|Δ\|) | The title | Glyphs |
    | --- | --- | --- | --- | --- |
    | Qrec's: manual, metrics hinted | 4 of 21 (4.3 px; −9.5…+4.8) | 30 of 48 (1.5 px; −5.2…+9.8) | 0 px | crisp; uneven letter gaps |
    | automatic | 21 (0.18 px) | 46 (0.27 px) | **12 px** | as the frames' |
    | manual, metrics off (the desktop's slight hinting) | 21 (0.18 px) | 46 (0.27 px) | **12 px** | as the frames' |
    | manual, metrics off, `hintstyle=none` | 21 (0.18 px) | 46 (0.27 px) | 0 px; **4 px** on the all-ascender title | blurred: every horizontal edge over two rows |
    | manual, metrics off, `hintstyle=full` (`medium` is the same to Cairo: FreeType's normal hinting) | 21 (0.28 px) | 45 (0.61 px) | **12 px** | misshapen |

    The UI texts the fractional setups leave over 1 px are style, the same
    in every setup (S06's clip tooltip title −6, S14's Preview +2). Scale 2
    measures the same (text is laid out at 1×) and looks alike in every
    setup. The ink's weight doesn't move with the setup (the UI texts 100 %
    of the frames' ink on average, the mono ones 93 %: lighter, in every
    setup). Unhinted metrics also round line heights differently: S16's Done
    page is 214 px high (the frame's and Qrec's 216: `gui_bin_export`'s
    `s16_export_done` fails), and the stderr box wraps "filter" to its own
    line (51 mono characters at 7.2 px don't fit it, where the frame's do).
    "The title" is `gui_text`'s GL case: on GSK's GL renderer in software
    (Mesa's llvmpipe; no GPU node opened: every open of the experiment's
    GL runs traced), the header title after a partial redraw against a
    full one's, the folder named in accented capitals (`ÅÉÂ…`, the tallest
    ink a title draws). GL draws a hinted font's glyphs on a whole-pixel
    baseline while the bounds that limit a partial redraw are the unhinted
    ink at the exact one; with unhinted metrics the baselines are
    fractional and the accents' tops land a row above the bounds (the mark,
    GL only: Cairo draws inside them, so the pixman runs never show it).
    Unhinted glyphs (`hintstyle=none`) GL puts on quarter pixels instead,
    which leaves a fainter mark (4 px on the all-ascender title). So Qrec
    keeps its setup: every setup that matches the frames' widths brings the
    mark back.
- **Icons**: Adwaita symbolic names (G§5.2). Custom symbolic SVGs (16 px
  viewBox, fills only) live in
  `resources/icons/scalable/actions/qrec-*-symbolic.svg`, registered with
  `gtk::IconTheme::for_display(..).add_resource_path("/com/rsensato/qrec/icons")`.
  `adwaita-icon-theme` and `hicolor-icon-theme` come from Nix (G§6).
- **Art** (pen `w7Qg8`), exported read-only by WP0b while `design/qrec.pen`
  is the active document (G§0.6): walk each node with
  `Get(includePathGeometry: true)` and write SVG by script.
  - `art/app-icon-24.svg` (`uH1BD`);
  - `art/quiet-swamp.svg` (`rfUfx`) and `art/signpost.svg` (`W1Ia5`), with
    `{{muted}}` and `{{border}}` placeholders substituted per theme, drawn
    through `gtk::Svg` (GTK 4.22; `Svg::new`, its `error` handler, then
    `load_from_bytes`: GtkSvg reports what it drops while it parses, so the
    handler must be there first; `theme::art::paintable_checked` counts);
  - the app icon (`ptPs6`) as SVG plus PNG 128/64/32 (`rsvg-convert`) for
    packaging;
  - the REC glyph `qrec-record-symbolic.svg` (`w8imU`).

  WP0b checks first that `gtk::Svg` renders all three art files like the
  pen PNG crops (an in-app capture next to the crop). If it doesn't, the
  fallback is PNGs pre-rendered per theme at 1× and 2× with
  `rsvg-convert` (librsvg) from the substituted SVGs, committed under
  `resources/art/<theme>/`. The synthetic sample images (`X6zTi`,
  `sqyyT`) are never shipped.

  As built (WP0b part 2): `theme::art` substitutes any `{{token}}` in a
  template with the palette's color and draws it through `gtk::Svg` (the
  test channel's `art`). Until the pen export, the three art files and the
  10 custom symbolic icons (the REC glyph `w8imU`, success, magnet,
  monitor-x, package-x, life-buoy, timer-off, gauge, layers, audio-lines;
  recoloring checked by the test channel's `icons`) are placeholders of
  the right size and colors, and the comparison with the pen crops is
  open.
- **GResource**: `build.rs` writes the `.gresource.xml` from every file
  under `resources/` (no hand-kept list, so WPs add files without
  conflicts) and runs `glib-compile-resources` (in the dev shell; added to
  the package's `nativeBuildInputs`). The app registers
  `include_bytes!(concat!(env!("OUT_DIR"), "/qrec.gresource"))` at
  startup. Prefix `/com/rsensato/qrec/`.

### G§3.15 Window layout and overlay layers (WP0b)

```
adw::ApplicationWindow #qrec  (default 1600×1000, min 800×600)
└ gtk::Overlay  window_layer          ← CheatSheet (scrim-strong over everything)
  └ Box V
    ├ gtk::Overlay  content_layer     ← Modal, Settings, Palette (scrim to y 968), ExportPanel, ToastHost
    │ └ adw::ToolbarView
    │   ├ top: Header (adw::HeaderBar, custom title, own close button, end title buttons off)
    │   └ Box H
    │     ├ Box V  work
    │     │   ├ Box H  stage_row: BinDrawer (0 ↔ 352, animated) | Stage (hexpand)
    │     │   └ Timeline (312 px; 232 compact)
    │     └ adw::OverlaySplitView, sidebar at the end: Panel (384; 320; overlay sheet)
    └ HintBar (32 px, not focusable)
```

- `WindowLayers::open(layer, widget)` and `close(layer)`, `layer` one of
  `Modal`, `Settings`, `Palette`, `Fullscreen`, `FloatingTrap`,
  `FloatingPassive`:
  - `Modal`, `Settings` and `Palette` add a `scrim` over 1600×968 (the
    content layer stops above the hint bar; stacked layers stack their
    scrims), trap focus (the regions below, the toast host too, get
    `can-target = false` and leave the focus chain; a widget in both the
    content and the window base is out while either trap applies, and
    each gets its own `can-target`/`can-focus` back when the trap goes,
    so the hint bar never becomes focusable), give the initial focus the
    widget asks for, and restore focus on close;
  - `Fullscreen` (the cheat sheet, gated) adds `scrim-strong` over the
    whole window;
  - `FloatingTrap` (the export panel in Setup, Done, Failed) traps focus
    and adds a transparent click-catcher that closes it, like a popover;
  - `FloatingPassive` (the export panel while Running, D-49) adds nothing:
    no trap, no click-catcher.
  - The layers' order is `LocalUi.overlays`' (top last): the export panel
    changes its layer only while it is the top overlay
    (`export::flow::layer`), so its export ending under Settings or a
    modal opened over the passive panel keeps it passive (and inert) under
    them; it takes the trap, Open file or Try again focused, once they
    close (review fix: it took the trap over them, and the focus, while
    `Esc` and the hints were theirs). A layer closing under a trap never
    takes the focus out of the top trap, and a layer opened over it that
    took the focus from inside it gives the focus back where it would
    have (the Done page opened over Settings gives it back to the timeline
    that had it before Settings).
- The export panel and the toasts are in-window widgets, not GTK popovers:
  an exact 12×6 arrow and deterministic screenshots. Menus (Layout ▾, Add
  layer, Add audio track, row overflows, dropdowns) are
  `gtk::PopoverMenu` / `gtk::DropDown`.
- Placement (BRIEF §2.8): toast bottom-centered, 12 px above the hint bar,
  360–560 wide; export panel 440 wide, 8 px from the right edge, top at
  y 52, arrow 172 px left of its right edge (under Export); palette 640
  wide at y 96; modal 520 wide, centered on the window (S18's frame:
  y 277 to 723), not in the 968 px content height; Settings 840×640 at
  (380, 180), centered on the window too; cheat sheet inset 40 px (WP6).
- **Stage geometry**: the preview is the canvas aspect fitted into
  (stage_w − 32) × (stage_h − 68), centered in x ∈ [16, stage_w − 16] and
  y ∈ [16, stage_h − 52]. The 36 px transport row is anchored to the
  stage bottom (top = stage_h − 44) and spans the preview's x and width.
  Checks, window coordinates: S02 preview (128, 64, 960, 540), transport
  (128, 612, 960, 36); S14 with the bin preview (368, 100, 832, 468),
  transport (368, 612, 832, 36).
- **Adaptive modes** (`window/adaptive.rs`, DESIGN Layout, G§0.3):
  `adw::Breakpoint`s set `LocalUi.layout`:
  - **Wide** (≥ 1400 px): panel docked at 384;
  - **Medium** (1100–1399): panel docked at 320, class `.panel-medium`
    (WP5 lays the inspector fields 2-up);
  - **Narrow** (< 1100): `adw::OverlaySplitView` collapsed; the panel is
    an overlay sheet (`Overlay::LayersSheet`, 240/160 ms) toggled by the
    header layers button (WP6) and closed by `Esc`;
  - **compact** (height < 820): the timeline keeps 232 px (toolbar 36,
    ruler 28, video 72, lanes 48; WP3) and the stage shrinks.

### G§3.16 Module layout of `crates/qrec-gui`

WP0b creates every file below with its `mod` line, its final public
signatures and a stub body (a region of the right size showing its name;
an API that returns `None` or nothing), so no WP edits another WP's file or
a module list.

| Path | Content | Owner |
| --- | --- | --- |
| `Cargo.toml`, `build.rs` | dependencies; gresource from `resources/` | WP0b |
| `resources/theme/tokens/*.css`, `src/theme/palette.rs` | generated tokens and palettes | WP0b (regenerated by the test) |
| `resources/theme/{adwaita,base}.css`, `components/common.css` | bridge, type roles, shared components (C01, C02, C20, C22, C25 menus, C26–C29, C33) | WP0b |
| `resources/theme/components/timeline.css` | | WP3 |
| `resources/theme/components/stage.css` | | WP4 |
| `resources/theme/components/panel.css` | | WP5 |
| `resources/theme/components/{chrome,settings}.css` | header, hint bar, toasts, modals; Settings shell and Project | WP6 |
| `resources/theme/components/{bin,export}.css` | | WP7 |
| `resources/theme/components/blocking.css` | interim pages (WP0b), drafts (WP8) | WP0b, WP8 |
| `resources/theme/components/overlays.css` | palette, cheat sheet, Settings pages | WP9 |
| `resources/icons/**`, `resources/art/**`, `resources/fonts/OFL-*.txt` | custom icons, art, font licenses | WP0b |
| `src/lib.rs` | `run`, `GuiArgs`, module list | WP0b |
| `src/app.rs` | Application, activate, close sequence, exit code | WP0b |
| `src/startup/mod.rs` | startup state machine (G§3.3), takeover flows | WP0b |
| `src/host/{mod,opener,queue}.rs` | `GuiPort` implementation, `GuiOpener`, queue | WP0b |
| `src/store/{mod,local,replies}.rs` | Store, LocalUi, Overlay, default reply policy | WP0b |
| `src/keys/{mod,router,actions}.rs` | KeyRouter glue, GTK-free `decide`, ActionRouter | WP0b |
| `src/desktop.rs` | `UiAction` handling | WP0b |
| `src/guard.rs` | the callback guard: `guarded!`, guarded sources, the panic report (G§3.12 R10) | WP0b |
| `src/testctl.rs` | test control channel (G§3.18) | WP0b |
| `src/theme/{mod,fonts,motion,css,art}.rs` | providers, fonts, MotionPolicy, scale preprocessing, art templates through `gtk::Svg` | WP0b |
| `src/media/mod.rs` | MediaHub: sinks, side pump, preview frames, meter toggle, player and cache handles | WP0b |
| `src/media/io.rs` | `IoWorker` (`qrec-gui-io`: jobs under `catch_unwind`, results through the queue, `hold_io_busy`; rounds 1–2 review) | WP0b |
| `src/media/{live,program}.rs` | live picture glue; Program sync state machine | WP4 |
| `src/media/{thumbs,peaks}.rs` | texture and peak caches (API in G§3.11) | WP2 |
| `src/widgets/mod.rs` | module list | WP0b |
| `src/widgets/{button,keycap,tooltip,spinner,chip,draw}.rs` | shared components; draw helpers (hatch, dashed rect, rounded rect, text) | WP0b |
| `src/widgets/{numeric,meter,scale_tooltip}.rs` | C29 numeric field, C11 meter, C26 value tooltip | WP5 |
| `src/widgets/mud_bar.rs` | C30 | WP7 |
| `src/widgets/theme_card.rs` | C32 | WP9 |
| `src/window/{mod,layers,adaptive}.rs` | window tree, overlay layers, focus trap, regions, breakpoints | WP0b |
| `src/header/**` | C04, C05, record button, export slot, narrow-mode layers button | WP6 |
| `src/{hintbar,toasts,modal}.rs` | C03, C23, C24 | WP6 |
| `src/settings/{mod,project}.rs` | Settings shell (S18b), Project page | WP6 |
| `src/settings/{appearance,recording,autocut,keyboard}.rs` | the other pages (stub: disabled tab, D-48) | WP9 |
| `src/stage/**` | preview, overlays, countdown, transport, first run, portal denied (gated) | WP4 |
| `src/timeline/**` | toolbar, headers, canvas, overlay, gestures, drop target, tooltip, zoom | WP3 |
| `src/panel/**` | header, banner, layer rows, audio rows, inspector, menus, autocut page | WP5 |
| `src/bin_drawer/**` | bin drawer, rows, drag source (`src/bin/` would be a Cargo binary target) | WP7 |
| `src/export/**` | export panel | WP7 |
| `src/blocking/{mod,interim}.rs` | `BlockingPage`, interim pages (G§1.1 #2) | WP0b |
| `src/blocking/{missing_deps,cli_recording,not_my_swamp,unreadable,export_wait}.rs` | the drafted pages | WP8 |
| `src/palette.rs`, `src/cheatsheet.rs` | S19, S20 | WP9 |
| `tests/tokens.rs` | token generation and hex grep | WP0b |

Outside the crate, WP0b's harness `crates/qrec/tests/gui_support/` (G§8.4)
is `mod.rs` (`Nested`, `Launch`, `Gui`, the region checks), `pointer.rs`
(the virtual pointer) and `scripts/` (`run-nested.sh`, `gpu-opens.sh`).

Region constructors are fixed by WP0b and never change signature:
`header::Header::new(&Ctx)`, `stage::Stage`, `timeline::Timeline`,
`panel::Panel`, `hintbar::HintBar`, `bin_drawer::BinDrawer`,
`export::ExportPanel`, `toasts::ToastHost`, `modal::ModalHost`,
`settings::Settings`, `palette::Palette`, `cheatsheet::CheatSheet` (each
with `fn widget(&self) -> gtk::Widget`) and
`blocking::BlockingPage::new(&Ctx, BlockKind)`.

### G§3.17 Changes outside `qrec-gui`

| Crate | File | Change | WP |
| --- | --- | --- | --- |
| core | `lib.rs` | `pub mod view` | WP0a |
| core | `view/mod.rs`, `view/format.rs`, `view/notices.rs` | G§4.1, G§4.2, G§4.9; `view::enabled` | WP0a |
| core | `view/{timeline,ruler}.rs` | G§4.5 (stubs: WP0a) | WP3 |
| core | `view/canvas.rs` | G§4.6 | WP4 |
| core | `view/{panel,meter,autocut}.rs` | G§4.7 | WP5 |
| core | `view/{hints,header,confirm,settings}.rs` | G§4.3, G§4.4, G§4.9, G§4.10 | WP6 |
| core | `view/{bin,export}.rs` | G§4.8 | WP7 |
| core | `view/blocking.rs` | G§4.11: interim content (WP0a), drafts (WP8) | WP0a, WP8 |
| core | `view/{palette,cheatsheet,preferences}.rs` | G§4.11 | WP9 |
| core | `state.rs`, `protocol/command.rs`, `apply/ui.rs`, `ui_normalize.rs` | `UiState.lane_mute`, `UiPatch.lane_mute` (whole set; ids no scene has are dropped), `Command::ui_only`, `ui_normalize::edit_target` | WP0a |
| core | `model.rs`, `history/external.rs`, `on_event/record.rs` | `Take.recovered` (G§4.8), set by `recover`, kept from hand edits like the other facts; `pub const EOS_TIMEOUT_REASON` | WP0a |
| core | `keymap.rs` | `KeyChord::cap_legends`, `pub fn usable_keys`, `pub fn gate_reason`, D-01, D-06 nudges, bindings `region.next`, `region.prev`, `app.quit` (WP0a); D-31 labels, `history.clear`, `set_binding`/`reset_binding` (WP9) | WP0a, WP9 |
| core | `on_event/output.rs` | `pub fn export_progress_view`, `pub fn export_readout` (WP0a); the export-done toast copy (WP7) | WP0a, WP7 |
| core | `on_event/{record,disk}.rs` | toast copy D-21 | WP9 |
| core | `ctx.rs` | `Ctx::preview(now, config)` | WP0a |
| core | `plans.rs`, `ges_plan.rs` | `GesVideoClip.{take, layer}`, `GesAudioClip.{take, track}` (WP0a); `plan_ges_take`, `without_tracks`, `scaled` (WP1) | WP0a, WP1 |
| core | `media_argv.rs`, `tests/props_preview.rs` (new) | preview argv (`strip_argv`, `frames_argv`, `peaks_argv`, `preview_probe_argv`), `preview_width`, key, `bucket_peaks`, `peak_levels`, `PeakLevels` (moved from the shell) | WP2 |
| core | `testkit.rs` | `design_project()`, `design_state()` (BRIEF §3), `perf_project(clips)` | WP0a |
| core | `tests/props_late_errors.rs` (new) | a late `Err` reply always comes with a `Notify` | WP0a |
| shell | `host/gui.rs` (new), `host/mod.rs` | GUI host, GUI exit rules, `HostEnd::Closed`, `first` request | WP0a |
| shell | `host/takeover.rs` (new) | G§3.4.3 | WP0a |
| shell | `ipc/server.rs` | GUI hosts refuse `shutdown` | WP0a |
| shell | `msg.rs`, `deps.rs` | `Responder::reply_late`, `Msg::RetryLive`, `CaptureEngine::retry_live` (default no-op) | WP0a |
| shell | `session/{mod,config,effects,dispatch}.rs` | `StateObserver` on every assignment, `reply_late` calls, D-47 hold exemption, `RetryLive` | WP0a |
| shell | `facts.rs` | `SharedSources` (`Sources` over a cache; `refresh()` runs `SourceInventory::discover` on the caller's thread; an empty cache discovers once, synchronously) | WP0a |
| shell | `capture/mod.rs` | `PreviewOutput`, `LiveSourceStatus`…, `SideEvent::LiveStatus`, `cast_grace`, `LiveControl` + `control()` as no-ops, `PREVIEW_SINK_SUPPORTED = false` (WP1 sets it true) | WP0a |
| shell | `capture/fake.rs` | test hooks (G§8.4): `QREC_FAKE_EOS_DELAY_MS`, `QREC_FAKE_PORTAL=deny\|cancel`, `QREC_FAKE_RECORD_OFFSET_FRAMES`, `QREC_FAKE_INVENTORY=design` | WP0a |
| shell | `capture/{engine,live,meters}.rs`, `capture/tests.rs` | G§3.9 (`meters.rs` stub: WP0a) | WP1 |
| shell | `capture/live.rs`, `capture/tests.rs` | the crop race (A§8.11): the `videoscale` guard, crops applied on the streaming thread (`CropSlot`), `transient_crops_never_lose_a_source` (WP4); only changed crop sides written (rounds 1–2 review, kept in `set_crop`), `takes_keep_every_stream_under_transient_crops` (round 3); a layout offered while the probe writes one waits for the next frame (`CropSlot::offer`, review R24), on the engine's path with the probe held after it took a layout (`live::hold`, `a_layout_back_on_the_applied_crop_waits_for_the_crop_in_flight`) | WP4 |
| shell | `player.rs` (new) | API stub (WP0a), implementation (WP1) | WP0a, WP1 |
| shell | `ges_render.rs` | `rebuild_into` | WP1 |
| shell | `media/preview.rs`, `media/sizes.rs` (new) | stubs (WP0a), implementation (WP2) | WP0a, WP2 |
| shell | `media/{child,logs,mod}.rs`, `proc.rs` | `child::Stdout::Stream`, `Stderr::Lines`, `Finished::pid`; both log prunes count only timestamp-named logs (`media::is_stamped_log_name`) | WP2 |
| shell | `gpu.rs` | H-3 | WP0a |
| shell | `paths.rs`, `host/cli_host.rs` | `UserDirs::gui_log(dir)`; `host_log` names it | WP0a |
| shell | `config_file.rs`, `keys_file.rs` (new) | readers (WP0a); atomic writers over `config::set` and the keymap edits (WP9) | WP0a, WP9 |
| shell | `watcher.rs` | watch `$XDG_CONFIG_HOME/qrec/` | WP10 |
| shell | `testkit/design.rs` (new) | `design_fixture(dir, DesignVariant)`, `perf_fixture(dir, clips)`, `playback_fixture(dir)` (G§8.4) | WP0a |
| shell | `lib.rs` | module list | WP0a |
| cli | `backend.rs`, `exec.rs`, `lib.rs` | `Backend::gui`, `Plan::Gui`, `--json` usage | WP0a |
| qrec | `src/shell.rs` | `Role::Gui` logging, `OWN_CRATES` + `qrec_gui` (WP0a); `Shell::gui` → `qrec_gui::run` (WP0b) | WP0a, WP0b |
| qrec | `Cargo.toml` | `qrec-gui` dependency | WP0b |
| qrec | `tests/gui_support/**` (`mod.rs`, `pointer.rs`, `scripts/`), `tests/gui_smoke.rs`, `tests/gui_shell.rs` | harness (G§8.4), smoke (G§8.5), WP0b's flows | WP0b (smoke completed by WP10) |
| qrec | `tests/preview.rs` | `CachedSilence` reads what `qrec autocut --dry-run` cached | WP2 |
| qrec | `tests/gui_<area>.rs`, `tests/gui_<area>_data/` | screenshot cases and their data | each area's owner |
| root | `Cargo.toml`, `Cargo.lock`, `flake.nix` | G§6 (desktop file and icon install: WP10) | WP0b, WP10 |
| docs | `ARCHITECTURE.md` | WP0a's list; A§1.2 after G§0.1 (WP0b); final pass (WP10) | WP0a, WP0b, WP10 |

Files of another owner the round 3 WPs and their integration changed
(G§7.0 lets only the integrator do it; each is in its merge or integration
commit): `keys/router.rs` and `router_tests.rs` (WP4: `NUDGE_BINDINGS`, the
nudge keys during a burst), `app.rs` (WP4: `Inbound::Player` also to the
program sync), `testctl.rs` (WP4: `stage::busy`; round 3: `playerNs`, the
drawn gear in `ADWAITA_ICONS`), `gui_shell.rs` (WP6: the joined subtitle
labels; round 3: the required regions and two integration cases),
`guard.rs` (round 3: zero-argument `.connect_…()` calls are wiring
methods, not signal connections), `base.css`, `common.css`,
`widgets/{button,keycap,chip,tooltip}.rs`, `blocking/interim.rs`,
`theme/css.rs` (round 3: G§3.14's focus ring and box spacing), and
`gui_support/mod.rs` (round 3: `MERGED_REGIONS`). The round 3 integration
fixes: `capture/{mod,engine}.rs` and `capture/tests.rs` (`LiveTracker`,
`LiveControl::progress`, G§3.9), `media/live.rs` (WP4's: `LiveWatch` waits
for it, G§3.18), `timeline/toolbar.rs` (WP3's: the toggles' centered caps
and count chip, G§3.14), `gui_support/mod.rs` (`mean_grey`,
`assert_live_picture_drawn`), and the S01b and S07 checks in
`gui_timeline.rs` and `gui_stage.rs`.

The WP5 merge (f84ad6d) and its integration changed: `testctl.rs`
(`labels` also returns `entries`, each text field's value by its
nearest given name; `local` reports `panelPage` and `autocutPreview`;
the doc names `QREC_GUI_TEST_LEVELS`), `widgets/button.rs`
(`menu_button` and `set_menu_label`, moved from `panel::menus`, with
`every_menu_button_is_made_by_the_helper`), `timeline/toolbar.rs` (the
settings icon is named `tl-autocut-settings`), `timeline/draw.rs` (the
preview's doc), `gui_support/mod.rs` (`Gui::entries`,
`Nested::keys_held` and `HeldKeys`, moved from `gui_panel.rs`, `poll`,
`rgb_pixels`, `count_near`, `token_rgb`), `gui_timeline.rs` (S12 opens
the page from the settings icon and probes the preview), `gui_chrome.rs`
(S18's Tab shot holds its keyboard), `gui_stage.rs` (S01a checks the
panel), and in the shell `capture/{record,engine}.rs` with A§8.11 (a
record bin that would get a buffer before any segment gets its zero
segment first, and `recorder.log` says so). In WP5's own files: the dry
runs' debounce token, `run_io_then` for the sources, `widgets/meter.rs`
(a 30 Hz timer instead of a tick callback, redrawn only on change,
G§8.6), `view::panel::inspector_shown` (S01a), and `gui_panel.rs` (S06's
tooltip, S12's tuning step, S13's real drag). Its audit changed
`view/ruler.rs` (WP3's: `tick_steps` at 10 s and 1 min, as S12, S04, S05
and S05b draw, G§4.5), WP5's `panel/mod.rs` (the page switch follows the
motion policy, D-30) with `testctl.rs` (`local`'s `panelTransition`) and
`gui_panel.rs` (`the_pages_slide_unless_motion_is_reduced`), and, for
G§8.4's deadlines, `gui_chrome.rs`,
`gui_stage.rs` and `gui_timeline.rs` (their ffmpeg compares and probes
through `run_tool`, their background CLIs through
`Nested::cli_background` and `reap_within`), `gui_panel.rs` (the
fixture's dry run through `run_tool`) and `gui_support/mod.rs` (those
two helpers).

The WP7 merge (e3b30a0) kept WP7's changes to `desktop.rs` (WP0b's:
`resolve()`, so Open file and Open folder find project-relative paths under
the project) and `timeline/mod.rs` (WP3's: the bin drop target's
`set_preload(true)`), and added `theme/motion.rs`'s `frozen()` (WP0b's).
Its integration changed `store/mod.rs` (WP0b's: `Store::ask_confirmation`,
the confirmed re-dispatch's hook, `unsettled_requests`, G§3.7, G§3.18),
`modal.rs` (WP6's: the confirmed command is sent before the overlay
closes), `toasts.rs` (WP6's: the export-failed toast is dropped while
the panel shows its Failed page, `shown_by_the_panel`), `testctl.rs`
(`waitIdle` through `unsettled_requests`; the doc names
`QREC_GUI_TEST_SAVE_AS`), `startup/mod.rs` (a test's job literal); in the
core `view/confirm.rs` (WP6's: the purge's loss list in BRIEF's order),
`state.rs` (`JobKind::Export::seg_clips`), `apply/output.rs`
(`segment_clips`), `on_event/output.rs` (`rendered_clips`, `clips_done`:
the label counts timeline clips, G§4.8, A§5.8; `EXPORT_FAILED_TITLE`), the job literals in
`apply/history.rs`, `keymap.rs`, `view/{header,hints,notices}.rs` and
`tests/{protocol_json,record_machine}.rs`, and `tests/props_output.rs`
(`export_jobs_count_the_timelines_clips`); in the shell `ipc/testing.rs` (a
job literal); and the harness and other suites: `gui_support/mod.rs`
(`preview_heights`, `scale_factor`, `assert_live_picture_drawn_at`),
`gui_stage.rs` (S02 at 1.5, S06 at 2 and 1.5, the adaptive S02 and S06
cases), `gui_timeline.rs` (S09 at 1.5 and the frames' heights, S10's trim
at 2 and 1.5), `gui_chrome.rs` (S16b's body, WP7's copy). Its audit
changed `testctl.rs` (`local`'s `binRowsAlive` and `mudBarsTicking`) and,
in WP7's own files, `bin_drawer/row.rs` (a row's click gesture and drag
source hold its widgets weakly, R11), `bin_drawer/mod.rs` and
`widgets/mud_bar.rs` (test mode's weak lists of rows and bars those two
count), `export/mod.rs` (an export not its own clears the panel's last
outcome) and `gui_bin_export.rs`. Its final shot and code audits changed,
outside WP7's files: in the core `view/timeline.rs` (`shown_playhead`
clamps to the timeline's end); in the shell `session/open.rs` (the UI a
session opens with goes through `ui_normalize`, A§5.10) and
`ges_render.rs` (fade points span the clip, A§9.6); in the GUI
`widgets/draw.rs` and `timeline/{paint,draw}.rs` (WP0b's and WP3's: the
hatch's step along x), `hintbar.rs`, `header/export_slot.rs`, `toasts.rs`
and `components/chrome.css` (WP6's: items left out whole, the slot's
`mono-sm`, the toast's actions and body), `timeline/tips.rs` (WP3's: the
tooltip measured again after a hide), `widgets/button.rs` (WP0b's: `link`),
`theme/mod.rs`, `store/mod.rs` and `testctl.rs` (WP0b's: the theme's
subscriber count, `hints`' `shown` and `cut`); and the harness and suites:
`gui_support/mod.rs` (`Gui::assert_no_criticals`), `gui_stage.rs` (the
adaptive cases' hint bar, the S06 scale runs' tooltip), `gui_timeline.rs`
and `gui_panel.rs` (no criticals).

### G§3.18 Test control channel (`src/testctl.rs`, WP0b)

Active only under `QREC_GUI_TEST` (G§8.4). A socket per window,
`$XDG_RUNTIME_DIR/qrec-gui-test-<pid>.sock` (0600, in the test's private
runtime dir; the harness's `Gui::ctl` connects to its window's pid, so a
case drives two windows at once), served by its own thread
(`qrec-testctl`); one JSON request line, one JSON reply line. Requests
reach the main thread as `Inbound::Test`; the socket thread waits for the
answer up to `timeoutMs` (default 10 000, at most 60 000) and then replies
`{"ok":false,"error":"timed out"}`. `TestRequest` carries that
`deadline`, so a `waitIdle` still polling gives up with
`{"ok":false,"error":"not idle: <why>"}` (the queue, unanswered requests,
previews, the player, io, opening, the portal).

| Request | Reply |
| --- | --- |
| `{"op":"waitIdle"}` | `{"ok":true,"rev":R}` once: `PreviewCache::in_flight()` is 0, the player isn't loading or seeking, and every `qrec-gui-io` job finished with no `MediaHub::hold_io_busy` token held (all checked before the queue: what they send is in the queue before they say they are idle; a debounce that leads to io work holds a token, as the autocut page's 250 ms dry-run debounce does until its dry run is sent, WP5); the inbound queue is empty; the GUI's own requests are answered, but for an export of its own once its job runs (`Store::unsettled_requests`: it reported progress and an export is in the job table; WP7 integration); no source is `AwaitingPortal`; the window isn't opening; the stage is idle (`stage::busy`, WP4: no program reload is due, and in Live the engine settled its live scenes, `LiveControl::progress`, and the picture drew 10 frames since its visible live sources had sizes, again for each new pipeline, `media::live::LiveWatch`); and a frame was drawn after all that and it still holds (frame clock `after-paint`) |
| `{"op":"regions","names":[…]}` | `{"regions":{"<name>":[x,y,w,h],…}}`: window-logical bounds of every widget named `region-*` and of the named parts in G§8.4, plus the visible widgets named in `names` (a button a case clicks) |
| `{"op":"labels","name":"region-header"}` | `{"ok":true,"labels":[…],"entries":{…}}`: the text of every visible label under the widget `name` (default: the window), in tree order, and each visible text field's value by its nearest given name (`"inspector-x": "1280"`, `Gui::entries`; WP5 integration) |
| `{"op":"dispatch","command":{…},"confirmed":false}` | `Store::dispatch_with` with the default reply policy (confirmations open their modal); `{"ok":true}` at once |
| `{"op":"action","id":"export.panel"}` | runs a binding or GUI action as its key would (pre-steps included) |
| `{"op":"sources"}` | `{"ok":true,"inventory":…}`: discovery's last answer (`LocalUi.sources`, null before one); with `"inventory": <SourceInventory>`, that answer instead, pushed through the queue as discovery's own (`Inbound::Sources`; the first run's rows follow it; R25's audit) |
| `{"op":"hints"}` | the rendered hint items (`keys`, `label`, `enabled`, `emphasized`) and the status text (`hintbar::current_items`); `shown`, the labels of the items the bar draws whole (a narrow window leaves the trailing ones out, `hintbar::fitting`), and `cut`, whether a drawn one runs past the bar (never; both WP7 integration audit) |
| `{"op":"local"}` | a summary of `LocalUi`: the startup phase (and a blocked window's page), focus, overlays, drag, layout, playing rate, bin selection, keymap problems; the theme, scale and reduced motion (and `reducedMotionClass`, the window's real class); the Store's `rev`; `panics` caught by the guard (G§3.12 R10); `focusPath`, the focused GTK widget's name (or type) and its ancestors', and `focusInTopLayer` (null without an open layer); the window's `pid`; `playerNs`, the Program player's position (null without a player; round 3); `playing`, the player plays (the program or a bin preview's take), and `programPlaying`, the program plays in it (`LocalUi.program_playing`, G§3.10 step 3; review R00, R16); `panelPage` and `autocutPreview`, the autocut page's preview the timeline draws (null when the page is closed; WP5 integration); `panelTransition`, the panel pages' transition (`"slide-left-right"`, `"none"` under reduced motion: test mode turns GTK's animations off, so a case reads the stack's; WP5 integration audit); `binRowsAlive`, the bin rows still alive (the rows shown, once a rebuild's dropped ones are gone, G§3.12 R11), and the panel's (`layerRowsAlive`, `audioRowsAlive`; review R43), and `mudBarsTicking`, the mud bars running a tick callback (none under the frozen clock, G§3.14; both WP7 integration audit); `metersTicking`, the meter bars on their refresh timer, and `exportClockTicking`, the export panel's Running clock (null without a panel): each comes back with the next change after a caught panic (G§3.12; R65's audit); `themeSubscribers`, the theme's callbacks (a dropped bin thumbnail's goes with it, R11; WP7 integration's shot and code audits); `timelinePaints`, how many times the timeline's canvas made its render node again (not while only the playhead moves, C16; review R10); `timelineGhostColumns`, how many columns of the ghost's waveform the last paint with a ghost worked out a lane (those in the time area, G§4.5; R01's audit); `binSlide`, the bin drawer's last slide as asked (`open`, `animate`: test mode animates nothing, so a case reads whether the open at start would have slid in; review fix, G§3.3); `takePreview`, the bin preview (`take`, `playing`: `Space` pauses it; null without one; review fix, D-17) |
| `{"op":"a11y"}` | every visible icon-only button, focusable region or focusable custom widget without an accessible label (expected empty; needs `GTK_A11Y=test`) |
| `{"op":"fonts"}` | the family Pango loads for each bundled font; `ok` when each resolves to itself; `rendering`, GTK's text settings as they are (`mode`, `hintMetrics`, `hinting`, `hintStyle`, `antialias`: G§3.14's experiment), and `renderer`, the window's GSK renderer (`GskGLRenderer`, `GskCairoRenderer`); with `"measure": [{"family", "sizePx", "weight", "text"}]`, `widths`: each text laid out in the window's Pango context, its `logical` and `ink` widths in px (WP10) |
| `{"op":"icons"}` | each custom symbolic icon drawn in a probe color: found, its pixels, recolored |
| `{"op":"art"}` | each illustration through `gtk::Svg` (`theme::art`): loaded, its size, drawn pixels and `errors` (GtkSvg's, none for `ok`), and `probeErrors` of a known-bad SVG (`<foreignObject>`; at least one for `ok`: the count works) |

Dispatches go through the Store (R8); the other ops only read.

The window's own export (WP7 integration): its request is answered only
when the export ends, so `waitIdle` used to block for the whole export and
WP7's cases clicked Export without it. A long job whose progress arrives as
events is settled once it started: the request counts until it reported
progress and an export is in the job table (then every progress event is a
snapshot through the queue, which `waitIdle` waits for like any), and again
once the job left the table until its answer is in. Only exports: the
window's own `record start` and `record stop` report progress too, and
their cases wait for their answers (the take landing) as before. Every
nested case still waits through deadlines (`timeoutMs`, `poll`).

The live picture's first frames: WP4's `LiveWatch` makes `waitIdle`
wait, in Live, until the visible live sources have sizes and the picture
drew 10 frames since (before, the stage could still show the sink's black
placeholder 0.2–0.7 s after a window opened; logged as "the live picture
is W×H"). A rebuild whose sources keep their states and sizes (an audio
track added: S01b's steps; a rebuilt branch reports its cached size at
once) sends no `LiveStatus`, and its new pipeline draws black until its
sources deliver, so `LiveWatch` also reads the engine's
`LiveControl::progress()` (round 3 integration fixes): busy ("the live
pipeline") until every `set_live`/`retry_live` so far was handled and
each video source of the live pipeline negotiated there (or died; at most
15 s), then 10 frames again for each new pipeline (`generation`). The
asks are counted on the session thread before the engine thread sees
them, so a `waitIdle` after a dispatch's answer never takes the old
pipeline for the new one (the S01b shots were black in nearly every run
before).

---

## G§4 View models (`qrec-core/src/view/`)

### G§4.1 Rules and shared types

- Pure: A§1.3 applies (no fs, clock, env, HashMap). No GTK types.
  Geometry in logical px at 100 % UI scale (D-46, `f64`); strings are
  final copy.
- Each module has insta snapshots over `testkit::design_state()` and the
  per-screen variants its owner builds in its own test module (never in
  the shared `testkit.rs`), plus the proptests named below.
- Shared types (`view/mod.rs`, WP0a, frozen):

```rust
pub struct GuiFacts {
    pub drag: Option<DragKind>,
    pub top_overlay: Option<OverlayKind>,
    pub export_panel: Option<ExportPanelKind>, // open, and its mode
    pub focus: FocusRegion,
    pub panel_page: PanelPageKind,
    pub px_per_frame: f64,                     // the timeline as shown
    pub fit_px_per_frame: f64,
    pub first_run_checked: Option<usize>,
    pub portal_denied: bool,                   // a screen is Refused in LiveStatus (used once S24 is approved)
    pub playing: bool,                         // the program in the player (LocalUi.program_playing), not runtime.playing
    pub bin_selection: usize,
    pub built: Built,                          // { palette, cheat_sheet, settings_pages }: false until WP9
}
pub enum DragKind { TrimStart, TrimEnd, Move, Scrub, LayerMove, LayerResize, LayerCrop, LayerNudge, LayerReorder, BinInsert }
pub enum OverlayKind { Modal, Settings, Palette, CheatSheet, ExportPanel, Menu, LayersSheet }
pub enum ExportPanelKind { Setup, Running, Done, Failed }
pub enum FocusRegion { Header, Stage, Transport, TimelineToolbar, Timeline, Panel, Bin, Field, Other }
pub enum PanelPageKind { Layers, Autocut }
pub struct AutocutPreview { pub spans: Vec<AutocutPreviewSpan>, pub analyzing: bool }
pub struct AutocutPreviewSpan { pub clip: ClipId, pub track: TrackId, pub from: Frame, pub to: Frame /* timeline */, pub tag: String /* "−1.6 s" */ }
pub struct HintItem { pub keys: Vec<Vec<String>> /* chords; each a list of cap legends */, pub label: String, pub enabled: bool, pub emphasized: bool }
pub fn enabled(state: &State, cmd: &Command) -> Result<(), String>;   // keymap::gate_reason (D-43)
```

### G§4.2 `view::format` (WP0a)

| Function | Output |
| --- | --- |
| `timecode(f, fps)` | `time::timecode` (`HH:MM:SS:FF`) |
| `size(bytes)` | "12 KB" (< 1 MB, integer), "129.0 MB", "1.2 GB" (SI units) |
| `gain_db(v)` | "0.0 dB", "−6.0 dB", "3.0 dB" (true minus U+2212) |
| `offset_ms(v)` | "+0 ms", "+80 ms", "−20 ms" |
| `count(n)` | "3,449" |
| `hms(secs)` | "00:00:38" |
| `fps(f)` | "30 fps", "29.97 fps" |
| `canvas(c)` | "1920×1080 · 30 fps" (× is U+00D7) |
| `clips_summary(clips, fps)` | `Some(("10 clips · ", "00:04:06:11"))`, "1 clip · …", `None` → "No clips yet" |
| `take_color(take)` | index (n − 1) mod 8 into moss, donkey, onion, mud, lily, ginger, dragon, fairy |
| `peak(db)` | "−12.4", "0.0"; "—" below −60 |
| `display_path(path, project_dir, home)` | inside the project → relative ("exports/…"); under `home` → "~/…"; else as given |
| `local_hm(at, utc_offset_secs)` | "14:12", with the offset in force at `at` (the GUI asks its zone per instant) |
| `local_when(at, now)` | "today at 15:02", "Oct 14 at 15:02", "Oct 14, 2025 at 15:02" (both `DateTime<FixedOffset>`, each on the clock of its own instant) |

Plus `KeyChord::cap_legends()` (WP0a, `keymap.rs`): modifiers first as
separate caps ("Ctrl", "Alt", "Shift", "Super"), then the key: letters
uppercase; characters literal (`minus` → "−", `equal` → "=", `backslash`
→ "\\", `less` → "<", `bracketleft` → "[", …); named keys `Escape` "Esc",
`Return` "Enter", `space` "Space", `Tab` "Tab", `Delete` "Del",
`BackSpace` "Backspace", `Home`, `End`, `Page_Up` "PgUp", `Page_Down`
"PgDn", arrows "←" "→" "↑" "↓", F-keys "F6". Rebound keys show;
conflicting chords drop out (`keymap::usable_keys`, made `pub`).

### G§4.3 `view::hints` (WP6)

`hint_state(&State, &GuiFacts) -> HintState`, first match:

1. top overlay Modal → Modal; Palette → Palette; Settings → Settings;
   LayersSheet (Narrow) → LayersSheet, `autocut` when the panel page is
   Autocut: the sheet keeps the keyboard (G§3.8 step 4), so the editor's
   keys under it don't run. (The cheat sheet covers the hint bar: keep the
   state below.)
2. drag TrimStart/TrimEnd → TrimDrag; Move → MoveDrag; BinInsert →
   BinDrag; Layer* → LayerEdit.
3. record Countdown → Countdown; Starting or Recording → Recording;
   Finalizing → Finalizing.
4. export panel open in Setup → ExportSetup, Done → ExportDone, Failed →
   ExportFailed. (Running is passive: the state below, with the export
   prefix.)
5. panel page Autocut → AutocutPage.
6. focus Bin → Bin.
7. focus Stage with `edit_target` and a selected layer → LayerEdit.
8. Live mode and the project scene empty → FirstRun (the stage shows S01a
   only in Live, D-27; in Program an empty scene goes on to step 9; one
   rule for both, `view::canvas::first_run`).
9. a screen refused and S24 approved → PortalDenied.
10. timeline empty → Empty.
11. px/frame ≥ 24 → FrameZoom.
12. selection not empty → ClipSelected.
13. Program → ProgramIdle; otherwise LiveIdle.

Export prefix: while an export runs, `export.panel` "Show export" comes
first, and while the passive panel is open `[Esc]` "Close panel (export
continues)" comes before it (S15b).

`hint_bar(&HintState, &State, &GuiFacts, &Keymap) -> (Vec<HintItem>,
String)`. Binding ids resolve to the user's keys (`usable_keys`; an item
whose binding has no usable key is left out); `[x]` is a literal cap. The
first label is `text`, the others `muted`, unless emphasized. Disabled
items show at 0.45. `hint_enabled(id, &State, &GuiFacts) -> bool`
decides, because some ids are handled by the GUI and not by
`action_for`:

| Id | Enabled when |
| --- | --- |
| `export.panel` (the `export` key) | an export job runs, or the timeline has clips and the phase is Idle |
| `autocut.page` (the `autocut.preview` key) | the timeline has clips and the phase is Idle |
| `zoom.selection` | the selection isn't empty (S07: disabled) |
| `zoom.in` / `zoom.out` | px/frame < 128 / px/frame > fit; a combined item when either is |
| `zoom.fit` | the timeline has clips |
| `snapping.toggle` | always, except during a move drag (D-23) |
| `palette.open`, `keys.help`, `settings.open`, `region.*`, `app.quit` | always (the "Not built yet" toast stands in until WP9, G§1.1) |
| anything else | `keymap::action_for(id, state)` is `Run` or `Gui` (not `Unavailable`) |

| State | Items |
| --- | --- |
| FirstRun | `[Enter]` "Add N sources" (disabled at 0 checked) · `record.toggle` "Record" (disabled) · `canvas.mode` "Program" · `palette.open` "Commands" · `keys.help` "All keys" |
| Empty | `record.toggle` "Record" · `canvas.mode` "Program" / "Live" · `palette.open` "Commands" · `keys.help` "All keys" |
| LiveIdle | `record.toggle` "Record" · `take.discard-last` "Discard last take" (emphasized while the cursor entry is a take placement, `cmd` `{"event":"takeFinalized"}`) · `play.toggle` "Play" (disabled, D-01) · `canvas.mode` "Program" · `autocut.run` "Autocut all" · `bin.toggle` "Bin" · `palette.open` "Commands" · `keys.help` "All keys" |
| ProgramIdle | `play.toggle` "Play" / "Pause" · `select.prev`+`select.next` "Select clip" · `zoom.in` "Zoom in" · `zoom.selection` "Zoom to selection" · `canvas.mode` "Live" · `keys.help` "All keys" |
| ClipSelected | `clip.split` "Split" · `clip.trim-start`+`clip.trim-end` "Trim to playhead" · `clip.move-left`+`clip.move-right` "Move" · `clip.remove` (prefer `x`) "Remove" · `autocut.run` "Autocut" · `record.over` "Record over" · `zoom.selection` "Zoom to clip" ("Zoom to selection" for ≥ 2) · `keys.help` "All keys" |
| FrameZoom | `seek.prev-frame`+`seek.next-frame` "Previous / next frame" · `clip.nudge-start`+`clip.nudge-end` "Nudge edge 1 frame" · `clip.split` "Split" · `zoom.out` "Zoom out" · `zoom.fit` "Fit" |
| Countdown | `cancel` "Cancel" · `record.toggle` "Cancel" |
| Recording | `record.toggle` "Stop" · `cancel` "Cancel take (files kept)" · `canvas.mode` "Switch view" · `zoom.in`+`zoom.out` "Zoom" · `zoom.fit` "Fit" |
| Finalizing | "Finalizing take N. Waiting for every file to close (up to 10 s)." · `canvas.mode` "Switch view" · `zoom.in`+`zoom.out` "Zoom" |
| TrimDrag | "Release to trim" · `cancel` "Cancel drag" · `snapping.toggle` "Snapping on/off" · `clip.nudge-start`+`clip.nudge-end` "Nudge after release" |
| MoveDrag | "Release to move" · `cancel` "Cancel drag" · `snapping.toggle` "Snapping on/off" (disabled) · `clip.move-left`+`clip.move-right` "Move with keys instead" |
| BinDrag (not drawn) | "Release to insert" · `cancel` "Cancel drag" |
| AutocutPage | `[Enter]` "Apply autocut" · `[Esc]` "Cancel" · `play.toggle` "Play" · `zoom.in`+`zoom.out` "Zoom" |
| LayerEdit | `layer.nudge-left`+`-up`+`-down`+`-right` "Nudge 1 px" · `[Shift]` "×10 · free aspect" · `[Alt]` "Crop edges" · `cancel` "Deselect" · `canvas.mode` "Program" / "Live" |
| Bin | `bin.toggle` "Close bin" · `[↑][↓]` "Select take" · `[Enter]` "Insert at end" · `[Space]` "Preview" |
| ExportSetup / ExportDone / ExportFailed | `[Enter]` "Export" / "Open file" / "Try again" · `[Tab]` "Move focus" · `[Esc]` "Close" |
| Modal | `[Tab]` "Move focus" · `[Enter]` "Activate focused button" · `[Esc]` "Cancel" |
| Settings (not drawn) | `[Tab]` "Move focus" · `[Esc]` "Close" |
| LayersSheet (not drawn) | `[Tab]` "Move focus" · `[Esc]` "Close"; with the autocut page: `[Enter]` "Apply autocut" · `[Tab]` "Move focus" · `[Esc]` "Cancel" |
| Palette (gated) | `[↑][↓]` "Choose" · `[Enter]` "Run" · `[Esc]` "Close" |
| PortalDenied (gated) | `record.toggle` "Try again" · `canvas.mode` "Program" · `bin.toggle` "Bin" · `keys.help` "All keys" |
| Blocking | S23: `[Enter]` "Check again" · `app.quit` "Quit"; other pages: `app.quit` "Quit" |

Status string (right side): "Snapping on" / "Snapping off", then
`format::canvas`: "Snapping on · 1920×1080 · 30 fps".

As built (WP6, round 3):
- FirstRun's count is `LocalUi.first_run_checked` (WP4's checklist) and
  its label is `view::canvas::add_sources_label`, the button's: "Add 4
  sources", "Add 1 source", "Add sources" (disabled) with none checked
  (round 3; WP6 had "Add 0 sources").
- The export prefix shows only in the window-key states (step 5 on), not
  over overlays, drags or record phases.
- The hint bar is empty while the window opens; Blocking pages show no
  status; a Menu over an overlay keeps that overlay's hints.
- S09: `[S]` Split shows disabled with the playhead on the c2d0/c2d1 cut
  (the core refuses a split there); the frame draws it enabled: this
  section's enablement rule wins.

### G§4.4 `view::header` (WP6)

- Title = `project.name`; subtitle = `format::clips_summary` or "No clips
  yet"; while `runtime.recovering` isn't empty: "Recovering take 7…" with
  a 12 px spinner (D-47).
- Record button (C01 Record). Idle, it says what its key does: disabled
  with `keymap::action_for("record.toggle")`'s reason when that is
  `Unavailable`, so the button, `R` and its hint agree:

  | State | Button | Tooltip |
  | --- | --- | --- |
  | Idle, can start | `● Record [R]` | "Record" + key |
  | Idle, scene empty | disabled | "Add a layer or audio track first" |
  | Idle, export running | disabled | "Wait for the export to finish" |
  | `runtime.engine_stuck` | disabled | "The capture engine is stuck (GPU driver?); restart Qrec" |
  | Countdown | outlined `Cancel [R]` | "Cancel the countdown" + key |
  | Starting, Recording | `■ Stop [R]`, 2 px `record` line under the header | "Stop recording" + key |
  | Finalizing | `Saving…` + spinner, disabled, line kept | "Take N is finalizing" |

- Export slot: `Export…`, disabled on an empty timeline ("The timeline is
  empty") and while recording ("Not while recording"):
  `view::export::panel_enabled`, `Ctrl+E`'s rule (G§4.8). While any export
  job runs (whoever started it): "42%" + mini bar (`export_readout`),
  tooltip "Exporting 42%" + `Ctrl` `E`, pressed look while the panel is
  open; a click toggles the panel.
- Mode switch: always enabled (R-allowed, A§5.3). Tooltips: "Commands" +
  `Ctrl` `K`; "Settings" + `Ctrl` `,`. Until WP9 the Commands button shows
  the "Not built yet" toast (G§1.1).
- Narrow layout only: a layers button (`qrec-layers-symbolic`, 32×32, left
  of Record) toggles `Overlay::LayersSheet`; tooltip "Onion layers".
  `header(&State, &GuiFacts, &Keymap, narrow: bool) -> HeaderView`: the
  GUI passes `LocalUi.layout == Narrow`, since `GuiFacts` (shared with the
  hint bar) has no layout, and `layers_button` = `narrow` (built by WP0a).
  (Changed in the WP0a review: `header` had no input that said the layout
  was narrow, so the button could never show.)
- As built (WP6, review fix R34): the idle record blockers are
  `keymap::start_blocker`'s, in the order engine stuck, empty scene,
  export (the stuck engine was the header's only, so `R` and `o` sent a
  take the core refused with a sticky error toast; now they say why in an
  info toast). An invalid `qrec.json` blocks none of them: a take moves
  the bad file aside first (A§7.7), as the key and niri's `qrec record
  toggle` always did (the header showed Record disabled). The subtitle
  while recovering is "Recovering N takes…"; the idle export tooltip is
  "Export"; disabled record tooltips name no keys. The summary is two
  labels on one baseline ("10 clips · " and "00:04:06:11"), so a reader of
  the labels joins them.

### G§4.5 `view::timeline` and `view::ruler` (WP3)

- **Viewport** `{ width: f64 /* time area */, px_per_frame: f64,
  scroll_px: f64, lanes_scroll_px: f64 }`, logical px at 100 % (D-46).
  - Fit: `(width − 8) / total` (an empty timeline has no viewport).
  - Range `[fit, 128]` px/frame; frame level ≥ 24.
  - Slider: `s ∈ [0, 1] ↦ fit · (128 / fit)^s`.
  - `ui.zoom = None` ⇔ fit, with scroll 0.
  - Readout: "Fit"; below 8 px/frame "<round(ppf × fps)> px/s"; otherwise
    "<round(ppf)> px/frame".
  - Steps: `=` / `−` ×2 or ÷2 (clamped), anchored at the playhead (the
    viewport center when the playhead is off-screen); Ctrl+wheel ×1.25 per
    notch, anchored at the pointer, taken from the scroll event itself, over
    the rows and over the ruler (review: the ruler recorded no pointer and
    zoomed at the time area's left edge); `\` fits.
  - `z` (zoom to selection): ppf = width ÷ the selection's frame span,
    scroll = span start × ppf: the selection fills the time area exactly,
    no margin (S12: 1040 / 3105 = 0.335 px/frame, c3e9 from x 176 to
    1216, readout "10 px/s"). With no selection it is unavailable
    (`hint_enabled`, S07) and the key toasts "Select the clips to zoom to"
    (D-34).
  - Animation: 200 ms `EASE_OUT_EXPO` in log(px/frame), keeping the anchor
    time's x fixed; instant under reduced motion (followed live through
    `Theme::follow_motion`, round 3) and in test mode. A pure `ZoomAnim`
    on the frame clock, not `adw::TimedAnimation` (WP3).
  - Fit uses the inset rule (G§1.3): c8a1 is 63.8 px wide and reads "1";
    S07 and S11 show "Take 1" at 64.3 px (WP3).
  - While recording, fit includes the ghost (total + elapsed frames) as
    it grows on the frame clock between record clocks, at once
    (`ViewModel::follow_ghost`; a clock behind that growth never shrinks
    it), so the ghost's end and the playhead parked there stay inside the
    8 px inset and the clips glide. The fit eases (200 ms, instant under
    reduced motion) when the ghost starts or ends. (Review: refitted only
    at each `RecordClock`, 4 Hz, with a 200 ms ease, they ran up to 25 %
    past the time area between clocks early in a take.) The timeline runs
    `control::follow_snapshot` per snapshot and `control::follow_frame`
    per frame, the steps its test drives (R05's audit: the test copied
    them, and the frame's step could go from the timeline unnoticed).
- **Wheel, pan, scrub**: D-39, D-40. `scroll_for_wheel`, `zoom_at(ppf,
  factor, anchor_x)` and `scrub_frame(x)` are pure. The middle-drag pan's
  vertical part follows the pointer in the lanes' scrolled window (the
  event's y plus `Cx::rows_top`, the canvas's top there as laid out for
  the event) and sets the lanes' offset from the press's: the canvas moves
  with the scroll the pan sets, so its own y fed back (review: the lanes
  moved half the pointer's travel, or swung back and forth).
- **Clip boxes** (time-area x = start × ppf − scroll; w = len × ppf, no
  minimum):

  | Width | Label |
  | --- | --- |
  | ≥ 64 | "Take N" |
  | 24–63 | "N" |
  | 12–23 | none |
  | < 12 | sliver: stripe + 1 px `text-35` edge marker |

  Plus the take color index, a 1 px `bg` divider at each right edge, and
  flags: selected, hovered, analyzing (a non-dry-run `Autocut` job targets
  it, D-15), missing (the clip's own thumbnail layer, its take scene's
  topmost visible one, has no media in its take: C19 hatch + warning
  icon), recorded-over (`muted-40` hatch, S04b),
  dimmed (D-43).
- **Rows**: toolbar 36, ruler 28, video 76 (4 stripe + 72), lanes 56,
  gaps 4. Compact (G§3.15): video 72, lanes 48, no gaps (36 + 28 + 72 +
  48 + 48 = 232); compact lane headers pad 3 px above and below, and the
  quiet swamp is 160×96 there (no frame for either; WP3).
- **Lanes**: the union of the tracks of the project scene and of the take
  scenes on the timeline; project-scene order first, then by id. Per clip
  segment:
  - the take tint (12 %);
  - the waveform of that take's stream for the track, mirrored ±22 px,
    `wave`; `wave-dim` when the lane is in `lane_mute`, or when a solo is
    set and it isn't this track; drawn from the coarsest `PeakLevels`
    level with at least one bucket per px;
  - hatches: silence spans on the autocut track's lane; autocut preview
    spans at full strength, the same ranges dimmed with `bg-55` on the
    other rows, and a `warning-text` tag ("−1.6 s") 20 px above. As built
    (WP3): the spans are the analysis intervals rounded to frames, made
    once per state; the autocut preview hatches in `warning`, the known
    silences in `hatch-stripe`; at frame level the waveform is the 1 ms
    min/max envelope (S09 draws a sample curve), and waveforms are color
    rects, not path fills (frame time on 300 clips).
- **Ruler**: label interval L = the smallest of 1f, 2f, 5f, 10f, 15f, 1s,
  2s, 5s, 10s, 15s, 30s, 1min, 2min, 5min, 10min, 15min, 30min, 1h with
  L × ppf ≥ 90 px. Ticks by L:

  | L | Major | Minor |
  | --- | --- | --- |
  | ≤ 15f | L | 1f |
  | 1s | 1s | 5f (4f at 24 fps) |
  | 2s | 1s | 10f (8f at 24 fps, 5f at 25) |
  | 5s | 1s | — |
  | 10s, 15s | L | 5s |
  | 30s | 30s | 10s |
  | 1min | 1min | 10s |
  | 2min, 5min | 1min | 30s |
  | ≥ 10min | 5min | 1min |

  - Minor ticks are hidden when closer than 6 px. Ticks are `muted`,
    rising from the bottom edge: major 10 px, minor 5 px.
  - The 1s and 2s rows' frame minors divide the nominal second (the
    largest divisor up to 5f and 10f, `ruler::second_minor`), and the
    ticks are the union of the major and the minor grid, so no second's
    major or label is skipped (review: at 24 fps 5f minors drew a label
    every 5 s at 1s labels, and at 25 fps no major at the odd seconds).
  - Labels: `HH:MM:SS:FF`, `mono-xs` `muted`, at y 4, 4 px right of the
    tick. A label shows while its tick shows: cut at the right edge,
    dropped at the left, as S07, S08 and S12 draw (WP3; this said "clipped,
    never dropped").
  - Frame level (≥ 24 px/frame): a 5 px tick per frame and the `FF`
    number centered in each cell (y 12). At second boundaries: a 28 px
    tick and the full timecode in `text` (y 1, 4 px right). The current
    frame's cell has an `accent-2-20` band.
  - Corner cell: the playhead timecode, `mono` `text`, at (12, 5).
  - Checks against the mocks: fit 0.1396 → 30 s / 30 s / 10 s; 48 px/s →
    2 s / 1 s / 10 f; 10 px/s → 10 s / 10 s / 5 s (S12); recording fit
    0.0850 → 1 min / 1 min / 10 s (S04, S05, S05b). The table said 5 s / 1 s
    for 10 s and 30 s / 10 s for 1 min, which drew 1 s ticks and a 10 px
    tick at every 5 s in S12 and one at every 30 s in S04, S05 and S05b;
    the frames have neither (WP5 integration audit, measured on the frames'
    pixels; `s12_ticks_are_the_labels_and_5_s_between`,
    `recording_fit_ticks_are_the_labels_and_10_s_between`).
- **Playhead**: 2 px from the ruler to the last lane, line at +5.5, head
  path `M0 0h13v6l-6.5 6-6.5-6z`. Scrub flag at (playhead + 10, 3)
  inside the ruler (WP3; this said (+16, −2)): `mono-sm` `text` on
  `raised`, 1 px `playhead` stroke. Auto-page: when the playing playhead
  passes the right edge − 8 px, jump one page. While recording, the
  playhead parks at the ghost's end (the corner shows it).
- **Hit test**: trim zones per D-45; a clip body → select (D-37) and move;
  the ruler → seek and scrub; an empty row → `Select{Clear}`. Lane headers
  are widgets.
- **Snapping** (`ui.snapping`): targets = the playhead and every clip edge
  but the dragged ones; 8 px threshold; a 1 px `snap-guide` flash for
  300 ms.
- **Move drag**:
  - starts after 4 px without a modifier; the block is the selection if
    the pressed clip is selected, else that clip (the press selected it,
    D-37);
  - the other clips close the gap (120 ms `EASE_OUT_EXPO`; `control::
    GapEase`: when a clip's place moves it eases there from where it is
    drawn, in timeline frames, so a slot change in the middle of an ease
    never jumps and a zoom during the drag moves the boxes, not the eases;
    at once under reduced motion and in test mode; review R11: they
    jumped, nothing filled the easing's map). The canvas paints
    `control::painted_clips`, the boxes, trim preview and move preview
    with the eases applied, which a unit test checks mid-ease (R11's
    audit);
  - the caret goes to the boundary nearest the pointer (slot k of n, n =
    clip count);
  - the lifted copy: 85 %, e1, 4 px up, over the video row and lanes;
  - tooltip "Move after c1b6 · slot 7 of 10" ("Move to the start · slot 1
    of 10"; "slot k of N" counts slots, the other clips plus one) and
    "Starts at <tc>"; ripple tag "← <tc>" on clips whose start moves,
    centered on the shifted run at the video row's bottom (S11; WP3);
  - release → `ClipMove{clips: block, to: MoveTo::Index(k − 1)}`, nothing
    when the block lands where it was; `Esc` cancels.
- **Trim drag**:
  - d = frames moved (snapped). Start edge: new in = in + d ∈
    [0, out − 1]. End edge: new out = out + d ∈ [in + 1, durationFrames].
  - Preview: a 6 px `accent` bar at the new edge; the trim ghost (media
    between the old and new edge, or out to the take bounds when
    extending: 40 % filmstrip, 1.5 px dashed outline, tag "−29 f" / "+12 f"
    at (4, 54)); ripple tag "← <tc>" or "→ <tc>" on the following clips;
    tooltip "In · take 5 frame 41 · 00:00:01:11" / "Δ −29 f" (Out for the
    end edge). As built (WP3): the ripple tag sits after the clip's label
    (S10); an extension draws the uncovered media at full strength, the
    rest of the take's media as the ghost, its tag on the extension.
  - `LocalUi.trim_frame` = (take, new in, or new out − 1).
  - Release → `ClipTrim{clip, start | end: Some(TimeArg::plus_frames(d) |
    minus_frames(−d)), timeline: false}` (relative edits, A§5.2); d = 0 →
    nothing.
- **Bin drop** (D-38): `drop_slot(&Viewport, clips, x) -> usize` (the
  nearest boundary, 0…n) and the caret at that boundary; tooltip "Insert
  take 4 · slot k of n + 1".
- **Ghost** (from `runtime.record`):
  - Append → at the end; Over → in the first replaced clip's slot, with
    the replaced clips hatched `muted-40`;
  - width = frames(elapsed) × ppf (`ns_to_frame_floor`, interpolated on
    the frame clock from the last `elapsed_ns`, never past it + 300 ms);
  - plate "● Take 10 · recording", with "over c2d0 (take 5)" as a second
    line for Over (never a second plate);
  - Finalizing → animated `record-25` stripes and "Take 10 · saving…";
  - lanes under the ghost draw the live waveform from the level ring
    buffer (RMS per 50 ms, × √2: a sine's peak, so a steady tone keeps
    its height when the take lands), newest at the right edge, where the
    playhead is parked; worked out for the ghost's columns in the time
    area only (`view::timeline::ghost_columns`: a long take zoomed in is
    a window of columns, not its length; review R01, and `local`'s
    `timelineGhostColumns` in the nested case its audit asked for).
  - As built (WP3): with record-over, the following clips move once the
    ghost outgrows the replaced run; several replaced clips read "over N
    clips"; a cancelled take's ghost hides while it finalizes.
- **Clip tooltip** (C33 clip): "Take 5 · c2d1"; "In 00:00:36:24 · Out
  00:01:13:24 · 00:00:37:00" (take-relative in and out, duration); keys
  `S` Split · `[` `]` Trim · `X` Remove (the user's keys), with no
  separators, as S06 and S07 draw them. The timeline's tooltips (clip
  hover, trim, move, drop) are widgets in a layer over the timeline, not
  GTK tooltips: 500 ms hover delay, none in test mode (WP3).
- **Toolbar** (C14): Bin (count = bin takes, `b`), Snap (`n`), divider,
  Autocut (`c`), the autocut-settings icon (`autocut.page`); right:
  summary, zoom out, slider, zoom in, Fit, readout. Empty and Recording
  variants (S01a, S04).
- **Headers** (C17): Video, "Thumbnails: Face (L2)" (the project scene's
  topmost visible layer); lanes: role icon, label, id chip, scissors on
  `autocut.track` (unset: each lane a clip is analyzed on, G§4.7's
  autocut page), `M` (D-08), `S` (`SetUi{solo}` toggle; tooltip "Solo
  A1 while playing (export unaffected)"). The header column and the named
  rows (`video-row`, `lane-A1`, …) stack at the canvas rows' whole px at
  the UI scale (`Rows::px_heights`: differences of rounded tops, so they
  stay on the lanes the canvas paints at `y × scale`; review: the headers
  asked for their 100 % heights, 40 px above A1's lane at 150 %), named
  `header-video`, `header-A1`, ….
- **Thumbnails** (Q4): per clip, the topmost visible layer of its take
  scene (`thumbnail_layer`), whatever its media. When that layer has none
  (`missing`, or no video stream: `thumbnail_stream` is `None`) nothing
  falls back to a lower layer: the clip is flagged missing (C19 hatch and
  the warning icon, DESIGN's "thumbnails replaced by hatch"), and a bin
  row's thumbnail is blank (WP7). (Review: this said "that has media".)
  - Fit and seconds zoom: 128 px tiles from the clip's x; tile i shows the
    strip image at the take time of its left edge (file time = take time
    − `startOffsetNs`, nearest second; `ThumbCache::strip_image`).
  - Frame level: one 32×72 center crop per frame (`ThumbCache::frame`,
    recorded frame via `shown_frame`); the strip image while it loads.
  - No data yet → `raised` fill.
- **Empty state** (S01b, C31 Timeline): toolbar Empty variant; the ruler
  and rows are replaced by the centered block: quiet swamp art 200×120,
  "This swamp is quiet." (`display`), "Press [R] and make some noise."
  (`body` `muted` with an lg key cap of `record.toggle`), 10 px under the
  `display` line (S01b). When `record.toggle` has no key that runs
  (unbound, or in a keys.toml conflict) the cap is left out and the line
  reads "Click Record and make some noise.", as the hint bar leaves its
  Record item out (`EmptyState.key` is `None`; review: it showed a
  hard-coded R).
- **Hatches** are the C19 geometry from `widgets::draw::HatchMetrics` at
  100 %: the canvas is painted in 100 % px into a snapshot scaled by
  `Theme::scale`, so they scale with the UI (`timeline/paint.rs`, which
  adds the saving ghost's moving phase; round 3).
- **Frame-time probe** (`QREC_GUI_PROBE=timeline`, G§8.6): runs twice and
  asserts p99 on the warm pass; the cold pass compiles GSK's shaders (the
  test HOME's Mesa cache is empty: 4 frames of 33–67 ms).

Proptests: boxes tile the timeline (x[i+1] = x[i] + w[i]); label rank is
monotone in width; `frame_at_x(x_of_frame(f)) == f`; `zoom_at` keeps the
anchor's frame under the anchor; a move preview's command, applied with
`apply`, gives the previewed order; `drop_slot` is monotone in x.

### G§4.6 `view::canvas` (WP4)

- `edit_target(&State) -> Option<SceneTarget>`: `ui_normalize::edit_target`
  (WP0a, D-06), re-exported.
- `stage_geometry(stage_w, stage_h, &Canvas) -> StageGeometry { preview:
  Rect, transport: Rect }` (G§3.15), stage-local. Checks: (1216, 608) →
  preview (128, 16, 960, 540), transport (128, 564, 960, 36); (864, 608),
  the bin open → preview (16, 52, 832, 468), transport (16, 564, 832, 36).
  Add the stage origin, (0, 48) or (352, 48), for window coordinates.
- `layer_boxes(scene, canvas, sizes, preview) -> Vec<LayerBox>`:
  `layout::place(src_w, src_h, crop, rect, fit)` dst × scale. Source size:
  Live → `LiveStatus.size`; Program → the take stream's width and height;
  unknown → the rect. Each box: visible, locked, z (array order, last on
  top). Checks: L2 at rest (1500, 760, 384, 288) is at preview-local
  (750, 380, 192, 144); dragged to x 1280 (S13) at (640, 380, 192, 144).
- **Hit**: the topmost visible box under the point. Handles: 8×8 on the
  corners and edge midpoints, 12 px hit area; with `Alt`, edge handles
  become crop brackets. The selection box and its handles are the layer's
  rect (`LayerBox.frame`), which resizes, crops and snapping change; a
  press on a body hits the picture (`LayerBox.rect`, place's dst). The two
  differ only for a Contain layer whose source has another aspect (a
  16:10 screen on a 16:9 canvas: pillarboxed inside its box); the handles
  sat on the picture, so a grabbed one drifted off the pointer (R20). Locked layers can be selected, never dragged
  ("Layer L2 is locked").
- **Snapping** (D-09): lines x ∈ {0, W/3, W/2, 2W/3, W} and y ∈ {0, H/3,
  H/2, 2H/3, H}; layer features left, center, right and top, middle,
  bottom; threshold 8 preview px; the nearest per axis. Guide labels:
  "Left edge", "Left third", "Center", "Right third", "Right edge"; "Top
  edge", "Top third", "Middle", "Bottom third", "Bottom edge" (S13: the
  left edge at x 1280 → "Right third").
- **Move**: rect + (dx, dy) in canvas px, snapped. **Resize**: a corner
  keeps the aspect of the rect at drag start (`Shift` frees it); an edge
  handle changes one axis; minimum 16×16. **Crop** (`Alt` + edge handle):
  that side's crop += delta × source / dst on that axis, keeping at least
  16 source px. **Nudge burst** (D-33): `nudge(rect, dx, dy, shift)`
  accumulates.
- Readout "x 1280 · y 760 · 384 × 288" (`mono-sm`), 8 px above the box,
  below it near the top edge; styled as C33 (raised, hairline, e1); guide
  tags are video-scrim captions (WP4). The selection box shows only while
  the stage canvas has the focus or a gesture runs (S06 and S14 show none,
  S13 does). Alt's crop brackets are 2.5×-long bars on the edge handles.
  Live crops show at the source's next frame (a still screen: its 500 ms
  keepalive, A§8.11).
- Overlays:
  - `countdown(&State, ms_since_tick) -> Option<{ digit, sweep: 0..1,
    take }>`;
  - `rec(&State) -> RecBadge::{None, Starting(take), Recording { take,
    elapsed_ns }, Saving(take)}`;
  - `transport(&State, player_frame, playing, rate) -> { left:
    Timecode { now, total } | Live("Live · 1920×1080 · 30 fps"), enabled,
    rate: Option<String> }`;
  - `badges(&State, take_preview, proxy) -> [Badge]`: LIVE, PROGRAM,
    `TAKE 4` (D-17), `PROXY` (H-9) 8 px left of the mode badge. The
    PROXY badge never shows yet: the player composites at canvas size
    (G§3.10).
  - As built (WP4): the Starting/Saving pill is 28 px, `body-strong`,
    with a 14 px spinner; the trim plate 28 px; the countdown ring 200 px
    with a 6 px stroke, its plate 12 px under it.
- **First run** (S01a): `first_run(&State)`, when: Live mode with an empty
  scene (D-27; the hint bar's FirstRun state reads it too, G§4.3 step 8);
  `first_run_rows(screens, cameras, mics, monitors, default_mic,
  default_monitor) -> Vec<SourceRow { kind, label, meta, checked }>`
  (labels "Screen · DP-4 · GIGA-BYTE GS27QA" with meta "2560×1440";
  "Camera · Logitech C920 · /dev/video0"; "Mic · Yeti Nano"; "System audio
  · Built-in Audio Analog Stereo"); `with_choices(rows, &FirstRunChoices)`:
  the rows the person toggled (by kind and source) keep their state when a
  refreshed inventory rebuilds the list, the others follow the defaults
  (R25), and the row that had the window's focus (active or not) has it
  again (R25's audit: `has_focus` is false in a window in the background,
  and GTK moved the focus to the first row; the nested
  `the_first_run_keeps_its_picks_across_a_new_inventory` pushes an
  inventory through the test channel's `sources`); `first_run_commands(&rows) ->
  Vec<Command>` (D-16); button "Add 4 sources" / "Add 1 source"
  (`add_sources_label`; "Add sources", insensitive, with none checked, no
  frame), the hint bar's label too (G§4.3). As built (WP4): rows 40 px
  (the list 202 px against the frame's 204), named `first-run-row-<i>`;
  before discovery answers, a "Looking for screens, cameras and mics…"
  spinner row (no frame); the layers get the core's names ("Screen
  FAKE-1", "Camera"), not BRIEF's "Main screen" / "Face"; the Add button
  takes the focus once per showing when it can be pressed, from GTK's
  initial focus (the header), the stage or nothing, never from another
  region.
- Trim plate (S10): "Trim start" / "Trim end" + "· take 5 · frame" + mono
  "41", and the 2 px `accent` preview stroke.
- Portal denied (S24, gated): each refused screen layer's box gets the
  `muted-20` hatch and a centered block: `qrec-monitor-x-symbolic` 32,
  "Screen access denied" or "Screen picker closed", "<output> · <label>",
  Try again (D-50). Not built (G§0.2): a refused screen has the core's
  toast, WP5's row error and a Live picture without it.

Proptests: snapping never moves a feature more than the threshold; an
aspect-locked resize keeps w/h within 1 px; a crop never leaves fewer than
16 source px; the preview always fits the stage at the canvas aspect.

### G§4.7 `view::panel`, `view::meter`, `view::autocut` (WP5)

- **Header**: "Onion layers". Subtitles: Live "Project scene · new takes
  use this"; Program with a selection "Take 5 layout · edits this take
  only", plus the banner "Changes here only affect take 5." and `Layout ▾`:
  - "Apply this layout to all takes" → `SceneApply{from: Take(t), to:
    AllTakes}`;
  - "Use this layout for new takes" → `SceneApply{from: Take(t), to:
    Project}`;
  - "Reset take to project scene" → `SceneApply{from: Project, to:
    Takes([t])}`.

  Program with no selection: "Select a clip to edit its take layout"
  (D-06).
- **Layer rows**, topmost first (the reversed array): name; source line
  "Camera · Logitech C920" or "Screen · DP-4 · GIGA-BYTE GS27QA"; states:
  - selected (`ui.selected_layer`), hidden, locked;
  - error (Live; `LiveStatus` Refused or Dead for its source):
    `record-text` icon and "Access denied by the desktop portal" / "Screen
    picker closed" / the message;
  - missing (take scene `missing`): "Missing in this take";
  - disabled (D-43, or `edit_target` is `None`).

  Commands: drag reorder → `LayerOrder{target, layer, to: n − 1 − row}`;
  eye → `LayerSet{visible}`; lock → `{locked}`; rename → `{name}` (Enter
  saves, Esc cancels); overflow: Rename, Move up, Move down, Remove
  (`LayerRemove`). A rename that sends nothing (blank, the same name) or
  that the core refuses shows the name again (`panel::layer_rename_outcome`,
  `track_rename_outcome`); edits stopping (D-43: a recording) end one
  unsaved, as they drop a field's edit (review R46).
- **Audio rows**: role icon, label, id chip, the "Autocut" tag on
  `autocut.track`, `M` → `AudioSet{muted}`; overflow: Rename (`label`),
  Change source (submenu of devices, project scene only), Use for autocut
  (`AutocutSettings{track}`), Remove (`AudioRemove`). Gain "0.0 dB"
  (−60…60, step 0.5, `AudioSet{gain_db}`); Sync "+0 ms" (−5000…5000,
  step 10, `AudioSet{sync_offset_ms}`). Device missing (Live: no `Live`
  status for its device): "Device not found. Plug it in, or pick another
  source."
- **Meter** (`view::meter`, C11): x(db) = clamp((db + 60) / 60, 0, 1) ×
  bar width (296 px in the 384 panel, 232 at 320); zones −60…−18
  `meter-green`, −18…−6 `meter-yellow`, −6…0 `meter-red`; 2 px segments
  with 1 px gaps (3 px steps); 2 px peak hold in `text`, falling at
  20 dB/s after 1.5 s; clip latch when a peak reaches −0.1 dBFS, until
  clicked; readout = the held peak (`format::peak`); muted → level and
  peak at 35 %; the channel value is the maximum over channels.
- **Inspector** (when a layer is selected): X, Y, W, H (+ aspect lock,
  default on, panel-local), crop T/R/B/L, opacity % (slider 0–100 +
  field), fit Contain/Cover. Canvas px and source px. Medium layout
  (320 px): the fields stack 2-up.
  - Invalid → helper text: "Width must be at least 1", "X must be between
    −8192 and 8192", "Crop leaves nothing to show".
  - Enter or blur commits one `LayerSet{target: edit_target, layer,
    patch}`; the slider commits on release. While the slider is being
    changed (dragged, or a key moved it) a snapshot leaves it and its field
    where the person has them; a change that ends unsent shows the model's
    value again (`ValueTooltip::changing`, `on_end`; review R44); a
    focused slider follows the model like the field (an undo while the
    keyboard is on it; `a_focused_slider_follows_an_undo`, R44's audit).
    The wheel moves a slider (this one, the autocut page's) only while the
    keyboard is on it, a burst of notches being one change that ends
    300 ms after its last notch; over another slider it scrolls the panel,
    as over a field (review R42). The keyboard is on a slider it moved the
    focus to, or that a key moved since: GTK 4.22 focuses a pressed slider
    despite `focus_on_click(false)`, so after a drag the wheel moved the
    slider and saved it whenever the window was active (R42's audit:
    `the_wheel_after_a_drag_scrolls_past_the_slider`).
  - An edit belongs to the scene and the layer (or track) it was typed
    for: when either changes under it (the mode, the selection, an agent's
    command), the field shows the new values at once, focused or not, and
    a rename ends unsaved; nothing goes to the new scene (review R38).
    That is for a change from outside the window (an agent's, the CLI's).
    One the person starts commits the edit first: a press anywhere outside
    the field or rename ends it before the press acts (G§3.12 R12), so a
    click on the mode switch, Play or any other chrome button sends the
    edit to the scene it was typed for, one undo step, ahead of what the
    button does (R38's remainder: chrome buttons never take the focus, so
    the field kept it, and the mode switch dropped the edit, Play left it
    unsent).
  - During a canvas drag or nudge burst the fields show
    `LocalUi.layer_drag`.
  - Empty: "Select a layer on the canvas or in the list to edit its
    position, size and crop." An empty scene shows no inspector at all
    (BRIEF S01a; `panel::inspector_shown`, the WP5 integration).
- **Autocut page** (`view::autocut`, S12, D-15):
  - `AutocutDraft::from(&Project)`. Ranges: threshold −60…−10 dB step 1;
    min silence 100…2000 ms step 50; padding 0…500 ms step 10; min keep
    0…1000 ms step 10. "Detect silence on": the project scene's tracks,
    "A1 · Yeti Nano (mic)", then a track the scope's clips are analyzed on
    that the project scene no longer has (a take recorded with it).
  - `dry_run(&State, &draft) -> Command`: scope = the selection, or All;
    overrides = the draft. `dry_run_key(&State, &draft) -> DryRunKey`: the
    command and what it doesn't carry but its answer is worked out on (the
    timeline's clips; the setting's track while the draft names none). The
    page asks again when the key changes, not only the command: a trim, a
    move, an undo or another client's edit under the same selection; and
    when the clips changed under the shown plan (`DryRunKey::stale_for`)
    its hatches, summary and regions go until the new answer (review R40,
    R76).
  - `apply(&State, &draft) -> Vec<Command>` (D-15).
  - `summary(&AutocutPlanView, &State) -> AutocutSummary { title, rows:
    Vec<(String, String)>, regions: Vec<RegionRow> }`, matching the S12
    PNG: title "Removes 4 silences (6.2 s)"; rows ("Take 8",
    "00:01:43:15 → 00:01:37:09") per affected take, then ("Timeline",
    "00:04:06:11 → 00:04:00:05"); regions `{ n, range: "00:02:41:12 →
    00:02:43:00", size: "48 f · 1.6 s", seek: Frame }` (a click seeks and
    highlights the row R1). Nothing to cut: title "Nothing to cut" and the
    core's message as one row.
  - `preview_spans(&AutocutPlanView, &State) -> Vec<AutocutPreviewSpan>`:
    the clip minus what is kept, in timeline frames before the cut.
  - Header subtitle: "Previewing take 8 · c3e9" (one clip), "Previewing 4
    clips", "Previewing all 10 clips".
- **As built** (WP5, 99197b5): Apply's answer is a GUI-owned success toast
  (S25 #6's copy from the reply, with Undo); a dry run's or Apply's error
  reads in the summary ("Nothing to cut" and the core's reason); the
  summary adds a "Skipped" row when the plan skipped clips; while
  `autocut.track` is null the dropdown, the "Autocut" tag and the hatches
  name the track the dry run analyzes (`autocut::default_track`: the
  scene's first mic, else its first track; review R77). Each take is
  analyzed on its own scene's (`autocut::analyzed_tracks`), so a take
  recorded with a track the project scene no longer has is analyzed on
  it: the dropdown shows the track its scope's first clip is analyzed on,
  each clip's hatch sits on its own, and the scissors on every lane a
  clip is analyzed on; the tag names the scene the panel shows (in
  Program the selected clip's take, in Live the project scene: what the
  next take is analyzed on). The dropdown and the Live tag read only the
  project scene, and the scissors the first mic among every lane (R77's
  audit: a take with a mic the project scene lost was cut on the mic
  while the page said "System audio"). The dropdown's
  open list is `Overlay::Menu` (`panel::menus::track_dropdown`, the
  helper for any `gtk::DropDown`; review R32, R41): measured on GTK 4.22,
  an open list's keys go to its popover and never reach the window's key
  router, so `Esc` closes the list, not the page. In the Narrow layout the
  page shows in the layers sheet and goes with it (review R39): the sheet
  closing (`Esc`, a click beside it, the header's layers button) closes
  the page, a window narrowing under the open page shows the sheet, and
  Apply or Cancel take away a sheet the page opened, not one opened before
  it (`panel::autocut::sheet_step`). A missing
  device's line replaces its meter; Change source lists the devices of
  the track's role; a layer problem's icon is `qrec-monitor-x-symbolic`
  (a screen), `dialog-error-symbolic` (another source) or
  `dialog-warning-symbolic` (missing in the take). Medium (320 px): the
  region rows are `mono-xs` with 8 px padding, ellipsized (no frame draws
  S12 that wide). The timeline draws `LocalUi.autocut_preview` (WP3's
  canvas): each region hatched in `warning` with its edges on its lane,
  the same range dimmed (`bg-55`) on the video row and the other lanes,
  its tag 22 px above the lane; the toolbar's settings icon runs
  `autocut.page` (`autocut.preview`, G§3.8 step 10).

### G§4.8 `view::bin` and `view::export` (WP7)

- **The fact `Take.recovered`** (WP0a): `#[serde(default,
  skip_serializing_if = "is_false")] pub recovered: bool`, set by startup
  recovery (A§5.3) when the take ends `ok` or `tooShort`. Like `placed`,
  hand edits can't change it (A§7.4: `history/external.rs` lists it with
  the facts) and `SceneApply` never copies it. It stays in schema v1:
  A§2's optional additions (`error`, `placed`, `replaced`, …) were made
  the same way before the first release. Older dev builds reject a file
  that has it (`deny_unknown_fields`); acceptable before release. The
  first schema change after the first release bumps `version` with a
  migration (SPEC §6.3). Recorded in A§2 and A§13 (WP0a).
- **Bin rows**: takes no clip references (status ok), plus broken and
  tooShort takes, newest first.
  - Title "Take 4" + chip: Broken (error) for broken; Too short (warning)
    for tooShort; Recovered (neutral) for ok and recovered; else Unused
    (neutral).
  - Mono line "<duration> · <local HH:MM of recordedAt> · <size>"
    (duration "—" when broken). The local time uses the UTC offset in
    force when the take was recorded, not the one now:
    `bin_rows(&State, sizes: &BTreeMap<TakeId, u64>, offsets:
    &BTreeMap<TakeId, i32>, fallback_offset_secs: i32)`, where the GUI
    fills `offsets` per take from its zone
    (`Local.offset_from_utc_datetime(&recorded_at.naive_utc())`) and
    `fallback_offset_secs` is the offset now; `bin::recorded_hm` (WP0a)
    gives the "14:12". (Changed in the WP0a review: one `utc_offset_secs`
    for every take showed a take recorded before a DST change an hour off.)
  - Secondary line (D-18), first that applies:

    | Take | Line |
    | --- | --- |
    | tooShort | "7 frames; takes need 10." ("1 frame; …") |
    | broken, `error` = `on_event::record::EOS_TIMEOUT_REASON` | "Files didn't close within 10 s." |
    | broken, `error` starts with "recovery failed: " | "Couldn't be recovered: <rest>." |
    | broken, otherwise | the error's first sentence, capitalized, with a period |
    | ok, `recovered` | "Recovered after Qrec closed mid-take." |
    | ok, not `placed` | "Cancelled with Esc." |
    | ok, another take's `replaced` holds one of its clips | "Replaced by take 5." |
    | ok, a history entry "Discard take N" at or before the cursor | "Discarded with D." |
    | ok, otherwise | "Removed from the timeline." |

  - Actions per status (BRIEF §3.4); "Open log" for broken takes.
  - Header "5 takes · 436.4 MB"; multi-select footer (D-37) "2 takes
    selected · 184.1 MB · Purge from disk".
  - Commands: Insert at end `ClipAdd{take, at: End}`; Insert after
    selection `ClipAdd{take, at: AfterSelection}` (disabled with no
    timeline selection: "Select a clip on the timeline first"); drag to
    the timeline `ClipAdd{take, at: Index(k)}` (D-38); Reveal
    `RevealPath(takes/<id>/)`; Open log `OpenPath(takes/<id>/recorder.log)`;
    Purge `TakePurge{takes}` (the modal comes from
    `confirmation_required`; `BinAction::Purge.disabled` is
    `view::enabled`'s reason, D-43: dimmed with it while recording or with
    `qrec.json` invalid, the row's button and the S18 footer's; review fix:
    both stayed live and the core's refusal toasted).
- **Export**:
  - `panel_enabled(&State) -> Result<(), String>`: the panel opens while
    an export runs, else not while recording ("Not while recording") or
    on an empty timeline ("The timeline is empty"); a refused `Ctrl+E`
    toasts that reason (D-34, `unavailable_notice("export", …)`; review
    fix: it opened nothing, silently). The header slot (enabled, its
    reason the tooltip) and `hint_enabled("export.panel")` follow it (each
    kept its own copy of the rule until the review's integration).
  - Captions, `caption(format, codec, &State, &UserConfig)` (built by
    WP0a; the GUI passes the user config it read at startup, the one it
    gives `Ctx::preview`): MP4 "H.264 + AAC 192 kbps · VA-API on
    renderD129" (the node's basename) when
    `export_plan::export_vaapi(project, config, capabilities)` names a
    node, else "H.264 + AAC 192 kbps · software encode (x264)". That is the
    rule the export encodes by (the effective encoder isn't x264, the node
    is the pinned one, its chain works), so x264 preferred by
    `config.toml` or the project, or a pin on another node, reads x264
    even where VA-API works. (Changed in the WP0a review: the caption was
    keyed on `capabilities.vaapi.ffmpeg_h264` alone and took only the
    capabilities.) WebM "VP9 + Opus 128 kbps · software encode"; WebM with
    AV1 "AV1 + Opus 128 kbps · software encode (libsvtav1)". The AV1
    switch is disabled without `libsvtav1` in the ffmpeg encoders: "This
    ffmpeg has no libsvtav1".
  - Output preview: `naming::export_file_name(name, local now)`.
  - Summary "1920×1080 · 30 fps · 00:04:06:11 · 10 clips".
  - Running, from `on_event::output::export_readout` (WP0a):

    ```rust
    pub struct ExportReadout {
        pub percent: u8,          // 100 · done / total to the nearest (half up, never 100 before the end), done = 0.9·Σseg_done + 0.1·stitch (export_progress_view)
        pub rendered: u64,        // Σ min(seg_done, seg_frames): the frames line
        pub total: u64,
        pub label: String,        // "Rendering clip 6/10" | "Stitching"
        pub elapsed_secs: u64,
        pub eta_secs: Option<u64>,
    }
    pub fn export_readout(job: &Job, now: DateTime<Utc>) -> Option<ExportReadout>;
    ```

    So 3,449 rendered frames of 7,391 read "42%" and "3,449 / 7,391
    frames" (S15b). "Elapsed 00:00:38 · About 00:00:52 left" (no ETA until
    known); "Exporting the timeline as it was at 15:30:12. You can keep
    editing." (`job.started`, local time).
    The label counts the timeline's clips, not the shell's render
    segments: a render unit joins continuous clips (A§9.3; the design
    timeline's c2d0 and c2d1 render as one, 9 units for 10 clips), so the
    export job keeps each segment's clips as their frame counts
    (`JobKind::Export::seg_clips`, from the plan and the snapshot it was
    planned from) and "clip k/N" is the clips its units' frames cover whole
    plus one, N the clips it renders (`on_event::output::rendered_clips`;
    the CLI's progress lines read the same). A job without `seg_clips`
    counts segments. (WP7 integration: the label read "clip 6/9".)
  - Done: "88.4 MB · 1920×1080 · 30 fps · 00:04:06:11 · took 00:01:31";
    buttons Copy path, Open folder, Open file (primary).
  - Failed:
    - lead "ffmpeg stopped while rendering clip 3/10 (take 3, c1b2).
      Nothing was written to exports/.", from the last progress seen and
      the render units of the timeline snapshot taken when the export
      started (runs of continuous clips, A§9.3): the failed unit is the
      error's segment ("segment 3/9 (take-0003, …)"), else the first unit
      the job hadn't finished; the clip is the one of its clips its
      rendered frames reach into (the last job seen, `FailedExport::job`),
      counted among the snapshot's clips the export renders ("3/10", not
      the unit's "3/9"); "joining the clips" for the stitch; else the
      error's message;
    - hint line from a pattern table over the stderr tail: VA-API init
      failure → "VA-API couldn't start on /dev/dri/renderD129. Try
      Settings › Recording › Encoder: x264." (the node from the tail, else
      from `capabilities`); "No space left" → "The disk is full. Free some
      space and try again."; "Unknown encoder" → "This ffmpeg has no
      <encoder>. Run qrec doctor."; otherwise no hint;
    - "Failed at 37%" (the last job seen's percent; `Failed::fraction`
      for the bar); the stderr tail (≤ 30 lines, `details.stderrTail`); the
      log (`details.log`).
  - The core's export-done toast (WP7, `on_event/output.rs`): body
    "<path> · <size>" (`format::size(bytes)`; the path as the job reports
    it, project-relative for auto names, A§1.3) and the actions Open file,
    Open folder (S16b). Copy path stays in the panel's Done state.

### G§4.9 `view::confirm` (WP6) and `view::notices` (WP0a)

`confirm::modal(&Command, &CoreError, &State, &ConfirmFacts) ->
ModalContent { title: "Get out of my swamp?", lead, body, list:
Vec<(String, Option<String>)>, cancel, confirm }`. `ConfirmFacts { files:
BTreeMap<String, SeenFile { display, modified }>, now }` is what the GUI
knows that the core doesn't, by the path `details.files[].path` gives:
`display` = `format::display_path(path, project_dir, home)`, `modified` =
the mtime it stats on a worker (`None` if that failed), and `now`, both
`DateTime<FixedOffset>` on the local clock of their own instant (so a DST
change between them doesn't shift the time shown). Modals that name no
existing file take `ConfirmFacts::default()`. (Changed in the WP0a review:
`modal(&Command, &CoreError, &State)` had no mtime, home or local time for
the overwrite copy, which WP0a now builds.)

| Command | Lead / body / list / button |
| --- | --- |
| `TakePurge` (`details {files, historyEntriesLost, oldestKept}`) | "Purge take 2 and take 4 from disk?" (one: "Purge take 4 from disk?"; ≥ 3: "Purge 3 takes from disk?") / "This permanently deletes 10 files (184.1 MB). It can't be undone, and the 6 history entries that mention these takes are removed too." (the clause is dropped at 0 entries) / files with sizes, each take's layers, then its tracks, then its other files (BRIEF S18's order; the shell lists them by path; WP7 integration) / "Purge 2 takes" or "Purge take 4" |
| `ProjectSet` with an fps change (Settings › Project; `details {clips:[{id,before,after}], removedClips}`) | "Change the canvas to 25 fps?" / "Clip bounds are re-quantized to the new frame rate. These 10 clips move by up to 1 frame:" (+ "c1b6 becomes empty and is removed." per removed clip) / every clip "c7f3 · take 1 · [0, 410) → [0, 342)" (8 visible, then it scrolls) / "Change fps" |
| `Export` or `FrameSave` overwrite (`details {files:[{path,bytes}]}` + `ConfirmFacts`) | "Replace talk-final.mp4?" / "~/Videos/talk-final.mp4 already exists (91.2 MB, modified today at 15:02). Exporting here overwrites it." ("Saving the frame here overwrites it."; another day "modified Oct 14 at 15:02", another year "Oct 14, 2025 at 15:02", `format::local_when`; an unknown size or mtime is left out, and an unknown display the path as given) / — / "Replace file" |
| `HistoryClear` (WP9) | "Clear undo history?" / "This removes 128 undo steps from .qrec/history.jsonl. The timeline stays as it is, but you can't undo anything done before now." / — / "Clear history" |
| anything else | the error message as lead / — / — / "Continue" |

Plus `confirm::quit_while_exporting(percent)` (D-12): title "Quit while
exporting?", lead "The export is 42 % done.", body "Quitting cancels it.
Nothing is written to exports/.", buttons Keep exporting (focused) and
Cancel export and quit (destructive).

Copy this table doesn't give, as WP6 wrote it: the singular purge forms;
removed-clip sentences before the moved list; "This clip moves…" for one
clip; the body of an fps change that moves no clip; the fallback's lead
without "confirmation required: " and without the CLI hint.

`notices::error_notice(&CoreError, &Command, &State) -> Option<Notice>`:
level Error for `io`, `internal`, `capture_failed`, `missing_dependency`;
Warning for the rest; title = the message with its first letter
uppercased; body = the hint. `notices::unavailable_notice(binding_id,
reason, &State) -> Option<Notice>` (D-34): `None` for "Nothing to cancel";
`record.*` during an export → "Can't record while exporting" / "Recording
works again once the export finishes or you cancel it." / Show export;
`palette.open` and `keys.help` before WP9 → "Not built yet" (G§1.1);
otherwise an info toast titled with the reason.

### G§4.10 `view::settings` (WP6): the Settings shell and the Project page (S18b)

- `tabs(&GuiFacts) -> [(SettingsPage, label, enabled)]`: Appearance,
  Recording, Project, Autocut, Keyboard; only Project is enabled until WP9
  (D-48, tooltip "Not built yet").
- `project_page(&State, dir_display: &str) -> ProjectPage`: the "Canvas"
  group and its caption; Size (dropdown: 1280×720 (16:9), 1920×1080
  (16:9), 2560×1440 (16:9), 3840×2160 (16:9), 1080×1920 (9:16),
  1080×1080 (1:1), plus the current size when it isn't listed); Frame
  rate (segmented 24 / 25 / 30 / 50 / 60; none selected for another rate);
  Background (`#RRGGBB` field); the footer "Saved in this project's
  ~/videos/rust-traits-talk/qrec.json." The row captions are taken from
  the pen frame `GPmrg` (the PNG cuts them off). `disabled`: why the rows
  wait (D-43: `view::enabled` of their `ProjectSet`, so while a take
  records or finalizes and while `qrec.json` on disk is invalid), or
  `None`; one reason fits the three rows.
- Commands: Size → `ProjectSet{canvas}` with the new size; Frame rate →
  `ProjectSet{canvas}` with the new fps (with takes →
  `confirmation_required` → the S18b modal over Settings); Background →
  `ProjectSet{background}` on Enter or blur (invalid → C29 invalid "Use
  #RRGGBB").
- As built (WP6): `ProjectPage` has `caption` and `footer_path`; the
  captions are stand-ins in `view::settings` until the pen export
  (`GPmrg`). The tabs are `adw::ToggleGroup` + `gtk::Stack`, not
  `adw::InlineViewSwitcher` (its pages can't be disabled, D-48). The rate
  snaps back to the project's value after a pick (it reads 30 under
  S18b's modal, as drawn); Size and Background show the pick until its
  snapshot, and a refused one shows the project's value again
  (`Store::dispatch_then`). The Size dropdown is tracked as a Menu
  overlay, and the dialog is centered on the window (G§3.15).
- Review fix R27: the rows follow D-43. While `disabled` is set they are
  insensitive with it as their tooltip, and an open Size list closes;
  the page follows every snapshot, not only canvas changes. (They stayed
  enabled while recording: a Size pick the core refused left the
  dropdown on a size the project didn't have, across reopening.)
- Review fix R35: the dialog opens on the project's values
  (`ProjectPage::refresh` before the layer opens): text typed in
  Background and left there, with its "Use #RRGGBB", no longer comes back
  with it.

### G§4.11 Interim and draft view models (WP0a interim, WP8 and WP9 gated)

- `view::blocking::interim(kind) -> BlockingContent { title, lines,
  monospace: Option<String>, buttons }` (WP0a): SPEC's copy for each
  interim page of G§1.1 #2 (X2 "Not my swamp: this folder isn't empty and
  has no qrec.json" + the D-14 line; X3 "qrec.json can't be read" + the
  validation message + `qrec history restore <seq> --yes` from the error's
  hint, or "qrec.json is missing" for the missing-file case; S23 "The
  swamp is missing something" + "Qrec can't start until these are
  installed. Run qrec doctor in a terminal for the same report." + the
  `DoctorReport::to_text` of the failed checks; X1 "A take is being
  recorded from the CLI." + "Take 11 started from the terminal 00:01:12
  ago. Stop it to open the project here. Quit leaves it recording."; the
  D-29 export wait; D-13; not responding; GStreamer stuck).
- After G§0.2 (WP8): S23 rows from the doctor checks (`CheckView { id,
  required, ok, detail, hint }`; item and what-it-does copy by check id
  per BRIEF S23; other ids: the id and its detail; "✓ 14 other checks
  passed"); X1 lines with the live timer; the D-29 lines; X2 and X3 as
  drawn.
- After G§0.2 (WP9): `view::palette` (rows from `keymap::palette` +
  `history.clear`; fuzzy match: subsequence, bonuses for consecutive
  characters and word starts, ties keep the table order, matched indices
  for bold; empty query: "Recent" (the last 3 ids run this session), then
  Record, Playback, Edit, Output, View, Canvas; the short CLI table of
  D-31); `view::cheatsheet` (three columns: Record, Output, Global (niri)
  with "Copy niri snippet" │ Playback, View │ Edit, Canvas; "default P"
  when rebound; search with bold matches; "3 shortcuts match “trim”");
  `view::preferences` (Appearance, Recording, Autocut and Keyboard pages,
  including the conflict banner "1 conflict in ~/.config/qrec/keys.toml",
  "X is bound to both Autocut and Remove clips. Neither runs until one is
  changed.").

---

## G§5 Components and icons

### G§5.1 C01–C34

Metrics are in DESIGN Components and in the pen component sheets (node ids
below; read them with `Get` while `design/qrec.pen` is active).

| Component | Pen base node | Implementation | WP |
| --- | --- | --- | --- |
| C01 Buttons: primary, secondary, flat, destructive, destructive-flat, icon 32/24, sizes 24/32/40, loading | `NYyEU`, `iCGAH`, `XY43M`, `XWCm9`, `dFU0w`, `eLHti`, `J7Biwe` | `gtk::Button` + classes `.qrec-btn.primary/.secondary/.flat/.destructive/.destructive-flat/.icon-32/.icon-24/.sm/.lg` (common.css); hover and pressed = `background-image: image(var(--qrec-hover))` over the fill; `widgets::button::labeled(label, icon, binding)` adds the key cap and the accessible name with the shortcut; `button::icon` (tooltip and accessible name with the key), `button::set_loading` (a 16 px spinner where the icon was), `button::close_window` (the header's close button) | WP0b |
| C01 Record (header only) | `ixaXu` + states | `RecordButton`, fixed 120×32, children switched from `view::header` | WP6 |
| C01 Spinner | `U574U` | `widgets::Spinner` (one 270° arc, 1 turn/s; 16/14/12 px) | WP0b |
| C02 Key cap | `siZBm`, `vBUAG`, `G9zt10`, `CIRyG`, `RpGdo`, `NtDyY`, `TC7rA` | `widgets::KeyCap` (`gtk::Label`, CSS name `keycap`, not focusable or targetable; sm/md/lg, on-fill, disabled, rebound, capture, conflict). CSS `min-width` applies to the content box: subtract padding and border | WP0b |
| C03 Hint bar | `g8llw` | `HintBar` from `view::hints` | WP6 |
| C04 Header | `cnIdP` + variants, `Xba2H` minimal | `adw::HeaderBar` + custom title; Export slot is a `gtk::Stack` (button / progress); `headerbar.recording { box-shadow: inset 0 -2px var(--qrec-record) }` | WP6 (minimal: WP0b) |
| C05 Mode switch | `UUcbL` | `adw::ToggleGroup` (icons + labels) + the Tab key cap | WP6 |
| C06 Badges, REC cluster, Saving | `D4pSZ`, `DlEnG`, `oHR8h`, `KwxRc`, `h5PLfO`, `hyWmc` | boxes in the stage overlay; the torch from `motion::now()` (G§3.14); timer label on the frame clock | WP4 |
| C07 Countdown | `sTIwH`, `HPxd8`, `Q6JiWV` | `CountdownOverlay` (snapshot: `gsk::PathBuilder` arc, `gsk::Stroke` 6 px; Pango digit) | WP4 |
| C08 Selection box, guides, readout | `QW4cD`, `RzfIE`, `qcdxC`, `HxSV4`, `j2VytN`, `Q72wi` | `CanvasOverlay` (snapshot; dashes with `gsk::Stroke::set_dash(&[4.0, 4.0])`) | WP4 |
| C09 Layer row | `zuB2D` + states | `gtk::ListBox` row; `gtk::EditableLabel` that takes no pointer or focus until Rename (a click selects the row; the source line hides while renaming); reorder is a `GestureDrag` inside the list (a lifted ghost row, a 2 px drop line, `Esc` through D-42's drag cancel), not `DragSource`/`DropTarget`; grip, eye and lock revealed by opacity on hover, or when the keyboard reaches them; a focused row wears only its ring | WP5 |
| C10 Audio row | `b5Kxv` + states | composite; M/S `gtk::ToggleButton.track-toggle` | WP5 |
| C11 Meter | `iZb1x` | `widgets::Meter` (snapshot; while shown it looks at its level slot 30 times a second from a timer, not a tick callback, G§8.6, and redraws only when what it shows changed; click resets the latch) | WP5 |
| C12 Inspector | `NZZoN` | grid of `NumericField`; lock `gtk::ToggleButton`; `gtk::Scale`; `adw::ToggleGroup`; 2-up in the Medium layout | WP5 |
| C13 Panel header, take banner, section header | `Osce5`, `KcYOV`, `XwpjR`, `trHYd` | boxes; `widgets::button::menu_button` + `gio::Menu`; panel/autocut page switch = `gtk::Stack` (`panel-pages`): a 200 ms slide, instant under reduced motion (`panel::page_transition` through `Theme::follow_motion`, D-30; WP5 integration audit) | WP5 |
| C14 Timeline toolbar | `AHVNt` + variants | toggle buttons, buttons, `gtk::Separator`, `gtk::Scale` | WP3 |
| C15 Ruler | `I8Owg`, `D5MNZ`, `Gd3pF`, `BbhGo` | drawn by `TimelineCanvas` | WP3 |
| C16 Clip | `L6XX2E` + states | drawn by `TimelineCanvas` (no widget per clip); textures via `append_texture`; cached render node per data/viewport change: the frame clock renders it again only for what it draws moving (`control::TickFacts::canvas_moves`: an easing view, a recording or saving ghost, an analysis, the gap closing, a trim snapping to the playing playhead) and once after, never for the playhead or the snap flash alone (review R10: every frame of a playback) | WP3 |
| C17 Lane headers and segments | `Q32yVJ`, `A1OIK`, `hBnjk` + states | headers are widgets (M/S focusable); segments drawn | WP3 |
| C18 Playhead, caret | `AgTAK`, `pKx7D`, `NhiaL` | `TimelineOverlay`, a separate widget redrawn per frame; the caret also for bin drops (D-38) | WP3 |
| C19 Hatch | `GjISD`, `S6jeR8`, `VJilp`, `pHuYi`, `vuBiA` | `widgets::draw::hatch(snapshot, rect, stripe, edges, edge, scale)`: 45°, 2 px stripe, 6 px pitch across the stripes (`HatchMetrics::step`, 6 · √2 = 8.49 px along x, as the frames draw it; the stripes started every 6 px along x, a 4.2 px pitch, until the WP7 integration audit), 1 px edges, × the UI scale (`HatchMetrics`), anchored to content x; the timeline paints the same metrics in its pre-scaled snapshot (G§4.5) | WP0b |
| C20 List rows, checkbox | `flVDh`, `lYSdk`, `s0cPIL`, `m3mLzK`, `l3NPvv` | `gtk::ListBox` rows `.row-32/40/44/72`; `gtk::CheckButton` styled; R1 on `surface`, R2 on `raised` | WP0b (CSS) |
| C21 Bin row, header, footer | `Se0zW` + states, `kD9zO`, `ZLlMZ` | custom row (`bin_drawer/row.rs`) whose actions show under it while it is the only one selected (a plain box, no `gtk::Revealer`: the frame draws no motion for them); `gtk::DragSource` (D-38); the drawer slides in `bin_drawer/slide.rs` (G§7.2) | WP7 |
| C22 Status chip | `m3Gc2`, `yX8If`, `c9f9HC`, `pn17Q` | `widgets::Chip` | WP0b |
| C23 Toast | `R7g4Dq` + levels, `PaHF5` | `ToastHost` / `ToastView` in `content_layer`; a body that starts with an action's path is `mono-sm` (S16b), others `body-sm`; a "+N" queue chip (not drawn) follows the title; Undo is guarded by the history cursor | WP6 |
| C24 Modal | `x5RrCQ`, `VDIBZ`, `NdrWq`, scrim `Y83G19` | `ModalView` in `content_layer` (scrim to y 968, stacked over Settings); Cancel focused | WP6 (text stub: WP0b) |
| C25 Popover, menu | `b2lnKh`, `S0EgQ`, `KTUMs` | the export panel is in-window (trap or passive layer, D-49); menus are `gtk::PopoverMenu` styled R2 (common.css); every menu's button is `widgets::button::menu_button` (its inner button styled as C01 and, like the MenuButton, named for screen readers; a test greps for others) | WP0b, WP7 |
| C26 Slider | `Bn6sz` + states | `gtk::Scale` styled; ticks with `add_mark`; the value tooltip while dragging (`widgets::scale_tooltip`, which also gives the range's own wheel only to a slider the keyboard is on, not one a press focused: R42 and its audit); S12's end labels are their own row flush with the track's ends (GTK centers mark labels) and the ticks sit at the trough's ends (S12 insets them by the thumb's radius, ~2 px) | WP0b (CSS), WP5 |
| C27 Switch | `z9laF`, `p3tmIy` | `gtk::Switch` styled | WP0b |
| C28 Segmented | `NZKuJ`, `L46uU`, `jmq6y` | `adw::ToggleGroup` styled; the Settings switcher too (`adw::ToggleGroup` + `gtk::Stack`: `adw::InlineViewSwitcher`'s pages can't be disabled, D-48; WP6) | WP0b, WP6 |
| C29 Fields | `j1hIDi`, `oc5uM`, `vpmJy`, `EfoCl`, `oE7jW` | `gtk::Entry`; `NumericField` (Box `.field` + `gtk::Text` right-aligned + unit label; arrows and scroll step; no ± buttons); `gtk::DropDown`; `gtk::SearchEntry` | WP0b (CSS), WP5 |
| C30 Mud bar, export progress | `S5YPm4`, `T2C2U5`, `J6HyOH`, `TKIqZ`, `c0pJo3`, `cUfiU` | `widgets::MudBar` (snapshot from `motion::now()` over the GTK-free `MudSim`, seeded PRNG; frame A, the clock's 0, holds the frames' own bubbles: `RUNNING_FRAME_A`, S15b's and the S15c board's three, and the done page's `DONE_FRAME_A`, S16's two; the 3 px sediment band counted from the bar's outer bottom, 2 px of it above the border under 16 px of mud; a tick callback only while it animates: running, motion allowed, the clock moving) | WP7 |
| C31 Empty states | `HVO2S`, `p3Dpn`, `uBSb8` | timeline (WP3), panel (WP5), blocking: interim pages (WP0b), drawn pages (WP8) | WP3, WP5, WP0b, WP8 |
| C32 Theme card | `qqnCi` + themes | `ThemeCard` drawn from that theme's `Palette` | WP9 |
| C33 Tooltip | `AbHWk`, `T7Ws1`, `m1fwku` | `widgets::tooltip::{text, keys}` via `query-tooltip` + `set_custom`; the clip tooltip is WP3 | WP0b |
| C34 Palette row | `G4Ecf`, `J1uAai`, `OE0J8`, `ROIMl` | `gtk::ListView` row; Pango markup for matches | WP9 |

Every icon-only button gets an accessible label naming the action and its
keys ("Settings, Ctrl+Comma"); each WP checks its own with the a11y walk
(G§8.4).

### G§5.2 Icons

Names checked against adwaita-icon-theme 50.0 (pinned nixpkgs).

| Purpose (Lucide in pen) | GTK icon |
| --- | --- |
| Record / Stop | custom `qrec-record-symbolic` / `media-playback-stop-symbolic` |
| play / pause | `media-playback-start-symbolic` / `media-playback-pause-symbolic` |
| chevron-left / chevron-right (frame) | `go-previous-symbolic` / `go-next-symbolic` |
| skip-back / skip-forward (edit) | `media-skip-backward-symbolic` / `media-skip-forward-symbolic` |
| camera (Save frame) | `camera-photo-symbolic` |
| download (Export) | `document-save-symbolic` |
| search / settings | `edit-find-symbolic` / `applications-system-symbolic` (the drawn gear; Adwaita 50 draws `preferences-system-symbolic` as tools; WP6) |
| video / film (Live / Program) | `camera-video-symbolic` / `video-x-generic-symbolic` |
| eye / eye-off | `view-reveal-symbolic` / `view-conceal-symbolic` |
| lock / lock-open | `changes-prevent-symbolic` / `changes-allow-symbolic` |
| grip-vertical / plus | `list-drag-handle-symbolic` / `list-add-symbolic` |
| monitor / webcam / mic / volume-2 | `video-display-symbolic` / `camera-web-symbolic` / `audio-input-microphone-symbolic` / `audio-speakers-symbolic` |
| archive (Bin) / scissors / sliders-horizontal | `package-x-generic-symbolic` / `edit-cut-symbolic` / `preferences-other-symbolic` |
| zoom-in / zoom-out / arrow-left-right (Fit) | `zoom-in-symbolic` / `zoom-out-symbolic` / `zoom-fit-best-symbolic` |
| trash-2 / folder-open / folder / copy / external-link | `user-trash-symbolic` / `folder-open-symbolic` / `folder-symbolic` / `edit-copy-symbolic` / `adw-external-link-symbolic` |
| info / triangle-alert / circle-x | `dialog-information-symbolic` / `dialog-warning-symbolic` / `dialog-error-symbolic` |
| check / chevron-down / ellipsis-vertical / x | `object-select-symbolic` / `pan-down-symbolic` / `view-more-symbolic` / `window-close-symbolic` |
| link-2 (aspect lock) / pencil / rotate-ccw | `insert-link-symbolic` / `document-edit-symbolic` / `edit-undo-symbolic` |
| arrow-left (ripple tag) | `go-previous-symbolic` |

The transport, badge, first-run (WP4), toolbar (WP3) and panel (WP5) icons
are these Adwaita symbolics, not the frames' Lucide outlines, until the
pen export; so are the Program segment's film (`video-x-generic-symbolic`
draws a camera) and the info toast (`dialog-information-symbolic` draws a
bulb) (WP6).

Custom symbolic icons (WP0b, 16 px grid, fills only, ISC notice in
`resources/icons/LICENSE-lucide.txt` where the geometry follows Lucide):
`qrec-record`, `qrec-success` (circle-check), `qrec-magnet`,
`qrec-monitor-x`, `qrec-package-x`, `qrec-life-buoy`, `qrec-timer-off`,
`qrec-gauge`, `qrec-layers` (also the narrow-mode header button),
`qrec-audio-lines`. Cursors: GDK names `grab`, `grabbing`, `col-resize`,
`move`, `nwse-resize`, `nesw-resize`, `ew-resize`, `ns-resize`,
`not-allowed`, `copy` (bin drag over the timeline).

---

## G§6 Dependencies and Nix

### G§6.1 Cargo (G§0.1)

A§1.2: agents never add dependencies without the owner's approval. This
list is that request; WP0b adds it once approved.

```toml
# [workspace.dependencies]
gtk = { package = "gtk4", version = "0.11.5", features = ["v4_22"] }
adw = { package = "libadwaita", version = "0.9.2", features = ["v1_9"] }
pango = { version = "0.22.9", features = ["v1_56"] }   # FontMap::add_font_file (bundled fonts)
```

| Crate | Dependencies |
| --- | --- |
| `qrec-gui` | qrec-core, qrec-shell (default features, `gst`), gtk, adw, pango, gst (workspace 0.25.4), crossbeam-channel, serde_json, chrono (`clock`), tracing; dev: qrec-core (`testkit`), qrec-shell (`testkit`), insta, tempfile |
| `qrec` | + qrec-gui (the harness in `crates/qrec/tests/gui_*` uses the existing dev-dependencies) |

- Built against GTK 4.22.4, libadwaita 1.9.3, Pango 1.57.1, GStreamer
  1.26.11 (the dev env). `gtk/v4_24` and `adw/v1_10` don't build here
  *(spike)*.
- glib and gio stay 0.22.10 and gstreamer 0.25.4; about 21 new packages
  *(spike)*.
- No `gst-plugin-gtk4` crate: it uses gstreamer 0.24 and gtk4 0.10, a
  second glib. The system plugin (gst-plugins-rs 0.14.4) links the same
  GTK as the app.
- No `glib-build-tools`: `build.rs` runs `glib-compile-resources` itself.
- No `async-channel`: GTK delivery uses `MainContext::invoke` (G§3.5).

### G§6.2 MSRV and checks

- `rust-version = "1.92"`: gtk4 0.11.5, glib 0.22.10 and gstreamer 0.25.4
  already require it (the dev env has 1.95). A§1.2's flock note still
  holds (std's `File::try_lock` is 1.89, but the code keeps libc `flock`).
- The A§1.2 "one glib" check becomes `cargo tree -i glib --depth 0`. It
  errors when two glib versions exist; `cargo tree -d -i glib` already
  lists smallvec twice today *(spike)*.
- CI: `cargo build`; `cargo test --workspace`; `cargo clippy --workspace
  --all-targets -- -D warnings`; `cargo check -p qrec-core` alone; the
  nested-compositor job runs the ignored GUI tests (G§8.4, G§8.5).

### G§6.3 devShell (`flake.nix`, WP0b)

```nix
# fonts bundled with the GUI (SPEC §15.3)
qrecFonts = pkgs: pkgs.runCommand "qrec-fonts" { } ''
  mkdir -p $out/share/qrec/fonts $out/share/licenses/qrec-fonts
  cp '${pkgs.fraunces}/share/fonts/truetype/Fraunces[SOFT,WONK,opsz,wght].ttf' \
     '${pkgs.atkinson-hyperlegible-next}/share/fonts/truetype/AtkinsonHyperlegibleNext[wght].ttf' \
     '${pkgs.jetbrains-mono}/share/fonts/truetype/JetBrainsMono[wght].ttf' \
     $out/share/qrec/fonts/
  cp ${./crates/qrec-gui/resources/fonts}/OFL-*.txt $out/share/licenses/qrec-fonts/
'';

devShells.default = pkgs.mkShell {
  nativeBuildInputs = [ /* existing */ ] ++ (with pkgs; [ sway grim wtype ]); # nested compositor, capture, keys
  buildInputs = (linkLibs pkgs) ++ (runtimeTools pkgs)
    ++ (with pkgs; [ adwaita-icon-theme hicolor-icon-theme gsettings-desktop-schemas librsvg ]);
  shellHook = filterGstPluginPath + ''
    # (existing lines)
    export QREC_FONT_DIR=${qrecFonts pkgs}/share/qrec/fonts
    # GSettings schemas and icon themes, as wrapGAppsHook4 does for the package
    export XDG_DATA_DIRS="''${GSETTINGS_SCHEMAS_PATH:-}:${pkgs.adwaita-icon-theme}/share:${pkgs.hicolor-icon-theme}/share:''${XDG_DATA_DIRS:-}"
  '';
};
```

- Font file names are verified in the pinned store (fraunces 1.000,
  atkinson-hyperlegible-next 2.001-unstable-2025-02-21, jetbrains-mono
  2.304). The OFL-1.1 texts with each family's copyright line are vendored
  in `crates/qrec-gui/resources/fonts/` (the packages don't install them).
- `dbus` (for the tests' private `dbus-daemon`) and `glib` (for
  `glib-compile-resources`) are already in the shell.
- sway 1.12 and grim 1.5.0 from the pinned nixpkgs worked headless
  *(spike)*; `librsvg` gives `rsvg-convert` (art PNGs, the G§3.14
  fallback).

### G§6.4 Package

- `nativeBuildInputs += [ glib librsvg ]` (`glib-compile-resources`;
  `rsvg-convert` for the icon PNGs).
- `buildInputs += [ adwaita-icon-theme hicolor-icon-theme
  gsettings-desktop-schemas librsvg ]`: wrapGAppsHook4 puts their share
  dirs and schemas on `XDG_DATA_DIRS`.
- `postInstall` (WP0b): the fonts to `$out/share/qrec/fonts`, the licenses
  to `$out/share/licenses/qrec-fonts`.
- `postInstall` (WP10): `share/applications/com.rsensato.qrec.desktop`;
  icons in `share/icons/hicolor/{scalable,128x128,64x64,32x32}/apps/com.rsensato.qrec.*`
  and `share/icons/hicolor/symbolic/apps/com.rsensato.qrec-symbolic.svg`.
- `cargoTestFlags` unchanged (core tests only in the sandbox).

---

## G§7 Work packages

### G§7.0 Process

- **Branches.** Each WP is a git worktree `.claude/worktrees/gui-<wp>`
  off the integration branch `worktree-qrec-v1`, created once its hard
  dependencies merged (G§7.1). It rebases on the integration branch
  before merging; the integrator merges and verifies one WP at a time.
  WP0a starts from the branch at or after `499df02` (today's head) with
  nothing uncommitted.
- **Ownership.** A WP edits only the files G§3.16 and G§3.17 give it,
  plus its own tests, snapshots and `crates/qrec/tests/gui_<area>*`.
  WP0a and WP0b create every file with its `mod` line, final public
  signatures and a stub body, so no later WP edits a module list or
  another WP's file.
  - Shared files — the `Cargo.toml`s, `store/local.rs`, `view/mod.rs`,
    `capture/mod.rs` types, `plans.rs`, `state.rs`, `protocol/*`, the
    `lib.rs` and `mod.rs` module lists, `window/mod.rs` — change only
    through the integrator: the WP asks, the integrator lands the change
    on the integration branch, and every active WP rebases. One exception:
    WP1 sets `PREVIEW_SINK_SUPPORTED`.
  - Per-screen test states live in each owner's own test module, never in
    the shared `testkit.rs`.
- **Interfaces** are frozen by WP0a (GTK-free) and WP0b (GTK side): the
  signatures in G§3 and G§4. A WP adds private items freely, public items
  only in files it owns.
- **Done** for a WP means: `cargo fmt --all`; `cargo clippy --workspace
  --all-targets -- -D warnings`; its tests; its screenshot cases shot
  against whatever has merged ("stub-based": regions of WPs not merged yet
  show WP0b's stubs), with the objective checks (regions, hints, a11y,
  G§8.4) passing for its own regions; the images in `tmp/gui-shots/<wp>/`;
  and one line per departure from a frame in the merge commit.
- **Final re-shoot.** After each merge the integrator re-runs the cases
  whose final set just completed (G§7.2).
- **Disk.** Measured on 2026-10-15: 17 GB free; the integration worktree's
  `target/` is 5.1 GB; a GTK build adds about 1–1.5 GB. Hence two
  compiling WP worktrees at most (G§7.1).
  - Each worktree keeps its own `target/` (never a shared
    `CARGO_TARGET_DIR`), debug only, `CARGO_INCREMENTAL=0`.
  - Build and test only the crates touched (`cargo test -p qrec-core
    view::timeline`, `cargo test -p qrec-gui --lib`, `cargo test -p qrec
    --test gui_timeline -- --ignored`).
  - `cargo clean` and remove the worktree after the merge; delete
    `tmp/gui-spike/target` once WP0b copied the scripts.
  - Before starting a lane, `df` must show 7 GB per compiling worktree
    plus 4 GB spare; otherwise run one lane.

### G§7.1 Dependency graph and lanes

| WP | Hard: merged before it starts | Soft: final look; stubs until then |
| --- | --- | --- |
| WP0a | — | — |
| WP0b | WP0a, G§0.1 | — |
| WP1 | WP0a | — |
| WP2 | WP0a (core and shell parts), WP0b (GTK caches) | — |
| WP3 | WP0b | WP1 (Program picture), WP2 (thumbnails, waveforms), WP6 (header, hint bar) |
| WP4 | WP0b | WP1 (sink, player, `LiveStatus`, meters, `plan_ges_take`), WP2, WP6 |
| WP5 | WP0b | WP1 (Program meters, `LiveStatus` errors), WP3 (S12 hatches), WP6 |
| WP6 | WP0b | WP7 (the S16b and S18 bases) |
| WP7 | WP0b, WP4 (player sync for D-17) | WP2 (thumbnails), WP3 (drop target), WP6 (modal, toast) |
| WP8 | WP0b, G§0.2 | — |
| WP9 | WP6, G§0.2 | — |
| WP10 | WP1–WP7; WP8 and WP9 once approved | — |

Two lanes, one compiling worktree each:

| Step | Lane A (media, stage, output) | Lane B (chrome, timeline, panel) |
| --- | --- | --- |
| 1 | WP0a (alone) | — |
| 2 | WP1 | WP0b (once G§0.1 is answered) |
| 3 | WP2 | WP6 |
| 4 | WP4 | WP3 |
| 5 | WP7 | WP5 |
| 6 | WP8 (gated) | WP9 (gated) |
| 7 | WP10 (one lane) | |

- The order meets every hard dependency whichever lane finishes first:
  WP2's GTK caches and WP4 need WP0b (lane B, step 2); WP7 needs WP4 (same
  lane, before it); WP9 needs WP6. If WP0b is late (G§0.1), lane A merges
  WP2's core and shell parts first and its GTK caches in a second merge.
- **M4 review** once WP1–WP7 merged and every slice case passed its final
  re-shoot (G§7.2): the screenshots, the 1300- and 1000-wide shots, the
  smoke run and G§8.7 go to the PO. **M5** after WP8, WP9 and WP10.
- **With more disk** the same WPs run wider (7 GB per lane plus 4 GB
  spare: ≥ 32 GB free for four lanes, ≥ 46 GB for six): WP1 and WP0b after
  WP0a; then WP2, WP3, WP4 and WP6 together, plus WP8 and WP9 if G§0.2 was
  answered; then WP5 and WP7; then WP10. Same dependencies.

### G§7.2 Final re-shoot gate

| Cases (G§8.4) | First shot by | Final once merged |
| --- | --- | --- |
| S01a | WP4 | WP3, WP4, WP5, WP6 |
| S01b, S02, S03, S04, S04b, S05, S06, S13 | WP4 | WP1–WP6 |
| S05b, S07, S08, S09, S10, S11 | WP3 | WP1–WP6 |
| S12, S13b | WP5 | WP1–WP6 |
| S18b | WP6 | WP1–WP6 |
| S14, S15a, S15b, S15c, S16, S17, S18 | WP7 | WP1–WP7 |
| S16b | WP6 | WP1–WP7 (WP7 changes its copy) |
| widths 1300 and 1000, height 800, scale 1.5 and 2, reduced motion | each region's WP | WP1–WP7 |

For each case whose final set just completed, the integrator runs `cargo
test -p qrec --test gui_<area> -- --ignored --test-threads=1`, lists the
cases in the merge commit ("final: S02 S06 …") and sends a failure back to
the case's first-shot owner.

Status after round 3 (WP0a, WP0b, WP1, WP2, WP3, WP4 and WP6 merged): no
case was final; the first shots of WP3, WP4 and WP6 were re-shot at each
round 3 merge (`tmp/gui-shots/{wp3,wp4,wp6}/`).

Status after the WP5 merge (f84ad6d) and its integration: **final: S01a
S01b S02 S03 S04 S04b S05 S05b S06 S07 S08 S09 S10 S11 S12 S13 S13b
S18b**, every suite green on pixman and gles2. Re-run after the WP5
integration audit's fixes: the workspace 1620 passed, 0 failed, 82
ignored; nested, on pixman and on gles2 alike, gui_shell 21, gui_smoke 1,
gui_chrome 11, gui_timeline 14, gui_stage 17, gui_panel 9 (gles2's warm
frame-time p99 16.51–16.61 ms in 6 runs). Each shot (pixman) is next
to its frame in `tmp/gui-shots/final/<case>-compare.png` (S13b: the row
crops stacked next to the state board). Still to come with WP7: S14, S15a,
S15b, S15c, S16, S16b, S17, S18, and the widths, heights, scales and
reduced motion (WP1–WP7). Visible departures of the final shots:

- Everywhere, by design (G§8.4's known differences): the fake sources'
  names and test patterns, the fake tone's levels and block waveforms in
  the cases that don't pin the meters (`QREC_GUI_TEST_LEVELS` pins only the
  panel's), real elapsed times, the test project's paths, no pointer in the
  shots; the icons are Adwaita stand-ins (G§5.2) and the empty-state art a
  placeholder until the pen export (G§0.6).
- Everywhere: mono text is the bundled JetBrains Mono, but each glyph
  advances a whole pixel (7 px at 11 and 12 px, 8 px at 13 px, where the
  frames advance 6.6, 7.2 and 7.8 px), so a mono string drifts 0.2–0.4 px
  per character: the ruler's labels are 4 px wider than the frames', the
  transport's "00:02:41:12 / 00:04:06:11" 4–5 px wider, S12's summary
  lines ("00:01:43:15 → 00:01:37:09") 6 px narrower, S06's clip tooltip
  12 px narrower; only a short field like "1500 px" (7 characters, the
  one the integration compared) is within a pixel (measured on S02, S06
  and S12 in the WP5 integration audit). With the stand-in icons' widths,
  the timeline toolbar's right group (the clip count, the zoom slider,
  Fit) sits up to 13 px left of the frames'. Kept by WP10's text-rendering
  experiment (G§3.14: unhinted metrics would bring every mono text within
  a pixel of the frames, 21 of 21 measured where Qrec's setup has 4, and
  bring back the header title's stray mark on GL); measured again there:
  the ruler's labels +4, the ruler corner's "00:04:06:11" +2, S06's
  transport +4.8, its tooltip's second line −9.5, S12's summary
  timecodes −2 each, S14's take lines −2, S16's file path −8.8 and S17's
  stderr lines −5…−10 (51 characters: −10).
- S01a: (the inspector showed in the empty scene: fixed, hidden as BRIEF
  says). S01b: the first run names its layers and tracks after their
  sources ("Camera", "Screen FAKE-1", "Built-in Audio Analog Stereo", A2 at
  0.0 dB) where the frame reuses the fixture's (Face, Main screen, System
  audio at −6 dB).
- S02, S03, S04b, S06, S07, S11, S13, S18b (every fit shot of the 10-clip
  timeline): take 1's second clip reads "1" where the frame reads "Take 1"
  (G§1.3's fit inset: c8a1 is 63.8 px wide, the frames' 64.3, G§4.5; the
  integration listed S02 and S11 only).
- S04, S04b, S05: the timers read the real elapsed time
  (`00:02:41:27` for `00:02:41:12`).
- S04, S04b, S05: the ghost's lanes are silent but for their newest
  columns, where the frames draw the take's waveform: the fake clock
  starts the take at 2:41 (0:31 in S04b) while the level ring holds only
  the readings since it started, and a column older than the readings is
  silent (review: it drew a flat band at the oldest reading's level, which
  passed for a waveform).
- S04, S05, S05b and S12: the ruler drew a 10 px tick every 30 s (the
  recording fit) and every 5 s with 1 s ticks between (S12's 10 px/s),
  where the frames tick only at their labels with 5 px ticks every 10 s
  (5 s in S12): fixed in the WP5 integration audit (`ruler::tick_steps`,
  G§4.5's table), and these shots re-taken.
- S06, S09, S10, S11: "+ Add layer", "+ Add audio track" and the header's
  `+` are disabled in a take layout (D-07); the frames draw them enabled.
- S07, S08: Program with no clip selected is read-only (D-06): the panel's
  rows, audio controls and adds are disabled where the frames draw them
  enabled.
- S12: the toolbar's settings icon is `preferences-other-symbolic` (the
  frame's sliders); a knob's center travels to within 2–3 px of its
  trough's ends where the frame's stops 7–8 px short of them, so Min
  silence, Padding and Min keep sit 3.5–5 px left of the frame's
  (Threshold, at the middle, matches); the summary heading is 11 px wider
  (the gap before "(6.2 s)" 4 px, its mono 5 px). The preview is as drawn:
  each region's hatch, edges and tag, and the dimmed rows, within 1–2 px.
- S13: the frame shows the drag and L1's hover at once; the drag shot has
  no hover (L1's hover is its own shot, `s13-layer-editing`).
- S13b: a dragged row's copy follows the pointer over the rows, with the
  drop line at the slot (the frame lifts the row above the line and moves
  the rows under it), and the row left behind keeps its hover reveal at
  0.4; the rename field opens with the name selected; the focus ring is
  shot on L1 ("Main screen"), the board's on a "Face" row.
- S18b: the Settings pages not built yet read disabled (D-48).

Status after the WP7 merge (e3b30a0) and its integration: **final: S14
S15a S15b S15c S16 S16b S17 S18**, the scale runs (S02, S06, S09 and S14
at 2 and 1.5, and S10's trim at both), the reduced-motion runs (S03, S04,
S15c, the panel's pages) and the adaptive runs (S02 at 1300×1000 and at
1000×1000 with the sheet, S06 at 1600×800): every case and run of the
table is final. Re-run after the integration's final shot and code audits
(deea7bd…3a36df5), on a clean build of the workspace's crates: the
workspace 1660 passed, 0 failed, 105 ignored; nested, on pixman and on
gles2 alike, gui_shell 21, gui_smoke 1, gui_chrome 11, gui_timeline 17,
gui_stage 23, gui_panel 9, gui_bin_export 14 (the stage, timeline, panel
and bin suites with no GLib or GTK critical); gles2's warm frame-time p99
16.60 ms (max 16.67). Each shot is next to its frame in
`tmp/gui-shots/final/<case>-compare.png` (the scale
runs at their device size with the frame scaled up, `<case>-scale2`,
`-scale1.5`; S15c's two panels next to its 1040×769 board; S03 and S04's
reduced-motion shots next to the animated frames); the adaptive shots are
there alone (`S02-1300`, `S02-1000-closed`, `S02-1000-sheet`, `S06-800`:
no frames, G§0.3); the gles2 run's in `final/gles2/`. Visible departures of
these shots (on top of the "everywhere" items above), measured against the
frames at full size in the final audit:

- Everywhere: the fixture's files are small, so every size is the
  fixture's ("5 takes · 31.4 MB", "10 files (17.0 MB)", "1.6 MB").
- Everywhere (the mono item above, for the UI font too): Atkinson's glyphs
  advance whole pixels as well, so a UI line runs a few px off the frame's
  width, wider or narrower with its size and letters (S16's "Layers! Like
  an onion. Your video is ready." 264 px for 254, S17's first lead line 405
  for 395, S14's "Recovered after Qrec closed mid-take." 195 for 200), and
  the mono text renders a little lighter than the frames' (the UI text
  doesn't: WP10's measure below; this item read "12–13 px text" until
  then). Text-sized controls follow it: S15a's Draft and High
  segments are 2 and 1 px narrower (55 and 54 for 57 and 55), the header
  slot's mini bar 2 px left of the frame's (its "42%" 20 px of ink for
  22), S16b's toast narrower (below). Kept by WP10's text-rendering
  experiment (G§3.14), which measured 48 UI texts of S02, S06, S12, S14,
  S16 and S17: 30 within a pixel of the frames (unhinted metrics: 46,
  the other two style), the rest −5.2…+9.8 (S16's body +9.8, S17's lead
  +9.3, the header title "rust-traits-talk" +3.5, S12's "Removes 4
  silences" +2.3, take 7's line −4.8, S12's subtitle −2.7); the ink's
  weight is the frames' for the UI font (100 % on average, every setup)
  and lighter for the mono one (93 %). Its crops (8×, and the scale-2
  shots at 4×) and numbers are in `tmp/text-rendering/` for the PO.
- Every timeline shot: the toolbar's left group sits right of the frames':
  the Snap toggle is 102 px for 100 and its contents 2–4 px right, the
  divider at x 224 for 217 (8 px each side of it where the frames leave 4
  and 5), Autocut's icon at 241 for 231 and its label at 265 for 252, the
  settings icon at 360 for 345 (WP3's toolbar spacing; with the stand-in
  icons' widths, the right group sits up to 13 px left, as above).
- S02, S03, S04b, S06, S07, S11, S13, S14, S15a, S15b, S17, S18, S18b
  (every fit shot of the 10-clip timeline): take 1's second clip reads
  "1" where the frame reads "Take 1" (G§1.3's fit inset: c8a1 is 63.8 px
  wide, the frames' 64.3, G§4.5).
- S14: the thumbnails are the fake test pattern's (take 6's from its
  first frame: 7 frames have no strip image); take 9's broken thumbnail
  carries `dialog-error-symbolic` and the bin's close Adwaita's smaller
  `window-close-symbolic` (8×8 where the frame's x is 12×12; G§5.2's
  stand-ins for circle-x and x); take 2 reads "Removed from the
  timeline." where the frame reads "Discarded with D." (the fixture writes
  no history, G§8.4).
- S15a: "software encode (x264)" where the frame names VA-API on
  renderD129 (the fake backend probes no VA device, G§8.4).
- S15b and S15c's running page: the file and "as it was at" carry the
  export's real start (the setup's preview is the test-mode now, 15:30:12;
  the job is named when it starts); elapsed and left are real; the shot is
  taken once `qrec status` has the job at 42 % or more, and it lands at
  42–43 % (43–44 % on gles2), where several units render at once, so the
  label is further on ("Rendering clip 8/10", the frame's 6/10 at 42 %).
- S16, S16b: the Smoke fixture's export (3 clips of 30 frames: 1.6 MB,
  00:00:03:00, took 00:00:01, its own name), in the Smoke project: the
  header's "3 clips · 00:00:03:00", its 3-clip timeline with its middle clip
  c1b2 (take 3) selected and the playhead in it at 00:00:01:15 (the frame
  selects take 5's c2d0 at 00:01:21:07), so the panel shows take 3's layout,
  and the bin chip reads 6 (the takes its timeline doesn't use; the frame's
  5). S16b shows that whole window, and its toast is 467 px wide where the
  frame's is 500: it sizes to its text (DESIGN's 360–560), 12 px from the
  body's end to the close button where the frame leaves 28.
- S17: it fails at 11 % (units 1 and 2 rendered, 867 of 7,391 frames;
  the frame's 37 %); the status icon is `dialog-error-symbolic` (G§5.2's
  stand-in for circle-x); Open full log names the failed segment's log
  (`.qrec/logs/<stamp>-<pid>-j1-seg2.log`, ellipsized at its start) where
  the frame names `export-20261014-153012.log`; the frame's first stderr
  line looks faded (not built: a pen-file need). The box shows BRIEF's
  last ten lines as drawn (the wrapper's render-node line comes first).
- S18: the modal has no history clause (the frame's "…, and the 6
  history entries that mention these takes are removed too.": the fixture
  has no history), so it is 20 px shorter (y 287–713 where the frame's is
  277–723).
- Fixed on the way (WP7 integration): the core's "Export failed" toast
  showed over S17's panel, which already shows the failure (dropped now,
  like the done toast under S16's Done page, G§3.7); S18's loss list read
  in path order (A1, A2, L1, L2, recorder.log per take) where BRIEF and
  the frame list the layers, then the tracks, then the log (G§4.9).
- Fixed in the final shot audit, measured (each with a check, WP7's "As
  merged"): every hatch is 6 px apart across its stripes (8.49 along x, as
  drawn; it was 6 along x: S04b, S05, S08, S12, S14); S16 and S16b read one
  playhead; the hint bar leaves out the items that don't fit whole; the S06
  scale runs have c2d1's tooltip; the export panel's content starts 16 px
  in and its pages are the frames' heights (S15a 355, S15c's WebM 405, S15b
  269, S16 216, S17 472), every text block within 1 px of the frames' (its
  segments 26 px, "42%" on the frames line, the AV1 row, "Output", S17's
  hint and stderr); the header slot's "42%" is `mono-sm`; the mud bar's
  frame A is the frames' bubbles, its sediment 2 px; the links' underlines
  sit under the descenders and the bin's link is 12 px; the bin's actions
  block is 78 px and S18's footer label 13/600; S16b's toast actions start
  at its text's x and its body sits 2 px under the title.
- S04, S04b, S05 (and S04's reduced-motion run): the ghost's lanes carry
  the live waveform only where the level ring has readings, the seconds
  since the window opened, at the ghost's right edge, and are silent to
  their left (the fake hook's record offset starts take 10 before any
  reading: 2:41 back, S04b's 31 s), where the frames' takes have levels
  all along (review R01: a column older than the readings drew the oldest
  one, so the fake tone's block waveform ran across the whole ghost).
- Scale runs: the departures of their cases at 1, nothing more at the
  device size (at 1.5 the window draws at its whole factor, 2, and sway
  scales the buffer down, so every 1 px rule is 1.5 px smeared over two
  device px: fractional-scale rendering is WP10's audit).
- Reduced motion: S04's REC badge has no torch glow (DESIGN's reduced
  motion; the frames are the animated look at frame A); S15c's bar is flat
  with no bubbles, as the board's. S03 shows nothing of it: test mode holds
  the countdown at its end (the digit at scale 1, a full ring) with motion
  reduced or not, so S03-reduced is S03's picture but for its live pixels
  (pixel for pixel on pixman; on gles2 the fake camera's moving dot and
  the unpinned A2 meter, 144 px; it was offered as the static ring's
  evidence until the final audit).
- Adaptive (DESIGN Layout, G§3.15; no frames, so checked against the
  rules): S02 at 1300×1000 is Medium: the panel docked at 320 (its source
  lines ellipsize, "GIGA-BYT…"; with no layer selected the inspector shows
  its empty line, so its 2-up fields aren't in the shot), the stage 980
  wide with the preview fitted at 948×534 and the transport under it, the
  timeline 980 wide (the ruler's last label cut at its edge); the header
  and hint bar keep every item. At 1000×1000 it is Narrow: no docked panel,
  the stage and the timeline full width (the preview at 960×540), the
  header's layers button left of Record, and the hint bar leaves out its
  last item ("? All keys") whole; the button opens the sheet over the
  right 384 px from the header to the hint bar, the content under it
  dimmed and the button pressed, the hint bar naming the sheet's keys
  only, `[Tab] Move focus · [Esc] Close` (review R33: it named the
  editor's, which the sheet's keyboard trap swallowed), and `Esc` closes
  it. S06 at 1600×800 is compact: the timeline 232 px (toolbar 36, ruler
  28, video 72, lanes 48), the stage 488 with the preview fitted at
  746×420 and the transport under it, the panel 720 high; nothing else
  moves.

Review of round 3, WP5 and WP7 (on 2101517): 84 confirmed findings (2
high, 33 medium, 49 low) in 70 groups, fixed in five lanes, merged in
this order: playback and stage (29edeeb), timeline (fde6481), panel
(12d9ae7), chrome and keys (9733525), bin and export (4942fb2); the
integration (57d65df) gave one home each to three rules two lanes had
each written (the export panel's, the first run's, a dropdown list's Menu
overlay). **Fixed: 64 groups** (74 findings), each with its tests (the
high one, R00+R16: a bin preview is never the program playing,
`LocalUi.program_playing`). **Already fixed: 6** (10 findings: R13,
R51+R62+R71, R54, R60+R72, R61+R81, R64, by f2bbc25, 05b8c31, c156e44,
e3b30a0, 86f63e8 and 88fbfe8). **Not a defect: none** (in part: R10's
corner label, which GTK doesn't set again unchanged; R32+R41's symptoms,
which GTK 4.22 doesn't show, though the open list wasn't a Menu overlay:
fixed). **Left: none** (R65's sources and ticks outside the timeline
moved to `guard::Slot` in the audit's follow-ups).

Audit of the fixes (on 02c31c7): three auditors reverted every fix and
ran its tests. 55 of the 70 groups held (every "already fixed" and "not
a defect" claim confirmed). **Incomplete: 2**: R65 (raw ids kept in
14 owners: a meter or the export clock stayed dead after a caught panic,
and their next stop panicked again) and R42 (GTK 4.22 focuses a pressed
slider, so after a drag the wheel moved it and saved it). **A test that
passed without its fix: 12**: R01, R05, R11 (the timeline's own call into
the helper could go), R69 (gui_stage's half), R22 (the router's wiring),
R23 (the stage's), R24 (the probe's), R25 (the GTK half), R44 (the
focused slider), R29 (R30 hid the coalescing), R37+R48 (the take-back),
R57 (S18's footer). **A residual: 1**: R77 (a take recorded with a track
the project scene lost: the dropdown named the project's). **A wrong
claim: 1**: G§3.6 (only the timeline's debounce went out before the
close: the playing program's playhead and a nudge burst were lost).
Followed up in 8a7f817…a4023af (17 commits: one per fix, R05 and R11
together): every one fixed, and each test checked to fail with its fix
reverted. Two follow-ups found more
and fixed it: R25's focus restore missed a window in the background
(`has_focus`), and R44's keys come within 3 s of the last arrow (GTK hides
the focus ring then, and the router gives the arrows to the timeline).
Minor gaps closed too: R79 (no cap without a record key), R53 (Copy
image, end to end; its relative path stays a unit test's: every producer
names an absolute one). Left, minor: R27's refused-pick resync (tested
through `Store::dispatch_then` only), R00+R16's `player_idle` input (the
glue's), R20 and R45 (visual), R31+R49's other modal variants, and R38's
commit before a chrome button (open, G§4.7). New test-mode hooks:
`QREC_GUI_TEST_PANIC_ONCE` (it replaced `panic-tick`) and
`QREC_GUI_TEST_SEND_DELAY_MS` (G§8.4), the test channel's `sources` op and
`local`'s `metersTicking`, `exportClockTicking` and
`timelineGhostColumns` (G§3.18). Re-run at the follow-ups' head: the
workspace 1740 passed, 0 failed, 152 ignored; nested, on pixman and on
gles2 alike, gui_shell 22, gui_smoke 1, gui_chrome 14, gui_timeline 30,
gui_stage 29, gui_panel 20, gui_bin_export 27 (no GLib or GTK critical:
the cases that inject a panic count exactly that one); gles2's warm
frame-time p99 16.58–16.60 ms in 3 runs (max 16.67). No final shot
changed (nothing the follow-ups touched draws differently), so none was
re-shot.

### WP0a — Interfaces, GTK-free (alone, first)

**Goal.** Every GTK-free interface of G§3 and G§4 exists, the GUI host
runs under a fake port, and the design fixture reproduces BRIEF §3. No
dependency is added.

**Owns.** The WP0a rows of G§3.17 (core, shell, CLI,
`crates/qrec/src/shell.rs`) with their tests and snapshots, and
`docs/ARCHITECTURE.md`.

**Builds.**
- Core: `view/mod.rs` (G§4.1), `view/format.rs` (G§4.2),
  `view/notices.rs` (G§4.9), `view/blocking.rs` interim content (G§4.11),
  and stubs with G§4's final signatures for every other `view/*.rs` (they
  return defaults; each owner replaces them); `lane_mute`;
  `Command::ui_only`; `ui_normalize::edit_target`; `Take.recovered` and
  `EOS_TIMEOUT_REASON`; keymap `cap_legends`, `usable_keys`,
  `gate_reason`, D-01, D-06, `region.next`, `region.prev`, `app.quit`;
  `export_progress_view` (pub) and `export_readout`; `Ctx::preview`; the
  GES clip ids; testkit `design_project()`, `design_state()`,
  `perf_project(n)`; `tests/props_late_errors.rs`, and a toast for any
  `on_event` path it finds without one.
- Shell: the observer (H-6), `reply_late` (H-7), D-47, `Msg::RetryLive`
  and `CaptureEngine::retry_live`; `host/gui.rs` (G§3.4.1);
  `host/takeover.rs` complete (G§3.4.3); the `shutdown` refusal;
  `SharedSources`; the capture types, `LiveControl` as a no-op,
  `PREVIEW_SINK_SUPPORTED = false`, and stubs for `capture/meters.rs`,
  `player.rs`, `media/{preview,sizes}.rs`; the fake hooks (G§8.4); H-3 in
  `gpu.rs`; the `config_file` and `keys_file` readers; `UserDirs::gui_log`
  and `host_log`; `testkit/design.rs` (G§8.4).
- CLI and binary: `Backend::gui`, `Plan::Gui` (`Shell::gui` still returns
  `GUI_NOT_BUILT` until WP0b), the `--json` usage error; `Role::Gui`
  logging; `OWN_CRATES`.
- `ARCHITECTURE.md`: A§1.1 (`qrec-gui` unblocked; `view/` in the core),
  A§1.4 (owners: `view/*` per G§3.17; `host/{gui,takeover}.rs` ipc;
  `player.rs` capture; `media/preview.rs` media; `config_file.rs` and
  `keys_file.rs` meta), A§2 (`Take.recovered` and its compatibility note),
  A§3 (`laneMute`), A§8.1 (the observer and snapshots), A§8.3 (the GUI
  host's start order and `first`), A§8.6 (GUI exit rules, takeover,
  `shutdown` refusal, D-47), A§8.7 (`preview.log`; the prune ignores it),
  A§11 (the GUI harness and fake hooks), A§12 → "see docs/GUI.md", A§13
  (D-47; `Take.recovered` without a version bump).

**Acceptance and tests.**
- `cargo test --workspace`; clippy clean; `cargo check -p qrec-core`
  alone.
- Core:
  - `format` and `notices` snapshots; `cap_legends` over every default
    chord;
  - D-01: `play.toggle` and `shuttle.*` unavailable in Live, available in
    Program; `qrec play` still switches (the existing CLI test);
  - D-06: `layer.nudge-*` unavailable in Program with no selection, and
    the palette row disabled with the same reason;
  - the `qrec keys` and agent-guide snapshots list `region.next`,
    `region.prev`, `app.quit`;
  - `SetUi{lane_mute}` round-trips through `ui.json`; ids no scene has are
    dropped;
  - `Take.recovered` is set by a recovery (record machine test) and kept
    against a hand edit (external edit test); the schema snapshot is
    updated;
  - `export_readout`: 3,449 rendered of 7,391 → 42 % with `rendered`
    3,449; while stitching, "Stitching";
  - `props_late_errors`: every `on_event` outcome whose reply is a late
    `Err` also has a `Notify`;
  - `design_state()`: 10 clips, 7,391 frames, BRIEF §3.2–§3.3 statuses
    and ids, playhead 2437.
- Shell:
  - observer: a `RecordClock` event and an `ExportProgress` event each
    produce a snapshot; a command's snapshot arrives before its reply;
    `reply_late` only for `Effect::Reply`;
  - D-47: while a recovery holds, `Select`, `Seek` and `SetUi` answer at
    once and their snapshots arrive while a parked `ClipSplit` waits; it
    runs afterwards, in order;
  - `GuiHost` (fake port, fake capture, shortened `ExitRules`): no exit
    after the unused or idle timeouts; `first` runs before a request
    queued during startup; `fail` answers the queue and frees the lock; a
    failed open frees the lock; the close, signal and second-signal paths;
    `shutdown` refused; CLI requests and `submit` share one FIFO;
  - takeover: a fake-backend headless recording → Stop & open finalizes
    the take and the lock becomes ours; the export wait; the idle
    shutdown; an unresponsive holder;
  - `SharedSources` never discovers on the session thread once cached;
  - the fake hooks: the EOS delay; `QREC_FAKE_PORTAL=deny` refuses
    `FAKE-1`; the record offset shows in `RecordClock`;
    `QREC_FAKE_INVENTORY=design` labels;
  - `RetryLive` reaches `CaptureEngine::retry_live` in order with a
    `SetLive`;
  - `plan_gpu_env`: `GDK_DISABLE=vulkan` unless `GDK_DISABLE` or
    `GSK_RENDERER` is set;
  - the GUI log is per project and appended to.
- Fixture, on `design_fixture(dir, Base)`: `qrec clip list --json` equals
  BRIEF §3.3; `qrec take list --json` has BRIEF §3.2's statuses (take 9
  broken with `EOS_TIMEOUT_REASON`, take 7 recovered, take 6 tooShort);
  `qrec autocut --clips c3e9 --dry-run --json` returns BRIEF §3.6's four
  regions (48, 42, 39 and 57 frames at its timecodes); `qrec autocut
  --clips c2d0 --dry-run --json` finds the two silences of BRIEF §3.3.
- CLI: `qrec --json` with no command → `usage`, exit 2.

### WP0b — GUI skeleton, theme, harness (lane B, after WP0a and G§0.1)

**Goal.** `qrec` opens a project in a nested compositor and shows the
window skeleton driven by real snapshots: the startup order, the interim
pages, key routing, the theme pipeline and the screenshot harness work.

**Owns.** The root `Cargo.toml` (dependencies, MSRV), `Cargo.lock` and
`flake.nix` (G§6.3, G§6.4 but the desktop file and icons);
`crates/qrec/Cargo.toml` and `Shell::gui` in `crates/qrec/src/shell.rs`;
every `qrec-gui` path marked WP0b in G§3.16 and the stubs of all the
others; `crates/qrec/tests/gui_support/**` (with the spike's
`run-nested.sh` and `gpu-opens.sh` copied to `scripts/`),
`crates/qrec/tests/gui_smoke.rs`, `crates/qrec/tests/gui_shell.rs`; A§1.2
in `ARCHITECTURE.md` (the approved dependencies, the glib check).

**Builds.** G§3.2–G§3.8, G§3.12–G§3.16, G§3.18; C01 (CSS and helpers),
C02, C19, C20, C22, C25 (menus), C26–C29 (CSS), C31 (interim pages), C33;
the interim pages of G§1.1 #2 with the takeover flows; the adaptive modes
(G§3.15); the "Not built yet" actions; test mode and
`QREC_GUI_TEST=stuck-gst` (the startup reports GStreamer stuck without
initializing it); `MediaHub` with the appsink picture (G§3.4.2); the art
export and the `gtk::Svg` check (G§3.14).

**Acceptance.**
- G§0.1 recorded in the merge commit; `cargo tree -i glib --depth 0`
  shows one glib; the workspace builds with `rust-version = "1.92"`.
- In nested sway on `design_fixture(Base)`: a 1600×1000 window; `qrec
  status --json` → `host: "gui"` with the GUI's pid; `qrec clip split
  --timeline --at 2437f` and `qrec undo` succeed and the stub header's
  subtitle (`clips_summary`) follows; `regions` equals BRIEF §2.1 (±1 px).
- Startup order: `qrec record toggle` sent 50 ms after launch is answered
  by the GUI host (status `host: "gui"`, phase recording; the lock record
  says `gui`; no headless host was spawned).
- `QREC_GUI_TEST=stuck-gst` → the stuck page, exit 5, the lock free while
  it shows. No ffmpeg on the GUI's PATH → interim S23 with the lock free:
  `qrec clip list` works in-process meanwhile; Check again with ffmpeg
  back opens the window.
- A corrupt `qrec.json` → interim X3 with the restore command; `qrec
  history restore <seq> --yes` succeeds while the page shows.
- An empty folder gets a `qrec.json` named after it before any IPC
  request runs (D-35). `Other` → X2, exit 3, nothing written; a folder
  inside a project → X2 with the D-14 line.
- A second `qrec` → the held-by-window page, exit 6. A fake headless
  recording → interim X1: Stop & open finalizes and opens; Quit exits 6
  and the recording continues.
- SIGTERM → exit 130 within 15 s; window close → exit 0 and the lock is
  free.
- `theme = "swamp-day"` in `config.toml` → the first frame uses Swamp
  Day's tokens; `record.toggle = "F9"` in `keys.toml` → `hints` shows F9;
  a conflict → one warning toast per `keymap::problems` line.
- Pango resolves the three bundled families; every custom icon recolors;
  the art renders through `gtk::Svg` like the pen crops, or the PNG
  fallback is in place (recorded in the merge commit).
- `keys::router::decide` table tests: every default binding with a fake
  dispatcher; the `Esc` chain order; editable passthrough; drags (D-42);
  `LocalUi.focus == Bin` (D-11); the passive export panel (D-49); the
  `region.next` order.
- `tests/tokens.rs`: the generated files match the JSON; no hex in
  `components/*.css` or in `src/` outside `theme/palette.rs`.
- Adaptive skeleton: `regions` at 1300×1000 (panel 320), 1000×1000 (panel
  overlay sheet toggled by the stub header button) and 1600×800 (timeline
  232).
- The control channel's ops work; `cargo test -p qrec --test gui_smoke --
  --ignored` passes smoke steps 1–3 and 10 (G§8.5); the a11y walk of the
  skeleton is empty.

### WP1 — Capture and playback, shell (lane A, after WP0a)

**Owns.** `qrec-shell/src/capture/{engine,live,meters}.rs`,
`capture/tests.rs`, the `PREVIEW_SINK_SUPPORTED` line, `player.rs`,
`ges_render.rs` and its tests; `qrec-core/src/ges_plan.rs` (planner
functions, snapshots).

**Builds.** H-1, H-2, H-4, H-9, D-03, the engine side of D-50, the player
side of D-08; G§3.9, G§3.10; `ges_plan::{plan_ges_take, without_tracks,
scaled}`; `ges_render::rebuild_into`.

**Measurement first** (SPEC §12, G§0.4): `playback_fixture` (two
2560×1440@30 H.264 screens and one 1280×720@30 H.264 camera, 60 s, a
1920×1080@30 canvas) played by `ProgramPlayer` on the dev machine for
30 s, once into `fakesink sync=false` (throughput) and once with
`sync=true` (lateness). Pass: ≥ 36 fps throughput and < 1 % late frames.
Otherwise set `out_size = (960, 540)` (H-9), measure again, and put both
sets of numbers in the merge commit and before the PO.

**Acceptance and tests** (`cargo test -p qrec-shell capture player
ges_render`, `cargo test -p qrec-core ges_plan`; GStreamer present, fake
backend):
- the preview through a given sink: an `appsink` standing in for the
  paintable sink gets BGRx at `max_size` and survives two rebuilds (the
  same element); without `preview`, the RGBA appsink path is unchanged;
- `transient_layout` moves a layer without a rebuild (generation
  unchanged) and the next `SetLive` clears it; `retry_live` rebuilds; cast
  grace (fake: the portal call count); `LiveStatus` sequences for a build,
  a refusal (`QREC_FAKE_PORTAL=deny` and `cancel`) and a source's death;
- `MeterMonitor`: readings about every 50 ms on the fake tone; stop ≤ 5 s;
- player with an appsink video sink and the frame-ID fixture: accurate
  seeks show the frame plan's frame (A§11 parity); `load` keeps the
  position; rates 2 and 4, and a rate change while playing; reverse
  stepping; `Ended`; `Error` on a missing file; shutdown ≤ 5 s; with
  `out_size`, frames arrive at the canvas fitted into it;
- `plan_ges_take` and `scaled` snapshots; properties `without_tracks(p,
  None, ∅) == p` and `scaled(p, canvas size) == p`;
- the measurement above.

### WP2 — Media previews (lane A, after WP1; its GTK caches after WP0b)

**Owns.** `qrec-core/src/media_argv.rs` (preview argv, key,
`bucket_peaks`, `peak_levels`, snapshots); `qrec-shell/src/media/{preview,sizes}.rs`
and tests, and the A§8.7 prune's name pattern; `qrec-gui/src/media/{thumbs,peaks}.rs`.

**Builds.** G§3.11.

**Acceptance and tests.**
- argv snapshots; the key is deterministic;
- a 95 s fixture gives chunk 0 with 60 images and chunk 1 with 35 (± 1),
  72 px high (as built 34: a chunk that reaches the end of the file has
  one image less than its whole seconds, G§3.11); a `Frames` request
  returns exactly `count` consecutive frames matching the frame-ID fixture
  (WP2's own: a 12-bit frame-ID video with a keyframe exactly every
  second; WP1's 8-bit one counts to 255 and x264 may add scene-cut
  keyframes); peaks have one bucket per ms, and each coarser bucket is the
  min and max of its finer ones (property);
- a cache hit spawns no ffmpeg (a spawn counter); concurrent requests are
  deduplicated; at most 2 run at once, 1 while exporting; a corrupt cache
  file is regenerated; no ffmpeg → `Failed`;
- logs: one `preview.log` across runs; a failed run's own log; the A§8.7
  prune keeps `preview.log` and the newest 200 timestamp-named logs; the
  disk caps trim oldest first;
- `take_sizes` equals the `take_contents` sums; `CachedSilence` reads an
  analysis written by `qrec autocut --dry-run`;
- GTK caches (their GTK-free parts): the LRU never exceeds its byte budget
  (a property over random request sequences: seeded splitmix64 sequences,
  as `qrec-gui` has no proptest dev-dependency); byte slicing; texture
  creation in a nested compositor (`media::thumbs`'s ignored
  `textures_in_a_nested_compositor`, run as a client of a headless sway:
  the test binary with `--ignored`, under a private `XDG_RUNTIME_DIR` and
  `WAYLAND_DISPLAY`; it refuses a runtime dir under `/run/user/`);
  `bytes()` stays under budget while a one-hour fixture scrolls (G§8.6).

### WP3 — Timeline (lane B, after WP6)

**Owns.** `qrec-core/src/view/{timeline,ruler}.rs` with snapshots and
`tests/props_view_timeline.rs`; `qrec-gui/src/timeline/**`;
`components/timeline.css`; `crates/qrec/tests/gui_timeline.rs`.

**Builds.** C14–C18 (C19 through the WP0b helper), the S01b empty state,
the timeline region of S02, S04, S04b, S05, S05b, S06, S07, S08, S09, S10,
S11, S12 and S14 (bin count), the compact rows; G§4.5; the timeline side
of D-23, D-37, D-38 (the drop target), D-43; D-25, D-36, D-39, D-40,
D-41, D-45, D-46. Registers `zoom.in`, `zoom.out`, `zoom.selection`.
Writes `LocalUi.{drag, viewport, hover_clip, trim_frame}`.

**Acceptance.**
- Snapshots of the four zoom levels on `design_state`: clip x, w and
  labels with the inset rule; S12's zoom to c3e9 (0.33494 px/frame, x 176
  → 1216, readout "10 px/s"); the ruler checks of G§4.5; `z` unavailable
  with no selection.
- GTK-free `decide_*` functions with a fake dispatcher: one undoable
  command per gesture and none on `Esc`; press, Shift+click, Ctrl+click and
  empty-row clicks give D-37's commands; keys during a drag (D-42); a
  scrub sends at most 10 `Seek` per second plus one on release; wheel and
  middle-drag send one `SetUi{scroll_px}` 300 ms after the last event; a
  bin drop gives `ClipAdd{at: Index(k)}`; trim zones at a cut and on narrow
  clips (D-45).
- The zoom animator is instant under reduced motion.
- Screenshots (stub-based, final per G§7.2): S01b; the S04, S04b and S05
  ghosts; S05b, S07, S08, S09, S10, S11; S12's timeline; S09 at
  `QREC_TEST_SCALE=2`; the compact timeline at 1600×800.
- Frame time p99 < 16.7 ms while zooming and scrolling the 300-clip perf
  fixture, asserted under `QREC_TEST_WLR=gles2` on the dev machine and
  recorded under pixman (G§8.6).

**As merged** (c3e5961, round 3): `gui_timeline.rs` has 14 cases (S01b,
S04, S04b, S05, S05b, S07, S08, S09 and at scale 2, S10 with a real trim,
Esc and undo, S11 with a real move and Esc, S12's zoom, the compact
timeline, the 300-clip probe). Stub-based when it was built: S01b added
its sources over the test channel (no first-run page yet; WP4's now
exists) and S12 showed the zoom only (no WP5 autocut page; since the WP5
integration it opens the page from the toolbar's settings icon and probes
the preview). Measured: warm pass gles2 p50 16.14, p99 16.61, max 16.67 ms;
pixman p50 16.05, p99 16.60 ms. The departures are folded into G§4.5,
D-38 and D-40.

### WP4 — Stage and canvas (lane A, after WP2)

**Owns.** `qrec-core/src/view/canvas.rs` with snapshots and properties;
`qrec-gui/src/stage/**`, `qrec-gui/src/media/{live,program}.rs`;
`components/stage.css`; `crates/qrec/tests/gui_stage.rs`.

**Builds.** The Live, Program and first-run stage; C06, C07, C08; the
transport and Save frame; S01a's source setup; take and trim previews on
the stage; the player sync (G§3.10); D-04, the stage side of D-05 and
D-17, D-09, D-16, D-26, D-27, D-32, D-33, the player side of D-40, the
canvas side of D-43, D-44, the H-9 badge (its `proxy` input is
`fit_size(canvas, out_size) != canvas`, G§3.10); the S24 overlay only after
G§0.2. Writes `LocalUi.{layer_drag, first_run_checked, playing_rate}`.

**Acceptance.**
- Canvas snapshots: the `stage_geometry` checks (G§4.6), the design
  scene's layer boxes, snapping at "Right third", aspect resize, crop
  limits.
- Exactly one `LayerSet` per drag and per nudge burst (D-33: a GTK-free
  coalescer test over press, repeat and release sequences);
  `transient_layout` during Live drags and `None` on cancel.
- The sync state machine with a fake player: the once-a-second seeks, an
  external seek, `Ended`, shuttle rates and a rate change while playing
  (D-44), the rate reset on `qrec play`, trim and bin previews loading and
  restoring.
- Screenshots: S01a, S01b, S02, S03, S04, S04b, S05, S06, S10's canvas,
  S13, S14's stage (preview 832×468, transport at y 612); reduced motion:
  S04 (static REC badge, no glow) and S03 (static full ring); S02 at
  `QREC_TEST_SCALE=2` (sink caps 1920×1080).

**As merged** (9d0dce2, round 3): `gui_stage.rs` has 17 cases (S01a,
S01b, S02 and at scale 2, S03 and reduced, S04 and reduced, S04b, S05,
S05b, S06, S10's canvas, S13, S14's stage, D-27's empty Program, arrow-key
nudges); S03 retries a shot that missed the first second (the countdown
ticks in real time) and since round 3 checks the hint bar's countdown
items; S10's was stub-based (no timeline video row, `trim_frame` or
`take_preview` then: the trim plate and D-17's TAKE badge wait for WP3's
drag, now merged, and WP7). WP4 also owns the crop-race fix (A§8.11,
G§3.9). The departures are folded into G§3.8, G§3.10, G§3.18 and G§4.6.

### WP5 — Onion layers panel (lane B, after WP3)

**Owns.** `qrec-core/src/view/{panel,meter,autocut}.rs` with snapshots;
`qrec-gui/src/panel/**`, `qrec-gui/src/widgets/{numeric,meter,scale_tooltip}.rs`;
`components/panel.css`; `crates/qrec/tests/gui_panel.rs`.

**Builds.** C09–C13, C11, the C26 value tooltip, the C29 `NumericField`,
the S12 page; the Medium layout (320 px, inspector 2-up) and the Narrow
sheet's content; the panel side of D-06, D-07, D-15, D-43; the Add layer
and Add audio track menus (from `LocalUi.sources`, refreshed when they
open). Registers `autocut.page`. Writes `LocalUi.{panel_page,
autocut_preview}`.

**Acceptance.**
- Snapshots: the design scene's rows, inspector values, the meter mapping
  at 384 and 320 px, S12's summary rows and regions for the fixture's dry
  run (BRIEF §3.6).
- Reorder index math; one command per inspector commit; the D-15 command
  sequence; one dry run in flight (a change made meanwhile waits and the
  newest is sent after the reply; stale replies are ignored).
- Screenshots: the panel of S02, S06 and S13; S13b, each state produced
  through the CLI, the fixture or `QREC_FAKE_PORTAL` and cropped; the S12
  page; the panel at 1300 wide.

**As merged** (99197b5, merged in f84ad6d, round 4): `gui_panel.rs` has 8
nested cases (S02 with `M` and Add layer; S06 with one inspector commit, an
invalid value and Layout's reset; S13 dragging L2 on WP4's canvas, the
inspector reading x 1280 from `LocalUi.layer_drag` before anything is
sent; S13b's states, each produced for real and cropped, the focus ring
and the inline rename with a held keyboard; the portal's refusal; a take's
missing source; S12: open, a region's seek, a tuning change's dry run
after one `waitIdle`, `Esc`, Apply; the panel at 1300 wide) and the
fixture's dry run read through `view::autocut`. The integration: the dry
runs' 250 ms debounce holds the io worker busy (G§3.18); the Add menus'
refresh is `run_io_then`; `QREC_GUI_TEST_LEVELS` pins the meters (G§8.4);
`widgets::button::menu_button` makes every menu's button (G§5.1);
`gui_timeline`'s S12 opens the page from the toolbar's settings icon and
probes the preview the canvas draws. The departures are folded into G§4.7,
G§5.1, G§5.2 and G§8.4. Its audit: the page switch slid under reduced
motion too (WP5 was built before `Theme::follow_motion`, c07c00b): it
follows the policy now, and a ninth nested case,
`the_pages_slide_unless_motion_is_reduced`, checks both policies.

### WP6 — Header, hint bar, toasts, modals, Settings shell and Project page (lane B, after WP0b)

**Owns.** `qrec-core/src/view/{hints,header,confirm,settings}.rs` with
snapshots; `qrec-gui/src/{header/**, hintbar.rs, toasts.rs, modal.rs,
settings/mod.rs, settings/project.rs}`; `components/{chrome,settings}.css`;
`crates/qrec/tests/gui_chrome.rs`.

**Builds.** C03, C04, C05, C23, C24, the record button, the export slot,
the narrow-mode layers button, the Settings shell and its Project page;
G§4.3, G§4.4, G§4.9's confirmations, G§4.10; D-02, D-12's modal, D-23's
hints, D-48. Registers `settings.open`.

**Acceptance.**
- Hint snapshots for every state, equal to the BRIEF lists (labels,
  order, disabled, emphasized), including S07 (`zoom.selection` disabled)
  and S15b (`export.panel` enabled, `record.over` disabled); header
  snapshots for every record phase.
- `ToastQueue` (GTK-free): timings, the "+N" queue, at most two actions,
  the Undo guard.
- Modal focus: `Enter` right after opening activates Cancel (nothing
  dispatched); `Tab` + `Enter` dispatches the confirmed command.
- Settings › Project on the fixture: picking 25 fps → the S18b modal over
  Settings, both scrims ending at y 968, the modal hints live; Change fps
  dispatches the confirmed `ProjectSet`.
- Screenshots: the header of S01a, S02, S03, S04, S05, S06 and S15b; S16b
  (the toast, from a CLI export of the `Smoke` fixture); S18 (the modal
  opened by the control channel's `dispatch` of `TakePurge{take-0002,
  take-0004}`, over the stub bin until WP7); S18b; the header at 1000 wide
  (layers button).

**As merged** (aece16a, round 3): `gui_chrome.rs` has 11 cases (S01a,
S02, S03–S05, S06, S15b, the narrow header, S16b, the toast queue and
Undo, S18, S18b, D12). WP6 worked around two WP0b CSS faults in its own
CSS (the focus ring on the whole focus chain, doubled box spacing); round
3 fixed both at the source and dropped the workarounds (G§3.14). The
departures are folded into G§3.15, G§4.3, G§4.4, G§4.9, G§4.10, G§5.1 and
G§5.2.

### WP7 — Bin and export (lane A, after WP4)

**Owns.** `qrec-core/src/view/{bin,export}.rs` with snapshots; the
export-done toast in `qrec-core/src/on_event/output.rs` with its core and
CLI test updates; `qrec-gui/src/{bin_drawer/**, export/**}`,
`widgets/mud_bar.rs`; `components/{bin,export}.css`;
`crates/qrec/tests/gui_bin_export.rs` and
`crates/qrec/tests/support/fail_ffmpeg.rs` (it writes a PATH wrapper at run
time: the design export's first two render units render, the third, c1b2,
fails with BRIEF S17's stderr once they are cached, and the later ones wait
until the export stops them, so it fails at 11 %; simpler than a checked-in
`gui_bin_export_data/fail_ffmpeg.sh`, the plan's first idea).

**Builds.** C21, C30, the export panel (S15a, S15b, S15c, S16, S17) with
its trap and passive modes (D-49), the S14 bin with multi-select (D-37)
and the drag source (D-38), the S18 base; the bin side of D-17, D-18,
D-24; the S16b toast copy. Registers `export.panel`, `bin.preview`.
Writes `LocalUi.{take_preview, export_view, bin_selection}`; calls
`PreviewCache::set_exporting`.

**Acceptance.**
- Snapshots: the fixture's bin rows with every D-18 line, export readouts
  (42 %), the export-done toast (body "<path> · 88.4 MB", two actions).
- Flows: a GUI export done, failed and cancelled; a CLI export shows
  Running, then the core toast; overwrite → modal → confirmed
  re-dispatch; a forbidden path shows inline; purging 2 takes → modal →
  confirmed `TakePurge`; a bin row dragged onto the timeline → `ClipAdd{at:
  Index(k)}`; with the passive panel open, `s` splits and the first click
  on the timeline selects a clip.
- Screenshots: S14, S15a, S15b (captured once `qrec status` shows the job
  at ≥ 42 %), S15c (the WebM setup; the reduced-motion bar), S16, S16b,
  S17, S18.

**As merged** (518e6cb, merged in e3b30a0, round 5): `gui_bin_export.rs`
has 12 nested cases (S14; the bin's keys, preview and insert; a bin row
dragged onto the timeline; S18 from the bin; S15a; S15b and a cancel;
S15c's WebM setup and its reduced-motion bar with D-49's split and click;
S16; S16b; S17; a CLI export in the panel; D-24's refused path and Replace
modal) and the S14 scale runs (two more). The merge put its waits behind
deadlines (`run_tool`, `Nested::cli_background` and `reap_within`,
`Nested::keys_held`) and gave the mud bar a tick callback only while it
animates. The integration: an export confirmed in the Replace modal is
still the panel's own (`Store::ask_confirmation`, G§3.7), so it ends on
Done or Failed; the running and failed lines count the timeline's clips
(G§4.8); S17's wrapper fails the third unit after the first two rendered
(11 %); `waitIdle` settles the window's own running export (G§3.18), so
the cases click Export and wait idle like any. Its departures are in
G§7.2's list. The integration audit: a bin row's click gesture held the
row (a cycle: every rebuild kept the rows it dropped; R11, the bin keys
case counts them), and the panel's own last Done or Failed page came
back after another export ran over it, dropping that export's toast
(now an export not its own clears it, so the panel closes when that one
ends; S16 runs a CLI export over its Done page). S15b and S15c's running
pages check no mud bar ticks under the frozen clock. The final shots'
audit, measured against the frames: the C19 hatch is 6 px apart across
its stripes (8.49 along x; it started a stripe every 6 px along x
wherever it was drawn: S04b, S05, S08, S12, S14); a playhead past the
timeline's end (a `ui.json` older than the project: S16's Smoke cases
used S06's preset) is clamped when the project opens (`ui_normalize`, as
every commit) and by `shown_playhead`, so the ruler's corner no longer
reads 00:01:21:07 over a 00:00:03:00 transport; the hint bar leaves out
the items that don't fit whole (at 1000 px the `?` cap showed without
"All keys"); the S06 scale runs hover c2d1 for its tooltip; the export
panel's content is 16 px in (15 of padding after its border), its
segments 26 px, the header slot's percent `mono-sm`, the running page's
percent on the frames line's baseline, WebM's AV1 row 12 px under the
caption, "Output" 3 px lower, the S17 stderr text 11 px in and its hint
10 px under the lead; the mud bar's frame A is the frames' own bubbles
(`RUNNING_FRAME_A`, `DONE_FRAME_A`) and its sediment shows 2 px over the
border; the links' underlines run under the descenders
(`widgets::button::link`, Pango's `low` underline) and the bin's link is
12 px; the bin's actions block is 78 px; S18's footer label is 13/600;
the toast's actions start at its text's x, its body 2 px under the title;
S17's wrapper writes its node line first, so the box shows BRIEF's last
ten lines. Its code audit's open items: bin thumbnails keep their theme
subscription and drop it with themselves (`themeSubscribers`); a bin row
dropped mid-timeline lands at its slot (slot 7 of 12); S16b's export is
slowed (each unit 3 s late) so its `Esc` beats it on any machine, and
S17's third unit fails on the case's go-ahead once it saw "Rendering clip
3/10"; GES fades span their clip, in-point to end (a fade-out alone made
GES's clamp extend its slope back to the in-point: GLib's out-of-range
`volume`, 1300 for a 13 s clip at −6 dB, on every Program load); the
timeline's tooltip is measured again when it shows after a hide (the
first one after a drop was allocated at the previous one's size, GTK's
"Allocation width too small"); and the stage, timeline, panel and bin
suites fail on any GLib or GTK critical (`Gui::assert_no_criticals`).

### WP8 — Blocking screens as drawn (lane A, after WP7 and G§0.2)

**Owns.** The drafted parts of `qrec-core/src/view/blocking.rs` and
`components/blocking.css`; `qrec-gui/src/blocking/{missing_deps,cli_recording,not_my_swamp,unreadable,export_wait}.rs`;
`crates/qrec/tests/gui_blocking.rs`.

**Builds.** S23 and its stuck variant, X1 with the D-29 export and idle
variants, X2 with the D-14 line, X3 and its missing-file variant, the D-13
page and the not-responding page, as the approved frames draw them (C31
Blocking with art and rows, the minimal header `Xba2H`), replacing the
interim pages one by one. The flows stay WP0b's.

**Acceptance.** Screenshots of every page next to its frame; the exit
codes of G§3.2; WP0b's takeover tests still pass.

### WP9 — Palette, cheat sheet, Settings pages, toast copy (lane B, after WP5 and G§0.2)

**Owns.** `qrec-core/src/view/{palette,cheatsheet,preferences}.rs`;
`qrec-core/src/keymap.rs` (D-31 labels, `history.clear`,
`set_binding`/`reset_binding`) with the `qrec keys` and agent-guide
snapshots; the toast copy in `qrec-core/src/on_event/{record,disk}.rs`
with the core snapshots and the `crates/qrec/tests/cli.rs` assertions it
changes; the writers in `qrec-shell/src/{config_file,keys_file}.rs`;
`qrec-gui/src/{palette.rs, cheatsheet.rs,
settings/{appearance,recording,autocut,keyboard}.rs}`,
`widgets/theme_card.rs`; `components/overlays.css`;
`crates/qrec/tests/gui_overlays.rs`.

**Builds.** S19, S19b, S20, S20b, S21, S22 as approved, and the Settings
Recording and Autocut pages once drawn or waived; C32, C34; D-19, D-21,
D-22, the D-30 caption, D-31. Re-registers `keys.help` and `palette.open`.

**Acceptance.**
- Screenshots S19–S22; S25's toasts, each triggered for real.
- `config.toml` and `keys.toml` edits keep comments and order (round-trip
  tests); a conflicting `keys.toml` shows the banner and both bindings
  stay inert (Q5).
- The palette runs every enabled row (a dispatch table test); Clear undo
  history → the modal → a confirmed `HistoryClear`.
- CLI tests pass with the D-21 copy.

### WP10 — Themes, packaging, audits (after WP1–WP7; WP8 and WP9 once approved)

Touches, by agreement with their former owners: CSS fixes,
`qrec-shell/src/watcher.rs` (the config dir), `flake.nix` (desktop file,
icons), `README.md`, `docs/ARCHITECTURE.md` (final pass),
`crates/qrec/tests/gui_smoke.rs`, `crates/qrec/tests/gui_themes.rs`.

**Builds.**
- The config watcher: `qrec config set theme | ui.scale |
  ui.reduced_motion` applies live (G§3.14); the keymap reloads when
  `keys.toml` changes, with its problem toasts.
- Theme proofs (BRIEF §7): S02 and S06 in all four themes; S18 and S25 in
  Swamp Day.
- Accessibility: `gtk::Accessible::announce` for record start and stop
  and export done; the focus-order audit (BRIEF §6).
- Audits: reduced motion; every case at scale 1.5 and 2; widths 1300 and
  1000, height 800; the performance probes (G§8.6).
- Packaging: the desktop file and icons; `nix build`, then the result run
  in nested sway.
- The complete smoke (G§8.5) and the PO checklist (G§8.7).

**As built (the text lane).** G§3.14's text-rendering experiment: every
setup GTK 4.22 offers measured against the frames on S02, S06, S12, S14,
S16 and S17, with crops at scale 1 and 2 and the stray mark on GL; none
kept the mark away with the frames' widths, so Qrec's setup stays (manual,
hinted metrics) and the crops and numbers are the PO's (G§8.7 #9). Built
for it: test mode's `QREC_GUI_TEST_FONT_RENDERING` (G§8.4), the `fonts`
op's `rendering`, `renderer` and `widths` (G§3.18), and `gui_text` (its GL
case guards the mark now: in today's header layout `gui_shell`'s title
case, all ascenders, shows it in no setup but unhinted fractional
metrics).

---

## G§8 Verification

### G§8.1 Core view models

- insta snapshots per `view` module over `testkit::design_state()` and the
  owner's per-screen variants; the proptests named in G§4. `cargo test -p
  qrec-core view`.
- A test asserts the BRIEF hint lists verbatim (labels, order, enabled)
  for every approved state, S07 and S15b included.
- `props_late_errors` (H-7).

### G§8.2 GTK-free logic in `qrec-gui`

Pure modules, no `gtk::init`: queue coalescing and ordering,
`keys::router::decide`, the `Esc` chain, drag keys, the echo rule, the
player sync state machine, the nudge coalescer, the scrub throttle, the
toast queue, the mud bar PRNG, the zoom animator (instant under reduced
motion), the byte-bounded caches. `cargo test -p qrec-gui --lib`.

### G§8.3 Shell

`GuiHost` (fake port, fake capture, shortened `ExitRules`), takeover, the
observer, D-47, the player (fixtures, appsink), the preview cache, the
meter monitor, the live sink's reuse across rebuilds, the fake hooks.
`cargo test -p qrec-shell`.

### G§8.4 Headless screenshots

**Harness** (`crates/qrec/tests/gui_support/`, WP0b; the logic of the
spike's `run-nested.sh`, copied to `gui_support/scripts/`):

- **Where.** In the `qrec` package: Cargo builds the `qrec` binary before
  that package's integration tests and sets `CARGO_BIN_EXE_qrec`, so a
  case never runs a stale binary. `gui_support` panics when
  `CARGO_BIN_EXE_qrec` is unset; it never falls back to
  `target/debug/qrec`. Run: `cargo test -p qrec --test gui_<area> --
  --ignored --test-threads=1`.
- **Compositor.** sway headless at 1600×1000@60:
  - a private `XDG_RUNTIME_DIR` from `TestEnv`; `WLR_BACKENDS=headless`,
    `WLR_LIBINPUT_NO_DEVICES=1`;
  - renderer `pixman` by default, and then the client draws with Cairo
    too (`GDK_DISABLE=gl,vulkan`, below): nothing opens a GPU node, as on
    CI. `QREC_TEST_WLR=gles2` renders on
    `WLR_RENDER_DRM_DEVICE=<the first Intel or AMD render node>`, chosen
    by PCI vendor (`gui_support::gl_node`; the node numbers swap between
    boots on the dev machine, NVIDIA's is never used), with Mesa's EGL
    pinned (`__EGL_VENDOR_LIBRARY_FILENAMES`), and the client uses GSK GL:
    the GL path (the header title's stray mark, G§3.14 fonts, only ever
    showed there);
  - config: `output HEADLESS-1 mode --custom <1600·s>x<1000·s>@60Hz scale
    <s>` (`QREC_TEST_SCALE` = s, default 1: a 1600×1000 logical output at
    any scale; `Options::size(w, h)` for other logical sizes),
    `default_border none`, `xwayland disable`, `swaybg_command -`,
    `swaynag_command -`;
  - `DISPLAY` and `WAYLAND_DISPLAY` unset for sway; whole process groups
    killed on teardown (`kill(2)`), and each child also dies with the
    test's thread (`PR_SET_PDEATHSIG`), so a test binary killed by Ctrl-C
    or a runner's timeout leaves no sway, bus or window behind; every tool
    run (grim, wtype, swaymsg, the CLI, ffmpeg probes, the cases' own
    ffmpeg compares) has a deadline, and a case's background CLI
    (`Nested::cli_background`: a `record stop` that waits for its take, an
    export) logs to `<shots>/logs/<case>-cli.log` and is reaped within one
    (`reap_within`; the round 3 cases waited on them, and on their ffmpeg
    runs, without one: WP5 integration audit);
  - guards: the private runtime dir is never under `/run/user/`, the
    nested display is a socket name right inside it, and a case can't give
    a window `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY`, `WAYLAND_SOCKET`,
    `DISPLAY` or `GDK_BACKEND` (`gui_support::DISPLAY_VARS`, a panic);
    grim and wtype get the nested display with `WAYLAND_SOCKET` removed.
    Never the user's `wayland-1`.
- **Client env.** The `TestEnv` vars (`HOME`, `XDG_*`,
  `QREC_RUNTIME_DIR`, `QREC_CAPTURE_BACKEND=fake`, `TZ=UTC0`: the fixture's
  recording times are UTC, so the bin's local HH:MM reads BRIEF's 14:31 …
  14:06 on any machine; added in the WP0a review);
  `WAYLAND_DISPLAY=<nested>`; `GDK_BACKEND=wayland`;
  `GSETTINGS_BACKEND=memory`; `GSETTINGS_SCHEMA_DIR` built like the spike;
  `GTK_A11Y=none` except for the a11y walk; a private `dbus-daemon` with no
  service activation; `QREC_GUI_TEST=1`; `QREC_FAKE_INVENTORY=design`;
  Mesa's EGL vendor file; `QREC_FONT_DIR` passed through (`TestEnv` drops
  `QREC_*`), and `QREC_GUI_TEST_FONT_RENDERING` when the test run has it
  (`gui_support::FONT_RENDERING`, G§3.14; a case's own wins); under pixman
  also `GDK_DISABLE=gl,vulkan`. `gui_text`'s GL case gives its windows
  `GDK_DISABLE=vulkan`, `GSK_RENDERER=gl`, `LIBGL_ALWAYS_SOFTWARE=1`,
  `GALLIUM_DRIVER=llvmpipe` and `LP_NUM_THREADS=2`: GSK's GL renderer on
  Mesa's llvmpipe, which opens no GPU node either (WP10 traced every open
  of 15 such runs: no device node under `/dev/dri` or `/dev/nvidia`, only
  listings of the `/dev/dri` folder, one a window by Qrec's own GPU facts,
  as in any pixman run, and one by Mesa's).
- **Test mode** (`QREC_GUI_TEST=1`):
  - `gtk-enable-animations = false`, so GTK and libadwaita transitions are
    instant. Reduced motion comes from `config.toml` only (D-30): cases
    shoot the animated look unless they set `ui.reduced_motion = true`;
  - `motion::now()` is frozen: looping animations at phase
    `QREC_GUI_TEST_T_MS` (default 0: the torch at frame A with its glow,
    the mud bar at frame A with its bubbles, spinners at 0°), one-shot
    animations at their end (the countdown digit at scale 1 with a full
    ring, toasts risen, the drawer open);
  - cursor blink off; a seeded PRNG; `QREC_GUI_TEST_NOW` (RFC 3339) as
    the local time of the auto-name preview (S15a);
  - `QREC_GUI_TEST_LEVELS=<device>=<rms>/<peak>,…` (dBFS) pins the
    meters (WP5): BRIEF's −22/−16 on A1 and −52/−48 on A2 in the panel
    cases, WP7's and the S06 scale runs (with Face selected, as the frames
    draw the inspector), a clipping A1 for S13b's latch;
  - `QREC_GUI_TEST_SAVE_AS=<path>|<path>…` stands in for the export
    panel's save dialog (WP7): each press of Change… takes the next path
    (the file dialog needs a portal, G§9 R7; D-24's cases);
  - `QREC_GUI_TEST_PANIC_ONCE=<what>|<what>…` makes the first run of the
    guarded callbacks named (their `what`: `timeline: a frame`, `meter:
    refresh`, `export: the running clock`) panic, caught like any other,
    so a case sees that what each drives comes back (G§3.12 R10; review
    R09, R65; it replaced the timeline-only flag `panic-tick`);
  - `QREC_GUI_TEST_SEND_DELAY_MS=<ms>` holds every command that long on
    its way to the host (`Store::dispatch_with`, in order): a slow
    Session, so a reply comes after the next gesture (the stage's
    in-flight commits, R23's audit; a key's pre-step seek while playing,
    R22's audit);
  - `QREC_GUI_TEST_FONT_RENDERING=<spec>` sets GTK's text up otherwise
    (`theme::fonts::Rendering::parse`: `mode=automatic`, `metrics=off`,
    `hintstyle=none`, …; empty: Qrec's own; a spec that doesn't parse is
    an error in the GUI log and Qrec's own): G§3.14's experiment, and
    `gui_text`'s cases (WP10);
  - the control channel (G§3.18).
- **Driving.** `qrec --project <dir> … --json` with the `TestEnv`
  (`Nested::cli`, `cli_json`); the pointer: `Nested::pointer()`, one
  `zwlr_virtual_pointer_v1` (`gui_support/pointer.rs`, the Wayland wire
  protocol over the compositor's socket, std only) created before any
  window maps and kept for the whole case: headless sway has no pointer
  device, so its seat offers clients no pointer until one exists, and a
  pointer that comes and goes with each click (one `wlrctl` run per
  click) loses the click. `move_to`, `button`,
  `click`, `drag_to` take output-layout px and each ends with a `frame` and
  a `wl_display.sync` round trip. Keys: `Nested::keys`, one `wtype` run
  per key sequence, `-s 400` first so the compositor has the virtual
  keyboard's keymap (`wtype -k <key>`, `-M ctrl -k e -m ctrl`). And the
  control channel's `dispatch` and `action`.
  The seat has a keyboard only while a `wtype` runs, so the window is
  active only then (round 3 integration audit: `is_active` false before
  the first key and after each run). GTK takes `:focus` and
  `:focus-visible` off the focus chain of an inactive window, so a shot
  after a key shows no focus ring (S18's Tab: the purge button is
  focused, ring-less); one before any key keeps the ring a widget got
  from `grab_focus` and `set_focus_visible`. GTK 4.22 also drops focus
  visibility 3 s after it was last set (`VISIBLE_FOCUS_DURATION`) and
  sets it again on the release of a key that moved the focus.
  `Nested::keys_held(args, hold_ms)` runs one `wtype` in the background
  that keeps its keyboard `hold_ms` after its keys, so the window stays
  active for a shot taken meanwhile (`poll` for what the keys did, then
  `waitIdle`): S13b's focus ring and inline rename, S18's Tab (WP5
  integration).
- **Readiness.** `waitIdle` (never a fixed sleep), then capture.
- **Capture.** `grim -o HEADLESS-1
  <repo>/tmp/gui-shots/<area>/<case>-<name>.png` (`Nested::screenshot`;
  menus and tooltips included; a GL client doesn't tear *(spike)*), with
  every log of the case in `<area>/logs/`: sway, the bus, each window's
  output, and, copied when the case ends (passed or failed, before its
  `TestEnv` goes), each window's own GUI log and the projects'
  `.qrec/logs`.
- **Checks per case.**
  1. **Regions** (objective, fails the test): `regions` against the
     case's rects, ±1 px (× scale). The base set, BRIEF §2.1 at
     1600×1000 with the ruler's corner cell counted in the ruler (so the
     track headers and the time area start at y 720, as in the pen
     frames): `region-header` (0, 0, 1600, 48), `region-stage` (0, 48,
     1216, 608), `preview` (128, 64, 960, 540), `transport` (128, 612,
     960, 36), `region-panel` (1216, 48, 384, 920), `region-timeline` (0,
     656, 1216, 312), `timeline-toolbar` (0, 656, 1216, 36), `ruler` (0,
     692, 1216, 28), `track-headers` (0, 720, 176, 248), `time-area` (176,
     720, 1040, 248), `video-row` (176, 720, 1040, 76), `lane-A1` (176,
     800, 1040, 56), `lane-A2` (176, 860, 1040, 56), `region-hintbar` (0,
     968, 1600, 32). Case extras: the bin (0, 48, 352, 608) with the
     preview at (368, 100, 832, 468) and the transport at (368, 612, 832,
     36); a toast whose bottom is at 956, centered on 800; the export panel
     (1152, 52, 440, h); a modal 520 wide centered on 800; Settings (380,
     180, 840, 640); scrims (0, 0, 1600, 968). Since round 3 every one of
     the base set is required on the design fixture
     (`gui_support::MERGED_REGIONS`: the stage's preview and transport and
     the timeline's parts came with WP4 and WP3); `WP0B_REGIONS` (the five
     regions) stays for layouts that move the parts.
  2. **Hints** (objective): `hints` equals the BRIEF list for the state.
  3. **Accessibility** (objective): `a11y` is empty.
  4. **Look** (by eye): `ffmpeg -i <shot> -i design/screens/<Sxx>.png
     -filter_complex hstack=inputs=2 <case>-compare.png`, SSIM printed for
     information only. Type, color, spacing and copy are judged here by the
     integrator and the PO.
- **Fixture.** `qrec_shell::testkit::design::design_fixture(dir,
  DesignVariant)` (WP0a):
  - `Base` (BRIEF §3): name `rust-traits-talk`, canvas 1920×1080@30,
    takes 1–9 with their numbers, statuses, durations and facts
    (`recovered` on take 7; take 9 broken with `EOS_TIMEOUT_REASON`; take 6
    tooShort; take 5's `replaced` holding take 4's clip; take 2 placed with
    no clips); clips c7f3 … c3e9;
    - the project scene on fake-backend sources with BRIEF's names: L1
      "Main screen" on `FAKE-1`; L2 "Face" on `/dev/fake0` at (1500, 760,
      384, 288), cover; A1 "Yeti Nano" on `alsa_input.fake` (the autocut
      track); A2 "System audio" on `alsa_output.fake.monitor`, −6 dB; take
      scenes on the recorded files with the same names;
    - media from `testkit::fixture` at 640×360 (screens) and 320×240
      (camera) with frame IDs, and audio whose speech intervals give take
      8's A1 the four BRIEF §3.6 regions and take 5's A1 the two silences
      of BRIEF §3.3 (WP0a tunes them until the dry runs match);
    - `.qrec/ui.json` presets (playhead, selection, zoom, scroll, canvas
      mode, bin) written per case before launch.
  - `Take10Landed`: `Base` plus take 10 (donkey, 4,842 frames, placed as
    the 11th clip), for S05b, with take 10's placement as the history's
    one entry ("Record take 10", `cmd` `{"event":"takeFinalized"}`, the
    cursor on it; the base is the timeline before it), so `[D] Discard
    last take` is emphasized (G§4.3). (Added in the WP0a review.)
  - `MissingL2`: `Base` with take 5's L2 marked `missing` (S13b's
    "Missing in this take" row, C16's missing-media clip).
  - `Smoke`: `Base`'s scene and takes with a 3-clip, 90-frame timeline, so
    a Draft export takes seconds (the smoke run, S16, S16b).
  - `perf_fixture(dir, 300)`; `playback_fixture(dir)` (WP1's
    measurement).
- **Fake hooks** (WP0a, `capture/fake.rs`; honored only with
  `QREC_CAPTURE_BACKEND=fake`): `QREC_FAKE_EOS_DELAY_MS` (EOS waits; S05);
  `QREC_FAKE_PORTAL=deny|cancel` (the fake screens come back refused;
  S13b, S24); `QREC_FAKE_RECORD_OFFSET_FRAMES` (added to `RecordClock`'s
  elapsed time: 4842 gives S04's 00:02:41:12 timer and ghost, 932 gives
  S04b's 00:00:31:02); `QREC_FAKE_INVENTORY=design` (the fake sources carry
  BRIEF's labels and sizes: "GIGA-BYTE GS27QA" 2560×1440, "Dell U2720Q"
  3840×2160, "Logitech C920", "Yeti Nano", "Built-in Audio Analog
  Stereo").
- **Known differences** (by design; regions, hints and view-model
  snapshots cover them): `FAKE-1`, `FAKE-2` and `/dev/fake0` instead of
  `DP-4`, `HDMI-A-1` and `/dev/video0`; test patterns instead of the
  generated screen and webcam images; file sizes; elapsed and ETA times;
  the export caption "software encode (x264)" (the fake backend probes no
  VA device; the VA-API caption is a `view::export` test); take 2 reads
  "Removed from the timeline." because the fixture writes no history but
  `Take10Landed`'s one entry ("Discarded with D." is a `view::bin`
  snapshot). Recording times aren't a difference: the client env pins
  `TZ=UTC0`, so they read BRIEF's on any machine.

**Cases** (the owner's `crates/qrec/tests/gui_<area>.rs`, `#[ignore]`):

| Case | Fixture and presets | Steps |
| --- | --- | --- |
| S01a | empty folder | launch |
| S01b | empty folder | launch; `wtype -k Return`; `waitIdle` |
| S02 | Base, Live, playhead at the end | launch |
| S03 | Base, `recording.countdownSecs` 3 | `qrec record start`; `waitIdle` (frozen at "3", full ring) |
| S04 | Base, `QREC_FAKE_RECORD_OFFSET_FRAMES=4842` | `qrec record start`; wait for phase recording; `waitIdle` |
| S04b | Base, selection [c2d0], offset 932 | `wtype -k o`; wait for recording |
| S05 | as S04, `QREC_FAKE_EOS_DELAY_MS=8000` | record; `qrec record stop` in the background; wait for phase finalizing |
| S05b | Take10Landed, Live | launch |
| S06 | Base, Program, selection [c2d0], playhead 2437 | pointer over c2d1; wait for the tooltip |
| S07 | Base, Program, no selection | pointer over c1b6 |
| S08 | Base, Program, zoom 1.6, scroll at 00:01:05:00; `qrec autocut --clips c2d0 --dry-run` before launch | launch |
| S09 | Base, Program, zoom 32, view from frame 3160, selection [c2d1], playhead 3176 | launch |
| S10 | as S08, selection [c2d0] | press on c2d0's start edge, move +46.4 px, `waitIdle`, capture, `Esc` |
| S11 | as S07 | press c8a1, move to x 405, `waitIdle`, capture, `Esc` |
| S12 | Base, Program, selection [c3e9], playhead 4842 | `wtype -k z`; `wtype -M shift -k c -m shift` (`gui_timeline`: a click on the toolbar's settings icon, then the preview's pixels probed); `waitIdle`; click region row 1; `waitIdle` |
| S13 | Base, Live | press L2's center, move until the left edge snaps at 1280, `waitIdle`, capture |
| S13b | Base; `QREC_FAKE_PORTAL=deny` for the error row; `MissingL2` for the missing row | `qrec scene layer set L1 --visible false`, then `--locked true`; L2's overflow → Rename (renaming); select c2d0 in `MissingL2` (missing); crop the panel per state |
| S14 | Base, Program, bin open | click take 4's row |
| S15a | Base, `QREC_GUI_TEST_NOW=2026-10-14T15:30:12` | `wtype -M ctrl -k e -m ctrl` |
| S15b | as S15a | `wtype -k Return`; poll `qrec status --json` until the job is at ≥ 42 %; capture |
| S15c | as S15a | click WebM, capture; S15b again with `ui.reduced_motion = true` (plain bar) |
| S16 | Smoke with its own S06 preset (its middle clip c1b2 selected, the playhead at 45 in it, Face selected; S06's c2d0 and 2437 aren't on its 90-frame timeline), panel open | the ruler's corner and the transport read one playhead (00:00:01:15); export; `waitIdle` after done; then `qrec export` over the Done page: the panel follows it and closes when it ends, with the core's toast (WP7 integration audit) |
| S16b | as S16, `tests/support/fail_ffmpeg.rs`'s slow wrapper first on the GUI's PATH (each render unit starts 3 s late, so the panel closes while the export runs on any machine; WP7 integration audit) | export; wait for the Running page; `wtype -k Escape`; `waitIdle` after the toast |
| S17 | Base, `tests/support/fail_ffmpeg.rs`'s wrapper first on the GUI's PATH (units 1 and 2 render, unit 3 fails after them and the case's go-ahead, the rest wait) | export; "Rendering clip 3/10" while unit 3 waits, then `fail_ffmpeg::go` (it waited a fixed 2 s until the WP7 integration audit); wait for Failed: "clip 3/10 (take 3, c1b2)", "Failed at 11%"; Try again sends it again, and the wrapper's second failure of unit 3 (`fail_ffmpeg::failures`) ends on Failed again (review fix: nothing pressed it) |
| S18 | Base, bin open | click take 2, Ctrl+click take 4, click Purge from disk |
| S18b | Base, Live | `wtype -M ctrl -k comma -m ctrl`; click Frame rate 25 |
| S19–S25, X1–X3 | per WP8 and WP9 | — |

- **Scale runs**: S02, S06, S09 and S14 at `QREC_TEST_SCALE=2` and 1.5:
  regions scale; the live sink's caps are 1920×1080 at 2; strip textures
  are 144 px high; a scripted trim like S10 hits the same edge. A case
  takes its pixel probes in device px (× `opts.scale`) and passes
  `opts.scale` to `assert_regions` (WP0b's gui_shell cases run at scale 1
  only). The cases (WP7 integration, each in its owner's suite):
  `gui_stage`'s `s02_at_scale_2`, `s02_at_scale_1_5` (the live picture
  probed in device px, the sink's size from the GUI log),
  `s06_at_scale_2`, `s06_at_scale_1_5` (the pointer over c2d1 for its
  tooltip, as at scale 1: the pointer moves in the output layout's logical
  px; the runs had no pointer until the WP7 integration audit);
  `gui_timeline`'s
  `s09_frame_level_at_scale_2`, `_1_5` and `s10_trimming_at_scale_2`,
  `_1_5` (the same logical drag trims c2d0 to 41 at every scale);
  `gui_bin_export`'s `s14_the_bin_drawer_at_scale_2`, `_1_5`. The image
  heights come from `gui_support::preview_heights` (each strip and frame
  run's `scale=-2:<h>` in the project's `preview.log`). At 1.5 GTK draws at
  the window's whole scale factor, 2 (`gui_support::scale_factor`), and the
  compositor scales the buffer down: the sink is 1920×1080 and the images
  144 px high there too.
- **Reduced motion**: S03 (static ring), S04 (static badge), S15c (plain
  bar) with `ui.reduced_motion = true`; zoom is a GTK-free test; the
  panel's page switch is `gui_panel`'s
  `the_pages_slide_unless_motion_is_reduced` (`local`'s
  `panelTransition`). The cases: `gui_stage`'s
  `s03_countdown_reduced_motion` and `s04_recording_reduced_motion`,
  `gui_bin_export`'s `s15c_webm_setup_and_the_reduced_motion_bar`.
- **Adaptive**: S02 at 1300×1000 and at 1000×1000 (sheet closed, then
  opened by the header button); S06 at 1600×800. The cases (WP7
  integration, in `gui_stage`, S02's and S06's owner's): `s02_at_1300_wide`,
  `s02_at_1000_wide_with_the_sheet`, `s06_at_1600_by_800`; each checks the
  regions against G§3.15 (the preview by `view::canvas::stage_geometry`),
  the layout class and the hint bar's items whole (`hints`' `shown` and
  `cut`: at 1000 the trailing ones are left out, at 1300 and 1600 none is;
  the bar clipped them until the WP7 integration audit, the `?` cap
  showing without "All keys"). WP0b's `gui_shell` has the same three
  layouts on the fixture's default UI
  (`medium_windows_dock_the_panel_at_320`,
  `narrow_windows_show_the_panel_as_a_sheet`,
  `short_windows_keep_a_232_px_timeline`).
- **Suites** (round 3): `gui_shell` (WP0b's flows and the integration
  cases: `the_first_runs_count_reaches_the_hint_bar`, WP4's checklist
  driving WP6's "[Enter] Add N sources"; `a_ruler_scrub_leaves_the_player_on_the_playhead`,
  WP3's scrub through WP4's sync with no seek after it, read from the
  GUI log's "program sync:" lines and `local`'s `playerNs`), `gui_smoke`,
  `gui_chrome` (WP6), `gui_timeline` (WP3), `gui_stage` (WP4), `gui_panel`
  (WP5), `gui_bin_export` (WP7), each run with `-- --ignored
  --test-threads=1` on pixman and on gles2. WP10 adds `gui_text` (G§3.14:
  `the_text_setup_is_manual_with_whole_pixel_advances`, and
  `the_header_title_leaves_no_trace_on_gl`, whose windows run GSK's GL
  renderer in software under either compositor renderer, with GTK's
  automatic mode as its control: 12 px of mark there).

### G§8.5 End-to-end smoke (SPEC §17)

`crates/qrec/tests/gui_smoke.rs` (WP0b writes steps 1–3 and 10, WP10
completes it). Nested sway, the GUI host on `design_fixture(Smoke)`,
`QREC_CAPTURE_BACKEND=fake`:

1. `qrec status --json` → `host: "gui"`, pid = the GUI's.
2. `qrec clip split --timeline --at 45f --json` → a seq; `qrec clip list
   --json` has one more clip; screenshot.
3. `qrec undo`, `qrec redo`, `qrec undo`.
4. `qrec record start --json` (routed, nothing spawned) → status
   recording; `waitIdle`; screenshot with REC; `qrec record stop --json`
   → a placed `TakeFinalizedView`; one more clip.
5. `qrec take discard-last` → the clip is gone; the bin count is one more.
6. `qrec seek --to 10f`, `qrec play`, 1 s, `qrec pause` → the playhead is
   past 10; screenshot in Program.
7. `wtype -k r` → recording; `wtype -k r` → stopped (keys reach the GUI).
8. `qrec export --quality draft --json` → progress lines and done; the
   file exists; ffprobe shows the canvas size and fps.
9. `qrec frame save --at 0f` → a canvas-size PNG.
10. SIGTERM → exit 130 within 15 s; the lock is free.

Throughout: no `Crashed`, no `ERROR` line in the GUI log, and the a11y walk
is empty. The main-loop probe (G§8.6) records stalls; a stall over 50 ms
fails the run only under `QREC_TEST_WLR=gles2` (with pixman, the
compositor, llvmpipe and ffmpeg share the CPU).

### G§8.6 Performance and robustness probes

- **Main loop** (`QREC_GUI_PROBE=1`): a 10 ms GLib timeout logs any drift
  over 50 ms with the name of the dispatch running.
- **Frame time**: frame-clock intervals (p50, p99) while zooming and
  scrolling the 300-clip perf fixture. Target p99 < 16.7 ms, asserted only
  under `QREC_TEST_WLR=gles2` on the dev machine (WP3), recorded under
  pixman (CI). With WP5's panel the meters first ran a tick callback each:
  the warm p99 went bimodal (16.1x or 16.7x ms, about half the runs over
  16.7, also with their levels pinned and nothing redrawn; 16.53–16.55
  without the callbacks). On a 30 Hz timer: 16.55–16.60 ms in 6 of 6 runs
  (WP5 integration).
- **Program playback**: WP1's measurement (G§3.10).
- **Live preview CPU**: manual, against *(spike)*: 29 % for one 1080p30
  BGRx stream into the sink with GSK GL, 46–65 % for a two-source 1080p30
  composite; ours composites at 960×540 and should cost less.
- **GPU device opens**: `gui_support/scripts/gpu-opens.sh` (strace) shows
  only the chosen render node.
- **Cache bounds**: `ThumbCache::bytes()` and `PeakCache::bytes()` stay
  under budget while a one-hour fixture scrolls (WP2, in the nested
  texture run: at most 24,330,240 bytes of a 24 MiB thumbnail budget; an
  hour of peaks, 3,600,000 buckets, in 15,356,252 of 134,217,728 bytes).

### G§8.7 Manual checks for the product owner

At the M4 review:

1. The slice next to the PNGs (`tmp/gui-shots/*-compare.png`), 1600×1000
   at scale 1, plus 1300 and 1000 wide and 800 high (G§0.3).
2. A real niri session: Live preview of the real screens and camera; the
   portal picker once per monitor, restored after a restart; meters; the
   record loop (R latency, torch, ghost, the take landing, D, O); niri
   binds while the window is open (`Mod+Shift+R/O/D`), and one pressed
   while the window is still opening.
3. Program playback with 2 screens and a camera at canvas fps, or the H-9
   fallback with its numbers (G§0.4); frame-accurate stepping; J/K/L.
4. Export done and failed; the purge modal; the fps-change modal from
   Settings › Project.
5. The departures marked PO in G§2: D-04, D-07, D-08, D-17, D-30, D-48,
   D-51 (a step while playing pauses first); and D-03 (meters in Program,
   as drawn).
6. Fraunces axes: DESIGN's SOFT/WONK against the PNGs' defaults.
7. The interim pages of G§1.1 #2, while G§0.2 is open.
8. Closing while recording and while exporting (D-12).
9. Text rendering (G§3.14, WP10's experiment): Qrec keeps whole-pixel
   advances (its texts up to 10 px off the frames' widths, the mono
   strings 0.2–0.4 px per character) because GTK's setups that match the
   frames bring back the header title's stray mark on GL. The crops and
   numbers in `tmp/text-rendering/`: `quality/` (each setup next to the
   frame: 8× at scale 1, 4× at scale 2, and the mark itself,
   `gl-stray-mark-automatic.png`), `out-scale1/` and `out-scale2/` (each
   measured text, and `widths.md`), `gl/` (the GL runs).

At M5: the drafted screens as built; the Swamp Day, Dragon's Keep and Ogre
Mode proofs; reduced motion; 125 % and 150 %.

---

## G§9 Risks and open questions

Each line has the default the build follows until someone decides
otherwise.

| # | Risk or question | Default |
| --- | --- | --- |
| R1 | Disk: 17 GB free, 5.1 GB in the integration target, about 6.5 GB per GUI build | two lanes (G§7.0, G§7.1); `cargo clean` after each merge; one lane when `df` says so |
| R2 | Reusing one `gtk4paintablesink` across live pipeline rebuilds is unproven (stale frame, a flash, finalizing off the main thread) | keep the element, NULL → remove → add; WP1 tests it first. If it fails: a new sink per rebuild, created on the GTK thread on request through the side channel (the engine waits ≤ 1 s, else builds without preview) |
| R3 | Rebuilding a GES timeline in place while playing may glitch | `load` pauses, rebuilds, commits and resumes (G§3.10) |
| R4 | Reverse playback by stepping costs CPU | acceptable for J; at most 15 seeks/s |
| R5 | Frame-zoom thumbnails need many short ffmpeg runs | blocks of 64 frames, a byte-bounded LRU, the strip image while loading |
| R6 | Keyframes aren't exactly 1 s apart (x264 scene cuts), so strip images can be up to 1 s off | fine for thumbnails; exact frames only at frame zoom |
| R7 | No portal or GSettings in the tests: FileDialog and FileLauncher fail there | the cases avoid them; failures toast |
| R8 | Symbolic SVGs with strokes may not recolor | fills only; WP0b checks recoloring |
| R9 | pen.dev has no SVG export, only reads are allowed, and the MCP reads the active document | art from `Get(includePathGeometry)` written to SVG by script while `design/qrec.pen` is active (G§0.6) |
| R10 | The drafts aren't approved (SPEC §15.1, CLAUDE.md) | gated on G§0.2; interim pages carry the SPEC-required behavior |
| R11 | `adw::Dialog` dims the whole window; the frames leave the hint bar live | custom overlay layers (G§3.15) |
| R12 | `gtk-interface-reduced-motion` might be missing at runtime | then `gtk-enable-animations` only |
| R13 | `hello` waits while the session thread gathers slow facts | `SharedSources` (H-5); an `AudioAdd` with an unknown source may still run `pactl` (≤ 3 s) |
| R14 | Cloning `State` per snapshot, now after every assignment (H-6) | WP0a logs the clone time at debug level; switch to `Arc<State>` above 1 ms |
| R15 | Q9: follow GNOME light/dark? | no; the theme is a manual choice |
| R16 | Program playback may miss canvas fps (software decoding, canvas-size compositing) | WP1 measures first; the H-9 fallback; the PO decides on proxy files (G§0.4) |
| R17 | The squelch sound has no asset | not built until an asset is approved |
| R18 | A desktop launcher runs `qrec` in `$HOME`, which gets X2 | `Exec=qrec`; ask the PO whether the launcher should take a folder argument |
| R19 | A second GUI can't raise the first window | refuse with the D-13 page; raising is later work |
| R20 | Lane `M` semantics (D-08) | playback only; ask the PO |
| R21 | The meter monitor opens a mic a second time | PipeWire allows it; a device that refuses shows idle meters |
| R22 | gtk4-rs `gdk::Texture` isn't usable off the main thread | textures are built on the main thread from shared bytes (G§3.11) |
| R23 | Parallel worktrees editing near the same files | ownership (G§3.16, G§3.17); WP0a and WP0b create every file as a stub; shared files only through the integrator |
| R24 | `Take.recovered` makes older dev builds reject a project that has it | accepted before the first release; the first schema change after it bumps `version` (G§4.8) |
| R25 | `gtk::Svg` (GTK 4.22's SVG subset) may misrender the generated art | WP0b checks first; PNG fallback (G§3.14) |
| R26 | The adaptive layouts have no frames, and niri tiles windows narrower than 1400 px | DESIGN's rules, built per region from WP0b on (G§1.1 #4); sign-off or frames (G§0.3) |
| R27 | M4's approved hint bars show `Ctrl+K` and `?`, but the palette and cheat sheet are M5 and gated | the hints as drawn; the "Not built yet" toast (G§1.1) |
| R28 | D-47 lets UI commands overtake parked edits | parked commands resolve the selection and playhead when they run, as every command does at its turn |
| R29 | Test mode turns animations off, which D-30 reads as reduced motion | test mode takes reduced motion from the config only (G§8.4) |
