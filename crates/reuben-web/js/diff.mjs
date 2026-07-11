// diff.mjs — the structural node-identity diff (issue #353, spec §4.6) the change-card renders.
//
// Web `swap` is restart-swap (ADR-0052 §2): `survived: 0` always, so the native survivor /
// `state_reset` stats are degenerate per turn and NOT what the card shows. Instead the card is
// driven by comparing the before/after documents structurally, keyed on `node.address`:
//
//   - added   = addresses present in `after` but not `before`   (new rows, pulse-in)
//   - removed = addresses present in `before` but not `after`   (removed rows, animate-out)
//   - changed = addresses present in BOTH whose node content differs (value-sweep glow)
//
// Survivors (present in both, byte-equal) hold position and appear in none of the three
// (spec §3.6 / §4.6). This is a PURE function over two parsed documents — no wasm, no engine,
// no validation (a dangling wire after a node is removed is validate's concern, not the diff's).

/**
 * Canonical stringification with recursively key-sorted objects, so that two nodes with the
 * same content but different key order compare equal (key order must never manufacture a
 * `changed`). Arrays keep their order (a reordered inputs array IS a different node); only
 * object keys are sorted. Primitives serialize as JSON.
 *
 * @param {*} value
 * @returns {string}
 */
function stableStringify(value) {
  if (Array.isArray(value)) {
    return `[${value.map(stableStringify).join(",")}]`;
  }
  if (value !== null && typeof value === "object") {
    const keys = Object.keys(value).sort();
    return `{${keys.map((k) => `${JSON.stringify(k)}:${stableStringify(value[k])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

/**
 * Index a document's nodes by address. Defensive: a missing/non-array `.nodes` is treated as
 * no nodes, and a node without a string `address` is ignored (it has no identity to diff on).
 * A later node re-using an address overwrites an earlier one — the loader rejects duplicate
 * addresses upstream, so the diff need not also police it.
 *
 * @param {{nodes?: Array<object>}|null|undefined} doc
 * @returns {Map<string, object>} address -> node object
 */
function indexByAddress(doc) {
  const byAddr = new Map();
  const nodes = doc && Array.isArray(doc.nodes) ? doc.nodes : [];
  for (const node of nodes) {
    if (node && typeof node.address === "string") byAddr.set(node.address, node);
  }
  return byAddr;
}

/**
 * Compute the structural node-identity diff between two parsed documents (spec §4.6).
 *
 * @param {{nodes?: Array<object>}|null|undefined} before - the currently-installed document
 * @param {{nodes?: Array<object>}|null|undefined} after - the proposed/new document
 * @returns {{added: string[], removed: string[], changed: string[]}} each a sorted array of
 *   node addresses; an address appears in at most one bucket (a survivor appears in none)
 */
export function structuralDiff(before, after) {
  const beforeNodes = indexByAddress(before);
  const afterNodes = indexByAddress(after);

  const added = [];
  const removed = [];
  const changed = [];

  for (const [addr, node] of afterNodes) {
    if (!beforeNodes.has(addr)) {
      added.push(addr);
    } else if (stableStringify(node) !== stableStringify(beforeNodes.get(addr))) {
      changed.push(addr);
    }
    // else: byte-equal survivor — holds position, no bucket.
  }
  for (const addr of beforeNodes.keys()) {
    if (!afterNodes.has(addr)) removed.push(addr);
  }

  added.sort();
  removed.sort();
  changed.sort();
  return { added, removed, changed };
}
