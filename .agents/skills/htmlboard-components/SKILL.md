---
name: htmlboard-components
description: Write or change an htmlboard component, a file in components/ that designs use as a custom element (components/tx-item.html is <tx-item>). Use when a block repeats across htmlboard designs, or before creating or editing a file in components/.
---

# HtmlBoard components

A component is markup that designs repeat (a sidebar, a tab bar, a list row, a card with content), written once and used as one tag: `<tx-item icon="☕" name="Blue Bottle" amount="-$6.75"></tx-item>`. The htmlboard skill covers the designs themselves.

- `components/tx-item.html`, at the top of the designs folder, is `<tx-item>`: the file name is the tag, with a hyphen and no number prefix.
- Designs load `components.js` once, after their stylesheet: `<script type="module" src="../components.js"></script>`. If the designs folder has no `components.js`, run `npx htmlboard components <designs folder>`.
- See what exists first: `npx htmlboard catalog <designs folder>` lists the components with their attributes, an example and the designs that use them.

A component file is a piece of HTML (no `<!DOCTYPE>`, `<html>`, `<head>` or `<body>`):

```html
<!-- <tx-item icon="💼" name="Invoice #1042" meta="Yesterday" amount="+$3,450.00" positive></tx-item> -->
<div class="icon">{{icon}}</div>
<div class="what"><b>{{name}}</b> <span class="muted">{{meta}}</span></div>
<div class="amount">{{amount}}</div>

<style>
  & { display: flex; align-items: center; gap: 14px; }   /* & is the element itself */
  .icon { width: 40px; height: 40px; border-radius: 12px; background: var(--surface-2); }
  &[positive] .amount { color: var(--emerald); }
</style>
```

- First line: a comment with an example of use, with real values, every attribute and its variants side by side. The catalog shows it, and it is the component's preview on the canvas.
- `{{name}}` is the element's `name="…"` attribute, as text. Content goes in slots: `<slot></slot>` takes the element's children, `<slot name="actions"></slot>` its children with `slot="actions"`, and what a slot holds shows when nothing fills it.
- No logic in the markup: variants are attributes styled in CSS (`&[positive]`), and a list is several elements.
- Its `<style>` only reaches inside the element, which is a block box unless `&` says otherwise. `styles.css` applies inside too: build with its classes (`card`, `btn`, `muted`) and name the component's own parts with names it doesn't use. A design's own `<style>` comes after and can restyle one element, but its selectors reach inside components too, so keep them specific.
- `<script>` runs for each element once it is built, with `this` as the element. Use it sparingly, a few lines for what markup and CSS can't do, like marking the current link (`<app-sidebar active="Customers">`) or a toggle. No fetching, data or frameworks.
- Relative links in a component start from its file (`../01-screens/02-customers.html`).
- Close the tag: `<tx-item …></tx-item>`, never `<tx-item />`. A component can't be a table's row or cell itself (the browser moves it out of the `<table>`), but it works inside a cell: `<td><org-cell …></org-cell></td>`. Where it sits (fixed, margins, the grid around it) belongs to the design.
- Editing a component changes every design that uses it, like `styles.css`. Afterwards, `npx htmlboard check <designs folder>` lists components that weren't built and errors in their scripts.
