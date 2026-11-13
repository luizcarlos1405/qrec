---
name: htmlboard
description: Design UI in plain HTML files and show them side by side on an infinite, live-reloading canvas with the htmlboard CLI. Use when the user asks for mockups, screens, design variants or explorations, flyers or any visual proposal, or mentions htmlboard or artboards.
---

# HtmlBoard

HtmlBoard serves a folder of HTML files as artboards on a zoomable canvas, like a Figma page. Every design is one ordinary HTML file that says its own name and size, so you design by writing HTML and CSS, and the user sees the result update live while you work.

Two more skills cover the rest when you need it: **htmlboard-components** to write or change a component, **htmlboard-designs-json** when the designs folder has a `designs.json`.

## Start the canvas

Run it in the background, pointed at the designs folder (it is created if missing):

```sh
npx htmlboard designs --open
```

- It prints the URL (default `http://localhost:3000`; if the port is busy it takes the next free one). Give the URL to the user.
- `-p <port>` picks the port. `--init` adds sample designs; skip it unless the user wants a starter.
- If the artboards link to files above the designs folder (`../css/theme.css`), serve from the project root: `npx htmlboard design --root .`
- An empty folder gets the samples automatically, so write your first artboard before starting the server.
- Running it again for a folder that already has a canvas doesn't start another: it prints that canvas's URL.

## Write an artboard

One design per file, with its facts in the `<head>`, linking the shared stylesheet and components (see below):

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>Checkout · one page</title>                         <!-- its name on the canvas -->
  <meta name="description" content="Address and payment on one screen">
  <meta name="htmlboard" content="width=390, height=844">   <!-- its size -->
  <link rel="stylesheet" href="../styles.css">              <!-- shared tokens and classes -->
  <script type="module" src="../components.js"></script>    <!-- shared components -->
  <style>
    .screen { height: 844px; /* only what this design needs */ }
  </style>
</head>
<body>
  <main class="screen">…</main>
</body>
</html>
```

- Always give the `width`: it is the frame's viewport, and the page's CSS needs it. Give the `height` for fixed sizes (a phone screen, a flyer: the frame cuts at it); leave it out for pages that should grow with their content.
- Design at the real size of the target: phone 390×844, tablet 768×1024, desktop 1280×800 (or 1440×900), and the exact size for print or social formats (e.g. 1080×1350).
- Use real content in the user's language, not lorem ipsum.
- Relative links to CSS, images, scripts and fonts work (`assets/logo.svg`; `../css/theme.css` with `--root`). No build step: plain HTML, CSS and browser JavaScript (ES modules and CDN scripts work).

More tags, all optional:

- `<meta name="htmlboard:variant" content="?state=empty · empty">` shows the same file again with that query string, named "<title> · empty": for states the page builds from its URL (empty or error states, data; themes need none, see Shared styles). A variant can also change the size: `content="width=390, height=844 · mobile"` shows a responsive page at a phone's size too, from the same file (`?state=empty width=390 · empty mobile` does both). One tag per variant; they follow the file.
- `<meta name="htmlboard:label" content="light">` adds a label to the name of the file's own frame ("Checkout · light"), to tell it from its variants.
- `category=…` in the `htmlboard` tag sets the explorer's filter (`mobile`, `components`, your own word) when the path and size don't say it.
- `<meta name="htmlboard" content="skip">` leaves a file out (partials, templates, a page that isn't a design).

## Shared styles

Put what the designs share in one stylesheet instead of repeating it in every file: it keeps the designs consistent and spares writing the same CSS again.

- The designs folder has a `styles.css` at its top (sample projects start with one). Before writing a design, see what the designs already share: `npx htmlboard catalog <designs folder>` prints it in a few lines, read from the files (each stylesheet's variables and classes, the scripts, the components with their attributes and an example). Use what fits (`var(--accent)`, `.btn`, `<tx-item>`) and add what's missing; open a file only for what the list doesn't say.
- If there is none and the project already has a stylesheet or design tokens, link those. Otherwise create `styles.css` with your first design: colors, type and spacing as custom properties, the base styles, and the classes you'll repeat.
- Link it with a relative path: `styles.css` from the top of the folder, `../styles.css` from a section folder.
- A design's own `<style>` holds only what is specific to it. When a second design needs the same thing, move it to `styles.css`. Split it when it grows (`forms.css`, `charts.css`) and link the extra files where they are used.
- Editing `styles.css` restyles every design that links it, which is the point for a product's screens. For one-off explorations that should keep their look (a different style, logo options), style them in their own file instead.
- Themes need no variants: the canvas's theme switch shows every design in a theme its stylesheets have, `[data-theme="dark"]` rules (the attribute on `<html>`) or the system's light and dark (`prefers-color-scheme`, `light-dark()`). For another attribute, or classes on `<html>`, say so in designs.json (htmlboard-designs-json skill).

## Components

Markup that designs repeat (a sidebar, a tab bar, a list row, a card with content) goes in a component, a file in `components/` that designs use as one tag instead of the block: `<tx-item icon="☕" name="Blue Bottle" amount="-$6.75"></tx-item>`.

- The catalog lists the project's components with their attributes, an example and the designs that use them. Use them; don't copy their markup into designs.
- Make one when a second design repeats a block (or a design repeats it three times); not for what a `styles.css` class already does, nor in one-off explorations. To write or change one, use the htmlboard-components skill.

## Group and order

- **A folder is a section** (a row on the canvas), named after the folder. Put related designs in a folder: `designs/03-checkout/`.
- **Number prefixes set the order** of files and folders: `01-one-page.html`, `02-steps.html`. They don't show in names or links, so you can renumber freely. Files at the top of the designs folder form a first row without a title.
- New files and folders show up on the canvas by themselves; you don't register them anywhere.
- A `designs.json` in the designs folder, when there is one, curates the canvas (section titles, an order across folders, designs it hides): use the htmlboard-designs-json skill before adding, moving or renaming designs there.

## Live reload

Saving a design reloads its frame, saving a file designs use (a stylesheet, a component, an image) reloads the designs that loaded it, and new or removed designs show up by themselves. The user's view stays put: there is nothing to refresh.

## Working with the user

- Put variants in separate files next to each other (`01-hero-a.html`, `02-hero-b.html`) instead of overwriting the current design, so they can be compared on the canvas. Keep the current one as the reference until the user picks.
- Say which files you created or changed and what to look at; the user is watching the canvas, not the code. Link straight to a design with its file after the `#`: `http://localhost:3000/#03-checkout/02-steps.html`.
- Before reporting back, run `npx htmlboard check <designs folder>` (or a design's file): the canvas loads each design again, in the user's browser or in one out of sight, and lists what went wrong (script errors, missing files and components, fonts that didn't load, content cut at the frame's edge, text cut off or spilling out). Fix it and run it again. A note about content below a fixed-height frame is fine for a screen that scrolls, not for a flyer. It works with no canvas running too; if it says there's no browser to run it in, ask the user to open the canvas's URL.
- When the catalog lists themes, check every design in each: `check --themes all` (a problem found in some themes only says which), and look at one with `shot --theme dark`.
- The check can't judge looks (colors, spacing, alignment): look at the design with `npx htmlboard shot <file>`, which saves it as a PNG and prints where. An image costs you by its area: `--max 1000` makes a smaller one of the whole screen, `-s <selector>` shows one part (`-s .table`), sharper and cheaper.
