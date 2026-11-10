---
name: htmlboard-designs-json
description: Curate an htmlboard canvas with designs.json (section titles and order, a design's name or size, hiding unlisted designs, showing components). Use when the designs folder has a designs.json, or the user wants sections named or ordered beyond what folders give.
---

# HtmlBoard designs.json

Without one, folders are the canvas's sections and number prefixes set the order (the htmlboard skill covers the designs themselves). A `designs.json` in the designs folder curates on top of that: sections with titles (accents, capitals, an order across folders), and values that win over the files'. The files still say the rest: their name, size and variants in their `<head>`.

```json
{
  "title": "Wallet app",
  "sections": [
    { "title": "Checkout · mobile", "designs": ["03-checkout/01-one-page.html", "03-checkout/02-steps.html"] },
    { "title": "Launch flyers", "folder": "04-flyers" }
  ]
}
```

- Sections come in their order, designs in theirs. An entry is a file name (its variants follow it; with a query string, it places one variant), or `{ "file", "title", "width", "height", "category", "description" }` to override the file's values.
- `"folder"` brings a whole folder, in prefix order.
- Designs it doesn't list still show after the ones it lists, in the section that holds their folder (or a section of their own), unless it says `"unlisted": "hide"`. When it groups designs by hand, add a new design to the right section.
- Components aren't on the canvas unless it says `"components": "show"` (or lists `components/<tag>.html`): each shows alone, from its first-line example, 480 wide unless its file has `<meta name="htmlboard" content="width=…">`.
- `"themes"` says how the designs' themes apply when it isn't the `data-theme` attribute on `<html>`, or which they are: `{ "attribute": "class", "values": ["light", "dark"] }` (classes on `<html>`), or just `["light", "dark"]`. The canvas's theme switch, `check --themes all` and `shot --theme` use them; without it, the canvas finds them in the stylesheets. A design can say its own in its `<head>`: `<meta name="htmlboard:themes" content="data-mode: day, night">`.
- Saving it rebuilds the canvas.
- `npx htmlboard migrate <designs folder>` moves an old designs.json's sizes, names and variants into the files and keeps only what they can't say (`--write` to apply; without it, it only shows the plan).
