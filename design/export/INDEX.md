# design/qrec.pen, exported

Read-only exports of `design/qrec.pen` made through the pen.dev MCP on 2026-10-17, while the product owner had the file open. The `.pen` file is encrypted; these are the copies tools can read. Regenerate them the same way after the design changes.

- `html/` (not committed, ignored): pen.dev's html-css export of a frame (no scaffold), made through the pen MCP when a tool needs exact positions, sizes, paddings, fonts and resolved colors (Swamp Night) or the icons as inline SVG (`data-icon-name`). `design/qrec.pen` is the source; read it through the pen MCP first. `crates/qrec-gui/tests/art.rs` regenerates the art from `html/artwork.html` and `tests/icons.rs` the Stop square from `html/S04.html` when those are present.
- `icons/`: one SVG per Lucide icon and drawn size (`<icon>@<w>x<h>.svg`), cut from the frames' HTML. Every size of an icon shares one path on a 14-unit grid.
- `art/`: the art components the GUI tests compare against (app icon, quiet swamp, signpost, REC glyph) as PNG at 1x and 2x, and the app icon at 16–512 px; the sample screen and webcam images are in `../../DESIGN-assets/`. Their vector form is the `w7Qg8` frame (`html/artwork.html` when exported). Token fills: the quiet swamp and the signpost use `$muted` and `$border`, the REC glyph `$text-on-record`; the rest are fixed colors.
- The component sheets and the token sheet live only in `design/qrec.pen`.
- `../screens/`: the frames at 1x; the drafts S19–S25 and X1–X3 were added in this export. The slice frames were re-exported and compared: 22 of 26 are pixel-identical to the committed PNGs, and S03, S13b, S14 and S17 differ only in icon antialiasing.

The variables (93: 75 colors × 4 themes, 15 numbers, 3 fonts) match `design/tokens/qrec-tokens.json` exactly (one fingerprint over every value).

The frames' node ids, to export one again (`<file>.html` was its name in the 2026-10-17 export):

| HTML file | Node |
| --- | --- |
| `S02.html` | `AAwr6` |
| `S21-settings-appearance.html` | `C6zGK` |
| `components-actions.html` | `D5YpW` |
| `S15b.html` | `FDGeR` |
| `S18b.html` | `GPmrg` |
| `S19b-command-palette-empty-query.html` | `GRSEE` |
| `S22-settings-keyboard.html` | `GoRnZ` |
| `components-progress-and-empty-states.html` | `Hpn5S` |
| `S04b.html` | `HrMeR` |
| `S08.html` | `I0MXCt` |
| `main-window-template.html` | `IgZQQ` |
| `S01a.html` | `Nb8Af` |
| `S25-toasts.html` | `PK7Ru` |
| `S20-cheat-sheet.html` | `QHCJG` |
| `S15c.html` | `SEFwj` |
| `S05.html` | `T9p3bU` |
| `S04.html` | `VCWz6` |
| `S20b-cheat-sheet-search.html` | `W5kkMx` |
| `S16.html` | `XTRnH` |
| `S01b.html` | `ZMsnB` |
| `S16b.html` | `ZonF5` |
| `S05b.html` | `ZozhM` |
| `components-inputs.html` | `a0EPw` |
| `S03.html` | `bQm7H` |
| `components-canvas.html` | `c2WnJ` |
| `X3-project-cant-be-read.html` | `cUduQ` |
| `s04-notes-rec-torch-flicker.html` | `d4rx6s` |
| `S23-missing-dependency.html` | `eIdTG` |
| `S24-portal-denied-error.html` | `g2i1Br` |
| `components-onion-layers-panel.html` | `gOGoh` |
| `S13.html` | `ghVHI` |
| `X2-not-my-swamp.html` | `h0SuZd` |
| `components-lists.html` | `jgyuK` |
| `S06.html` | `jnKIl` |
| `components-timeline.html` | `kXTUV` |
| `S15a.html` | `l0fva` |
| `components-window-chrome.html` | `lgfRg` |
| `S07.html` | `m5RAx` |
| `tokens.html` | `mSTv0` |
| `S17.html` | `mkkLb` |
| `S10.html` | `nZyqg` |
| `S11.html` | `rPfN1` |
| `components-overlays.html` | `u47lz` |
| `S18.html` | `uSm2m` |
| `S09.html` | `uZjSh` |
| `S14.html` | `vRbn3` |
| `S19-command-palette.html` | `vXZe6` |
| `artwork.html` | `w7Qg8` |
| `X1-cli-recording-at-gui-start.html` | `wsZqr` |
| `S12.html` | `ykvG3` |
| `S13b.html` | `zXBb6` |
