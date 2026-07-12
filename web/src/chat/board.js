// chat/board.js — the node-identity surface board (spec §3.6, an OBSERVABLE REQUIREMENT of
// #355). The surface is the always-on primary artifact (spec §3.2); this is where its controls
// live, and the load-bearing rule is that layout keys on NODE IDENTITY, not a fresh sort each
// render:
//
//   - a survivor (a control whose node is present before AND after a reshape) HOLDS ITS POSITION
//     — the board evolves, it never reshuffles, so the knob the user just learned stays put;
//   - an added node animates IN (appended, marked entering);
//   - a removed node animates OUT (marked exiting, then detached).
//
// A board that re-sorts every turn re-buries that knob — so `update()` below reuses each
// surviving cell IN PLACE and only ever appends genuinely new nodes. The negative of this
// property is the load-bearing test: a shuffled re-sort MUST move controls (proving the board
// isn't doing that) — `__resortRebuild` exposes exactly that anti-pattern for the spec to assert
// against.
//
// Each control is rendered through the surface's REAL binding (render.mjs `renderSurface`, one
// widget at a time) so the board never re-derives an address or a scaling — the on-screen
// control and the headless check drive the engine through the identical `emit`/`initial` path
// (ADR-0043). Re-layout (explicit "rearrange / clean up" + the agent's proactive suggestion on a
// big diff) is a §3.6 seam left as a no-op here; the behavior lands with the change-card work
// (#358).

import { renderSurface } from "../../../crates/reuben-web/js/surface/render.mjs";
import { h } from "../dom.js";

// The fields that make two resolved widgets for the SAME node "the same" for the board's purpose:
// a differing default/range/label/kind is a changed control (re-render in place, value-sweep);
// everything else about a widget is derived from these. Kept narrow + stable so an incidental
// object-identity change never reads as a reshape.
function sameWidget(a, b) {
  const key = (w) =>
    JSON.stringify([w.kind, w.widget, w.label, w.min, w.max, w.default, w.note, w.degree, w.group]);
  return key(a) === key(b);
}

// Render exactly ONE widget into a cell through the surface's real binding — a single-widget
// mini-surface so `renderSurface` wires this control's events to the engine the same way the
// full surface would. The cell carries its node identity (`data-node`) and a creation stamp
// (`data-cell-uid`, a monotonic counter) so a reused cell is provably the SAME element across a
// reshape (identity held) and a rebuilt one provably is not.
function makeCell(widget, engine, uid) {
  const cell = h("div", {
    class: "board-cell",
    dataset: { node: widget.address, cellUid: String(uid), boardState: "resident" },
  });
  renderSurface({ widgets: [widget], rows: [[widget]] }, engine, cell);
  return cell;
}

/**
 * Create a node-identity surface board (spec §3.6). Returns the mount element plus the identity-
 * keyed `update` and the re-layout seams.
 *
 * @returns {{
 *   el: HTMLElement,
 *   update: (widgets: object[], engine: object) => void,
 *   nodes: () => Array<{node: string, uid: string, index: number}>,
 *   relayout: () => void,
 *   onSuggestRelayout: () => void,
 *   __resortRebuild: (widgets: object[], engine: object) => void,
 * }}
 */
export function createBoard() {
  const el = h("div", { class: "surface-board" });
  const cells = new Map(); // node address -> { cell, widget }
  let uidSeq = 0;

  // Detach an exiting cell once its animate-out finishes (or a fallback timeout, so a
  // reduced-motion / animation-suppressed context still cleans up). The cells map is updated
  // synchronously in `update` — this only reaps the DOM node after its exit is seen.
  function reap(cell) {
    let done = false;
    const remove = () => {
      if (done) return;
      done = true;
      cell.remove();
    };
    cell.addEventListener("animationend", remove, { once: true });
    setTimeout(remove, 260);
  }

  return {
    el,

    /**
     * Reconcile the board to `widgets`, keyed on node identity (spec §3.6). Survivors hold their
     * DOM slot (reused in place; re-rendered only if the control's own values changed), removed
     * nodes animate out, genuinely new nodes are appended + animate in. Never re-sorts.
     */
    update(widgets, engine) {
      const incoming = new Map(widgets.map((w) => [w.address, w]));

      // Removed: a mounted node absent from the new set animates out, then detaches.
      for (const [key, entry] of cells) {
        if (!incoming.has(key)) {
          entry.cell.dataset.boardState = "exiting";
          reap(entry.cell);
          cells.delete(key);
        }
      }

      // Survivors + added: walk the incoming order. A survivor is reused WHERE IT IS (position
      // held); only a genuinely new node is created and appended (so a re-sorted input can never
      // move an existing control). A survivor whose values changed re-renders in place.
      for (const w of widgets) {
        const existing = cells.get(w.address);
        if (existing) {
          if (!sameWidget(existing.widget, w)) {
            renderSurface({ widgets: [w], rows: [[w]] }, engine, existing.cell);
            existing.widget = w;
            existing.cell.dataset.boardState = "changed";
          }
          continue;
        }
        const cell = makeCell(w, engine, ++uidSeq);
        cell.dataset.boardState = "entering";
        el.appendChild(cell);
        cells.set(w.address, { cell, widget: w });
      }
    },

    // The live board as an ordered list of {node, uid, index} — the test hook reads this to
    // assert position + identity stability. Excludes cells mid-exit so it reflects resident nodes.
    nodes() {
      return [...el.children]
        .filter((c) => c.dataset.boardState !== "exiting")
        .map((c, index) => ({ node: c.dataset.node, uid: c.dataset.cellUid, index }));
    },

    // §3.6 seam — explicit user-directed re-layout ("rearrange this / clean up the layout"). The
    // BEHAVIOR (re-sorting into a tidier arrangement) is #358's; this is the named no-op hook so
    // that ticket has somewhere to land it without reopening the spine.
    relayout() {
      /* no-op seam (#358) */
    },

    // §3.6 seam — the agent PROACTIVELY suggesting a re-layout on a big structural change (the
    // ~4–5-change §4.4 threshold). A no-op sink today; #358 wires the suggestion + accept here.
    onSuggestRelayout() {
      /* no-op seam (#358) */
    },

    // TEST-ONLY negative control. The FRESH-SORT anti-pattern §3.6 forbids: wipe the board and
    // re-append every cell in the given (re-sorted) order, minting new elements — so positions
    // follow the input and a shuffle MOVES the controls. Exposed solely so the spine spec can
    // prove its stability assertion has teeth (a shuffled re-sort must FAIL). Production reshapes
    // go through `update`, never this.
    __resortRebuild(widgets, engine) {
      el.textContent = "";
      cells.clear();
      for (const w of widgets) {
        const cell = makeCell(w, engine, ++uidSeq);
        el.appendChild(cell);
        cells.set(w.address, { cell, widget: w });
      }
    },
  };
}
