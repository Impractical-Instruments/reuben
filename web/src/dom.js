// dom.js — the tiny vanilla-DOM element helper shared by the shell (main.js) and the chat
// module (chat/*). Extracted verbatim from main.js so both build their DOM through ONE helper
// (no framework, ADR-0041's "the shell owns only the shell"). Props: `class` sets className,
// `dataset` merges into el.dataset, an `on*` function value adds a listener, anything else is a
// non-null attribute. Children flatten, and a plain value becomes a text node.

export function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === "class") el.className = v;
    else if (k === "dataset") Object.assign(el.dataset, v);
    else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v);
    else if (v != null) el.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c == null) continue;
    el.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return el;
}
