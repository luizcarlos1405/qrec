---
version: 1
slug: "design-qrec-pen"
primary_target: "design/qrec.pen"
related_targets: ["design/BRIEF.md"]
---

# Surface brief: design/qrec.pen (Qrec main window and all §15.1 states)

## Scope and mode

- **Surface:** every screen and state in SPEC §15.1, designed on the pen.dev canvas at 1600×1000, plus the reusable component sheets.
- **Visitor mode:** Operate. The visitor records takes and edits a timeline. The screen must read as a native libadwaita recorder/editor first.
- **Working brief:** `design/BRIEF.md` (geometry, sample data, copy, hint-bar contents and theme coverage per state). **Visual system:** `DESIGN.md`. **Product truth:** `PRODUCT.md`.
- **Audience and job:** a keyboard-first developer recording talks on Wayland. They glance at the window between takes, so the record state and the last take must read in about a second.
- **Constraints:** native GTK4 + libadwaita conventions. The direction is brief-pinned by SPEC §15 (four themes, three fonts, pinned copy, the onion icon, motion). Color proposals P1–P8 await the product owner.

## Direction contract

THESIS: A working NLE-and-recorder in one window, where the hot loop (R, talk, R) is always one key away and always visible. It refuses the category default: a grey OBS or Resolve clone with a blue accent, a red dot that also means "error", and hidden shortcuts.

OWN-WORLD: Bog-tinted neutrals per theme. Ogre green for what you act on, onion gold for time (playhead, guides), torch red only for recording and destruction. Eight swamp take colors on 4 px clip stripes. JetBrains Mono for every number, Atkinson for the UI, Fraunces only in the wordmark, empty states and modal titles. Key-cap chips on every action, flat tonal layers with 1 px hairlines, and a mud-filled export bar.

STORY: The user sees exactly what will be recorded (the Live canvas and live meters). They press R and watch the torch flicker and a ghost clip grow, then press R and see the take land in its color. They trim, split and autocut with keys the hint bar keeps teaching, export, and read "Layers! Like an onion. Your video is ready."

FIRST VIEWPORT: 1600×1000.
- A 48 px header: icon + wordmark, the Live/Program switch, the centered project title, the torch Record [R] button, Export…, palette, settings.
- A 960×540 preview centered on a 1216×608 stage, with the LIVE badge, the transport row and Save frame [P].
- The 384 px Onion layers panel at full height on the right.
- A 1216×312 timeline: toolbar, ruler, filmstrip row with take stripes, A1/A2 waveform lanes.
- A 32 px hint bar of key caps.
- The primary action is Record: top-right in red, and first in the hint bar.

FORM: Brief-pinned, with native libadwaita conventions plus the SPEC §15 world. No ordered candidate list; the world and composition come from the spec and the owner's layout. The signature move is "the torchlit take": the REC torch flicker plus the ghost clip that settles into its take color. seed: none (unavailable)

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Memorable moment

The torchlit take: the flickering onion-trumpet REC badge, and the ghost clip settling into its take color when recording stops.

## Unresolved decisions

See `design/BRIEF.md` §8 (Q1–Q11). The biggest are seeding the scene on first run (Q1), the filmstrip source (Q4), the keybinding-conflict behavior (Q5), and the PO verdict on the color proposals P1–P8 (Q10).
