//! The canvas: the open document drawn as boxes and wires, and the gestures that edit it.
//!
//! Everything drawn comes from [`read_instrument`]'s structured answer, read when the canvas opens
//! and again after every change on disk. A box the document stores a position for is drawn there;
//! every other box is placed by [`place`](place::place)'s rule.
//!
//! Dragging empty canvas pans. Scrolling pans too, and zooms about the pointer when it is a pinch
//! or has the zoom modifier held. A click selects the box or wire under it, or nothing; with shift
//! held it adds a box to the selection or takes it out. A double-click on empty canvas brings the
//! whole document back into view.
//!
//! Each finished gesture is one [`Gesture`], which the caller makes through [`Gesture::apply`];
//! nothing here writes. Dragging a box moves the selection with it. Dragging from an output port
//! draws a wire, and only the inputs whose type takes it are marked; dropping on one wires it.
//! Delete removes the selected node or unwires the selected wire, F2 renames the selected node,
//! and right-click offers the same, or the operator palette on empty canvas.

mod camera;
mod geometry;
mod gesture;
mod graph;
mod palette;
mod place;
mod view;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Pos2, Rect, Sense, Vec2};
use reuben_api::authoring::{
    read_instrument, wire_compatibility, EditResult, LayoutPosition, ReadInstrument, Refusal,
    WireCompatibility, WireTarget,
};
use reuben_api::FsResolver;

use self::camera::Camera;
pub use self::gesture::Gesture;
use self::graph::{Graph, Item, PortRef};
use self::palette::Palette;

/// How near a port's dot a press or drop must be to take it, in canvas units: half a port row,
/// so a press never takes the port of the row next to it.
const PORT_REACH: f32 = geometry::LINE / 2.0;

/// How near a wire a click must be to select it, in points on screen.
const WIRE_REACH: f32 = 6.0;

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

/// The store the document at `instrument` is read through. `root` is the library root its nested
/// references fall back to, as for the engine. Only structure is drawn, so samples are checked for
/// presence rather than decoded.
fn store(instrument: &Path, root: Option<&Path>) -> FsResolver {
    let store = FsResolver::for_document(&instrument.display().to_string()).stat_only();
    match root {
        Some(root) => store.with_root(root),
        None => store,
    }
}

/// Read the instrument file at `instrument`.
///
/// `Err` is a document that cannot be read or parsed. One that reads but does not load is `Ok`,
/// with the load report's diagnostics on the boxes they name.
fn load(instrument: &Path, root: Option<&Path>) -> Result<Document, String> {
    let source = instrument.display().to_string();
    let structure = read_instrument(&ReadInstrument { source }, &store(instrument, root))
        .map_err(|refusal| refusal.message)?
        .output;
    Ok(Document::of(Graph::from_structure(&structure)))
}

/// A drag under way.
#[derive(Debug, Clone, PartialEq)]
enum Drag {
    Pan,
    /// The boxes `boxes`, moved `by` canvas units so far.
    Move {
        boxes: Vec<usize>,
        by: Vec2,
    },
    /// A wire from the output `from`, its loose end at `to`.
    Wire {
        from: PortRef,
        to: Pos2,
        /// Per input the wire could land on, `None` when its type takes the wire and why not when
        /// it does not. An input not listed is not a target at all.
        verdicts: BTreeMap<PortRef, Option<String>>,
    },
}

/// What is under a point.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Box(Item),
    /// A wire, by its index in the graph.
    Wire(usize),
    Nothing,
}

#[derive(Debug, Clone, PartialEq)]
struct Renaming {
    from: String,
    text: String,
}

/// The canvas of the open instrument, with how it is being looked at and what is selected.
pub struct Canvas {
    /// The instrument file and the library root, while one is open.
    instrument: Option<(PathBuf, Option<PathBuf>)>,
    loaded: Option<Result<Document, String>>,
    /// `None` until the first frame, when the size of the area to fit the document into is known.
    camera: Option<Camera>,
    selection: Option<Item>,
    /// Boxes selected besides `selection`, with shift held; they move with it.
    also: Vec<Item>,
    /// The selected wire, by its index in the graph; a wire and boxes are never selected together.
    wire: Option<usize>,
    drag: Option<Drag>,
    /// What the open context menu was opened on.
    menu: Option<Target>,
    renaming: Option<Renaming>,
    palette: Palette,
    /// Why the last gesture did not change the document, until the next one does.
    notice: Option<String>,
}

impl Canvas {
    /// The canvas for `instrument`, or an empty one when no instrument is open.
    pub fn open(instrument: Option<&Path>, root: Option<&Path>) -> Self {
        Self {
            instrument: instrument.map(|path| (path.to_path_buf(), root.map(Path::to_path_buf))),
            loaded: instrument.map(|path| load(path, root)),
            camera: None,
            selection: None,
            also: Vec::new(),
            wire: None,
            drag: None,
            menu: None,
            renaming: None,
            palette: Palette::new(),
            notice: None,
        }
    }

    /// Read `instrument` again, keeping the view. The selection is kept while the document still
    /// has the boxes it names; a selected wire, a drag under way and an open menu are dropped,
    /// since what they point at may have moved.
    pub fn reload(&mut self, instrument: &Path, root: Option<&Path>) {
        let loaded = load(instrument, root);
        if let Ok(doc) = &loaded {
            if (self.selection.as_ref()).is_some_and(|item| doc.graph.index_of(item).is_none()) {
                self.selection = None;
            }
            self.also.retain(|item| doc.graph.index_of(item).is_some());
            if self.selection.is_none() && !self.also.is_empty() {
                self.selection = Some(self.also.remove(0));
            }
        }
        self.instrument = Some((instrument.to_path_buf(), root.map(Path::to_path_buf)));
        self.loaded = Some(loaded);
        self.wire = None;
        if self.drag != Some(Drag::Pan) {
            self.drag = None;
        }
        self.menu = None;
    }

    fn doc(&self) -> Option<&Document> {
        self.loaded.as_ref().and_then(|loaded| loaded.as_ref().ok())
    }

    /// Every selected box, the first-selected first.
    fn selected(&self) -> Vec<&Item> {
        self.selection.iter().chain(&self.also).collect()
    }

    /// What is at `point`, in canvas units, seen at `zoom`.
    fn at(&self, point: Pos2, zoom: f32) -> Target {
        let Some(doc) = self.doc() else {
            return Target::Nothing;
        };
        if let Some(i) = geometry::hit(&doc.rects, point) {
            return Target::Box(doc.graph.boxes[i].item.clone());
        }
        match geometry::wire_at(&doc.graph, &doc.rects, point, WIRE_REACH / zoom) {
            Some(wire) => Target::Wire(wire),
            None => Target::Nothing,
        }
    }

    /// Select what is at `point`, in canvas units, seen at `zoom`. With `shift`, add the box there
    /// to the selection, or take it out.
    fn click(&mut self, point: Pos2, zoom: f32, shift: bool) {
        match (self.at(point, zoom), shift) {
            (Target::Box(item), true) if self.wire.is_none() => self.toggle(item),
            (Target::Box(item), _) => self.select(Some(item)),
            (Target::Wire(_), true) => {}
            (Target::Wire(wire), false) => {
                self.select(None);
                self.wire = Some(wire);
            }
            (Target::Nothing, true) => {}
            (Target::Nothing, false) => self.select(None),
        }
    }

    fn select(&mut self, item: Option<Item>) {
        self.selection = item;
        self.also.clear();
        self.wire = None;
    }

    fn toggle(&mut self, item: Item) {
        if self.selection.as_ref() == Some(&item) {
            self.selection = (!self.also.is_empty()).then(|| self.also.remove(0));
        } else if let Some(i) = self.also.iter().position(|a| a == &item) {
            self.also.remove(i);
        } else if self.selection.is_none() {
            self.selection = Some(item);
        } else {
            self.also.push(item);
        }
    }

    /// Begin a drag pressed at `point`, in canvas units: a wire from the output port there, else a
    /// move of the box there and the rest of the selection with it, else a pan.
    fn press(&mut self, point: Pos2, shift: bool) {
        let Some(doc) = self.doc() else {
            self.drag = Some(Drag::Pan);
            return;
        };
        if let Some(from) = geometry::output_at(&doc.graph, &doc.rects, point, PORT_REACH) {
            let verdicts = self.verdicts(from).unwrap_or_else(|why| {
                self.notice = Some(why);
                BTreeMap::new()
            });
            self.drag = Some(Drag::Wire {
                from,
                to: point,
                verdicts,
            });
            return;
        }
        let Some(i) = geometry::hit(&doc.rects, point) else {
            self.drag = Some(Drag::Pan);
            return;
        };
        let item = doc.graph.boxes[i].item.clone();
        if !self.selected().contains(&&item) {
            if shift && self.wire.is_none() {
                self.toggle(item);
            } else {
                self.select(Some(item));
            }
        }
        let Some(doc) = self.doc() else {
            unreachable!("checked above")
        };
        let boxes = (self.selected().into_iter())
            .filter_map(|item| doc.graph.index_of(item))
            .collect();
        self.drag = Some(Drag::Move {
            boxes,
            by: Vec2::ZERO,
        });
    }

    /// Carry the drag on to `point`, `by` canvas units further than the last call.
    fn drag_to(&mut self, point: Pos2, by: Vec2) {
        match &mut self.drag {
            Some(Drag::Move { by: moved, .. }) => *moved += by,
            Some(Drag::Wire { to, .. }) => *to = point,
            Some(Drag::Pan) | None => {}
        }
    }

    /// End the drag at `point`, in canvas units, with the edit it makes, if any.
    fn release(&mut self, point: Pos2) -> Option<Gesture> {
        let drag = self.drag.take()?;
        let doc = self.doc()?;
        match drag {
            Drag::Pan => None,
            Drag::Move { boxes, by } => {
                (by != Vec2::ZERO).then(|| Gesture::Move(moved(doc, &boxes, by)))
            }
            Drag::Wire { from, verdicts, .. } => {
                let to = geometry::input_at(&doc.graph, &doc.rects, point, PORT_REACH)?;
                match verdicts.get(&to)? {
                    Some(why) => {
                        self.notice = Some(why.clone());
                        None
                    }
                    None => {
                        let source = gesture::wire_source(&doc.graph, from)?;
                        let (address, input) = gesture::input_end(&doc.graph, to)?;
                        Some(Gesture::Wire {
                            address,
                            input,
                            from: gesture::wire_ref(&source),
                        })
                    }
                }
            }
        }
    }

    /// Ask the document which inputs take a wire from `from`.
    fn verdicts(&self, from: PortRef) -> Result<BTreeMap<PortRef, Option<String>>, String> {
        let (Some((path, root)), Some(doc)) = (&self.instrument, self.doc()) else {
            return Ok(BTreeMap::new());
        };
        let Some(source) = gesture::wire_source(&doc.graph, from) else {
            return Ok(BTreeMap::new());
        };
        let answer = wire_compatibility(
            &WireCompatibility {
                source: path.display().to_string(),
                from: source,
                to: None,
            },
            &store(path, root.as_deref()),
        )
        .map_err(|refusal| refusal.message)?;
        Ok((answer.output.verdicts.into_iter())
            .filter_map(|verdict| {
                // An output pipe already has its one feed; it is not a target a drop can wire.
                let WireTarget::NodeInput { node, port } = verdict.target else {
                    return None;
                };
                let at = gesture::input_ref(&doc.graph, &node, &port)?;
                Some((at, verdict.reason.filter(|_| !verdict.ok)))
            })
            .collect())
    }

    /// The edit the Delete key makes: unwire the selected wire, or remove the selected node.
    fn delete(&self) -> Option<Gesture> {
        let doc = self.doc()?;
        if let Some(wire) = self.wire {
            let (address, input) = gesture::input_end(&doc.graph, doc.graph.wires[wire].to)?;
            return Some(Gesture::Unwire { address, input });
        }
        match &self.selection {
            Some(Item::Node(address)) => Some(Gesture::Remove {
                address: address.clone(),
            }),
            _ => None,
        }
    }

    /// Start renaming the selected node.
    fn start_renaming(&mut self) {
        if let Some(Item::Node(address)) = &self.selection {
            self.renaming = Some(Renaming {
                from: address.clone(),
                text: address.clone(),
            });
        }
    }

    /// Take in how `gesture` went: a refusal or rejection is said until the next edit lands, and
    /// the selection follows a node added or renamed.
    pub fn applied(&mut self, gesture: &Gesture, result: &Result<EditResult, Refusal>) {
        let edit = match result {
            Err(refusal) => {
                self.notice = Some(refusal.message.clone());
                return;
            }
            Ok(edit) => edit,
        };
        if !edit.written {
            let why: Vec<String> = (edit.report.errors.iter())
                .map(|e| match &e.node {
                    Some(node) => format!("{node}: {}", e.message),
                    None => e.message.clone(),
                })
                .collect();
            self.notice = Some(why.join("\n"));
            return;
        }
        self.notice = None;
        match gesture {
            Gesture::Add { address, .. } => self.select(Some(Item::Node(address.clone()))),
            Gesture::Rename { from, to } => {
                for item in self.selection.iter_mut().chain(&mut self.also) {
                    if item == &Item::Node(from.clone()) {
                        *item = Item::Node(to.clone());
                    }
                }
            }
            _ => {}
        }
    }

    /// Draw the canvas over the rest of `ui`, and return the edit a gesture finished this frame
    /// asks for. Draws nothing when no instrument is open.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<Gesture> {
        let doc = match &self.loaded {
            None => {
                ui.take_available_space();
                return None;
            }
            Some(Err(e)) => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("No canvas: the instrument could not be read. {e}"),
                );
                ui.take_available_space();
                return None;
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
        if let Some(notice) = &self.notice {
            ui.colored_label(ui.visuals().warn_fg_color, notice);
        }
        let mut gesture = None;
        ui.horizontal(|ui| {
            ui.menu_button("Add node", |ui| {
                if let Some(type_name) = self.palette.ui(ui) {
                    gesture = self.add(&type_name);
                }
            });
            ui.weak(
                "Drag from an output to wire it · Delete removes the selection · F2 renames · \
                 right-click for more",
            );
        });

        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let area = response.rect;
        if self.camera.is_none() {
            self.camera = self.doc().map(|doc| doc.fitted(area));
        }
        let Some(camera) = self.camera else {
            unreachable!("a loaded document has a view")
        };
        let to_canvas = |p: Pos2| camera.to_canvas(area.min, p);
        let pointer = response.hover_pos();
        let shift = ui.input(|i| i.modifiers.shift);

        let primary = egui::PointerButton::Primary;
        if response.drag_started_by(primary) {
            if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                self.press(to_canvas(origin), shift);
            }
        } else if response.drag_started() {
            self.drag = Some(Drag::Pan);
        }
        if response.dragged() {
            let delta = response.drag_delta();
            match &self.drag {
                Some(Drag::Pan) | None => self.pan(delta),
                Some(_) => {
                    if let Some(p) = response.interact_pointer_pos() {
                        self.drag_to(to_canvas(p), delta / camera.zoom);
                    }
                }
            }
        }
        if let Some(pointer) = pointer {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta(), i.zoom_delta()));
            self.pan(scroll);
            if zoom != 1.0 {
                if let Some(camera) = &mut self.camera {
                    camera.zoom_about(pointer - area.min, zoom);
                }
            }
        }
        if response.double_clicked() && !pointer.is_some_and(|p| self.at_box(to_canvas(p))) {
            self.camera = self.doc().map(|doc| doc.fitted(area));
        }
        if response.clicked() || response.drag_started_by(primary) {
            response.request_focus();
        }
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                self.click(to_canvas(p), camera.zoom, shift);
            }
        }
        if response.secondary_clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let target = self.at(to_canvas(p), camera.zoom);
                match &target {
                    Target::Box(item) if !self.selected().contains(&item) => {
                        self.select(Some(item.clone()))
                    }
                    Target::Wire(wire) => {
                        self.select(None);
                        self.wire = Some(*wire);
                    }
                    _ => {}
                }
                self.menu = Some(target);
            }
        }
        response.context_menu(|ui| {
            if let Some(edit) = self.menu_ui(ui) {
                gesture = Some(edit);
            }
        });
        if response.has_focus() {
            let (delete, rename, escape) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace),
                    i.key_pressed(egui::Key::F2),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if delete {
                gesture = gesture.or_else(|| self.delete());
            }
            if rename {
                self.start_renaming();
            }
            if escape {
                self.select(None);
            }
        }

        // Painted before a released drag is let go, so the frame it ends on still shows where the
        // boxes were dropped rather than where they were.
        self.paint(&painter, ui.visuals(), area);
        if response.drag_stopped() {
            let at = (response.interact_pointer_pos())
                .or(pointer)
                .map_or(Pos2::ZERO, to_canvas);
            gesture = gesture.or_else(|| self.release(at));
        }
        if !response.dragged() && !response.drag_stopped() && self.drag == Some(Drag::Pan) {
            self.drag = None;
        }
        let said: Vec<String> = (pointer.zip(self.doc()))
            .and_then(|(p, doc)| geometry::hit(&doc.rects, to_canvas(p)).map(|i| (i, doc)))
            .map(|(i, doc)| doc.graph.boxes[i].problems.iter())
            .into_iter()
            .flatten()
            .map(|p| p.message.clone())
            .collect();
        if !said.is_empty() {
            response.on_hover_text_at_pointer(said.join("\n"));
        }
        self.rename_ui(ui, area).or(gesture)
    }

    fn at_box(&self, point: Pos2) -> bool {
        self.doc()
            .is_some_and(|doc| geometry::hit(&doc.rects, point).is_some())
    }

    fn pan(&mut self, delta: Vec2) {
        if let Some(camera) = &mut self.camera {
            camera.pan(delta);
        }
    }

    /// The edit adding a node of `type_name` at a free address.
    fn add(&self, type_name: &str) -> Option<Gesture> {
        Some(Gesture::Add {
            address: gesture::free_address(&self.doc()?.graph, type_name),
            type_name: type_name.to_string(),
        })
    }

    fn menu_ui(&mut self, ui: &mut egui::Ui) -> Option<Gesture> {
        match self.menu.clone() {
            Some(Target::Box(Item::Node(_))) => {
                if ui.button("Rename…").clicked() {
                    self.start_renaming();
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    ui.close();
                    return self.delete();
                }
                None
            }
            Some(Target::Wire(_)) if self.delete().is_some() => {
                if ui.button("Unwire").clicked() {
                    ui.close();
                    return self.delete();
                }
                None
            }
            Some(Target::Nothing) => {
                let type_name = self.palette.ui(ui)?;
                self.add(&type_name)
            }
            // An interface pipe, or the wire feeding an output pipe: nothing here edits them.
            _ => {
                ui.close();
                None
            }
        }
    }

    /// The rename field, over the node being renamed; returns the rename once it is entered.
    fn rename_ui(&mut self, ui: &egui::Ui, area: Rect) -> Option<Gesture> {
        let renaming = self.renaming.as_mut()?;
        let camera = self.camera?;
        let doc = self.loaded.as_ref()?.as_ref().ok()?;
        let Some(i) = doc.graph.index_of(&Item::Node(renaming.from.clone())) else {
            self.renaming = None;
            return None;
        };
        let at = camera.to_screen(area.min, doc.rects[i].min);
        let mut done = None;
        egui::Window::new("Rename node")
            .collapsible(false)
            .resizable(false)
            .fixed_pos(at)
            .show(ui.ctx(), |ui| {
                let field = ui.text_edit_singleline(&mut renaming.text);
                field.request_focus();
                let (enter, escape) = ui.input(|i| {
                    (
                        i.key_pressed(egui::Key::Enter),
                        i.key_pressed(egui::Key::Escape),
                    )
                });
                ui.horizontal(|ui| {
                    if ui.button("Rename").clicked() || enter {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() || escape {
                        done = Some(false);
                    }
                });
            });
        let rename = match done? {
            true => {
                let to = gesture::typed_address(&renaming.text);
                (to != renaming.from).then(|| Gesture::Rename {
                    from: renaming.from.clone(),
                    to,
                })
            }
            false => None,
        };
        self.renaming = None;
        rename
    }

    fn paint(&self, painter: &egui::Painter, visuals: &egui::Visuals, area: Rect) {
        let (Some(doc), Some(camera)) = (self.doc(), self.camera) else {
            return;
        };
        let mut rects = doc.rects.clone();
        if let Some(Drag::Move { boxes, by }) = &self.drag {
            for &i in boxes {
                rects[i] = rects[i].translate(*by);
            }
        }
        let selected = (self.selected().into_iter())
            .filter_map(|item| doc.graph.index_of(item))
            .collect();
        let wiring = match &self.drag {
            Some(Drag::Wire { from, to, verdicts }) => Some(view::Wiring {
                from: *from,
                to: *to,
                verdicts,
            }),
            _ => None,
        };
        view::Scene {
            graph: &doc.graph,
            rects: &rects,
            camera,
            origin: area.min,
            selected,
            wire: self.wire,
            wiring,
        }
        .paint(painter, visuals);
    }
}

/// Where each box goes when `boxes` are dropped `by` canvas units from where they are drawn: those
/// moved, and every box the document stores no position for yet, at the position it is drawn at.
/// A box placed by the rule would otherwise be placed again around the moved ones and jump.
fn moved(doc: &Document, boxes: &[usize], by: Vec2) -> Vec<LayoutPosition> {
    (doc.graph.boxes.iter().zip(&doc.rects).enumerate())
        .filter_map(|(i, (b, rect))| {
            let at = if boxes.contains(&i) {
                rect.min + by
            } else if b.stored.is_none() {
                rect.min
            } else {
                return None;
            };
            Some(LayoutPosition {
                target: gesture::layout_target(&b.item),
                x: at.x,
                y: at.y,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use reuben_api::authoring::{
        add_instrument_interface_output, add_instrument_node, new_instrument,
        set_instrument_layout, AddInstrumentInterfaceOutput, AddInstrumentNode, LayoutTarget,
        NewInstrument, Report, SetInstrumentLayout,
    };
    use serde_json::json;

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
    fn a_reload_draws_the_file_again_and_keeps_the_view_and_a_selection_that_still_exists() {
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
        let add = |address: &str| {
            add_instrument_node(
                &AddInstrumentNode {
                    source: source.clone(),
                    address: address.into(),
                    type_name: "oscillator".into(),
                    ..Default::default()
                },
                &store,
            )
            .unwrap();
        };
        add("/kept");
        let mut canvas = Canvas::open(Some(&path), None);
        let camera = Camera {
            offset: egui::Vec2::new(7.0, -3.0),
            zoom: 0.5,
        };
        canvas.camera = Some(camera);
        canvas.selection = Some(Item::Node("/kept".into()));
        let boxes = |canvas: &Canvas| match &canvas.loaded {
            Some(Ok(doc)) => doc.graph.boxes.len(),
            other => panic!("not loaded: {other:?}"),
        };
        let before = boxes(&canvas);

        add("/added");
        canvas.reload(&path, None);
        assert_eq!(boxes(&canvas), before + 1);
        assert_eq!(canvas.camera, Some(camera));
        assert_eq!(canvas.selection, Some(Item::Node("/kept".into())));

        canvas.selection = Some(Item::Node("/gone".into()));
        canvas.reload(&path, None);
        assert_eq!(canvas.selection, None);
    }

    #[test]
    fn a_file_that_is_not_a_document_is_reported_not_drawn() {
        let missing = PathBuf::from("does-not-exist.json");
        assert!(load(&missing, None).is_err());
    }

    #[test]
    fn a_click_selects_the_box_under_it_and_a_click_on_nothing_clears() {
        let mut canvas = Canvas::open(None, None);
        canvas.loaded = Some(Ok(library("breath-harp")));
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
        canvas.click(inside, 1.0, false);
        assert_eq!(canvas.selection, Some(item));
        canvas.click(outside, 1.0, false);
        assert_eq!(canvas.selection, None);
    }

    #[test]
    fn no_instrument_has_nothing_to_select() {
        let mut canvas = Canvas::open(None, None);
        canvas.click(Pos2::ZERO, 1.0, false);
        assert_eq!(canvas.selection, None);
    }

    /// An instrument file: `/osc` wired into `/filter`, which feeds the output pipe `main`, and a
    /// `/clock` nothing is wired to. Nothing has a stored position.
    struct Seed {
        _temp: TempConfig,
        path: PathBuf,
    }

    impl Seed {
        fn new() -> Self {
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
            let node = |address: &str, type_name: &str, inputs: serde_json::Value| {
                let edit = add_instrument_node(
                    &AddInstrumentNode {
                        source: source.clone(),
                        address: address.into(),
                        type_name: type_name.into(),
                        inputs: serde_json::from_value(inputs).unwrap(),
                        ..Default::default()
                    },
                    &store,
                )
                .unwrap()
                .output;
                assert!(edit.written, "{:?}", edit.report);
            };
            node("/osc", "oscillator", json!({}));
            node(
                "/filter",
                "filter",
                json!({ "audio": { "from": "/osc.audio" } }),
            );
            node("/clock", "clock", json!({}));
            let edit = add_instrument_interface_output(
                &AddInstrumentInterfaceOutput {
                    source,
                    name: "main".into(),
                    from: "/filter.audio".into(),
                    ..Default::default()
                },
                &store,
            )
            .unwrap()
            .output;
            assert!(edit.written, "{:?}", edit.report);
            Self { _temp: temp, path }
        }

        fn canvas(&self) -> Canvas {
            Canvas::open(Some(&self.path), None)
        }
    }

    fn doc(canvas: &Canvas) -> &Document {
        canvas.doc().expect("the document loaded")
    }

    fn port(canvas: &Canvas, node: &str, name: &str, output: bool) -> (PortRef, Pos2) {
        let doc = doc(canvas);
        let i = doc.graph.index_of(&Item::Node(node.into())).unwrap();
        let b = &doc.graph.boxes[i];
        let ports = if output { &b.outputs } else { &b.inputs };
        let p = ports.iter().position(|p| p.name == name).unwrap();
        let at = if output {
            geometry::output_pos(b, doc.rects[i], p)
        } else {
            geometry::input_pos(b, doc.rects[i], p)
        };
        (PortRef { node: i, port: p }, at)
    }

    fn middle_of(canvas: &Canvas, item: &Item) -> Pos2 {
        let doc = doc(canvas);
        doc.rects[doc.graph.index_of(item).unwrap()].center()
    }

    /// A point on the wire into `to`, a little way along it so no box is under it.
    fn on_wire(canvas: &Canvas, to: &Item) -> Pos2 {
        let doc = doc(canvas);
        let to = doc.graph.index_of(to).unwrap();
        let wire = doc.graph.wires.iter().find(|w| w.to.node == to).unwrap();
        let [a, b, c, d] = geometry::wire_curve(&doc.graph, &doc.rects, wire);
        ((a.to_vec2() + b.to_vec2() * 3.0 + c.to_vec2() * 3.0 + d.to_vec2()) / 8.0).to_pos2()
    }

    #[test]
    fn a_wire_dragged_from_an_output_marks_the_inputs_that_take_it_and_wires_the_one_dropped_on() {
        let seed = Seed::new();
        let mut canvas = seed.canvas();
        let (from, start) = port(&canvas, "/osc", "audio", true);
        let (cutoff, cutoff_at) = port(&canvas, "/filter", "cutoff", false);
        let (sync, _) = port(&canvas, "/clock", "sync", false);

        canvas.press(start + Vec2::new(2.0, 1.0), false);
        let Some(Drag::Wire {
            from: f, verdicts, ..
        }) = &canvas.drag
        else {
            panic!("not a wire: {:?}", canvas.drag)
        };
        assert_eq!(*f, from);
        assert_eq!(verdicts.get(&cutoff), Some(&None));
        assert!(matches!(verdicts.get(&sync), Some(Some(_))), "{verdicts:?}");

        canvas.drag_to(cutoff_at, Vec2::ZERO);
        assert_eq!(
            canvas.release(cutoff_at + Vec2::new(-2.0, 2.0)),
            Some(Gesture::Wire {
                address: "/filter".into(),
                input: "cutoff".into(),
                from: "/osc.audio".into(),
            })
        );
        assert_eq!(canvas.drag, None);
    }

    #[test]
    fn a_wire_dropped_on_an_input_that_refuses_it_edits_nothing_and_says_why() {
        let seed = Seed::new();
        let mut canvas = seed.canvas();
        let (_, start) = port(&canvas, "/osc", "audio", true);
        let (_, sync_at) = port(&canvas, "/clock", "sync", false);
        canvas.press(start, false);
        assert_eq!(canvas.release(sync_at), None);
        assert!(canvas.notice.is_some());

        canvas.notice = None;
        canvas.press(start, false);
        assert_eq!(canvas.release(sync_at + Vec2::new(0.0, 400.0)), None);
        assert_eq!(canvas.notice, None, "a drop on nothing is not a refusal");
    }

    #[test]
    fn a_box_dragged_is_moved_and_every_unplaced_box_is_fixed_where_it_is_drawn() {
        let seed = Seed::new();
        let mut canvas = seed.canvas();
        let osc = Item::Node("/osc".into());
        let by = Vec2::new(10.0, 25.0);
        canvas.press(middle_of(&canvas, &osc), false);
        assert_eq!(canvas.selection, Some(osc.clone()));
        canvas.drag_to(Pos2::ZERO, by * 0.5);
        canvas.drag_to(Pos2::ZERO, by * 0.5);
        let Some(Gesture::Move(positions)) = canvas.release(Pos2::ZERO) else {
            panic!("not a move")
        };
        let before = doc(&canvas).rects.clone();
        assert_eq!(positions.len(), before.len(), "{positions:?}");
        let osc_at = doc(&canvas).graph.index_of(&osc).unwrap();

        set_instrument_layout(
            &SetInstrumentLayout {
                source: seed.path.display().to_string(),
                positions,
                expect: None,
            },
            &FsResolver::for_document(&seed.path.display().to_string()),
        )
        .unwrap();
        canvas.reload(&seed.path, None);
        for (i, (now, was)) in doc(&canvas).rects.iter().zip(&before).enumerate() {
            let expected = if i == osc_at { was.translate(by) } else { *was };
            assert_eq!(*now, expected, "box {i}");
        }
    }

    #[test]
    fn a_box_added_with_shift_moves_with_the_selection_and_a_press_on_nothing_pans() {
        let seed = Seed::new();
        let mut canvas = seed.canvas();
        let (osc, clock) = (Item::Node("/osc".into()), Item::Node("/clock".into()));
        canvas.click(middle_of(&canvas, &osc), 1.0, false);
        canvas.press(middle_of(&canvas, &clock), true);
        assert_eq!(canvas.selected(), [&osc, &clock]);
        canvas.drag_to(Pos2::ZERO, Vec2::new(5.0, 0.0));
        let Some(Gesture::Move(positions)) = canvas.release(Pos2::ZERO) else {
            panic!("not a move")
        };
        let doc = doc(&canvas);
        for item in [&osc, &clock] {
            let was = doc.rects[doc.graph.index_of(item).unwrap()].min;
            let to = positions
                .iter()
                .find(|p| p.target == gesture::layout_target(item))
                .unwrap();
            assert_eq!(Pos2::new(to.x, to.y), was + Vec2::new(5.0, 0.0), "{item:?}");
        }

        let nowhere = geometry::bounds(doc.rects.iter().copied()).unwrap().max + Vec2::splat(500.0);
        canvas.press(nowhere, false);
        assert_eq!(canvas.drag, Some(Drag::Pan));
        assert_eq!(canvas.release(nowhere), None);
    }

    #[test]
    fn a_box_pressed_without_moving_is_no_edit() {
        let seed = Seed::new();
        let mut canvas = seed.canvas();
        canvas.press(middle_of(&canvas, &Item::Node("/osc".into())), false);
        assert_eq!(canvas.release(Pos2::ZERO), None);
    }

    #[test]
    fn delete_removes_the_selected_node_or_unwires_the_selected_wire() {
        let seed = Seed::new();
        let mut canvas = seed.canvas();
        assert_eq!(canvas.delete(), None);

        let filter = Item::Node("/filter".into());
        canvas.click(middle_of(&canvas, &filter), 1.0, false);
        assert_eq!(
            canvas.delete(),
            Some(Gesture::Remove {
                address: "/filter".into()
            })
        );

        canvas.click(on_wire(&canvas, &filter), 1.0, false);
        assert_eq!(canvas.selection, None);
        assert!(canvas.wire.is_some());
        assert_eq!(
            canvas.delete(),
            Some(Gesture::Unwire {
                address: "/filter".into(),
                input: "audio".into(),
            })
        );

        // An output pipe always has its feed.
        canvas.click(on_wire(&canvas, &Item::Output("main".into())), 1.0, false);
        assert!(canvas.wire.is_some());
        assert_eq!(canvas.delete(), None);

        // An interface pipe is not a node to remove.
        canvas.click(middle_of(&canvas, &Item::Output("main".into())), 1.0, false);
        assert_eq!(canvas.delete(), None);
    }

    #[test]
    fn a_node_is_added_at_a_free_address_and_selected_once_it_lands() {
        let seed = Seed::new();
        let mut canvas = seed.canvas();
        let add = canvas.add("oscillator").unwrap();
        assert_eq!(
            add,
            Gesture::Add {
                address: "/oscillator".into(),
                type_name: "oscillator".into(),
            }
        );
        let landed = |written: bool| {
            Ok(EditResult {
                report: Report {
                    ok: written,
                    errors: Vec::new(),
                    warnings: Vec::new(),
                },
                written,
                hash: String::new(),
                notes: Vec::new(),
                zoom: String::new(),
            })
        };
        canvas.applied(&add, &landed(true));
        assert_eq!(canvas.selection, Some(Item::Node("/oscillator".into())));

        let rename = Gesture::Rename {
            from: "/oscillator".into(),
            to: "/lead".into(),
        };
        canvas.applied(&rename, &landed(true));
        assert_eq!(canvas.selection, Some(Item::Node("/lead".into())));

        canvas.applied(&add, &landed(false));
        assert!(canvas.notice.is_some());
        assert_eq!(canvas.selection, Some(Item::Node("/lead".into())));
        canvas.applied(
            &add,
            &Err(Refusal {
                message: "no".into(),
            }),
        );
        assert_eq!(canvas.notice.as_deref(), Some("no"));
    }

    #[test]
    fn renaming_starts_on_the_selected_node_only() {
        let seed = Seed::new();
        let mut canvas = seed.canvas();
        canvas.start_renaming();
        assert_eq!(canvas.renaming, None);
        canvas.click(middle_of(&canvas, &Item::Output("main".into())), 1.0, false);
        canvas.start_renaming();
        assert_eq!(canvas.renaming, None);
        canvas.click(middle_of(&canvas, &Item::Node("/osc".into())), 1.0, false);
        canvas.start_renaming();
        assert_eq!(
            canvas.renaming,
            Some(Renaming {
                from: "/osc".into(),
                text: "/osc".into(),
            })
        );
    }
}
