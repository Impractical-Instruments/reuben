//! The open document as the canvas draws it: one box per node and per interface pipe, the ports
//! on each, and the wires between them. Built from the structured read of the document; nothing
//! here draws.

use eframe::egui::Pos2;
use reuben_api::authoring::{
    InputValue, InstrumentStructure, PortInfo, Position, StructureNode, WireSource,
};

/// What a box stands for, and what a selection names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Item {
    /// A node, by address.
    Node(String),
    /// An interface input pipe, by name.
    Input(String),
    /// An interface output pipe, by name.
    Output(String),
}

/// A port's type, which decides its colour and its wires'.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortKind {
    Signal,
    Value,
    Int,
    Enum,
    Message,
    Harmony,
    Vocab,
    Text,
    Arg,
    /// A kind this canvas has no colour for, or a port the document names and the node's resolved
    /// ports do not have.
    Unknown,
}

impl PortKind {
    fn of(port: &PortInfo) -> Self {
        match port.kind.as_str() {
            "signal" => Self::Signal,
            "value" => Self::Value,
            "int" => Self::Int,
            "enum" => Self::Enum,
            "message" => Self::Message,
            "harmony" => Self::Harmony,
            "vocab" => Self::Vocab,
            "string" => Self::Text,
            "arg" => Self::Arg,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub name: String,
    pub kind: PortKind,
}

/// Something the load report says about one box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// An error stops the document loading; a warning does not.
    pub error: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeBox {
    pub item: Item,
    /// The node's address, or the pipe's name.
    pub title: String,
    /// The node's type name. Empty for an interface pipe.
    pub subtitle: String,
    /// A node's wirable inputs, in the order its type declares them. An interface output pipe has
    /// exactly one, unnamed.
    pub inputs: Vec<Port>,
    /// A node's outputs. An interface input pipe has exactly one, unnamed.
    pub outputs: Vec<Port>,
    /// The position the document stores for this box's top-left corner, when it stores one.
    pub stored: Option<Pos2>,
    pub problems: Vec<Problem>,
}

impl NodeBox {
    /// An interface pipe rather than a node.
    pub fn is_boundary(&self) -> bool {
        !matches!(self.item, Item::Node(_))
    }
}

/// One end of a wire: a box and the index of a port in its `outputs` (a wire's `from`) or its
/// `inputs` (a wire's `to`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortRef {
    pub node: usize,
    pub port: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wire {
    pub from: PortRef,
    pub to: PortRef,
    /// The source port's kind, or the target's when the source's is unknown.
    pub kind: PortKind,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Graph {
    /// Interface inputs, then nodes, then interface outputs, each in document order.
    pub boxes: Vec<NodeBox>,
    pub wires: Vec<Wire>,
    /// What the load report says that names no box of this document.
    pub problems: Vec<Problem>,
}

impl Graph {
    pub fn from_structure(doc: &InstrumentStructure) -> Self {
        let mut boxes = Vec::with_capacity(doc.inputs.len() + doc.nodes.len() + doc.outputs.len());
        for pipe in &doc.inputs {
            boxes.push(NodeBox {
                item: Item::Input(pipe.port.name.clone()),
                title: pipe.port.name.clone(),
                subtitle: String::new(),
                inputs: Vec::new(),
                outputs: vec![Port {
                    name: String::new(),
                    kind: PortKind::of(&pipe.port),
                }],
                stored: pipe.layout.map(pos),
                problems: Vec::new(),
            });
        }
        let first_node = boxes.len();
        boxes.extend(doc.nodes.iter().map(node_box));
        let first_output = boxes.len();
        for pipe in &doc.outputs {
            boxes.push(NodeBox {
                item: Item::Output(pipe.name.clone()),
                title: pipe.name.clone(),
                subtitle: String::new(),
                inputs: vec![Port {
                    name: String::new(),
                    kind: PortKind::Unknown,
                }],
                outputs: Vec::new(),
                stored: pipe.layout.map(pos),
                problems: Vec::new(),
            });
        }

        let mut graph = Graph {
            boxes,
            wires: Vec::new(),
            problems: Vec::new(),
        };
        for (n, node) in doc.nodes.iter().enumerate() {
            for input in &node.inputs {
                let InputValue::Wire { from } = &input.value else {
                    continue;
                };
                let Some(from) = graph.source(from) else {
                    continue;
                };
                let to = PortRef {
                    node: first_node + n,
                    port: named(&mut graph.boxes[first_node + n].inputs, &input.name),
                };
                graph.wire(from, to);
            }
        }
        for (n, pipe) in doc.outputs.iter().enumerate() {
            let Some(from) = graph.source(&pipe.from) else {
                continue;
            };
            let to = PortRef {
                node: first_output + n,
                port: 0,
            };
            graph.wire(from, to);
            let kind = graph.boxes[from.node].outputs[from.port].kind;
            graph.boxes[to.node].inputs[0].kind = kind;
        }

        let diags = (doc.report.errors.iter().map(|d| (true, d)))
            .chain(doc.report.warnings.iter().map(|d| (false, d)));
        for (error, diag) in diags {
            let problem = Problem {
                error,
                message: diag.message.clone(),
            };
            // A diagnostic with a port and no node names an interface pipe, and does not say
            // whether an input or an output: where one name is both, it goes on both.
            let at: Vec<usize> = match (&diag.node, &diag.port) {
                (Some(address), _) => (graph.index_of(&Item::Node(address.clone())))
                    .into_iter()
                    .collect(),
                (None, Some(name)) => [Item::Input(name.clone()), Item::Output(name.clone())]
                    .iter()
                    .filter_map(|item| graph.index_of(item))
                    .collect(),
                (None, None) => Vec::new(),
            };
            if at.is_empty() {
                graph.problems.push(problem);
            } else {
                for i in at {
                    graph.boxes[i].problems.push(problem.clone());
                }
            }
        }
        graph
    }

    pub fn index_of(&self, item: &Item) -> Option<usize> {
        self.boxes.iter().position(|b| &b.item == item)
    }

    /// The output port a wire-ref names. `None` when it names a node or pipe this document does
    /// not have: such a wire has no end to draw from, and the load report carries the error.
    fn source(&mut self, from: &WireSource) -> Option<PortRef> {
        match from {
            WireSource::InterfaceInput { name } => Some(PortRef {
                node: self.index_of(&Item::Input(name.clone()))?,
                port: 0,
            }),
            WireSource::NodeOutput { node, port } => {
                let node = self.index_of(&Item::Node(node.clone()))?;
                Some(PortRef {
                    node,
                    port: named(&mut self.boxes[node].outputs, port),
                })
            }
        }
    }

    fn wire(&mut self, from: PortRef, to: PortRef) {
        let kind = match self.boxes[from.node].outputs[from.port].kind {
            PortKind::Unknown => self.boxes[to.node].inputs[to.port].kind,
            known => known,
        };
        self.wires.push(Wire { from, to, kind });
    }
}

fn pos(p: Position) -> Pos2 {
    Pos2::new(p.x, p.y)
}

fn node_box(node: &StructureNode) -> NodeBox {
    let ports = |infos: &[PortInfo]| {
        infos
            .iter()
            // A constant is set, never wired: it belongs to the inspector, not the box.
            .filter(|p| !p.constant)
            .map(|p| Port {
                name: p.name.clone(),
                kind: PortKind::of(p),
            })
            .collect()
    };
    NodeBox {
        item: Item::Node(node.address.clone()),
        title: node.address.clone(),
        subtitle: node.type_name.clone(),
        inputs: ports(&node.ports.inputs),
        outputs: ports(&node.ports.outputs),
        stored: node.layout.map(pos),
        problems: Vec::new(),
    }
}

/// The index of the port called `name`, added as an [`PortKind::Unknown`] port when the box has
/// none: a wire the document holds is drawn even where the node's resolved ports do not have its
/// end (an unknown type, a `subpatch` whose child did not load, a mistyped port).
fn named(ports: &mut Vec<Port>, name: &str) -> usize {
    if let Some(i) = ports.iter().position(|p| p.name == name) {
        return i;
    }
    ports.push(Port {
        name: name.to_string(),
        kind: PortKind::Unknown,
    });
    ports.len() - 1
}

#[cfg(test)]
pub mod tests {
    use serde_json::json;

    use super::*;

    /// A structure answer, written as the JSON `read_instrument` serializes to.
    pub fn structure(value: serde_json::Value) -> InstrumentStructure {
        serde_json::from_value(value).expect("a structure answer")
    }

    fn port(name: &str, kind: &str) -> serde_json::Value {
        json!({ "name": name, "kind": kind })
    }

    /// `/osc` feeding `/filter`, which feeds the `main` output; `cutoff` feeds the filter.
    pub fn small() -> InstrumentStructure {
        structure(json!({
            "instrument": "small",
            "hash": "h",
            "nodes": [
                {
                    "address": "/osc",
                    "type_name": "sine",
                    "ports": {
                        "inputs": [port("freq", "value")],
                        "outputs": [port("out", "signal")],
                    },
                },
                {
                    "address": "/filter",
                    "type_name": "lowpass",
                    "inputs": [
                        { "name": "in", "value": { "kind": "wire", "from":
                            { "kind": "node_output", "node": "/osc", "port": "out" } } },
                        { "name": "cutoff", "value": { "kind": "wire", "from":
                            { "kind": "interface_input", "name": "cutoff" } } },
                        { "name": "q", "value": { "kind": "literal", "value": 0.5 } },
                    ],
                    "ports": {
                        "inputs": [
                            port("in", "signal"),
                            port("cutoff", "value"),
                            port("q", "value"),
                            { "name": "mode", "kind": "enum", "constant": true },
                        ],
                        "outputs": [port("out", "signal")],
                    },
                },
            ],
            "inputs": [{ "port": port("cutoff", "value") }],
            "outputs": [{ "name": "main", "from":
                { "kind": "node_output", "node": "/filter", "port": "out" } }],
            "resources": [],
            "report": { "ok": true, "errors": [], "warnings": [] },
        }))
    }

    fn find<'a>(graph: &'a Graph, item: &Item) -> &'a NodeBox {
        &graph.boxes[graph.index_of(item).expect("the box exists")]
    }

    #[test]
    fn every_node_and_interface_pipe_is_a_box() {
        let graph = Graph::from_structure(&small());
        let items: Vec<&Item> = graph.boxes.iter().map(|b| &b.item).collect();
        assert_eq!(
            items,
            [
                &Item::Input("cutoff".into()),
                &Item::Node("/osc".into()),
                &Item::Node("/filter".into()),
                &Item::Output("main".into()),
            ]
        );
        let filter = find(&graph, &Item::Node("/filter".into()));
        assert_eq!(filter.title, "/filter");
        assert_eq!(filter.subtitle, "lowpass");
        assert!(!filter.is_boundary());
        assert!(find(&graph, &Item::Input("cutoff".into())).is_boundary());
    }

    #[test]
    fn a_box_shows_its_wirable_ports_and_not_its_constants() {
        let graph = Graph::from_structure(&small());
        let filter = find(&graph, &Item::Node("/filter".into()));
        let names: Vec<&str> = filter.inputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["in", "cutoff", "q"]);
        assert_eq!(filter.inputs[0].kind, PortKind::Signal);
        assert_eq!(filter.inputs[1].kind, PortKind::Value);
    }

    #[test]
    fn wires_join_the_ports_the_document_names() {
        let graph = Graph::from_structure(&small());
        let (cutoff, osc, filter, main) = (0, 1, 2, 3);
        let ends: Vec<(PortRef, PortRef, PortKind)> =
            graph.wires.iter().map(|w| (w.from, w.to, w.kind)).collect();
        let at = |node, port| PortRef { node, port };
        assert_eq!(
            ends,
            [
                (at(osc, 0), at(filter, 0), PortKind::Signal),
                (at(cutoff, 0), at(filter, 1), PortKind::Value),
                (at(filter, 0), at(main, 0), PortKind::Signal),
            ]
        );
    }

    #[test]
    fn an_interface_output_takes_the_type_of_the_port_feeding_it() {
        let graph = Graph::from_structure(&small());
        let main = find(&graph, &Item::Output("main".into()));
        assert_eq!(main.inputs[0].kind, PortKind::Signal);
    }

    #[test]
    fn a_stored_position_is_carried_and_an_absent_one_is_not() {
        let mut doc = small();
        doc.nodes[0].layout = Some(Position { x: 40.0, y: -8.0 });
        doc.inputs[0].layout = Some(Position { x: 1.0, y: 2.0 });
        let graph = Graph::from_structure(&doc);
        assert_eq!(
            find(&graph, &Item::Node("/osc".into())).stored,
            Some(Pos2::new(40.0, -8.0))
        );
        assert_eq!(
            find(&graph, &Item::Input("cutoff".into())).stored,
            Some(Pos2::new(1.0, 2.0))
        );
        assert_eq!(find(&graph, &Item::Node("/filter".into())).stored, None);
    }

    #[test]
    fn a_wire_to_a_port_the_node_does_not_resolve_is_still_drawn() {
        let mut doc = small();
        // What a node of a type the registry lacks reads as.
        doc.nodes[0].ports.outputs.clear();
        let graph = Graph::from_structure(&doc);
        let osc = find(&graph, &Item::Node("/osc".into()));
        assert_eq!(
            osc.outputs,
            [Port {
                name: "out".into(),
                kind: PortKind::Unknown
            }]
        );
        assert_eq!(graph.wires.len(), 3);
        // The wire falls back on the kind of the end that is known.
        assert_eq!(graph.wires[0].kind, PortKind::Signal);
    }

    #[test]
    fn a_wire_from_a_node_the_document_lacks_is_not_drawn() {
        let mut doc = small();
        doc.outputs[0].from = WireSource::NodeOutput {
            node: "/gone".into(),
            port: "out".into(),
        };
        let graph = Graph::from_structure(&doc);
        assert_eq!(graph.wires.len(), 2);
    }

    #[test]
    fn the_load_report_lands_on_the_boxes_it_names() {
        let mut doc = small();
        doc.report = serde_json::from_value(json!({
            "ok": false,
            "errors": [
                { "node": "/filter", "port": "in", "message": "refused" },
                { "port": "main", "message": "bad feed" },
                { "message": "nowhere" },
            ],
            "warnings": [{ "node": "/osc", "message": "careful" }],
        }))
        .unwrap();
        let graph = Graph::from_structure(&doc);
        let problems = |item| find(&graph, &item).problems.clone();
        let problem = |error, message: &str| Problem {
            error,
            message: message.into(),
        };
        assert_eq!(
            problems(Item::Node("/filter".into())),
            [problem(true, "refused")]
        );
        assert_eq!(
            problems(Item::Output("main".into())),
            [problem(true, "bad feed")]
        );
        assert_eq!(
            problems(Item::Node("/osc".into())),
            [problem(false, "careful")]
        );
        assert_eq!(graph.problems, [problem(true, "nowhere")]);
    }

    #[test]
    fn a_report_about_a_name_that_is_an_input_and_an_output_lands_on_both() {
        let mut doc = small();
        doc.outputs[0].name = "cutoff".into();
        doc.report = serde_json::from_value(json!({
            "ok": true,
            "errors": [],
            "warnings": [{ "port": "cutoff", "message": "unbound" }],
        }))
        .unwrap();
        let graph = Graph::from_structure(&doc);
        let unbound = [Problem {
            error: false,
            message: "unbound".into(),
        }];
        assert_eq!(
            find(&graph, &Item::Input("cutoff".into())).problems,
            unbound
        );
        assert_eq!(
            find(&graph, &Item::Output("cutoff".into())).problems,
            unbound
        );
        assert!(graph.problems.is_empty());
    }

    #[test]
    fn a_report_about_an_interface_input_lands_on_it() {
        let mut doc = small();
        doc.report = serde_json::from_value(json!({
            "ok": true,
            "errors": [],
            "warnings": [{ "port": "cutoff", "message": "unbound" }],
        }))
        .unwrap();
        let graph = Graph::from_structure(&doc);
        assert_eq!(
            find(&graph, &Item::Input("cutoff".into())).problems.len(),
            1
        );
    }
}
