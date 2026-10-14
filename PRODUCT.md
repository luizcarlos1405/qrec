# Product

<!-- impeccable:product-schema 1 -->

> Source: `SPEC.md` (the product-owner interview, 2026-10-14). No separate interview was run for this file. Facts come from the spec unless marked **[inferred]**. Inferred items are assumptions, listed again as open questions in `design/BRIEF.md` §8.

## Platform

web

## Users

- **Primary: a developer or creator recording tutorials, talks and demos on a Linux desktop.** They run Wayland (the product owner runs niri on NixOS) and use one or more monitors, a webcam and a USB mic. They are power users and keyboard-first. They record in short takes, often redo the last one, and want the video mostly edited by the time they stop. **[inferred]** They know NLE basics (timeline, trim, split) and OBS-style scenes. They record in a dim room lit by monitors and a ring light. Between takes they glance at Qrec for seconds; while recording, its window may sit on another monitor or be covered.
- **Secondary: LLM agents and scripts driving the CLI** (`qrec … --json`). They read `qrec.json`, run the same commands as the GUI, and must get identical behavior. They never see the GUI. The GUI teaches the shared vocabulary by showing each command's CLI form, for example `Split clip — s — qrec clip split`.

## Product Purpose

Qrec (_Quick Recording_, rhymes with Shrek) is a Wayland screen and camera recorder built for one loop: **press R, talk, press R, repeat.** Each stop adds a take to a magnetic timeline. Trimming, cutting, reordering and autocut (silence removal) are built in, so the video is mostly edited when recording ends. The finished timeline exports straight to MP4 or WebM.

Success looks like this:

- The user goes from launching Qrec in a folder to an exported video without leaving the window or opening a dialog to record.
- Nothing recorded is ever lost by accident.
- Everything done in the GUI can be repeated from the CLI.

## Positioning

- **One binary, one command dispatcher.** The GUI and the CLI share one `Session::dispatch`, so an LLM can do anything the UI can, and it runs the same code.
- **Recording and editing are one surface.** Takes land on the timeline as you record. `d` discards the last take, `o` records over a selection, and autocut can run after every take.
- **Native, frame-accurate, non-destructive.** GTK4 + libadwaita, a GStreamer capture engine with one clock, and integer-frame timeline math. Media is never modified, and undo history is shared with the CLI.
- **Ogres have layers.** OBS-style layers (any number of screens and cameras) are called _Onion layers_, inside a fully swamp-themed app.

## Operating Context

- **Platform note.** Qrec is a **native Linux desktop app** (Wayland; GTK4 + libadwaita with a fully custom CSS theme; custom `GtkSnapshot` widgets for the canvas and timeline). Impeccable's platform field only accepts `web`, `ios`, `android` or `adaptive`, so `web` is recorded above: it is the engine's fallback and loads no mobile guidance. Treat every surface as a native GNOME-style desktop window. That means a libadwaita header bar, native widget behavior and focus handling, pointer, wheel and keyboard input, and no touch targets. The theme ships as GTK CSS variables in `crates/qrec-gui/resources/theme/*.css`.
- **Environment.** Wayland (niri first; GNOME and KDE should work through the portal but are untested). Screens come from the xdg-desktop-portal ScreenCast picker, shown once per monitor. Audio comes from PipeWire (`pipewire-pulse`). The stack also needs GStreamer ≥ 1.24, ffmpeg and ffprobe, and optionally `wl-copy`.
- **Launch ritual.** Run `qrec` in the project folder. The folder must be empty or contain `qrec.json`; anything else gets _"Not my swamp: this folder isn't empty and has no qrec.json"_. There is no project browser.
- **Global binds.** niri binds such as `Mod+Shift+R` run `qrec record toggle` while another app has focus. `qrec doctor` prints the snippet.
- **Files on disk.** `qrec.json` (the project), `takes/take-NNNN/` (one file per layer and track, plus `recorder.log`), `exports/` and `frames/` (timestamped, never overwritten), and `.qrec/` (lock, history, UI state, cache, logs).
- **Default canvas.** 1920×1080 at 30 fps. Times are integer frames shown as `HH:MM:SS:FF`.

## Capabilities and Constraints

- **Recording.** Any number of screen and camera layers, plus zero or more audio tracks (mic and system audio recorded as separate tracks). Recording starts with no delay. The countdown is optional (0–10 s, default 0). A 500 ms health check catches failures early. Takes shorter than 10 frames are marked `tooShort` and go to the bin. Crashed takes are recovered on startup.
- **States.** `Idle → Countdown → Recording → Finalizing → Idle`. While recording, editing is disabled; only stop (`r`), cancel (`Esc`), selection, seeking, playback and view changes work. Recording is blocked while an export runs.
- **Canvas.** Two modes, switched with `Tab`. **Live** shows the live sources with the project scene. **Program** plays the timeline at the playhead, and edits the selected clip's take layout. Layers can be moved, resized, cropped (`Alt`-drag) and nudged, with snapping to edges, center and thirds.
- **Timeline.** One magnetic, gapless track. Clip width is proportional to duration, with no minimum width. Zoom runs from fit-all down to frame level (≥ 24 px per frame). The video row is a filmstrip with a take label and a take color stripe. There is one waveform lane per audio track, with mute and solo. Silence regions are hatched. Snapping is toggled with `n`.
- **Editing.** Select, move, trim (with an edge-frame preview), split, remove, insert from the bin, autocut (with a preview), and edit a take's layout. Undo and redo persist and are shared with the CLI.
- **Output.** Export to MP4 (H.264 + AAC) or WebM (VP9 + Opus, AV1 optional) at Draft, Standard or High quality. Progress shows phase, percent, elapsed time and ETA, with a cancel action. Editing can continue during an export. **Save frame** (`p`) writes a canvas-size PNG.
- **Non-destructive rules.** Media is never modified. Purge, overwriting an export path, clearing history, and changing fps on a project with takes all need confirmation in a modal titled _"Get out of my swamp?"_ that states exactly what will be lost.
- **Discoverability.** A context-aware hint bar, a searchable `?` cheat sheet, and a `Ctrl+K` / `:` command palette that shows each command's key and CLI form. Tooltips and menus always show the shortcut. Keys can be rebound in `keys.toml`, and conflicts are reported.
- **Terminology** (use these words exactly): Project, Source, Layer (shown as "Onion layers"), Audio track, Scene, Take, Clip, Timeline, Bin, Canvas, Live, Program, Autocut, Purge.
- **Non-goals for v1:** X11, a project browser, multi-track editing, B-roll, transitions, titles, effects, single-window capture, and drawing a screen region.

## Brand Commitments

- **Name and voice.** The name is **Qrec**, the app ID is `com.rsensato.qrec`, and the tagline is _"Ogres have layers. So do your recordings."_ The whole app is swamp-themed (Shrek), but swamp flavor goes **only in secondary text**. Primary labels stay plain verbs ("Record", "Export"), so the app stays usable.
- **Pinned strings** (verbatim):
  - Empty state: _"This swamp is quiet. Press **R** and make some noise."_
  - Destructive modal title: _"Get out of my swamp?"_
  - Export done: _"Layers! Like an onion. Your video is ready."_
  - Too-short take: _"That was shorter than Lord Farquaad. Take moved to the bin."_
  - Recovered take: _"Recovered take 7 from the bog."_
  - Missing dependencies: _"The swamp is missing something"_
  - Bad folder: _"Not my swamp: this folder isn't empty and has no qrec.json"_
- **Binding visual constraints** (SPEC §15). Four themes with a fixed token table: Swamp Night (default), Swamp Day, Dragon's Keep and Ogre Mode. Three fonts, all bundled: a storybook serif for display moments only, a hyperlegible UI sans, and a tabular monospace for timecodes. The icon is an onion with ogre-ear trumpets, also used as the REC badge motif. Motion is subtle: a torch-flicker REC pulse, a bubbling mud export bar and eased zoom. An optional "squelch" sound on record start and stop is off by default. Details live in `DESIGN.md`.
- **Process gate.** Every screen and state is designed in pen.dev (`design/qrec.pen`) and approved before any GTK UI code is written. Tokens flow from pen.dev variables into the GTK CSS.

## Evidence on Hand

- `SPEC.md`: the full product spec. `docs/ARCHITECTURE.md`: core model, commands, events and toasts. `docs/RECORDING.md`: capture lessons learned.
- Prior attempts give context only: `qrec-tui`, `qrec-electron`, `qrec-sky` and `qrec-glm`.
- **Absent:** the app icon artwork, illustrations, screenshots, user research, usage data and testimonials. The bundled font files aren't in the repo yet. Do not invent metrics, quotes or users. All sample projects, takes, file sizes and screen content in `design/BRIEF.md` are synthetic and labeled as such.

## Product Principles

1. **The hot loop is sacred.** `R` always works with one key and no dialog. Nothing on screen competes with the recording state, and recording failures surface within half a second at the source that failed.
2. **Show the key, show the command.** Every action shows its shortcut where it appears (hint bar, buttons for the hot loop, tooltips, menus). The palette also shows its CLI form, so humans and agents share one vocabulary.
3. **Nothing is lost by accident.** Removing a clip is undoable and keeps the take. Deleting media is rare, explicit, torch-red, and spells out exactly what will go.
4. **Frame-true.** Timecodes, clip widths and previews are exact to the frame. The UI never rounds in ways that hide what will be exported.
5. **Swamp in the margins.** Personality lives in secondary text, empty states, the icon and a few moments of motion. The working surface reads as a serious editor.

## Accessibility & Inclusion

- **WCAG 2.2 AA in all four themes.** Text needs 4.5:1, large text 3:1, and UI indicators and focus rings 3:1 (SC 1.4.11). Measured results and the proposals needed to meet them are in `DESIGN.md` → Colors.
- **Color-blind-safe recording state.** REC always combines a shape (the ● onion glyph), a pulse and the word "REC". The `record` color must stay distinguishable from `accent` under simulated protanopia and deuteranopia. Take identity is never carried by color alone: the take number appears on the clip and in tooltips.
- **Reduced motion.** It follows `ui.reduced_motion` in `config.toml` and the system setting. Every animation has a defined fallback, and live meters and progress stay.
- **Keyboard and focus.** Everything works from the keyboard, with a visible 2 px focus ring. Destructive confirmations focus **Cancel** first.
- **Text size.** A UI scale setting is available. The type floor is 12 px for UI text and 11 px for monospace labels and key caps.
- **Screen readers.** Icon-only buttons carry accessible names that include their shortcut. **[inferred]** Recording start and stop, and export completion, are announced.
- **Sound** is off by default.
