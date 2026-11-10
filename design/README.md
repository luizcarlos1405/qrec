# Qrec design

The GTK GUI is designed before any UI code is written ([SPEC](../SPEC.md) §15.1). **The designs are HTML boards in [`boards/`](boards/)**, shown on an [htmlboard](https://www.npmjs.com/package/htmlboard) canvas. Until 2026-11-10 they were drawn in pen.dev (`qrec.pen`); every frame of that document was ported then, and `qrec.pen` stays only as the archive of the approved original.

The first round was a **vertical slice in the main theme, Swamp Night**: 26 frames covering what the GUI milestone M4 (SPEC §18) needed first (first run, the record loop, the timeline editor, layer editing, the bin, export and the destructive confirmations), approved by the product owner on 2026-10-15, plus the drafts S19–S25 and X1–X3, approved as drawn the same day. The tables below still describe them.

## Open the canvas

```sh
npx htmlboard@0.9.0 design/boards --root . -p 3060   # from the repo root, then open http://localhost:3060
```

`--root .` because the boards load the sample screen and webcam images from `DESIGN-assets/`. The canvas's theme switch shows every board in Swamp Night, Swamp Day, Dragon's Keep and Ogre Mode.

| Folder | What is there |
| --- | --- |
| `00-foundations/` | Tokens (all four themes side by side) and the artwork |
| `01-components/` | The nine component sheets, C01–C34 |
| `02-record-loop/` | The main window template, S01a–S06 and the S04 torch notes |
| `03-timeline/` | S07–S12 |
| `04-layers-bin-export/` | S13–S17 |
| `05-dialogs-and-settings/` | S18, S18b, S19–S22 |
| `06-errors-and-notices/` | S23–S25, X1–X3 |
| `07-named-scenes/` | Issue #3, named scenes (not approved yet; its notes board lists the open questions) |

- **The ported boards** (00–06) are pen.dev's html-css export of each frame: absolutely positioned, as pen drew them, with every color a theme token (`var(--qrec-*)`). Each file names its pen frame id in a comment. Rendered in a browser they match pen's own export of the same frame to within 0.3% of pixels (text antialiasing). Edit them in place for small changes; new work is better written as new boards.
- **New boards** (07 on) are written by hand on the shared chrome: `boards/styles.css` (type roles, buttons, key caps, fields, popovers, toasts, modals) and `boards/components/` (`<layer-row>`, `<audio-row>`, `<scene-thumb>`, `<scene-panel state="…">`). `npx htmlboard@0.9.0 catalog design/boards` lists them.
- **Tokens:** [`tokens/qrec-tokens.json`](tokens/qrec-tokens.json) is the source. `QREC_UPDATE_TOKENS=1 cargo test -p qrec-gui --test tokens` regenerates `boards/themes.css` with the GTK theme CSS and `palette.rs`, and the test fails when they drift.
- **Agents:** the htmlboard skills are in `.claude/skills/htmlboard*` (and `.agents/skills/`). After an htmlboard upgrade, run `npx htmlboard@<version> skills install` and bump the version above.
- The PNGs in [`screens/`](screens/) are 1:1 exports of the approved pen frames: 1600×1000, except S13b (384×920) and S15c (1040×769).
- All sample data is synthetic (BRIEF §3). The screen content and the webcam person are AI-generated images. The icon, the REC glyph and the illustrations are generated SVGs.
- The node ids below are the pen frames the boards came from.

## Screens in the slice

### Row 1 · Record loop (y 6400)

| Screen | Node | What it shows | PNG |
| --- | --- | --- | --- |
| S01a · First run, no sources | `Nb8Af` | Pick the detected sources, then **Add 4 sources [Enter]**. Record is disabled and its tooltip says why. | [S01a](screens/S01a-first-run-no-sources.png) |
| S01b · First run, sources added | `ZMsnB` | Live preview with idle meters; the timeline reads "This swamp is quiet. Press R and make some noise." | [S01b](screens/S01b-first-run-sources-added.png) |
| S02 · Live mode, idle | `AAwr6` | The resting state between takes: the whole window, 10 clips at fit zoom. | [S02](screens/S02-live-idle.png) |
| S03 · Countdown | `bQm7H` | A 3 s ring over the dimmed canvas; Record becomes an outlined **Cancel [R]**. | [S03](screens/S03-countdown.png) |
| S04 · Recording | `VCWz6` | The REC torch badge and timer, **Stop [R]**, a red line under the header, and the ghost clip growing at the end. | [S04](screens/S04-recording.png) |
| S04 notes · REC torch flicker | `d4rx6s` | Flicker keyframes (frames A and B) and the reduced-motion static badge. | canvas only |
| S04b · Recording over c2d0 | `HrMeR` | Record over (`O`): the ghost takes c2d0's slot and the replaced clip is hatched. | [S04b](screens/S04b-recording-over-c2d0.png) |
| S05 · Finalizing | `T9p3bU` | "Saving take 10…", a disabled **Saving…** button and the striped ghost. | [S05](screens/S05-finalizing.png) |
| S05b · Take lands | `ZozhM` | The ghost settles into a solid "Take 10" clip (11 clips); `[D] Discard last take` is emphasized. | [S05b](screens/S05b-take-lands.png) |
| S06 · Program with selection | `jnKIl` | Program mode with c2d0 selected: the take 5 layout banner, L2 Face in the inspector, a clip tooltip. | [S06](screens/S06-program-with-selection.png) |

### Row 2 · Timeline (y 7600)

| Screen | Node | What it shows | PNG |
| --- | --- | --- | --- |
| S07 · Timeline at fit zoom | `m5RAx` | Nothing selected: the ruler every 30 s, and clip labels collapsing with width. | [S07](screens/S07-timeline-fit.png) |
| S08 · Seconds zoom | `I0MXCt` | 48 px/s around the playhead, with silence hatches on A1. | [S08](screens/S08-timeline-seconds.png) |
| S09 · Frame level | `uZjSh` | 32 px/frame at the c2d0/c2d1 cut: one thumbnail and one frame number per frame. | [S09](screens/S09-timeline-frame-level.png) |
| S10 · Trimming | `nZyqg` | Dragging c2d0's start edge by +29 f; the preview shows the new first frame. | [S10](screens/S10-trimming.png) |
| S11 · Drag-move | `rPfN1` | Moving c8a1 after c1b6: the lifted copy, the insertion caret, "slot 7 of 10". | [S11](screens/S11-drag-move.png) |
| S12 · Autocut preview | `ykvG3` | The Autocut page replaces the panel; 4 silences in c3e9 are hatched and tagged. | [S12](screens/S12-autocut-preview.png) |

### Row 3 · Layers, bin and export (y 8800)

| Screen | Node | What it shows | PNG |
| --- | --- | --- | --- |
| S13 · Layer editing | `ghVHI` | Dragging L2 Face onto the right-third snap guide, with a live position readout. | [S13](screens/S13-layer-editing.png) |
| S13b · Onion layers panel states | `zXBb6` | Every layer-row state, plus an audio row clipping. | [S13b](screens/S13b-onion-layers-panel-states.png) |
| S14 · Bin drawer | `vRbn3` | Five takes with status chips; take 4 is selected, with its actions and **Purge from disk**. | [S14](screens/S14-bin-drawer.png) |
| S15a · Export setup | `l0fva` | The export popover: format, quality, output path, **Export [Enter]**. | [S15a](screens/S15a-export-setup.png) |
| S15b · Export running | `FDGeR` | The mud bar at 42% with phase, frames and time left; Record is disabled while exporting. | [S15b](screens/S15b-export-running.png) |
| S15c · Export panel frames | `SEFwj` | The WebM setup, the header while exporting, the mud bar's animation frames and its reduced-motion version. | [S15c](screens/S15c-export-panel-frames.png) |
| S16 · Export done | `XTRnH` | "Layers! Like an onion. Your video is ready." with **Open file**, Open folder and Copy path. | [S16](screens/S16-export-done.png) |
| S16b · Export done, panel closed | `ZonF5` | The same result as a success toast. | [S16b](screens/S16b-export-done-panel-closed.png) |
| S17 · Export failed | `mkkLb` | Failed at 37%: what failed, how to fix it, the ffmpeg error tail and a link to the full log. | [S17](screens/S17-export-failed.png) |

### Row 4 · Confirmations (y 10000)

| Screen | Node | What it shows | PNG |
| --- | --- | --- | --- |
| S18 · Destructive modal | `uSm2m` | "Get out of my swamp?" before purging takes 2 and 4: 10 files, 184.1 MB, **Cancel** focused. | [S18](screens/S18-destructive-modal.png) |
| S18b · Change fps confirmation | `GPmrg` | The same modal from Settings: going from 30 to 25 fps re-quantizes all 10 clips, each listed. | [S18b](screens/S18b-change-fps-confirmation.png) |

## Tokens

| File | What it holds |
| --- | --- |
| [`tokens/qrec-tokens.json`](tokens/qrec-tokens.json) | The source: 75 colors with their four theme values, 15 numbers (spacing and radii, in px) and 3 font families |
| [`boards/themes.css`](boards/themes.css) | Generated from it: the four themes as `--qrec-<token>` custom properties, Swamp Night on `:root` |

The JSON was generated from the pen.dev variables on 2026-10-14 and is edited by hand since 2026-11-10. Every color, spacing and radius value matches the frontmatter of [DESIGN.md](../DESIGN.md). The type scale, font fallbacks, shadow geometry, the 0.45 disabled opacity and the motion timings aren't variables; they stay in DESIGN.md.

## Later, not in this review

- **The other three themes.** Their values are already in the variables and in `qrec-tokens.json`, but no themed frames exist yet. BRIEF §7 lists the frames that get a theme proof.
- **Draft screens,** left as they are (approved as drawn later on 2026-10-15; exported to [`screens/`](screens/) on 2026-10-17):
  - S19 Command palette `vXZe6` · S19b Command palette, empty query `GRSEE` · S20 Cheat sheet `QHCJG` · S20b Cheat sheet, search `W5kkMx`
  - S21 Settings, Appearance `C6zGK` · S22 Settings, Keyboard `GoRnZ`
  - S23 Missing dependency `eIdTG` · S24 Portal-denied error `g2i1Br` · S25 Toasts `PK7Ru`
  - X1 CLI recording at GUI start `wsZqr` · X2 Not my swamp `h0SuZd` · X3 Project can't be read `cUduQ`

## Exports for tools (2026-10-17)

`qrec.pen` is encrypted and archived; the boards replace it. [`export/`](export/INDEX.md) keeps what the build and tests need from it: one SVG per icon and size and the art as PNG (frames are exported as HTML on demand, uncommitted). [`export/INDEX.md`](export/INDEX.md) lists what is where and how the exports were checked (the slice PNGs and the tokens still match the document).

## Decisions (product owner, 2026-10-15)

**The slice is approved.** The product owner's answers:

- **Color proposals P1–P8: accepted, with one rule:** "the best contrast, not the highest, but good contrast according to norms and recommendations." Each value is the smallest OKLCH-lightness shift that meets WCAG 2.2 AA with a 0.05 margin on every background the token is drawn on, hover states included. In Swamp Night this changes `record-text` (`#FC6555` → `#FF7868`), `border-strong` (`#6B7D5D` → `#6C7E5E`) and `mud` (`#7E5C3D` → `#7F5D3E`). The final values and the reasons are in [DESIGN.md](../DESIGN.md) → Colors → Decisions.
- **Product questions Q1–Q11, engineering calls and polishing calls:** accepted as mocked. Q9 (follow GNOME's light/dark) wasn't designed, so v1 keeps the theme a manual choice in Settings, Swamp Night by default. Q2 was changed on 2026-10-23 (issue #12): `Space`, `J` and `L` in Live switch to Program and play, and the transport's Play is enabled there (docs/GUI.md D-01).
- **Draft screens** (listed under "Later, not in this review" above: S19–S25 and X1–X3): **approved** as drawn, later the same day (docs/GUI.md G§0.2). They are built as drawn (work packages WP8, WP9 and the S24 overlay), not as interim pages.
- **GUI dependencies:** `gtk4` 0.11.5, `libadwaita` 0.9.2, `pango` 0.22.9 and `rust-version` 1.92 approved, and `gstreamer-allocators` 0.25 (added for the niri screencast bridge) approved after the fact (ARCHITECTURE A§1.2, GUI.md G§0.1).

The tables below keep what was asked.


### 1. Color proposals P1–P8

From DESIGN.md → Colors → Proposals. The designs and both token files already use the proposed values. Declining one means setting one variable back and following that proposal's "If declined" rule. **Only P6 changes Swamp Night**, so it is the only one this slice needs now; the others can wait for the theme pass.

| ID | Theme | Token | Spec → proposed | Fails as specified |
| --- | --- | --- | --- | --- |
| P1 | Swamp Day | `muted` | `#6E6A4B` → `#635F41` | 4.23:1 on `surface` (text needs 4.5:1) |
| P2 | Ogre Mode | `muted` | `#C2B98F` → `#CFC69B` | 3.95:1 on `raised` |
| P3 | Swamp Day | `accent` | `#4F7A19` → `#446E04` | as text, 4.35:1 on `bg` and 3.91:1 on `surface`; too close to `record` for deuteranopes |
| P4 | Swamp Day | `record` | `#C8352A` → `#CF3B3C` | too close to `accent` under protanopia and deuteranopia (ΔE 12.6 and 4.6) |
| P5 | Swamp Day | `accent-2` | `#B5831A` → `#A87811` | playhead 2.89:1 on `bg`, 2.60:1 on `surface` (indicators need 3:1) |
| P6 | all four | `record-text` (new) | Night `#FC6555`, Day `#BF2A2F`, Keep `#FD5648`, Ogre `#FEB3A5` | red text fails AA in every theme; in Night, 4.05:1 on `surface` |
| P7 | Dragon's Keep | `accent-text` (new) | `#F3578B` | pink text 4.21:1 on `surface`, 3.65:1 on `raised` |
| P8 | Swamp Day, Ogre Mode | `warning-text` (new) | Day `#9A5103`, Ogre `#FFB66B` | warning text 3.26:1 (Day, on `surface`), 2.83:1 (Ogre, on `raised`) |

In this slice, `record-text` draws **Cancel [R]** (S03), the Broken chip and **Purge from disk** (S14), and "Failed at 37%" (S17).

### 2. Product questions (BRIEF §8)

| # | Question | What the mocks assume |
| --- | --- | --- |
| Q1 | Should `qrec init` seed the scene (primary screen, default mic, system audio) so R works on first launch? | No seeding: S01a asks which sources to add |
| Q2 | What does `Space` do in Live mode? | Switching to Program and playing (decided on 2026-10-23, issue #12, docs/GUI.md D-01; the frames show it enabled). It was assumed to do nothing, its hint disabled |
| Q3 | Does `Esc` cancel a drag? | Yes, a drag-move or a trim (S10, S11) |
| Q4 | Should the filmstrip use the topmost visible layer (SPEC §7.2) or the composited frame? | The topmost layer, so the filmstrip shows the face cam |
| Q5 | What happens with conflicting key bindings? | Both stop working until one changes, with a banner (S22 draft). The alternative: the later entry wins |
| Q6 | Which keys work in the bin? | `↑` `↓` select, `Enter` inserts at the end, `Space` previews; Purge only from its button (S14) |
| Q7 | What does the header button do during a countdown? | It becomes an outlined **Cancel [R]** (S03) |
| Q8 | Where do the take-layout actions live? | In a `Layout ▾` menu in the panel banner (S06) |
| Q9 | Should Qrec follow GNOME's light/dark setting (Swamp Night ↔ Swamp Day)? | Not designed yet |
| Q10 | Accept the color proposals P1–P8? | All of them are applied (section 1) |
| Q11 | Use Atkinson Hyperlegible Next instead of the original? | Next, for its 600 and 800 weights; same family, same license |

### 3. Engineering calls made without you ([ARCHITECTURE](../docs/ARCHITECTURE.md) A§13)

1. `d` after a record-over also puts back the clips the take replaced (A§5.5).
2. `d` skips tooShort, broken and cancelled takes and targets the newest placed take (A§5.5).
3. `layer remove --take` and `audio remove --take` are allowed and only touch that take's snapshot (A§4.4).
4. Autocut never removes a whole clip; an all-silent clip is left as it is and reported (A§5.7).
5. Hand edits to `qrec.json` can't remove takes or lower `nextTakeNumber`; those parts are ignored with a warning (A§7.4).
6. A CLI command run with no GUI open becomes a temporary host while its job runs (A§8.5).
7. The Draft and High export presets around SPEC's Standard; for x264, `veryfast` CRF 23 and `slow` CRF 18 (A§9.3).

### 4. Calls made while polishing the slice

- **S04b:** the record-over label wraps to "● Take 10 · recording" over "over c2d0 (take 5)", because the ghost is 131 px wide.
- **S07, S08:** with nothing selected, the Program panel subtitle reads "Select a clip to edit its take layout".
- **`N` during a drag:** S10 (trim) shows `[N] Snapping on/off` enabled and S11 (move) shows it disabled. Should `N` work mid-drag?
- **S15b:** while exporting, the Export slot shows "42%" and a small mud bar, so Record never moves. "Exporting 42%" moved to the tooltip (S15c). BRIEF §2.3 and §5 (S15b) still say "Exporting 42%".
- **S14:** take 7's second line is shortened to "Recovered after Qrec closed mid-take." so it fits on one line (BRIEF §3.4 says "…closed while recording.").
- **S18:** the bin's Purge button is drawn at rest, not pressed, because the modal holds focus.
- **S18b:** the modal lists all 10 clips (8 visible, then it scrolls) instead of the brief's "…and 6 more".

## Known gaps

- **Fit-zoom end inset.** DESIGN.md (Timeline) and BRIEF §2.6 set px/frame = (time-area width − 8) ÷ total frames, but the mocks still use 1040 ÷ 7391. A playhead parked at the end is therefore half clipped in S02, S03, S04, S05, S05b and S13. Build from the formula and don't measure clip positions off the fit-zoom mocks. Redrawing them takes one pass over `C15 Ruler/Fit` (`I8Owg`) and every fit-zoom frame.
- **`C01 Button/Destructive flat · pressed`** (`h7trs`): its label is about 3.9:1 on `surface`, below AA. No slice screen uses it. `record` at 16% (the value `ghost-fill` already has) would reach about 4.6:1.
- **Links:** pen.dev doesn't keep `underline`, so "Open log" (S14) and "Open full log" (S17) draw a 1 px rule under the text. GTK should use a real underline.
