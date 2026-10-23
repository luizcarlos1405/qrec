# Qrec canvas brief

For the designers building `design/qrec.pen` through the Pencil MCP. It says **what to draw, where, with what data and copy**.

- **Visual rules:** `DESIGN.md` is normative for tokens, type, components and motion.
- **Product truth:** `PRODUCT.md`.
- **The binding source:** `SPEC.md` §15.

When this brief and DESIGN.md disagree on a visual value, DESIGN.md wins. All sample data below is **synthetic**; label generated imagery as synthetic in frame notes.

---

## 0. Set up pen.dev first

1. **Variables with a theme axis.** Add a document theme axis `theme` with the values `swamp-night`, `swamp-day`, `dragons-keep` and `ogre-mode`. Create one color variable per token in DESIGN.md's `colors` frontmatter, without the `day-`/`keep-`/`ogre-` prefixes. For example, `$bg` holds four theme values: `#12170F`, `#F1EEDA`, `#151316` and `#1E2A0E`. Alpha tokens are `#RRGGBBAA`. Add number variables for spacing (`$space-*`) and radii (`$radius-*`). Never hardcode a hex in a screen.
2. **Fonts.** Use Fraunces (display only), Atkinson Hyperlegible Next (UI) and JetBrains Mono (numbers and key caps). If Atkinson Hyperlegible Next is missing, use Atkinson Hyperlegible and map weight 600 → 700 and 800 → 700.
3. **Icons.** Use `icon` nodes from the `lucide` library. Names are in DESIGN.md → Components → Icons.
4. **Artwork.** Use `Generate` (`type: "svg"`), never hand-drawn paths, for:
   - (a) the app icon: an onion with ogre-ear trumpets, full color, 128 px;
   - (b) its 16 px symbolic glyph (the REC motif);
   - (c) the "quiet swamp" empty-state illustration: a still pool, reeds, the onion icon peeking out, no text;
   - (d) the "wooden signpost" illustration for the missing-dependency screen: an outhouse-door wood signpost, blank board, no text.

   Use generated images for the synthetic screen content and the webcam person (§3.5).
5. **Canvas layout.** Put all components first, in one "Components" frame per family. Under them come the Swamp Night screen rows (S01 → S25, then X1–X3), then one row per alternate theme. Name frames `S06 · Program with selection · Swamp Night`. Every screen frame is **1600×1000** with `clip: true`, and is built from component instances.
6. **Theme proof.** Duplicate a screen frame and set `theme: { theme: "swamp-day" }` on it. Do not rebuild it.
7. **Hatch.** Build the silence hatch once as a reusable component: a clipped frame with 45° rectangles, 2 px stripe and 4 px gap, filled `$hatch-stripe`, with 1 px `$warning` edges.

---

## 1. Direction (impeccable, Operate mode)

This surface is **Operate**: the visitor completes a task. It must read as a serious native editor first, with a libadwaita header bar, standard controls and an NLE layout. The swamp lends type, palette, density and one signature move, and nothing else. The direction is pinned by SPEC §15 (themes, fonts, copy, motion, icon) and the owner's layout, so no concept roll was run.

**Direction contract**

- **THESIS.** A working NLE-and-recorder in one window, where the hot loop (R, talk, R) is always one key away and always visible. It refuses the category default: a grey OBS or Resolve clone with a blue accent, a red dot that also means "error", and hidden shortcuts.
- **OWN-WORLD.**
  - **Palette:** bog-tinted neutrals per theme; ogre green for what you act on; onion gold for time (playhead, guides); torch red only for recording and destruction; eight swamp take colors on 4 px clip stripes.
  - **Type:** JetBrains Mono for every number; Atkinson for the UI; Fraunces only in the wordmark, empty states and modal titles.
  - **Chrome:** key-cap chips on every action; flat tonal layers with 1 px hairlines; a mud-filled export bar.
- **STORY.** The user sees exactly what will be recorded (the Live canvas and live meters). They press R and watch the torch flicker and a ghost clip grow, then press R and see the take land in its color. They trim, split and autocut using keys the hint bar keeps teaching. They export, and hear "Layers! Like an onion. Your video is ready."
- **FIRST VIEWPORT.** Shown in §2 (1600×1000):
  - A 48 px header: icon + wordmark, Live/Program switch, centered project title, the torch **Record [R]** button, Export…, palette, settings.
  - A 960×540 preview centered on a 1216×608 stage, with the LIVE badge, the transport row and **Save frame [P]**.
  - The 384 px Onion layers panel at full height on the right.
  - A 1216×312 timeline: toolbar, ruler, filmstrip row with take stripes, A1/A2 waveform lanes.
  - A 32 px hint bar of key caps.
  - The primary action is Record: top-right in red, and first in the hint bar.
- **FORM.** Brief-pinned: native libadwaita conventions plus the SPEC §15 world; no ordered candidate list. The signature move is **the torchlit take**: the REC torch flicker plus the ghost clip that settles into its take color. seed: none (unavailable)
- **FINISH.** unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

**It looks right when** a category-fluent user, with the name covered, sees a recorder and editor they could operate immediately: the Record button, the canvas and the timeline are where they expect, and every action shows its key. **It looks wrong when** the swamp shows up as texture, wood grain, cartoon frames or jokes on buttons, or when red appears anywhere that isn't recording or destruction.

---

## 2. Window anatomy (1600×1000 logical px)

### 2.1 Region map

| Region | x | y | w | h | Ground | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| Header bar | 0 | 0 | 1600 | 48 | `surface`, bottom 1 px `border` | libadwaita header bar |
| Stage | 0 | 48 | 1216 | 608 | `bg` | Preview plus transport |
| Preview | 128 | 64 | 960 | 540 | the project canvas color (`#000000`) | Exactly 50% of 1920×1080, so 1 preview px = 2 canvas px |
| Transport row | 128 | 612 | 960 | 36 | `bg` | Under the preview |
| Onion layers panel | 1216 | 48 | 384 | 920 | `surface`, left 1 px `border` | Full height down to the hint bar |
| Timeline | 0 | 656 | 1216 | 312 | `bg` lanes, `surface` toolbar, ruler and headers | |
| Timeline toolbar | 0 | 656 | 1216 | 36 | `surface`, bottom `border` | |
| Ruler | 0 | 692 | 1216 | 28 | `surface` | The corner cell (0–176) shows the playhead timecode |
| Track header column | 0 | 692 | 176 | 276 | `surface`, right `border` | |
| Time area | 176 | 692 | 1040 | 276 | `bg` | Zoom math uses 1040 px |
| Video row | 176 | 720 | 1040 | 76 | `bg` | Clips: 4 px stripe plus 72 px filmstrip |
| A1 lane | 176 | 800 | 1040 | 56 | `bg` | 4 px gaps between rows |
| A2 lane | 176 | 860 | 1040 | 56 | `bg` | Room for a third lane, y 920–968 |
| Hint bar | 0 | 968 | 1600 | 32 | `surface`, top `border` | |

### 2.2 Why this layout

- **Preview and timeline share one column** (x 0–1216). The eye only travels vertically between the frame and the playhead. The timeline sits under the stage, where every NLE puts it.
- **The Onion layers panel runs full height** instead of letting the timeline span the window. It holds three dense groups: layers, audio meters and the inspector. At 920 px it shows all of them without scrolling for a typical scene (Program mode uses 840 of 920 px). The timeline still gets 1040 px of time area, which fits the whole project at fit zoom and 32 frames at frame zoom.
- **The preview is exactly 50% of the canvas** (960×540). Inspector numbers map 2:1 to preview pixels, and canvas text stays legible.
- **Live audio meters sit in the panel, on the right**, so they stay visible while recording, next to the REC badge's corner of the eye path. The header holds Record, the one action that must never move.
- **The hint bar spans the full width**, so it stays a fixed reading line under everything.

### 2.3 Header bar (48 px, items centered at y 24)

| Slot | Item | Rect / size | Details |
| --- | --- | --- | --- |
| Start | App icon | 24×24 at (12, 12) | Full-color onion-trumpet icon |
| Start | Wordmark "Qrec" | `wordmark` at x 44 | Fraunces 20/24 700, `text` |
| Start | Mode switch | 176×32 at (110, 8) | Segments `Live` (video icon, 72 wide) and `Program` (film icon, 100 wide), then a `Tab` key cap at (294, 14) |
| Center | Window title | centered on x 800 | `rust-traits-talk` in `body-strong` over the subtitle `10 clips · 00:04:06:11` in `caption` `muted` |
| End | Record button | 120×32 at (1240, 8) | `button-record`: onion glyph 16, `Record`, key cap `R` (on-fill) |
| End | Export… | 104×32 at (1368, 8) | `button-secondary`: download icon, `Export…`. While exporting it becomes `Exporting 42%` with a 40×4 mud bar |
| End | Command palette | 32×32 at (1484, 8) | Search icon. Tooltip: "Commands" with key caps `Ctrl` `K` |
| End | Settings | 32×32 at (1520, 8) | Settings icon. Tooltip: "Settings" with `Ctrl` `,` |
| End | Close | 32×32 at (1560, 8) | `x`, the libadwaita window control |

### 2.4 Stage

- **Preview** at (128, 64, 960, 540) with a 1 px `border` stroke outside. Overlays are inset 12 px:
  - **REC badge** (only while recording) top-left at (140, 76). Its timer plate sits below at (140, 110).
  - **Mode badge** (`LIVE` or `PROGRAM`) top-right, with its right edge at x 1076 and y 76.
  - **PROXY badge** (only when playback is degraded) to the left of the mode badge, with an 8 px gap.
- **Transport row** at y 612–648:
  - **Left:** the timecode `00:01:21:07` in `mono` `text`, followed by ` / 00:04:06:11` in `mono` `muted`.
  - **Center:** five icon buttons centered on x 608: previous edit (`↑`), previous frame (`←`), play/pause (`Space`, 40×32), next frame (`→`), next edit (`↓`). Each tooltip shows its key.
  - **Right:** **Save frame**, a flat button at (956, 614): camera icon, the label `Save frame`, key cap `P`. It is always enabled.
  - **In Live mode,** the left side reads `Live · 1920×1080 · 30 fps`; play/pause stays enabled and does what `Space` does there, switch to Program and play (Q2, decided on 2026-10-23: GUI D-01), and the other four buttons are disabled (tooltip: "Switch to Program (Tab) to step through the timeline").
- **Bin open:** the drawer occupies (0, 48, 352, 608) and pushes the stage. The preview becomes 832×468 at (368, 100) and the transport row stays at y 612.

### 2.5 Onion layers panel (x 1232–1584 content, 16 px padding)

| Block | y (Live) | Height | Content |
| --- | --- | --- | --- |
| Panel header | 48 | 48 | `Onion layers` (`title`). Subtitle: `Project scene · new takes use this`. Trailing `+` Add layer icon button |
| Take banner (Program with a clip selected only) | 96 | 40 | `Take 5 layout · changes edit take 5 only` plus a `Layout` menu button (see S06). Everything below moves down 40 px |
| Section "Layers" | 96 | 32 | `Layers` (`heading`) and `2` (`caption` `muted`) |
| Layer rows | 128 | 2 × 44 | Topmost first: L2 Face, then L1 Main screen |
| Add layer | 220 | 32 | Flat button `+ Add layer` (menu: Screen ▸ DP-4, HDMI-A-1 · Camera ▸ Logitech C920) |
| Section "Audio" | 268 | 32 | `Audio` and `2` |
| Audio rows | 300 | 2 × 92 | A1 Yeti Nano (Autocut tag), A2 System audio |
| Add audio track | 488 | 32 | Flat button `+ Add audio track` |
| Section "Inspector" | 536 | 32 | `Inspector` and the selected layer, for example `Face · L2` |
| Fields | 568 | 5 × 56 | Position X/Y · Size W/H with aspect lock · Crop T/R/B/L · Opacity slider + field · Fit `Contain`/`Cover` |
| Free | 848 | 120 | Program mode uses 40 of it |

With no layer selected, the inspector shows one `caption` `muted` line: "Select a layer on the canvas or in the list to edit its position, size and crop."

### 2.6 Timeline

- **Toolbar** (y 656–692, items 28 tall, 8 px padding):
  - Left: **Bin** toggle (archive icon, `Bin`, a `5` count chip, key cap `B`); **Snap** toggle (magnet icon, `Snap`, key cap `N`; on = R1 selected treatment); a 1 px divider; **Autocut** (scissors icon, `Autocut`, key cap `C`); and an icon button for autocut settings (sliders icon; tooltip "Autocut settings and preview", `Shift` `C`).
  - Right-aligned: `10 clips · 00:04:06:11` (`caption` `muted`), then the zoom cluster: zoom out (`−`), a 120 px logarithmic slider (from fit to 128 px/frame), zoom in (`=`), Fit (key cap `\`), and a readout in `mono-xs` `muted`: `Fit` / `48 px/s` / `32 px/frame`.
- **Ruler** (28 px): ticks and labels per zoom (table below). The corner cell shows the playhead timecode in `mono` `text` at x 12.
- **Rows:**
  - **Video** (76): header `Video` (`body-sm` 600) with the caption `Thumbnails: Face (L2)`. Per SPEC §7.2 the filmstrip uses the topmost visible layer; see Q4.
  - **A1** (56): header with mic icon, `Yeti Nano`, chip `A1`, a 12 px scissors marker (the autocut track), and `M` `S`.
  - **A2** (56): header with speaker icon, `System audio`, `A2`, and `M` `S`.
- **Playhead:** 2 px `playhead` from y 692 to y 916, with a 13×12 head at the top of the ruler.
- **Zoom levels used in the mocks** (time area 1040 px, 30 fps):

| Zoom | px/frame | Visible span | Ruler labels | Ticks | Clip content |
| --- | --- | --- | --- | --- | --- |
| Fit | 0.1407 (= 1040 / 7391) | 00:00:00:00 → 00:04:06:11 | Every 30 s (`00:00:30:00`…) | Major 30 s, minor 10 s | Stripe, filmstrip, take label by width |
| Seconds | 1.6 (48 px/s) | 00:01:05:00 → 00:01:26:20 | Every 2 s | Major 1 s (48 px), minor 10 frames (16 px) | Filmstrip of 128×72 thumbnails |
| Frame | 32 (≥ 24 required) | Frames 3160 → 3192 (00:01:45:10 → 00:01:46:12) | Each frame's `FF` centered in its 32 px cell; full timecode at second boundaries | A tick at every frame boundary, taller at seconds | One thumbnail per frame, a 32×72 center crop |

**Fit inset (DESIGN.md → Timeline wins).** The build uses an 8 px end inset at fit zoom: (1040 − 8) / 7391 = 0.1396 px/frame, so the playhead parked at the end is never half clipped. The mocks and the "x at fit" column in §3.3 still draw 1040 / 7391. Re-lay every fit-zoom frame in one pass, with x′ = 176 + (x − 176) × 1032 / 1040, and update the C15 Ruler/Fit ticks with it.

### 2.7 Hint bar

The left side holds the hints, from x 12. Each item is a key cap, a 6 px gap and a label, with 20 px between items. The first label is in `text`, the others `muted`. The right side holds the status, right-aligned at x 1588: `Snapping on · 1920×1080 · 30 fps` (`caption` `muted`). Each state's hints are listed with its screen in §5.

### 2.8 Overlay placement

| Overlay | Placement |
| --- | --- |
| Toast | Bottom center, centered on x 800, bottom edge at y 956. 360–560 wide |
| Export panel (popover) | 440 wide at x 1152–1592, top at y 52, with its arrow pointing at the Export button (x 1420) |
| Command palette | 640 wide at x 480, top at y 96, height fits content. Over `scrim` |
| Modal | 520 wide, centered (x 540). Over `scrim` |
| Settings | 840×640 at (380, 180). Over `scrim` |
| Cheat sheet | `scrim-strong` over the whole window; sheet at (40, 40, 1520, 920) |
| Bin drawer | (0, 48, 352, 608); pushes the stage |
| Autocut preview | Replaces the panel's content, as a pushed page with a back button |
| Tooltips | 8 px below the pointer or anchor, e1 |

---

## 3. Sample project (synthetic, used on every screen)

### 3.1 Project and scene

- **Folder:** `~/videos/rust-traits-talk/` · **project name:** `rust-traits-talk` · **canvas:** 1920×1080 @ 30 fps, background `#000000`.
- **Layers** (list order is topmost first; JSON array order is the reverse):

| Id | Name | Source | Native | Rect (x, y, w, h) in canvas px | Fit | Opacity | Visible |
| --- | --- | --- | --- | --- | --- | --- | --- |
| L2 | Face | Camera · `/dev/video0` · Logitech C920 | 1280×720 | 1500, 760, 384, 288 → preview (878, 444, 192, 144) | Cover | 100% | yes |
| L1 | Main screen | Screen · `DP-4` · GIGA-BYTE GS27QA | 2560×1440 | 0, 0, 1920, 1080 | Contain | 100% | yes |

- **Audio tracks:**

| Id | Label | Role | Source | Gain | Sync | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| A1 | Yeti Nano | mic | `alsa_input.usb-Blue_Microphones_Yeti_Nano…` | 0.0 dB | +0 ms | Autocut track |
| A2 | System audio | system | `alsa_output.pci-0000_00_1f.3.analog-stereo.monitor` | −6.0 dB | +0 ms | |

- **Detected but unused:** screen `HDMI-A-1` · Dell U2720Q · 3840×2160.

### 3.2 Takes

Take colors follow DESIGN.md's cycle: 1 moss, 2 donkey, 3 onion, 4 mud, 5 lily, 6 ginger, 7 dragon, 8 fairy, 9 moss, 10 donkey.

| Take | Color | Status | Duration | Recorded (2026-10-14) | Size | Where |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | mossy green | ok | 912 f · 00:00:30:12 | 14:03 | 76.0 MB | c7f3, c8a1 |
| 2 | donkey grey | ok, unused (discarded with `D`) | 664 f · 00:00:22:04 | 14:06 | 55.1 MB | Bin |
| 3 | onion gold | ok | 1,377 f · 00:00:45:27 | 14:08 | 114.8 MB | c1b2–c1b6 (autocut) |
| 4 | mud | ok, unused (replaced by take 5 with `O`) | 1,548 f · 00:00:51:18 | 14:12 | 129.0 MB | Bin |
| 5 | lily-pad teal | ok | 2,214 f · 00:01:13:24 | 14:15 | 184.5 MB | c2d0, c2d1 (split) |
| 6 | gingerbread brown | tooShort | 7 f · 00:00:00:07 | 14:19 | 1.1 MB | Bin |
| 7 | dragon pink | ok, recovered after a crash | 1,870 f · 00:01:02:10 | 14:21 | 155.8 MB | Bin |
| 8 | fairy blue | ok | 3,105 f · 00:01:43:15 | 14:26 | 258.8 MB | c3e9 |
| 9 | mossy green | broken (EOS timeout) | — | 14:31 | 95.4 MB | Bin |
| 10 | donkey grey | recording (S03–S05) | grows | 14:38 | — | Ghost clip |

The files for take 3 are `takes/take-0003/L1-screen-DP-4.mkv`, `L2-camera-0.mkv`, `A1-mic.mka`, `A2-system.mka` and `recorder.log`.

### 3.3 Timeline (10 clips, 7,391 frames = 00:04:06:11)

| # | Clip | Take | In | Out | Frames | Starts at | x at fit (window px) | Width at fit | Label at fit |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | c7f3 | 1 | 0 | 410 | 410 | 00:00:00:00 | 176.0 | 57.7 | `1` |
| 2 | c8a1 | 1 | 455 | 912 | 457 | 00:00:13:20 | 233.7 | 64.3 | `Take 1` |
| 3 | c1b2 | 3 | 0 | 268 | 268 | 00:00:28:27 | 298.0 | 37.7 | `3` |
| 4 | c1b3 | 3 | 304 | 611 | 307 | 00:00:37:25 | 335.7 | 43.2 | `3` |
| 5 | c1b4 | 3 | 652 | 890 | 238 | 00:00:48:02 | 378.9 | 33.5 | `3` |
| 6 | c1b5 | 3 | 931 | 1180 | 249 | 00:00:56:00 | 412.4 | 35.0 | `3` |
| 7 | c1b6 | 3 | 1222 | 1377 | 155 | 00:01:04:09 | 447.4 | 21.8 | none (stripe only) |
| 8 | c2d0 | 5 | 12 | 1104 | 1092 | 00:01:09:14 | 469.2 | 153.6 | `Take 5` |
| 9 | c2d1 | 5 | 1104 | 2214 | 1110 | 00:01:45:26 | 622.9 | 156.2 | `Take 5` |
| 10 | c3e9 | 8 | 0 | 3105 | 3105 | 00:02:22:26 | 779.1 | 436.9 | `Take 8` |

- **Playhead** (default for Program screens): `00:01:21:07` = frame 2437, inside c2d0. That is take 5 frame 365, at x 518.9 at fit.
- **Detected silences on A1** (hatched at seconds zoom, inside c2d0): 00:01:13:02–00:01:14:10 and 00:01:18:20–00:01:19:05.

### 3.4 Bin (5 takes · 436.4 MB, newest first)

| Take | Chip | Duration | Recorded | Size | Secondary line | Actions |
| --- | --- | --- | --- | --- | --- | --- |
| 9 | `Broken` (`record-text`) | — | 14:31 | 95.4 MB | "Files didn't close within 10 s." | Open log · Reveal in file manager · Purge from disk |
| 7 | `Recovered` (`text` on `pressed`) | 00:01:02:10 | 14:21 | 155.8 MB | "Recovered after Qrec closed while recording." | Preview · Insert at end · Insert after selection · Reveal · Purge |
| 6 | `Too short` (`warning-text`) | 00:00:00:07 | 14:19 | 1.1 MB | "7 frames; takes need 10." | Reveal · Purge |
| 4 | `Unused` (`text` on `pressed`) | 00:00:51:18 | 14:12 | 129.0 MB | "Replaced by take 5." | Preview · Insert at end · Insert after selection · Reveal · Purge |
| 2 | `Unused` | 00:00:22:04 | 14:06 | 55.1 MB | "Discarded with D." | Same as take 4 |

### 3.5 Canvas content (synthetic, generated)

- **L1 Main screen:** a dark code editor (generic, no real product branding). It shows `src/onion.rs`:

```rust
trait Layers {
    fn peel(&mut self) -> Option<Layer>;
}

impl Layers for Onion {
    fn peel(&mut self) -> Option<Layer> {
        self.layers.pop()
    }
}
```

  In Live mode, a terminal pane at the bottom shows `cargo test` passing.
- **L2 Face:** a generated photo of a person in a hoodie talking toward the camera, warm ring-light glow, center-cropped 4:3.
- **Video is theme-independent.** It looks the same in every theme.

### 3.6 Other sample values

- **Export:**
  - Output: `exports/rust-traits-talk-20261014-153012.mp4`, MP4 (H.264 + AAC 192 kbps, VA-API), Standard, 1920×1080 · 30 fps.
  - Running: `42%`, phase `Rendering clip 6/10`, `3,449 / 7,391 frames`, elapsed `00:00:38`, about `00:00:52` left.
  - Done: `88.4 MB`, took `00:01:31`.
  - Failed: at `37%` on `Rendering clip 3/10`.
- **Frame save:** `frames/rust-traits-talk-take-0005-00012107.png`.
- **Autocut** (take 8, c3e9): threshold −35 dB, min silence 400 ms, padding 120 ms, min keep 200 ms, track A1. Four regions:

| Region | Range | Frames | Removed |
| --- | --- | --- | --- |
| 1 | 00:02:41:12 → 00:02:43:00 | 48 | 1.6 s |
| 2 | 00:03:05:20 → 00:03:07:02 | 42 | 1.4 s |
| 3 | 00:03:29:06 → 00:03:30:15 | 39 | 1.3 s |
| 4 | 00:03:51:18 → 00:03:53:15 | 57 | 1.9 s |

  The total is 186 frames (6.2 s). Take 8 goes from 00:01:43:15 to 00:01:37:09, and the timeline from 00:04:06:11 to 00:04:00:05.
- **History labels:** `Record take 8` · `Split clip c2d0 at 00:01:45:26` · `Autocut take 3: removed 4 silences (6.2 s)`.

---

## 4. Components to build first

Build each as a reusable pen.dev component with its variants as sibling components, for example `Button/Primary` and `Button/Primary · hover`. Sizes and colors come from DESIGN.md → Components. Every interactive component needs: default, hover, pressed, focus-visible, disabled.

| # | Component | Variants | States and extras |
| --- | --- | --- | --- |
| C01 | Button | primary, record (header), destructive (large), secondary, flat, destructive-flat, icon-only 32 and 24; sizes 24/32/40 | + loading (spinner). The record button has idle `● Record [R]`, countdown `Cancel [R]` (outlined), recording `■ Stop [R]` and finalizing `Saving…` (disabled + spinner) |
| C02 | Key-cap chip | sm 16 · md 20 · lg 28; single and combo (`Ctrl` `K`); on-fill | disabled, rebound (+ "default P"), capture (dashed `accent`, "…"), conflict (`warning-text` stroke + icon) |
| C03 | Hint bar item and hint bar | item = key cap(s) + label; bar = items + status cluster | first item in `text`, others `muted`; disabled item; label-only item ("Release to trim") |
| C04 | Header bar | idle, recording (2 px `record` bottom line), exporting (Export → progress), minimal (blocking screens: icon, wordmark, close) | slots: start, title, end |
| C05 | Mode switch | Live selected / Program selected | hover per segment, focus, disabled |
| C06 | Canvas badges | REC (flicker frames A = opacity 1.0 with glow, B = 0.74), REC static (reduced motion), Saving, LIVE, PROGRAM, PROXY | timer plate (`mono-lg` + `Take 10`) |
| C07 | Countdown overlay | digits 3 / 2 / 1; ring at 100% / 50% | reduced motion: static ring |
| C08 | Canvas selection box | move, resize (corner handles), crop (Alt: edge brackets) | snap guide (vertical/horizontal + tag), position readout tooltip |
| C09 | Layer row | camera, screen | hover, selected (R1), hidden, locked, renaming, dragging + drop line, error, missing media |
| C10 | Audio track row with meter | mic, system; autocut tag on/off | idle meter, speaking meter, muted, clipping (latch), device missing |
| C11 | Level meter | horizontal 8 px (panel); mini vertical 4 px for lane headers (optional) | peak hold, clip latch |
| C12 | Inspector field | numeric with unit; group Position, Size (+ aspect lock), Crop (4-up), Opacity (slider + field), Fit (segmented) | hover, focus/editing, disabled, invalid + helper text |
| C13 | Panel header and section header | Live subtitle / Program subtitle; take-layout banner with `Layout` menu | trailing actions |
| C14 | Timeline toolbar | Bin toggle (with count), Snap toggle, Autocut, autocut settings, clip summary, zoom cluster | toggles on/off; zoom readout per level |
| C15 | Timeline ruler | fit, seconds, frame | playhead head, scrub flag |
| C16 | Clip (video row) | wide (≥ 64), medium (24–63), narrow (12–23), sliver (< 12); per-frame filmstrip at frame zoom | hover, selected, trimming (start or end edge), dragging (lifted), recording ghost, finalizing ghost, analyzing, missing media |
| C17 | Waveform lane segment and lane header | speech waveform, quiet waveform; header with `M` `S` | muted, soloed (others dim), selected, silence hatch (normal and preview-strong), ghost (live waveform) |
| C18 | Playhead and insertion caret | playhead (idle, scrubbing with flag), caret | — |
| C19 | Silence hatch | width-flexible clipped stripes | with "−1.6 s" tag (preview mode) |
| C20 | List row | 32, 40, 44 (two-line), 72 (thumbnail) | hover (surface), highlight (raised, R2), selected (R1), focused, disabled |
| C21 | Bin row | Unused, Recovered, Too short, Broken | selected (actions revealed), hover |
| C22 | Status chip | neutral, success, warning, error | — |
| C23 | Toast | info, success, warning, error × title-only or title + body × 0, 1 or 2 actions | close button; queued "+2" count (optional) |
| C24 | Modal shell | default; with loss list; with clip list (fps change) | Cancel focused (focus ring visible) |
| C25 | Popover shell | with arrow, without arrow | — |
| C26 | Slider | plain, with ticks, with value field | hover, dragging (value tooltip), focus, disabled |
| C27 | Toggle switch | off, on | focus, disabled |
| C28 | Segmented control | 2, 3 and 5 segments | — |
| C29 | Field family | text field, numeric spin field, dropdown, search (32 and 40) | placeholder, focus, invalid |
| C30 | Progress | mud (frames: 42%, 100%, failed 37%; bubbles shown), plain (reduced motion), indeterminate stripes | — |
| C31 | Empty-state block | with illustration; display title; body with lg key cap; actions | — |
| C32 | Theme preview card | Swamp Night, Swamp Day, Dragon's Keep, Ogre Mode (one instance per theme via `theme` override) | selected (2 px `accent` + check) |
| C33 | Tooltip | text only; text + key caps; clip hover tooltip (take, in/out, duration, keys) | — |
| C34 | Command palette row | enabled, highlighted (R2), disabled + reason | matched characters in `body-strong` |

---

## 5. Screens and states

These are Swamp Night frames unless noted. "Base" names the frame to duplicate. Copy in quotes is exact. `[X]` means a key cap.

### S01 · First run / empty swamp

- **Purpose:** first launch in an empty folder. The user must add something to record, then press R.
- **S01a, empty scene.**
  - **Stage:** no preview. A centered block at the preview rect holds the `heading` "What should Qrec record?", then a checklist of detected sources (40 px rows with checkboxes):
    - ☑ Screen · DP-4 · GIGA-BYTE GS27QA · 2560×1440
    - ☐ Screen · HDMI-A-1 · Dell U2720Q · 3840×2160
    - ☑ Camera · Logitech C920 · /dev/video0
    - ☑ Mic · Yeti Nano
    - ☑ System audio · Built-in Audio Analog Stereo
  - Below the checklist: the primary button "Add 4 sources" and the caption "Screens ask for permission once. Your desktop shows a picker for each one."
  - **Panel:** Layers is empty ("No layers yet"); Audio is empty ("No audio tracks"); the inspector is hidden.
  - **Timeline:** the empty state (as in S01b).
  - **Header subtitle:** "No clips yet".
- **S01b, sources added, no takes.**
  - **Canvas:** Live preview of L1 + L2 with the `LIVE` badge. The panel lists L2 and L1, with A1 and A2 meters idle at about −48 dBFS.
  - **Timeline:** the toolbar is shown, but the ruler and rows are replaced by the empty state, centered in the time area. The quiet swamp illustration (120 px) sits above the `display` line "This swamp is quiet." and the `body` line "Press [R] and make some noise.", with an lg key cap. Together they are the pinned string "This swamp is quiet. Press **R** and make some noise."
- **Hint bar:** `[R] Record` · `[Tab] Program` · `[Ctrl][K] Commands` · `[?] All keys`. In S01a the bar leads with `[Enter] Add 4 sources` (the focused primary button) and shows `R` disabled. The disabled header Record button carries the explanation as a tooltip 8 px under it: "Add a layer or audio track first". The hint bar is not focusable, so it never anchors a tooltip.
- **Timeline toolbar:** the C14 Empty variant: Bin without a count, Autocut, its settings and the zoom cluster disabled, "No clips yet" in place of the clip summary.
- **Visual details:** Fraunces appears only in the empty-state title. No other decoration.

### S02 · Main window, Live mode, idle

- **Purpose:** the resting state between takes. This is the default screen and the one that proves the theme set.
- **On screen:** the full anatomy from §2.
  - **Canvas:** Live (the code editor, plus the terminal pane running `cargo test`, plus the face cam), with the `LIVE` badge.
  - **Transport:** "Live · 1920×1080 · 30 fps"; play/pause enabled (it switches to Program and plays, GUI D-01), the other transport buttons disabled; Save frame enabled.
  - **Panel:** project scene; L2 and L1 unselected; A1 meter at about −22 dBFS with the peak at −16 (room tone and breathing); A2 at about −52; inspector empty-state line.
  - **Timeline:** fit zoom, 10 clips (§3.3), nothing selected, playhead at the end (00:04:06:11).
  - **Toolbar:** Bin `5`, Snap on.
- **Hint bar:** `[R] Record` · `[D] Discard last take` · `[Space] Play` (enabled: it switches to Program and plays, GUI D-01) · `[Tab] Program` · `[C] Autocut all` · `[B] Bin` · `[Ctrl][K] Commands` · `[?] All keys`. This is the default content of the C03 Hint bar component.
- **Visual details:** the Record button is the only red on screen. Take stripes read in order: moss ×2, onion ×5, lily ×2, fairy ×1.

### S03 · Countdown

- **Base:** S02, with `recording.countdownSecs` = 3 for this mock (the default is 0).
- **On screen:**
  - **Canvas:** dimmed 40%, with the 200 px ring centered at (608, 334). The digit is "3" and the ring sweep is full. Under it: "Take 10" (`body-strong`) and "Esc or R cancels" (`caption`).
  - **Record button:** outlined `Cancel [R]`.
  - **Panel:** layer and inspector controls disabled (0.45); meters stay live.
  - **Timeline:** unchanged, but editing is disabled. Clips are at 0.7 opacity.
- **Hint bar:** `[Esc] Cancel` · `[R] Cancel`.
- **Frames:** draw the digit 3 with a full ring, and optionally 2 with a ring at 50%.

### S04 · Recording

- **Base:** S02.
- **On screen:**
  - **Canvas:** the REC badge top-left (the onion glyph with "REC"); timer plate `00:02:41:12` with "Take 10"; `LIVE` top-right.
  - **Header:** `■ Stop [R]`, a 2 px `record` line under the header, and Export disabled.
  - **Panel:** meters live (A1 at about −14 dBFS with the peak at −9.8 while speaking; A2 at −30). Layer and inspector controls disabled.
  - **Timeline:**
    - Zoom is fit including the ghost (12,233 frames, 0.0850 px/f). Existing clips are at 0.6 opacity.
    - The **ghost clip** for take 10 (donkey stripe) runs from x 804.4 to 1216, with `ghost-fill`, a dashed `record` stroke, and the label "● Take 10 · recording".
    - The ghost's A1/A2 segments draw a live waveform, newest at the right edge, where the playhead is parked.
- **Hint bar:** `[R] Stop` · `[Esc] Cancel take (files kept)` · `[Tab] Switch view` · `[=][−] Zoom` · `[\] Fit`.
- **Frames:** REC flicker frame A (bright, with glow) and frame B (dim). Add a note frame with the flicker keyframes and the reduced-motion static badge.
- **Variant S04b, Record over** (`O` with c2d0 selected):
  - **Ghost:** occupies c2d0's slot (x 469.2) at its current length (`00:00:31:02`). The replaced clip c2d0 sits behind it, hatched with muted stripes at 40%.
  - **Ghost label:** one plate reading "● Take 10 · recording over c2d0 (take 5)". At 131 px it wraps to "● Take 10 · recording" over "over c2d0 (take 5)". It names the replaced clip, so the hatch needs no plate of its own; never stack a second plate in the ghost.
  - **Hint bar:** as in S04.

### S05 · Finalizing

- **Base:** S04.
- **On screen:**
  - **Canvas:** the REC badge is replaced by the "Saving take 10…" pill with a spinner; the timer plate is gone.
  - **Header:** the button shows `Saving…` with a spinner, disabled.
  - **Ghost:** animated `record` stripes at 25%, labeled "Take 10 · saving…".
- **Hint bar:** the label-only item "Finalizing take 10. Waiting for every file to close (up to 10 s)." followed by `[Tab] Switch view` · `[=][−] Zoom`.
- **S05b, the take lands** (drawn; it sits between S05 and S06 on the canvas):
  - **Timeline:** the ghost becomes a solid clip with a donkey-grey stripe at the end of the timeline (x 804.4 → 1216 at the same fit zoom, 12,233 frames). Its label is "Take 10", plus 00:02:41:12 in the hover tooltip. Clips return to full opacity, and the toolbar and header read "11 clips · 00:06:47:23". The torch is out.
  - **Header and hint bar:** the 2 px line is gone, and the hint bar is back to S02 with `[D] Discard last take` emphasized (label in `text`).

### S06 · Program mode with a selection

- **Base:** S02.
- **On screen:**
  - **Switch:** Program selected.
  - **Canvas:** the program frame at 00:01:21:07 (the `impl Layers for Onion` block plus the face cam), with the `PROGRAM` badge.
  - **Transport:** `00:01:21:07 / 00:04:06:11`, buttons enabled.
  - **Panel:**
    - Subtitle "Take 5 layout · edits this take only", plus the take banner: "Changes here only affect take 5." and a `Layout ▾` menu button. Its menu has three items: "Apply this layout to all takes", "Use this layout for new takes" and "Reset take to project scene".
    - L2 Face selected (R1), with the inspector showing X 1500, Y 760, W 384, H 288, crop 0/0/0/0, opacity 100%, fit Cover.
  - **Timeline:** fit zoom, c2d0 selected (2 px `accent` stroke plus trim handles), playhead at x 518.9.
  - **Clip hover tooltip** (show it on c2d1): "Take 5 · c2d1", "In 00:00:36:24 · Out 00:01:13:24 · 00:00:37:00" (mono), and keys `[S] Split · [[] []] Trim · [X] Remove`.
- **Hint bar:** `[S] Split` · `[[] []] Trim to playhead` · `[<] [>] Move` · `[X] Remove` · `[C] Autocut` · `[O] Record over` · `[Z] Zoom to clip` · `[?] All keys`.
- **Visual details:** the gold playhead, green selection and red Record must read as three different jobs at a glance. This frame also proves the alternate themes (§7).

### S07 · Timeline at fit zoom

- **Base:** S06, with the selection cleared.
- **Focus:** the ruler labels every 30 s (`00:00:30:00` … `00:04:00:00`) and the label collapse by width (§3.3: "1", "Take 1", "3", stripe-only c1b6).
- **Zoom cluster:** the readout says `Fit` and the slider is at its minimum.
- **Hint bar:** `[Space] Play` · `[,] [.] Select clip` · `[=] Zoom in` · `[Z] Zoom to selection` · `[Tab] Live` · `[?] All keys`.

### S08 · Timeline at seconds zoom (48 px/s)

- **Base:** S07.
- **View:** 00:01:05:00 → 00:01:26:20.
- **Clips:** the end of c1b6 (onion stripe) from x 176 to 390.4, then c2d0 (lily stripe, label "Take 5") from x 390.4 to 1216.
- **Playhead:** x 955.2 (00:01:21:07).
- **Ruler labels:** every 2 s at x 224 (`00:01:06:00`), 320, 416, 512, 608, 704, 800, 896, 992, 1088 and 1184.
- **A1 silence hatch:** x 563.2–624.0 and x 832.0–856.0 (normal strength; this track drives autocut).
- **Filmstrip:** 128×72 face-cam thumbnails.
- **Zoom readout:** `48 px/s`.
- **Hint bar:** as in S07.

### S09 · Timeline at frame level (32 px/frame)

- **Base:** S08.
- **View:** frames 3160 → 3192.
- **Clips:** the cut between c2d0 and c2d1 (both take 5, lily) is at x 688, with a 1 px `bg` divider. c2d1 is selected and carries the label "Take 5".
- **Playhead:** at x 688 (00:01:45:26).
- **Ruler:**
  - A tick at every frame boundary, every 32 px.
  - Frame numbers `10` … `29`, then `00` … `12`, centered in each cell.
  - `00:01:46:00` in `text` at x 816, with a taller tick.
- **Filmstrip:** one 32×72 center-cropped thumbnail per frame.
- **Lanes:** the waveform drawn at sample resolution (a smooth curve, not bars).
- **Zoom readout:** `32 px/frame`.
- **Hint bar:** `[←] [→] Previous / next frame` · `[Alt][[] [Alt][]] Nudge edge 1 frame` · `[S] Split` · `[−] Zoom out` · `[\] Fit`.

### S10 · Trimming, with the edge-frame preview

- **Base:** S08, with c2d0 selected. The user is dragging c2d0's **start** edge right by +29 frames.
- **Timeline:**
  - The active edge (a 6 px `accent` bar) is at x 436.8.
  - Frames 2084 → 2113 (x 390.4–436.8) show as trimmed away: a dimmed filmstrip (40%) inside a dashed `accent` outline, tagged "−29 f".
  - Clips after the edge show a small ripple tag: "← 00:00:00:29".
  - A tooltip at the edge reads: "In · take 5 frame 41 · 00:00:01:11", "Δ −29 f".
- **Canvas (scrub-while-trim):** Program shows **take 5 frame 41** (the new first frame), not the playhead. A 2 px `accent` stroke surrounds the preview, and a bottom-left `video-scrim` plate reads "Trim start · take 5 · frame 41".
- **Hint bar:** label-only "Release to trim" · `[Esc] Cancel drag` · `[N] Snapping on/off` · `[Alt][[] [Alt][]] Nudge after release`.
- **Visual details:** releasing creates one history entry ("Trim clip c2d0"). Snapping flashes a 1 px `snap-guide` line when the edge meets the playhead.

### S11 · Drag-move, with the insertion caret

- **Base:** S07 (fit). The user is dragging c8a1 (take 1, 64.3 px) to land after c1b6.
- **Timeline:**
  - c8a1's original slot has closed: c1b2–c1b6 shifted left by 64.3 px, animated 120 ms.
  - The **insertion caret** (2 px `accent` with triangles) sits at x 404.9, between c1b6 and c2d0.
  - A **lifted copy** of c8a1 (85%, e1, 4 px up) spans the video row and both lanes, centered on the pointer at about x 405.
  - A tooltip reads "Move after c1b6 · slot 7 of 10".
- **Canvas:** unchanged (the program frame at the playhead).
- **Hint bar:** label-only "Release to move" · `[Esc] Cancel drag` · `[N] Snapping on/off` · `[<] [>] Move with keys instead`.

### S12 · Autocut preview

- **Base:** S06 with c3e9 selected, zoomed to the clip (`Z`). The view is 00:02:22:26 → 00:04:06:11 at 0.335 px/f, with ruler labels every 10 s.
- **Timeline:**
  - Four hatched regions on A1 at full strength: x 362.2 (w 16.1), x 606.1 (w 14.1), x 842.5 (w 13.1) and x 1067.6 (w 19.1).
  - The same ranges are dimmed (`bg` at 55%) across the video row and A2.
  - A `caption` `warning-text` tag sits above each region: "−1.6 s", "−1.4 s", "−1.3 s", "−1.9 s".
- **Canvas:** Program at 00:02:41:12.
- **Panel:** replaced by the **Autocut** page.
  - **Header:** a `‹` back button, `Autocut` (`title`) and the subtitle "Previewing take 8 · c3e9".
  - **Controls:**
    - "Detect silence on" dropdown: `A1 · Yeti Nano (mic)`.
    - Sliders with value fields and range ticks:

      | Slider | Value | Range |
      | --- | --- | --- |
      | Threshold | −35 dB | −60 … −10 |
      | Min silence | 400 ms | 100 … 2000 |
      | Padding | 120 ms | 0 … 500 |
      | Min keep | 200 ms | 0 … 1000 |

  - **Summary block:** "Removes 4 silences (6.2 s)" in `title`, with "Take 8: 00:01:43:15 → 00:01:37:09 · Timeline: 00:04:06:11 → 00:04:00:05" in `caption` `muted`.
  - **Region list:** 4 rows in `mono-sm` (§3.6), each with its duration. Clicking a row seeks.
  - **Switch:** "Run autocut after every take" (off).
  - **Note** (`caption` `muted`): "Removed audio and video come back if you drag a clip edge out, or with Ctrl+Z."
  - **Footer:** `Cancel` (secondary) and `Apply autocut` (primary, with an `Enter` key cap).
- **Hint bar:** `[Enter] Apply autocut` · `[Esc] Cancel` · `[Space] Play` · `[=][−] Zoom`.
- **Visual details:** use the hatch component unchanged; widths come from the data. The selected clip keeps its 2 px stroke.

### S13 · Layer editing on the canvas, and the Onion layers panel

- **Base:** S02 (Live). The user is dragging L2 Face. Its left edge snaps to the canvas **right third** (x 1280).
- **Canvas:**
  - L2's selection box sits at preview (768, 444, 192, 144), with 8 handles.
  - A vertical dashed `snap-guide` runs at preview x 768 from top to bottom, tagged "Right third".
  - A readout tooltip near the box reads "x 1280 · y 760 · 384 × 288".
- **Panel:**
  - The L2 row is selected (R1).
  - The inspector shows X `1280` (live), Y `760`, W `384` with the aspect lock on, H `288`, crop `0 0 0 0`, opacity `100 %`, and Fit `Cover`.
  - Show the L1 row in its hover state (its lock and visibility toggles visible).
- **Panel state frame** (a second, smaller frame, 384×920): every layer-row state stacked (hidden, locked, renaming, dragging with drop line, error, missing media), plus an audio row clipping with the red latch.
- **Hint bar:** `[←][↑][↓][→] Nudge 1 px` · `[Shift] ×10 · free aspect` · `[Alt] Crop edges` · `[Esc] Deselect` · `[Tab] Program`.

### S14 · Bin drawer

- **Base:** S06 (Program) with the bin open (`B`). The drawer is at (0, 48, 352, 608) and the preview shrinks to 832×468 at (368, 100).
- **Drawer:**
  - **Header** (48): `Bin` (`title`), "5 takes · 436.4 MB" (`caption` `muted`), and a close button (tooltip `B`).
  - **Rows** (72 each): §3.4 data. Each row has a 96×54 thumbnail with a 3 px take-color stripe on top, the title "Take 4" (`body-strong`) with its status chip, a mono line `00:00:51:18 · 14:12 · 129.0 MB`, and the secondary line.
  - **Selected row:** Take 4 (R1). Its actions are revealed as a 32 px row of flat buttons: `Preview`, `Insert at end`, `Insert after selection`, a reveal icon button, and **`Purge from disk`** (destructive-flat, `record-text`, trash icon).
  - **Broken take 9:** a hatched thumbnail with an error icon, and an `Open log` link.
- **Hint bar:** `[B] Close bin` · `[↑][↓] Select take` · `[Enter] Insert at end` · `[Space] Preview`.

### S15 · Export panel, running (and its setup step)

- **S15a, setup** (base S06; Ctrl+E opens the popover at §2.8):
  - **Title:** `Export`.
  - **Format:** segmented `MP4` | `WebM`, with the caption "H.264 + AAC 192 kbps · VA-API on renderD129". For WebM, a switch "Use AV1 (slower, smaller)".
  - **Quality:** segmented `Draft` | `Standard` | `High`.
  - **Output:** the field `exports/rust-traits-talk-20261014-153012.mp4` with `Change…`.
  - **Summary:** "1920×1080 · 30 fps · 00:04:06:11 · 10 clips".
  - **Primary:** `Export` `[Enter]`.
- **S15b, running:**
  - **Header button:** `Exporting 42%` with a mini mud bar.
  - **Popover:**
    - Title "Exporting" and the file name in `mono-sm` `muted`.
    - The **mud bar** at 42% with 3 bubbles, `42%` in `mono-xl`, "Rendering clip 6/10" in `body`, and "3,449 / 7,391 frames" in `mono-sm` `muted`.
    - "Elapsed 00:00:38 · About 00:00:52 left" in `mono-sm`.
    - The caption "Exporting the timeline as it was at 15:30:12. You can keep editing."
    - `Cancel export` (secondary).
- **Timeline:** editable as normal underneath.
- **Hint bar:** `[Esc] Close panel (export continues)` · `[Ctrl][E] Show export` · plus the S06 hints after them.
- **Frames:** bubble frames A and B, and a reduced-motion plain bar.

### S16 · Export panel, done

- **Popover:**
  - `circle-check` (`success`) with "Export finished" (`headline`).
  - "Layers! Like an onion. Your video is ready." (`body`).
  - `exports/rust-traits-talk-20261014-153012.mp4` (`mono-sm`).
  - "88.4 MB · 1920×1080 · 30 fps · 00:04:06:11 · took 00:01:31" (`caption` `muted`).
  - A full mud bar.
  - Buttons: `Open file` (primary), `Open folder`, `Copy path`.
- **Header:** the button is back to `Export…`.
- **If the panel was closed:** the S25 success toast appears instead.

### S17 · Export panel, failed

- **Popover:**
  - `circle-x` (`record-text`) with "Export failed" (`headline`).
  - The lead: "ffmpeg stopped while rendering clip 3/10 (take 3, c1b2). Nothing was written to exports/."
  - The hint line (`body-sm`): "VA-API couldn't start on /dev/dri/renderD129. Try Settings › Recording › Encoder: x264."
  - The mud bar stopped at 37% with "Failed at 37%" (`record-text`).
  - A **stderr box**: a `field` well, `mono-sm`, 10 lines visible, scrollable, up to 30 lines. Sample tail:

```
ffmpeg version 7.1 Copyright (c) 2000-2024 the FFmpeg developers
Input #0, matroska,webm, from 'takes/take-0003/L1-screen-DP-4.mkv':
  Duration: 00:00:45.90, start: 0.000000, bitrate: 11982 kb/s
  Stream #0:0: Video: h264 (High), yuv420p(tv, bt709, progressive), 2560x1440, 30 fps
Input #1, matroska,webm, from 'takes/take-0003/L2-camera-0.mkv':
  Stream #1:0: Video: h264 (High), yuv420p(tv, bt709, progressive), 1280x720, 30 fps
[AVHWDeviceContext @ 0x5581c3e0a2c0] Failed to initialise VAAPI connection: -1 (unknown libva error).
Device creation failed: -5.
[vf#0:0 @ 0x5581c3dfa840] Error initializing filter 'hwupload' with args 'derive_device=vaapi'
Error opening output file .qrec/cache/segments/9f3c2a71e4.mkv.
Error opening output files: Input/output error
```

  - The link "Open full log" (`accent-text`, underlined) with `.qrec/logs/export-20261014-153012.log`.
  - Buttons: `Copy error` (secondary), `Try again` (primary), `Close` (flat).

### S18 · Destructive confirmation modal

- **Base:** S14 with takes 2 and 4 selected in the bin and **Purge from disk** pressed. The `scrim` covers the window.
- **Modal** (520 wide):
  - Title "Get out of my swamp?" (`display-sm`).
  - Lead (`body-strong`): "Purge take 2 and take 4 from disk?"
  - Body: "This permanently deletes 10 files (184.1 MB). It can't be undone, and the 6 history entries that mention these takes are removed too."
  - **Loss list** (`mono-sm`, right-aligned sizes):

    | File | Size |
    | --- | --- |
    | `takes/take-0002/L1-screen-DP-4.mkv` | 33.0 MB |
    | `takes/take-0002/L2-camera-0.mkv` | 16.6 MB |
    | `takes/take-0002/A1-mic.mka` | 2.0 MB |
    | `takes/take-0002/A2-system.mka` | 3.5 MB |
    | `takes/take-0002/recorder.log` | 12 KB |
    | `takes/take-0004/L1-screen-DP-4.mkv` | 77.3 MB |
    | `takes/take-0004/L2-camera-0.mkv` | 38.8 MB |
    | `takes/take-0004/A1-mic.mka` | 4.6 MB |
    | `takes/take-0004/A2-system.mka` | 8.3 MB |
    | `takes/take-0004/recorder.log` | 14 KB |

  - Footer: `Cancel` (secondary, **focused** with the ring visible) and `Purge 2 takes` (destructive, torch red).
- **Hint bar:** `[Tab] Move focus` · `[Enter] Activate focused button` · `[Esc] Cancel`.
- **Variant notes** (same shell; draw S18b for the fps change, the rest as copy notes):
  - **Export overwrite:**
    - Lead: "Replace talk-final.mp4?"
    - Body: "~/Videos/talk-final.mp4 already exists (91.2 MB, modified today at 15:02). Exporting here overwrites it."
    - Button: `Replace file`.
  - **Clear history:**
    - Lead: "Clear undo history?"
    - Body: "This removes 128 undo steps from .qrec/history.jsonl. The timeline stays as it is, but you can't undo anything done before now."
    - Button: `Clear history`.
  - **S18b, change fps:**
    - Lead: "Change the canvas to 25 fps?"
    - Body: "Clip bounds are re-quantized to the new frame rate. These 10 clips move by up to 1 frame:"
    - List: `c7f3 · take 1 · [0, 410) → [0, 342)`, `c8a1 · take 1 · [455, 912) → [379, 760)`, `c1b2 · take 3 · [0, 268) → [0, 224)`, `c1b3 · take 3 · [304, 611) → [253, 510)`, "…and 6 more".
    - Button: `Change fps`.

### S19 · Command palette

- **Base:** S06. `Ctrl+K` was pressed. The `scrim` covers the window and the palette is at §2.8.
- **Palette:**
  - A 40 px search field with the query `sp`.
  - Six result rows (label · key caps · CLI form in `mono-sm` `muted`). Matched letters are in `body-strong`, and the first row is highlighted (R2).

    | Command | Key | CLI |
    | --- | --- | --- |
    | Split clip | `S` | `qrec clip split` |
    | Save frame as PNG | `P` | `qrec frame save` |
    | Autocut settings and preview | `Shift` `C` | `qrec autocut --dry-run` |
    | Toggle snapping | `N` | — |
    | Select previous clip | `,` | — |
    | Seek to previous edit point | `↑` | `qrec seek --edit prev` |

  - Show a disabled row below: "Record over selection" `O` `qrec record start --over`, dimmed, with the reason "Needs a contiguous selection" (from a frame variant with c3e9 and c7f3 both selected).
  - An empty-query variant (optional frame): a "Recent" group with 3 rows, then every command grouped Record / Playback / Edit / Output / View.
- **Hint bar:** `[↑][↓] Choose` · `[Enter] Run` · `[Esc] Close`.

### S20 · Cheat sheet

- **Base:** S06. `?` was pressed. `scrim-strong` covers everything, with the sheet at (40, 40, 1520, 920).
- **Header:** "Keyboard shortcuts" (`headline`), a search field (320 px, placeholder "Search shortcuts"), and a close button (`Esc`).
- **Body:** three columns. Rows are 40 px with lg key caps, grouped as in SPEC §10.2:
  - **Column 1:** Record, Output, then **Global (niri)**: `Mod+Shift+R` Record toggle, `Mod+Shift+O` Record over, `Mod+Shift+D` Discard last take, plus a `Copy niri snippet` button.
  - **Column 2:** Playback, View.
  - **Column 3:** Edit.
- **Rebound example:** "Save frame as PNG" shows `[F]` followed by `caption` `muted` "default P".
- **Footer** (`caption` `muted`): "Keys come from ~/.config/qrec/keys.toml. Change them in Settings › Keyboard."
- **Variant (optional):** a search frame with `trim`, showing three rows with matches in `body-strong`.
- **Hint bar:** hidden under the scrim. The sheet itself is the reference.

### S21 · Settings, Appearance (theme picker)

- **Dialog** (840×640): title "Settings" (`headline`), a view switcher (Appearance · Recording · Project · Autocut · Keyboard), and a close button.
- **Appearance page:**
  - **Theme:** four C32 cards (180×132, a mini window preview plus a name). "Swamp Night" is selected and captioned "Default". The others are "Swamp Day", "Dragon's Keep" and "Ogre Mode". Each mini preview is the same component instanced with its own `theme`.
  - **Interface size:** segmented `100%` · `125%` · `150%`.
  - **Reduce motion:** a switch (off), captioned "Follows your system setting until you change it here."
  - **Sounds:** a switch "Squelch on record start and stop" (off).
- **Hint bar:** under the scrim; not needed.

### S22 · Settings, Keyboard (editing and conflicts)

- **Dialog:** the S21 shell with the Keyboard tab.
- **Conflict banner** at the top (a `warning` 16% tint with a 1 px `warning` stroke, `md` radius):
  - Warning icon, then "1 conflict in ~/.config/qrec/keys.toml".
  - Body: "X is bound to both Autocut and Remove clips. Neither runs until one is changed."
  - Button: `Open keys.toml`.
- A search field.
- **Group "Edit"** (rows: label · key caps · state · edit and reset icon buttons):
  - Split clip `[S]`.
  - Trim start to playhead `[[]`.
  - Remove clips `[Del]` `[X]`, with a conflict flag on `X`: "Also bound to Autocut".
  - Autocut `[X]` (rebound, "default C"), with a conflict flag: "Also bound to Remove clips".
  - Autocut settings and preview `[Shift]` `[C]`.
- **Group "Output":**
  - Save frame as PNG `[F]` (rebound, "default P") with a reset button.
  - Export… `[Ctrl]` `[E]`.
- **Capture state:** "Toggle snapping" shows the capture key cap "…" with the caption "Press a key · Esc cancels · Backspace clears".
- **Footer caption:** "Changes are saved to ~/.config/qrec/keys.toml."

### S23 · Missing-dependency screen

- **Base:** a blank window. The header bar is the minimal variant (icon, wordmark, close).
- **Content:** a centered column, 640 wide, top at y 120.
  - The wooden signpost illustration (160 px).
  - "The swamp is missing something" (`display`).
  - The body line: "Qrec can't start until these are installed. Run `qrec doctor` in a terminal for the same report."
- **Rows** (C20, 72 px):

| Icon | Item | What it does | Hint (mono box with Copy) |
| --- | --- | --- | --- |
| ✕ `record-text` | `gtk4paintablesink` | GStreamer GTK 4 video sink (gst-plugins-rs). Qrec draws the preview with it. | `nix develop` (the flake provides gst-plugins-rs) |
| ✕ `record-text` | ScreenCast portal backend | Needed to capture screens on Wayland. | `On niri, install xdg-desktop-portal-gnome, then log out and back in.` |
| ⚠ `warning-text` | `libsvtav1` in ffmpeg (optional) | AV1 export will be unavailable. Everything else works. | — |

- **Collapsed disclosure:** "✓ 14 other checks passed".
- **Buttons:** `Check again` (primary), `Copy report` (secondary), `Quit` (flat).
- **Hint bar:** `[Enter] Check again` · `[Ctrl][Q] Quit`.

### S24 · Portal-denied error

- **Base:** S02, at the moment after pressing `R`. The ScreenCast portal refused DP-4, so take 10 aborted within 500 ms.
- **Canvas:**
  - The L1 area (the whole preview, under L2) shows a muted hatch (20%) with a centered block: the `monitor-x` icon (32), "Screen access denied" (`title`), "DP-4 · GIGA-BYTE GS27QA" (`caption`) and `Try again` (secondary).
  - L2 Face stays live on top.
  - The `LIVE` badge. No REC badge, because recording never started.
- **Panel:** the L1 row in its error state (a `record-text` icon, subtitle "Access denied by the desktop portal").
- **Header:** idle `● Record [R]`. The toolbar Bin count is `6`.
- **Toast** (error, persistent): title "Recording didn't start". Body: "Screen access to DP-4 was denied, so take 10 stopped before any frames. It's marked broken in the bin." Actions: `Try again`, `Open bin`.
- **Variant note:** if the user closed the picker, the canvas title reads "Screen picker closed" and the body reads "Choose DP-4 in your desktop's dialog to record it."
- **Hint bar:** `[R] Try again` · `[Tab] Program` · `[B] Bin` · `[?] All keys`.

### S25 · Toasts

- **Frame:** a sheet (1600×1000) with every variant stacked on `bg`, each labeled. Also place one live toast in S16's "panel closed" variant.

| # | Level | Title | Body | Actions |
| --- | --- | --- | --- | --- |
| 1 | success | "Layers! Like an onion. Your video is ready." | `exports/rust-traits-talk-20261014-153012.mp4 · 88.4 MB` | Open file · Open folder |
| 2 | info | "Recovered take 7 from the bog." | "Qrec closed while it was recording. The take is in the bin." | Open bin |
| 3 | warning | "That was shorter than Lord Farquaad. Take moved to the bin." | "Take 6 lasted 7 frames. Takes need at least 10." | Open bin |
| 4 | success | "Frame saved" | `frames/rust-traits-talk-take-0005-00012107.png` | Copy image · Open folder |
| 5 | info | "Take 10 moved to the bin" | "Its clip left the timeline. Files are kept." | Undo `[Ctrl][Z]` |
| 6 | success | "Autocut take 8: removed 4 silences (6.2 s)" | — | Undo |
| 7 | info | "Nothing to discard" | "Take 10 has no clips on the timeline." | — |
| 8 | warning | "Camera disconnected" | "Logitech C920 stopped sending frames. Take 10 keeps recording without L2 Face." | — |
| 9 | warning | "Can't record while exporting" | "Recording works again once the export finishes or you cancel it." | Show export |
| 10 | error (stays) | "qrec.json changed on disk but isn't valid" | "/timeline/clips/4/out: 1400 is past the end of take 3 (1,377 frames). Keeping the last good state." | Copy details |
| 11 | error (stays) | "Recording didn't start" | (S24 body) | Try again · Open bin |

### Additional states (recommended; outside §15.1)

- **X1 · "A take is being recorded from the CLI."** (SPEC §4.3)
  - A modal at GUI start: the `display-sm` title "A take is being recorded from the CLI."
  - Body: "Take 11 started from the terminal 00:01:12 ago. Stop it to open the project here."
  - Buttons: `Quit` (secondary, focused) and `Stop & open` (primary).
- **X2 · Not my swamp** (folder rule, GUI start)
  - A blocking screen (S23 shell) with the signpost illustration and the `display` line "Not my swamp: this folder isn't empty and has no qrec.json".
  - Body: "Start Qrec in an empty folder to create a project, or in a folder that has qrec.json."
  - Shows `~/Downloads` (mono) and the button `Quit`.
- **X3 · Project can't be read** (SPEC §16)
  - The same shell. Title (`display-sm`): "qrec.json can't be read".
  - Shows the validation message, for example `/timeline/clips/4/out: 1400 > 1377`.
  - Recovery line: "Restore the last good state from history:" with `qrec history restore 212 --yes` and a `Copy command` button.
  - Button: `Quit`.

---

## 6. Interaction notes that affect drawing

- Clip **hover tooltip** (C33): take, clip id, in/out, duration, plus `[S] [[] []] [X]`.
- **Snapping on:** dragged edges snap to the playhead and clip edges with a 1 px `snap-guide` flash. On the canvas, layers snap to edges, the center and thirds.
- **Analyzing autocut:** each affected clip shows a spinner in its label plate (draw one in the S12 component sheet).
- **Proxy:** when playback can't hold 30 fps, the PROXY badge appears next to PROGRAM (draw it in the C06 sheet).
- **Focus order** in the main window: header → stage transport → timeline toolbar → timeline → panel → hint bar (the hint bar is not focusable). Draw one focused-state example per component in the sheets.

---

## 7. Theme coverage

| Frame | Swamp Night | Swamp Day | Dragon's Keep | Ogre Mode |
| --- | --- | --- | --- | --- |
| Component sheets C01–C34 | ✓ | ✓ (duplicate, theme override) | ✓ | ✓ |
| S01–S05, S07–S17, S19–S24, X1–X3 | ✓ | — | — | — |
| **S02 Main window, Live idle** | ✓ | ✓ | ✓ | ✓ |
| **S06 Program with selection** (selection, playhead, take stripes, waveforms) | ✓ | ✓ | ✓ | ✓ |
| S18 Destructive modal (scrim, Fraunces title, `text-on-record`) | ✓ | ✓ | — | — |
| S25 Toasts (all `-text` variants, light-theme shadows) | ✓ | ✓ | — | — |

Swamp Day gets the extra frames because it is the only light theme. Shadows, `field`, `scrim`, `text-on-record` and the proposals P1, P3, P4 and P5 all change there. Put each theme on its own canvas row under the Swamp Night row, in the same column order.

---

## 8. Assumptions and open questions for the product owner

**Q1 · First run.** SPEC defines no default scene, so S01a asks which detected sources to add. Should `qrec init` seed the scene with the primary screen, the default mic and system audio, so that R works on first launch?

**Q2 · `Space` in Live mode.** Assumed: it does nothing, the transport is disabled, and a tooltip suggests `Tab`. The alternative is that it switches to Program and plays. **Decided (issue #12, 2026-10-23): the alternative.** `Space`, `J` and `L` in Live switch to Program and play (docs/GUI.md D-01).

**Q3 · `Esc` during a drag.** Assumed: it cancels the drag-move or trim. SPEC lists `Esc` only for countdown, recording, selection and overlays.

**Q4 · Filmstrip source.** SPEC §7.2 says thumbnails come from the topmost visible layer. With the default scene that is the small face cam, so the filmstrip shows faces. Should it use the composited frame (what viewers see) instead?

**Q5 · Keybinding conflicts.** Assumed: both conflicting bindings are disabled until one changes, with a banner. Alternative: the later entry in `keys.toml` wins.

**Q6 · Bin keys.** Assumed: `↑` `↓` select, `Enter` inserts at the end, `Space` previews. Purge only from its button, never from a key.

**Q7 · Countdown control.** Assumed: during the countdown the header button becomes an outlined `Cancel [R]`.

**Q8 · Program-mode take layout actions.** They live in a `Layout ▾` menu in the panel banner rather than as three always-visible buttons.

**Q9 · Theme follows the system.** Should Qrec switch Swamp Night ↔ Swamp Day with the GNOME light/dark preference? Not designed yet.

**Q10 · Color proposals P1–P8** (DESIGN.md → Colors → Proposals). The designs use them; each can be declined one variable at a time.

**Q11 · Atkinson Hyperlegible Next.** Use it rather than the original Atkinson Hyperlegible, for its 600 and 800 weights. Same family, same license.
