// chat/board.js — the node-identity surface board (spec §3.6, an OBSERVABLE REQUIREMENT of
// #355). The surface is the always-on primary artifact (spec §3.2); this is where its controls
// live, and the load-bearing rule is that layout keys on IDENTITY, not a fresh sort each render:
//
//   - a survivor (a control present before AND after a reshape) HOLDS ITS POSITION — the board
//     evolves, it never reshuffles, so the knob the user just learned stays put;
//   - an added control animates IN (appended, marked entering);
//   - a removed control animates OUT (marked exiting, then detached).
//
// A board that re-sorts every turn re-buries that knob — so `update()` below reuses each
// surviving cell IN PLACE and only ever appends genuinely new controls. The negative of this
// property is the load-bearing test: a shuffled re-sort MUST move controls (proving the board
// isn't doing that) — `__resortRebuild` exposes exactly that anti-pattern for the spec to assert
// against.
//
// IDENTITY GRANULARITY — read before wiring #358's change-card highlight. Spec §3.6 says "keys on
// NODE identity", but this board keys each cell on the CONTROL's address (`widget.address`, the
// pipe port e.g. `/cutoff/in`) — INTENTIONALLY finer than a graph node: the surface is a list of
// controls, one per bound pipe, and a control is what holds/animates position. #354's structural
// diff (`structuralDiff` in js/diff.mjs), which #358 renders as the change-card + surface glow,
// keys on the NODE address (`node.address`, e.g. `/cutoff`). So there is a mapping #358 MUST
// bridge: the diff is node-addressed, the board is control/pipe-addressed — a node-address entry
// in the diff must be resolved to the control(s) belonging to that node (a node may back several
// controls, or none) before it can highlight/pulse a cell here. Edge case to be aware of: two
// controls binding the SAME pipe collapse to ONE cell (the address is the key), so a per-node
// highlight can legitimately map to a single shared cell.
//
// LANE / GROUP DENSITY — owed to #358. This board renders every widget as its own uniform cell
// and DISCARDS `surface.rows` (the step-lane + grouped-channel packing that widget-model.mjs
// `layoutRows` produces). That is a deliberate M1 trade-off — node-identity stability is the
// requirement here, and lane/group RE-LAYOUT is the §3.6 re-layout seam #358 owns — but it means
// a dense surface (groovebox's 16-step lanes + grouped channel cards) reads as many loose cells,
// not the tight lanes the player it replaces shows. Restoring lane/group density by honoring
// `surface.rows` (identity-stably) is #358's to add; the board does not preserve grouping today.
//
// Each control is rendered through the surface's REAL binding (render.mjs `renderSurface`, one
// widget at a time) so the board never re-derives an address or a scaling — the on-screen
// control and the headless check drive the engine through the identical `emit`/`initial` path
// (ADR-0043). Re-layout (explicit "rearrange / clean up" + the agent's proactive suggestion on a
// big diff) is a §3.6 seam left as a no-op here; the behavior lands with the change-card work
// (#358).

import { renderSurface } from "../../../crates/reuben-web/js/surface/render.mjs";
import { h } from "../dom.js";

// The fields that make two resolved widgets for the SAME control "the same" for the board's
// purpose: a differing default/range/label/kind is a changed control (re-render in place,
// value-sweep); everything else about a widget is derived from these. Kept narrow + stable so an
// incidental object-identity change never reads as a reshape.
function sameWidget(a, b) {
  const key = (w) =>
    JSON.stringify([w.kind, w.widget, w.label, w.min, w.max, w.default, w.note, w.degree, w.group]);
  return key(a) === key(b);
}

// Render exactly ONE widget into a cell through the surface's real binding — a single-widget
// mini-surface so `renderSurface` wires this control's events to the engine the same way the
// full surface would. The cell carries its CONTROL identity (`data-control` = the pipe address,
// see the granularity note atop this file) and a creation stamp (`data-cell-uid`, a monotonic
// counter) so a reused cell is provably the SAME element across a reshape (identity held) and a
// rebuilt one provably is not.
function makeCell(widget, engine, uid) {
  const cell = h("div", {
    class: "board-cell",
    dataset: { control: widget.address, cellUid: String(uid), boardState: "resident" },
  });
  renderSurface({ widgets: [widget], rows: [[widget]] }, engine, cell);
  return cell;
}

// The diff → surface bridge (spec §4.1/§4.6, issue #358). The change-card renders the NODE-addressed
// structural diff (js/diff.mjs keys on `node.address`, e.g. "/cutoff"); this board keys each cell on
// the CONTROL/pipe address (`widget.address`, e.g. "/cutoff/in" — the granularity note atop this
// file). So a diff node address must be resolved to the cell(s) it backs before it can highlight one.
// A control's node is its address minus the trailing "/<port>" segment.
function nodeOfControl(controlAddr) {
  const cut = controlAddr.lastIndexOf("/");
  return cut > 0 ? controlAddr.slice(0, cut) : controlAddr;
}

export function createBoard() {
  const el = h("div", { class: "surface-board" });
  const cells = new Map(); // control address -> { cell, widget }
  let uidSeq = 0;
  // TEST-ONLY provenance of the last diff-driven highlight (issue #358): the node addresses whose
  // controls actually animated, per bucket. Lets the change-card spec assert "the reshape swept THIS
  // control" deterministically, without racing the settle/reap animation timers. Not load-bearing.
  let lastHighlight = { added: [], changed: [], removed: [] };

  // Resolve a diff NODE address (e.g. "/cutoff") to the mounted cell(s) that back it (spec §4.1).
  // A node may back several controls (each a shared cell collapses by address) or NONE — a no-knob
  // change (§4.3) resolves to [] and simply has no surface echo.
  function cellsForNode(nodeAddr) {
    const out = [];
    for (const { cell } of cells.values()) {
      if (nodeOfControl(cell.dataset.control) === nodeAddr) out.push(cell);
    }
    return out;
  }

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

  // A one-shot transient state (entering / changed) resets to the neutral "resident" once its
  // animation finishes, so `data-board-state` reflects the cell's steady status instead of
  // STICKING on the last transition. Guarded against a cell that has since started exiting; a
  // fallback timeout (> the longest transient animation, cell-glow 0.6s) covers no-animation.
  function settle(cell) {
    const reset = () => {
      if (cell.dataset.boardState !== "exiting") cell.dataset.boardState = "resident";
    };
    cell.addEventListener("animationend", reset, { once: true });
    setTimeout(reset, 650);
  }

  return {
    el,

    /**
     * Reconcile the board to `widgets`, keyed on control identity (spec §3.6; see the granularity
     * note atop this file). Survivors hold their DOM slot (reused in place; re-rendered only if
     * the control's own values changed), removed controls animate out, genuinely new controls are
     * appended + animate in. Never re-sorts.
     */
    update(widgets, engine) {
      const incoming = new Map(widgets.map((w) => [w.address, w]));

      // Removed: a mounted control absent from the new set animates out, then detaches.
      for (const [key, entry] of cells) {
        if (!incoming.has(key)) {
          entry.cell.dataset.boardState = "exiting";
          reap(entry.cell);
          cells.delete(key);
        }
      }

      // Survivors + added: walk the incoming order. A survivor is reused WHERE IT IS (position
      // held); only a genuinely new control is created and appended (so a re-sorted input can
      // never move an existing control). A survivor whose values changed re-renders in place.
      for (const w of widgets) {
        const existing = cells.get(w.address);
        if (existing) {
          if (!sameWidget(existing.widget, w)) {
            renderSurface({ widgets: [w], rows: [[w]] }, engine, existing.cell);
            existing.widget = w;
            existing.cell.dataset.boardState = "changed";
            settle(existing.cell); // one-shot glow → back to resident
          }
          continue;
        }
        const cell = makeCell(w, engine, ++uidSeq);
        cell.dataset.boardState = "entering";
        el.appendChild(cell);
        cells.set(w.address, { cell, widget: w });
        settle(cell); // one-shot animate-in → back to resident
      }
    },

    // The live board as an ordered list of {control, uid, index} — the test hook reads this to
    // assert position + identity stability. Excludes cells mid-exit so it reflects resident cells.
    nodes() {
      return [...el.children]
        .filter((c) => c.dataset.boardState !== "exiting")
        .map((c, index) => ({ control: c.dataset.control, uid: c.dataset.cellUid, index }));
    },

    // --- §4.1 diff → surface highlight (issue #358) -----------------------------------------
    // The card + the surface are two linked views of one reshape, both driven off the node-identity
    // diff. These are the surface half; chat/change-card.js is the transcript half.

    // Expose the node→cell bridge so the change-card can tell whether a change HAS a control to echo
    // (a no-knob change, §4.3, resolves to []).
    cellsForNode,

    // Echo-highlight the control(s) for a node (spec §4.1): hovering/focusing a card row glows its
    // control on the surface. A PERSISTENT state (`data-echo`) held while the row is hovered and
    // cleared on leave — distinct from the transient landing animations below. No-op for a no-knob
    // node (nothing resolves).
    echoNode(nodeAddr) {
      for (const cell of cellsForNode(nodeAddr)) cell.dataset.echo = "on";
    },
    clearEchoNode(nodeAddr) {
      for (const cell of cellsForNode(nodeAddr)) delete cell.dataset.echo;
    },

    /**
     * Animate the controls a landed reshape touched, keyed on NODE IDENTITY from the structural diff
     * (spec §4.1/§4.6): added → pulse in, changed → value-sweep glow, removed → animate out (and
     * detach — a removed node's control leaves the board, §3.6). This is the diff-driven companion to
     * `update`'s widget-reconcile animation: `update` is the authority on membership, this fires the
     * node-identity glow the card is the record of (they agree in the live flow; the diff path also
     * covers a change that isn't visible in the widget's own fields). Records `lastHighlight` for the
     * test hook. Returns the count of controls actually touched (0 ⇒ the whole diff was no-knob).
     */
    highlightDiff(diff) {
      const touched = { added: [], changed: [], removed: [] };
      for (const addr of diff?.changed ?? []) {
        for (const cell of cellsForNode(addr)) {
          if (cell.dataset.boardState === "exiting") continue;
          cell.dataset.boardState = "changed";
          settle(cell);
          touched.changed.push(addr);
        }
      }
      for (const addr of diff?.added ?? []) {
        for (const cell of cellsForNode(addr)) {
          cell.dataset.boardState = "entering";
          settle(cell);
          touched.added.push(addr);
        }
      }
      for (const addr of diff?.removed ?? []) {
        for (const cell of cellsForNode(addr)) {
          cell.dataset.boardState = "exiting";
          reap(cell);
          cells.delete(cell.dataset.control);
          touched.removed.push(addr);
        }
      }
      lastHighlight = touched;
      return touched.added.length + touched.changed.length + touched.removed.length;
    },

    // TEST-ONLY readout of the last highlightDiff's touched node addresses (see `lastHighlight`).
    lastHighlight: () => lastHighlight,

    // §3.6 seam — explicit user-directed re-layout ("rearrange this / clean up the layout"). The
    // BEHAVIOR (re-sorting into a tidier arrangement, and restoring lane/group density — see the
    // density note atop this file) is #358's; this is the named no-op hook so that ticket has
    // somewhere to land it without reopening the spine.
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
