// components.js: every file in components/ is a custom element named after it, so
// components/tx-item.html is <tx-item>. Designs load this file once, beside styles.css:
//
//   <script type="module" src="../components.js"></script>
//
// A component file is a piece of HTML (no <html>, <head> or <body>):
//
//   <!-- <tx-item name="Coffee" amount="-$6.75"></tx-item> -->   first: how to use it
//   <b>{{name}}</b> <span class="amount">{{amount}}</span>          the element's attributes, as text
//   <slot></slot> <slot name="icon"></slot>                         its content; its children with slot="icon"
//   <style> & { display: flex } .amount { … } </style>              only for this element (& is the element)
//   <script> this.… </script>                                       runs for each element, this = the element
//
// Relative links in it start from its file. Components can use other components.
// Added by htmlboard (npx htmlboard components); yours to edit.

const FOLDER = new URL('components/', import.meta.url);
const AsyncFunction = (async () => {}).constructor;
// At-rules that can't go inside a style rule: they stay at the top of the component's CSS
const TOP_LEVEL_RULES = ['CSSKeyframesRule', 'CSSFontFaceRule', 'CSSPropertyRule', 'CSSCounterStyleRule',
  'CSSFontFeatureValuesRule', 'CSSFontPaletteValuesRule'].map((name) => globalThis[name]).filter(Boolean);

const loading = new Map(); // tag → its component loading
const scripts = new Map(); // URL of a <script src> a component needs → it loading
const built = new WeakSet();

const isContent = (node) => node.nodeType === Node.ELEMENT_NODE || node.textContent.trim() !== '';
const escape = (text) => text.replace(/[&<>"']/g, (char) => `&#${char.charCodeAt(0)};`);
// Not "#…", "?…", "/…", "{{…}}" or with a scheme ("https:", "data:", "mailto:")
const isRelative = (url) => url.trim() !== '' && !/^\s*([a-z][a-z\d+.-]*:|[/#?]|\{\{)/i.test(url);
const absolute = (el, attribute, base) => new URL(el.getAttribute(attribute), base).href;

// The elements of tags nobody defined yet, in root and under it, load their components. A custom
// element's name has a hyphen: other elements can be undefined too (<tr is="…">), and aren't ours.
function discover(root) {
  for (const el of [root, ...root.querySelectorAll(':not(:defined)')]) {
    if (el.localName.includes('-') && el.matches(':not(:defined)')) load(el.localName);
  }
}

function load(tag) {
  if (!loading.has(tag)) loading.set(tag, define(tag).catch((error) => console.error(`<${tag}>`, error)));
  return loading.get(tag);
}

async function define(tag) {
  const url = new URL(`${tag}.html`, FOLDER);
  const response = await fetch(url);
  if (!response.ok) {
    if (!customElements.get(tag)) console.warn(`<${tag}>: there is no ${decodeURI(url.pathname)}`);
    return;
  }
  const component = parse(await response.text(), url, tag);
  await Promise.all(component.needs.map(addScript));
  if (customElements.get(tag)) return; // defined by another script meanwhile
  customElements.define(tag, class extends HTMLElement {
    connectedCallback() {
      if (built.has(this)) return;
      built.add(this);
      build(this, component);
    }
  });
}

// A component file: its markup, without its <style>, <script>, comments and what the canvas reads.
function parse(source, url, tag) {
  const template = document.createElement('template');
  template.innerHTML = source;
  const { content } = template;
  let css = '';
  let code = '';
  const needs = [];
  for (const node of [...content.children]) {
    if (!['style', 'script', 'link', 'meta', 'title'].includes(node.localName)) continue;
    node.remove();
    if (node.localName === 'style') css += `${node.textContent}\n`;
    else if (node.localName === 'script' && node.hasAttribute('src')) needs.push(absolute(node, 'src', url));
    else if (node.localName === 'script') code += `${node.textContent}\n`;
    else if (node.localName === 'link' && /\bstylesheet\b/i.test(node.getAttribute('rel'))) addLink(absolute(node, 'href', url));
  }
  const walker = document.createTreeWalker(content, NodeFilter.SHOW_COMMENT);
  const comments = [];
  while (walker.nextNode()) comments.push(walker.currentNode);
  for (const comment of comments) comment.remove();
  for (const el of content.querySelectorAll('[src], [href], [poster]')) {
    for (const attribute of ['src', 'href', 'poster']) {
      const value = el.getAttribute(attribute);
      if (value && isRelative(value)) el.setAttribute(attribute, new URL(value, url).href);
    }
  }
  addStyle(tag, css, url);
  let run = null;
  if (code.trim()) {
    try {
      run = new AsyncFunction(code);
    } catch (error) {
      console.error(`<${tag}> <script>`, error);
    }
  }
  return { markup: template.innerHTML.trim(), run, needs };
}

// An element of the component: the markup with the element's attributes, and its content in the slots.
function build(el, { markup, run }) {
  let depth = 0;
  for (let node = el.parentElement; node; node = node.parentElement) {
    if (node.localName === el.localName && ++depth > 25) return console.error(`<${el.localName}> holds itself`);
  }
  const template = document.createElement('template');
  template.innerHTML = markup.replace(/\{\{\s*([^{}\s]+)\s*\}\}/g, (_, name) => escape(el.getAttribute(name) ?? ''));
  const { content } = template;
  const given = [...el.childNodes];
  const slots = content.querySelectorAll('slot');
  if (slots.length) {
    const placed = new Set();
    for (const slot of slots) {
      const name = slot.getAttribute('name') || '';
      const nodes = given.filter((node) => !placed.has(node) && (node.getAttribute?.('slot') || '') === name);
      nodes.forEach((node) => placed.add(node));
      // A slot nobody fills shows what it holds
      slot.replaceWith(...(nodes.some(isContent) ? nodes : slot.childNodes));
    }
  } else if (given.some(isContent)) {
    // <tx-item /> isn't self-closing in HTML: what came after it was parsed into it, so it goes back
    el.after(...given);
  }
  el.replaceChildren(content);
  run?.call(el).catch((error) => console.error(`<${el.localName}> <script>`, error));
}

// The component's box and CSS, once per page. Its CSS goes inside :where(tag) { … }, so it only
// reaches the element (& is the element itself) without adding weight to its selectors. It comes
// after the shared stylesheets and before the page's own <style>, so a design can still restyle it.
function addStyle(tag, css, url) {
  css = css.replace(/url\(\s*(['"]?)(.*?)\1\s*\)/g, (whole, quote, value) =>
    isRelative(value) ? `url("${new URL(value, url)}")` : whole);
  const top = [];
  try {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(css);
    for (const rule of sheet.cssRules) if (TOP_LEVEL_RULES.some((type) => rule instanceof type)) top.push(rule.cssText);
  } catch {
    // Nothing to take to the top
  }
  const style = document.createElement('style');
  style.dataset.component = tag;
  style.textContent = `:where(${tag}:not([hidden])) { display: block; }\n${top.join('\n')}\n:where(${tag}) {\n${css}\n}\n`;
  toHead(style);
}

function addLink(href) {
  if ([...document.querySelectorAll('link[rel~="stylesheet"]')].some((link) => link.href === href)) return;
  toHead(Object.assign(document.createElement('link'), { rel: 'stylesheet', href }));
}

// A script a component needs (<script src="…">), loaded once, before its elements are built
function addScript(src) {
  if (!scripts.has(src)) {
    scripts.set(src, [...document.scripts].some((script) => script.src === src) ? Promise.resolve() : new Promise((resolve) => {
      const script = Object.assign(document.createElement('script'), { src });
      script.onload = script.onerror = resolve;
      document.head.append(script);
    }));
  }
  return scripts.get(src);
}

// Ahead of the page's own <style>, after everything else in the <head>
function toHead(node) {
  document.head.insertBefore(node, document.head.querySelector('style:not([data-component])'));
}

// Every element of the page gets built, and so do the ones added later (by the page's scripts, or
// inside other components).
discover(document.documentElement);
new MutationObserver((records) => {
  for (const record of records) {
    for (const node of record.addedNodes) if (node.nodeType === Node.ELEMENT_NODE) discover(node);
  }
}).observe(document.documentElement, { childList: true, subtree: true });
