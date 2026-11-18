//! The canvas: the open document drawn as boxes and wires, read-only.
//!
//! Everything drawn comes from [`read_instrument`]'s structured answer, read once when the canvas
//! opens. A box the document stores a position for is drawn there; every other box is placed by
//! [`place`](place::place)'s rule.
//!
//! Drag pans. Scrolling pans too, and zooms about the pointer when it is a pinch or has the zoom
//! modifier held. A click selects the box under it, or nothing; a double-click on empty canvas
//! brings the whole document back into view.

mod camera;
mod geometry;
mod graph;
mod place;
mod view;

use std::path::Path;

use eframe::egui::{self, Pos2, Rect, Sense};
use reuben_api::authoring::{read_instrument, ReadInstrument};
use reuben_api::FsResolver;

use self::camera::Camera;
use self::graph::{Graph, Item};

/// A document's graph and where each of its boxes is, in canvas units.
#[derive(Debug)]
struct Document {
    graph: Graph,
    /// Indexed as `graph.boxes`.
    rects: Vec<Rect>,
}

impl Document {
    fn of(graph: Graph) -> Self {
        let rects = place::place(&graph);
        Self { graph, rects }
    }

    /// The view that shows the whole document in `area`.
    fn fitted(&self, area: Rect) -> Camera {
        match geometry::bounds(self.rects.iter().copied()) {
            Some(bounds) => Camera::fit(bounds, area.size()),
            None => Camera {
                offset: egui::Vec2::ZERO,
                zoom: 1.0,
            },
        }
    }
}

/// Read the instrument file at `instrument`. `root` is the library root its nested references
/// fall back to, as for the engine.
///
/// `Err` is a document that cannot be read or parsed. One that reads but does not load is `Ok`,
/// with the load report's diagnostics on the boxes they name.
fn load(instrument: &Path, root: Option<&Path>) -> Result<Document, String> {
    let source = instrument.display().to_string();
    // Only structure is drawn, so samples are checked for presence rather than decoded.
    let mut store = FsResolver::for_document(&source).stat_only();
    if let Some(root) = root {
        store = store.with_root(root);
    }
    let structure = read_instrument(&ReadInstrument { source }, &store)
        .map_err(|refusal| refusal.message)?
        .output;
    Ok(Document::of(Graph::from_structure(&structure)))
}

/// The canvas of the open instrument, with how it is being looked at and what is selected.
pub struct Canvas {
    loaded: Option<Result<Document, String>>,
    /// `None` until the first frame, when the size of the area to fit the document into is known.
    camera: Option<Camera>,
    selection: Option<Item>,
}

impl Canvas {
    /// The canvas for `instrument`, or an empty one when no instrument is open.
    pub fn open(instrument: Option<&Path>, root: Option<&Path>) -> Self {
        Self {
            loaded: instrument.map(|path| load(path, root)),
            camera: None,
            selection: None,
        }
    }

    /// Select the box at `point`, in canvas units, or nothing when no box is there.
    fn click(&mut self, point: Pos2) {
        self.selection = match &self.loaded {
            Some(Ok(doc)) => {
                geometry::hit(&doc.rects, point).map(|i| doc.graph.boxes[i].item.clone())
            }
            _ => None,
        };
    }

    /// Draw the canvas over the rest of `ui`. Draws nothing when no instrument is open.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let doc = match &self.loaded {
            None => {
                ui.take_available_space();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("No canvas: the instrument could not be read. {e}"),
                );
                ui.take_available_space();
                return;
            }
            Some(Ok(doc)) => doc,
        };
        for problem in &doc.graph.problems {
            let color = if problem.error {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().warn_fg_color
            };
            ui.colored_label(color, &problem.message);
        }

        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let area = response.rect;
        let camera = self.camera.get_or_insert_with(|| doc.fitted(area));
        let pointer = response.hover_pos();
        let over = pointer.and_then(|p| geometry::hit(&doc.rects, camera.to_canvas(area.min, p)));

        if response.dragged() {
            camera.pan(response.drag_delta());
        }
        if let Some(pointer) = pointer {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta(), i.zoom_delta()));
            camera.pan(scroll);
            if zoom != 1.0 {
                camera.zoom_about(pointer - area.min, zoom);
            }
        }
        if response.double_clicked() && over.is_none() {
            *camera = doc.fitted(area);
        }
        let camera = *camera;

        let clicked = response
            .clicked()
            .then(|| response.interact_pointer_pos())
            .flatten()
            .map(|p| camera.to_canvas(area.min, p));
        let said: Vec<&str> = over
            .map(|i| &doc.graph.boxes[i].problems)
            .into_iter()
            .flatten()
            .map(|p| p.message.as_str())
            .collect();
        if !said.is_empty() {
            response.on_hover_text_at_pointer(said.join("\n"));
        }

        let selected = (self.selection.as_ref()).and_then(|item| doc.graph.index_of(item));
        view::Scene {
            graph: &doc.graph,
            rects: &doc.rects,
            camera,
            origin: area.min,
            selected,
        }
        .paint(&painter, ui.visuals());

        if let Some(point) = clicked {
            self.click(point);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use reuben_api::authoring::{
        add_instrument_node, new_instrument, set_instrument_layout, AddInstrumentNode,
        LayoutPosition, LayoutTarget, NewInstrument, SetInstrumentLayout,
    };

    use super::place::{overlaps, place};
    use super::*;
    use crate::config::test_support::TempConfig;

    /// The three documents the canvas has to stay legible on.
    const LIBRARY: [&str; 3] = ["acid-techno", "groovebox", "breath-harp"];

    fn library(name: &str) -> Document {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../instruments")
            .join(format!("{name}.json"));
        load(&path, None).unwrap_or_else(|e| panic!("{name} reads: {e}"))
    }

    /// `doc`'s graph with every stored position dropped, so the test is of the placement rule
    /// whether or not the library document has since been given a layout.
    fn unplaced(doc: &Document) -> Graph {
        let mut graph = doc.graph.clone();
        for b in &mut graph.boxes {
            b.stored = None;
        }
        graph
    }

    #[test]
    fn the_library_instruments_read_whole_and_clean() {
        for name in LIBRARY {
            let doc = library(name);
            let graph = &doc.graph;
            assert!(graph.problems.is_empty(), "{name}: {:?}", graph.problems);
            for b in &graph.boxes {
                assert!(b.problems.is_empty(), "{name} {:?}", b.problems);
            }
            let nodes = graph.boxes.iter().filter(|b| !b.is_boundary()).count();
            assert!(nodes >= 20, "{name}: {nodes} nodes");
            assert!(graph.wires.len() >= nodes, "{name}");
            assert_eq!(doc.rects.len(), graph.boxes.len());
        }
    }

    #[test]
    fn no_two_placed_boxes_of_a_library_instrument_overlap() {
        for name in LIBRARY {
            let rects = place(&unplaced(&library(name)));
            assert!(rects.iter().all(|r| r.is_finite()), "{name}");
            assert_eq!(overlaps(&rects), [], "{name}");
        }
    }

    #[test]
    fn a_library_instrument_is_placed_the_same_on_every_read() {
        for name in LIBRARY {
            let (first, second) = (library(name), library(name));
            assert_eq!(first.graph, second.graph, "{name}");
            assert_eq!(
                place(&unplaced(&first)),
                place(&unplaced(&second)),
                "{name}"
            );
        }
    }

    #[test]
    fn every_wire_of_a_library_instrument_runs_left_to_right() {
        for name in LIBRARY {
            let graph = unplaced(&library(name));
            let rects = place(&graph);
            for wire in &graph.wires {
                let (from, to) = (rects[wire.from.node], rects[wire.to.node]);
                assert!(
                    from.right() < to.left(),
                    "{name}: {:?} → {:?}",
                    graph.boxes[wire.from.node].item,
                    graph.boxes[wire.to.node].item
                );
            }
        }
    }

    #[test]
    fn a_position_stored_in_the_document_is_where_the_box_is_drawn() {
        let temp = TempConfig::new();
        std::fs::create_dir_all(&temp.path).unwrap();
        let path = temp.path.join("seed.json");
        let source = path.display().to_string();
        let store = FsResolver::for_document(&source);
        new_instrument(
            &NewInstrument {
                source: source.clone(),
                name: "seed".into(),
            },
            &store,
        )
        .unwrap();
        let address = "/osc".to_string();
        add_instrument_node(
            &AddInstrumentNode {
                source: source.clone(),
                address: address.clone(),
                type_name: "oscillator".into(),
                ..Default::default()
            },
            &store,
        )
        .unwrap();
        let before = load(&path, None).unwrap();
        let unmoved = before.graph.index_of(&Item::Node(address.clone())).unwrap();
        assert_eq!(before.graph.boxes[unmoved].stored, None);
        set_instrument_layout(
            &SetInstrumentLayout {
                source,
                positions: vec![LayoutPosition {
                    target: LayoutTarget::Node(address.clone()),
                    x: 640.0,
                    y: -120.0,
                }],
                expect: None,
            },
            &store,
        )
        .unwrap();

        let after = load(&path, None).unwrap();
        let moved = after.graph.index_of(&Item::Node(address)).unwrap();
        assert_eq!(after.rects[moved].min, Pos2::new(640.0, -120.0));
        assert_eq!(overlaps(&after.rects), []);
    }

    #[test]
    fn a_file_that_is_not_a_document_is_reported_not_drawn() {
        let missing = PathBuf::from("does-not-exist.json");
        assert!(load(&missing, None).is_err());
    }

    #[test]
    fn a_click_selects_the_box_under_it_and_a_click_on_nothing_clears() {
        let mut canvas = Canvas {
            loaded: Some(Ok(library("breath-harp"))),
            camera: None,
            selection: None,
        };
        let (item, inside, outside) = {
            let Some(Ok(doc)) = &canvas.loaded else {
                unreachable!()
            };
            let all = geometry::bounds(doc.rects.iter().copied()).unwrap();
            (
                doc.graph.boxes[3].item.clone(),
                doc.rects[3].center(),
                all.max + egui::Vec2::splat(50.0),
            )
        };
        canvas.click(inside);
        assert_eq!(canvas.selection, Some(item));
        canvas.click(outside);
        assert_eq!(canvas.selection, None);
    }

    #[test]
    fn no_instrument_has_nothing_to_select() {
        let mut canvas = Canvas::open(None, None);
        canvas.click(Pos2::ZERO);
        assert_eq!(canvas.selection, None);
    }
}
