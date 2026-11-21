//! The cycles in a built graph, found from whichever wires resolved — so a load that fails for
//! another reason still reports them, and an edit cannot add one unseen behind that failure.
//!
//! Every wire the loader connects is an ordering constraint at Instantiate, so a cycle here is
//! exactly one Instantiate would refuse.

use std::collections::BTreeSet;

use reuben_core::graph::{Graph, NodeKey};
use slotmap::SecondaryMap;

/// Each strongly connected component of `graph` that holds a cycle — two or more nodes, or one
/// wired to itself — as the sorted document addresses of its nodes, the list sorted too.
///
/// A spliced `subpatch` child's node is named by the document node that holds it: the address rule
/// keeps a `/` out of every authored address, so the first segment of a spliced address is its
/// holder's.
pub(super) fn cycles(graph: &Graph) -> Vec<Vec<String>> {
    let mut out: SecondaryMap<NodeKey, Vec<NodeKey>> =
        graph.nodes.keys().map(|k| (k, Vec::new())).collect();
    let mut back: SecondaryMap<NodeKey, Vec<NodeKey>> =
        graph.nodes.keys().map(|k| (k, Vec::new())).collect();
    for c in &graph.connections {
        if !(out.contains_key(c.src) && out.contains_key(c.dst)) {
            continue;
        }
        out[c.src].push(c.dst);
        back[c.dst].push(c.src);
    }

    // Kosaraju, both passes iterative so a long generated chain cannot overflow the stack.
    let mut finished: Vec<NodeKey> = Vec::with_capacity(graph.nodes.len());
    let mut seen: SecondaryMap<NodeKey, ()> = SecondaryMap::new();
    for root in graph.nodes.keys() {
        if seen.insert(root, ()).is_some() {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        while let Some((node, next)) = stack.pop() {
            match out[node].get(next) {
                Some(&succ) => {
                    stack.push((node, next + 1));
                    if seen.insert(succ, ()).is_none() {
                        stack.push((succ, 0));
                    }
                }
                None => finished.push(node),
            }
        }
    }

    let mut component: SecondaryMap<NodeKey, ()> = SecondaryMap::new();
    let mut found: BTreeSet<Vec<String>> = BTreeSet::new();
    for &root in finished.iter().rev() {
        if component.insert(root, ()).is_some() {
            continue;
        }
        let mut members = vec![root];
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            for &pred in &back[node] {
                if component.insert(pred, ()).is_none() {
                    members.push(pred);
                    stack.push(pred);
                }
            }
        }
        let holders: BTreeSet<String> = members
            .iter()
            .map(|k| holder(&graph.nodes[*k].address))
            .collect();
        let self_wired = members.len() == 1 && out[root].contains(&root);
        if members.len() > 1 || self_wired {
            found.insert(holders.into_iter().collect());
        }
    }
    found.into_iter().collect()
}

/// The document node an address in a built graph belongs to.
fn holder(address: &str) -> String {
    match address.strip_prefix('/').and_then(|rest| rest.find('/')) {
        Some(end) => address[..end + 1].to_string(),
        None => address.to_string(),
    }
}
