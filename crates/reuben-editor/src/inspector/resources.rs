//! The viewed document's `resources` table in the inspector, with nothing selected: each id and the
//! source it names, added and repointed through the native file picker, and removed. With a node
//! selected, the id each of its resource slots reads, picked from the table.
//!
//! A picked file is stored relative to the viewed document's directory, which is where that
//! document's references resolve from, so the table survives the instrument being moved whole.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use eframe::egui;
use reuben_api::authoring::{
    add_instrument_resource, describe_operators, remove_instrument_resource,
    set_instrument_node_resource, set_instrument_resource, AddInstrumentResource, Answer,
    DescribeOperators, EditResult, InstrumentStructure, Refusal, RemoveInstrumentResource,
    ResourceEntry, Resources, SetInstrumentNodeResource, SetInstrumentResource, StructureNode,
};

use super::view::Ask;
use super::{Edit, Inspector};
use crate::pick;

/// One change to the resources table, or to what a node reads from it.
#[derive(Debug, Clone, PartialEq)]
pub enum ResourceEdit {
    Add {
        id: String,
        source: String,
    },
    Remove(String),
    /// The id `id`, naming `source` in place of what it named. One write, where a remove and an
    /// add would be two undo steps and would Swap a document in which the id's readers read
    /// nothing.
    Repoint {
        id: String,
        source: String,
    },
    /// The node at `address` reading `id` through `slot`.
    Slot {
        address: String,
        slot: String,
        id: String,
    },
}

impl ResourceEdit {
    pub fn run(
        self,
        source: String,
        store: &dyn Resources,
        expect: Option<String>,
    ) -> Result<Answer<EditResult>, Refusal> {
        match self {
            ResourceEdit::Add { id, source: named } => add_instrument_resource(
                &AddInstrumentResource {
                    source,
                    id,
                    resource_source: named,
                    expect,
                },
                store,
            ),
            ResourceEdit::Remove(id) => {
                remove_instrument_resource(&RemoveInstrumentResource { source, id, expect }, store)
            }
            ResourceEdit::Repoint { id, source: named } => set_instrument_resource(
                &SetInstrumentResource {
                    source,
                    id,
                    resource_source: named,
                    expect,
                },
                store,
            ),
            ResourceEdit::Slot { address, slot, id } => set_instrument_node_resource(
                &SetInstrumentNodeResource {
                    source,
                    address,
                    slot,
                    id,
                    expect,
                },
                store,
            ),
        }
    }
}

/// What a picked file is named as: a sample is a WAV, anything else here an instrument document.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Sample,
    Document,
}

impl Kind {
    fn of(source: &str) -> Self {
        if (Path::new(source).extension()).is_some_and(|ext| ext.eq_ignore_ascii_case("json")) {
            Kind::Document
        } else {
            Kind::Sample
        }
    }

    fn pick(self, dir: Option<&Path>) -> Option<PathBuf> {
        match self {
            Kind::Sample => pick::sample(dir),
            Kind::Document => pick::instrument_to_open(dir),
        }
    }

    /// What a node's `slot` reads: a `sample` a sample, a `voice` or `patch` a document.
    fn read_by(slot: &str) -> Self {
        if slot == "sample" {
            Kind::Sample
        } else {
            Kind::Document
        }
    }
}

/// The id typed into the "add a resource" row, held between frames for the document it was typed
/// in: viewing another document starts the row empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    id: String,
    document: PathBuf,
}

impl Draft {
    fn viewing(&mut self, document: &Path) {
        if self.document != document {
            *self = Draft {
                id: String::new(),
                document: document.to_path_buf(),
            };
        }
    }

    /// The new entry naming `picked` from a document in `dir`: under the id typed, else one made
    /// from the file's name that `top` does not have yet.
    pub fn add(
        &self,
        top: &InstrumentStructure,
        dir: &Path,
        picked: &Path,
    ) -> Result<ResourceEdit, String> {
        let source = relative_source(dir, picked)?;
        let id = match self.id.trim() {
            "" => free_id(top, picked),
            typed => typed.to_string(),
        };
        Ok(ResourceEdit::Add { id, source })
    }

    pub fn added(&mut self) {
        self.id.clear();
    }
}

/// `id` repointed at `picked`, from a document in `dir`.
pub fn repoint(id: &str, dir: &Path, picked: &Path) -> Result<ResourceEdit, String> {
    Ok(ResourceEdit::Repoint {
        id: id.to_string(),
        source: relative_source(dir, picked)?,
    })
}

/// `picked` as a source relative to `dir`, with `/` between its components on every platform.
///
/// Worked out on the spelling of both paths, never through symlinks: the resolver joins a source
/// to the document's directory and folds each `..` into the component before it without asking
/// the filesystem, so only a path made the same way names the file that was picked.
pub fn relative_source(dir: &Path, picked: &Path) -> Result<String, String> {
    let (from, to) = (lexical(dir), lexical(picked));
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let shared = (from.iter().zip(&to)).take_while(|(a, b)| a == b).count();
    let unnamed = || {
        format!(
            "{} cannot be named from {}: a resource is stored relative to the instrument",
            picked.display(),
            dir.display()
        )
    };
    if shared == 0 || shared == to.len() {
        return Err(unnamed());
    }
    let ups = from[shared..].iter().map(|_| Some(".."));
    let downs = to[shared..].iter().map(|c| c.as_os_str().to_str());
    let parts: Option<Vec<&str>> = ups.chain(downs).collect();
    Ok(parts.ok_or_else(unnamed)?.join("/"))
}

/// `path` made absolute, with each `.` dropped and each `..` folded into the component before it.
fn lexical(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut folded = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other),
        }
    }
    folded
}

/// The file's name without its extension, as an id `top` does not have: `testvox`, else
/// `testvox_2`, `testvox_3`, ….
fn free_id(top: &InstrumentStructure, picked: &Path) -> String {
    let stem: String = (picked.file_stem().and_then(|s| s.to_str()))
        .unwrap_or_default()
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => c,
            _ => '_',
        })
        .collect();
    let stem = if stem.is_empty() {
        "resource".to_string()
    } else {
        stem
    };
    let taken = |id: &str| top.resources.iter().any(|r| r.id == id);
    std::iter::once(stem.clone())
        .chain((2..).map(|n| format!("{stem}_{n}")))
        .find(|id| !taken(id))
        .expect("an unbounded range has a free id")
}

/// The addresses of the nodes of `top` that read `id`, through any slot.
fn readers<'a>(top: &'a InstrumentStructure, id: &str) -> Vec<&'a str> {
    (top.nodes.iter())
        .filter(|node| node.resources.iter().any(|r| r.id == id))
        .map(|node| node.address.as_str())
        .collect()
}

/// What a button in the table asked to pick a file for.
enum Want {
    Add(Kind),
    Repoint(String, Kind),
}

/// The resources table of `top`, the document at `document`, with a row to add an entry.
pub fn list(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    top: &InstrumentStructure,
    document: &Path,
    asks: &mut Vec<Ask>,
) {
    inspector.resource.viewing(document);
    let dir = document.parent();
    ui.separator();
    crate::theme::section(ui, "Resources");
    let mut want = None;
    for entry in &top.resources {
        ui.horizontal_wrapped(|ui| {
            ui.strong(&entry.id);
            ui.weak(&entry.source).on_hover_text(&entry.canonical);
        });
        ui.horizontal(|ui| {
            let readers = readers(top, &entry.id);
            if readers.is_empty() {
                ui.weak("read by no node");
            } else {
                ui.weak(format!("read by {}", readers.join(", ")));
            }
            if ui.button("Repoint…").clicked() {
                want = Some(Want::Repoint(entry.id.clone(), Kind::of(&entry.source)));
            }
            if ui.button("Remove").clicked() {
                let edit = ResourceEdit::Remove(entry.id.clone());
                asks.push(Ask::Edit(Edit::Resource(edit), None));
            }
        });
    }
    egui::Grid::new("add resource")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Id");
            ui.add(
                egui::TextEdit::singleline(&mut inspector.resource.id)
                    .hint_text("from the file name"),
            );
            ui.end_row();
        });
    ui.horizontal(|ui| {
        if ui.button("Add sample…").clicked() {
            want = Some(Want::Add(Kind::Sample));
        }
        if ui.button("Add instrument…").clicked() {
            want = Some(Want::Add(Kind::Document));
        }
    });
    let Some(want) = want else { return };
    let Some(dir) = dir else {
        asks.push(Ask::Refuse(
            "The document has no directory to name a file from.".to_string(),
        ));
        return;
    };
    let edit = match want {
        Want::Add(kind) => {
            let Some(picked) = kind.pick(Some(dir)) else {
                return;
            };
            inspector.resource.add(top, dir, &picked)
        }
        Want::Repoint(id, kind) => {
            let Some(picked) = kind.pick(Some(dir)) else {
                return;
            };
            repoint(&id, dir, &picked)
        }
    };
    asks.push(match edit {
        Ok(edit) => Ask::Edit(Edit::Resource(edit), None),
        Err(why) => Ask::Refuse(why),
    });
}

/// The resource slots each Operator type reads through, by type name.
fn operator_slots() -> &'static BTreeMap<String, Vec<String>> {
    static SLOTS: OnceLock<BTreeMap<String, Vec<String>>> = OnceLock::new();
    SLOTS.get_or_init(|| {
        (describe_operators(&DescribeOperators::default()).ok())
            .and_then(|answer| answer.output.operators)
            .unwrap_or_default()
            .into_iter()
            .map(|op| (op.type_name, op.resources))
            .collect()
    })
}

/// Each resource slot of `node`, and the id it reads, if any: the slots its Operator reads
/// through, then any other the document sets on it.
fn node_slots(node: &StructureNode) -> Vec<(String, Option<String>)> {
    let declared = operator_slots().get(&node.type_name).cloned();
    let held = |slot: &str| {
        (node.resources.iter())
            .find(|r| r.slot == slot)
            .map(|r| r.id.clone())
    };
    let mut slots: Vec<(String, Option<String>)> = (declared.unwrap_or_default().into_iter())
        .map(|slot| {
            let id = held(&slot);
            (slot, id)
        })
        .collect();
    for extra in &node.resources {
        if !slots.iter().any(|(slot, _)| *slot == extra.slot) {
            slots.push((extra.slot.clone(), Some(extra.id.clone())));
        }
    }
    slots
}

/// The entries of `top` a node's `slot` can read: samples for `sample`, documents otherwise.
fn offered<'a>(top: &'a InstrumentStructure, slot: &str) -> Vec<&'a ResourceEntry> {
    let kind = Kind::read_by(slot);
    (top.resources.iter())
        .filter(|entry| Kind::of(&entry.source) == kind)
        .collect()
}

/// The resource slots of `node`, each with the id it reads picked from the table of `top`.
pub fn slots(
    ui: &mut egui::Ui,
    node: &StructureNode,
    top: &InstrumentStructure,
    asks: &mut Vec<Ask>,
) {
    let slots = node_slots(node);
    if slots.is_empty() {
        return;
    }
    ui.separator();
    crate::theme::section(ui, "Resources");
    egui::Grid::new("node resources")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for (slot, held) in slots {
                ui.label(&slot);
                let mut picked = held.clone();
                egui::ComboBox::from_id_salt(("node resource", &node.address, &slot))
                    .selected_text(picked.as_deref().unwrap_or("none"))
                    .show_ui(ui, |ui| {
                        for entry in offered(top, &slot) {
                            ui.selectable_value(&mut picked, Some(entry.id.clone()), &entry.id)
                                .on_hover_text(&entry.source);
                        }
                    });
                ui.end_row();
                if let Some(id) = picked.filter(|id| Some(id) != held.as_ref()) {
                    let edit = ResourceEdit::Slot {
                        address: node.address.clone(),
                        slot,
                        id,
                    };
                    asks.push(Ask::Edit(Edit::Resource(edit), None));
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use reuben_api::FsResolver;
    use reuben_native::test_support::within;
    use serde_json::json;

    use super::super::interface::tests::structure;
    use super::*;
    use crate::feedback::Feedback;
    use crate::session::test_support::Rig;
    use crate::session::{Direction, Status};

    const HANG_SECS: u64 = 60;

    fn top() -> InstrumentStructure {
        structure(json!({
            "instrument": "t", "hash": "h", "inputs": [], "outputs": [],
            "report": { "ok": true, "errors": [], "warnings": [] },
            "resources": [
                { "id": "testvox", "source": "samples/testvox.wav", "canonical": "/l/samples/testvox.wav" },
                { "id": "testvox_2", "source": "b.wav", "canonical": "/l/b.wav" }
            ],
            "nodes": [
                { "address": "/cloud", "type_name": "granulator",
                  "resources": [ { "slot": "sample", "id": "testvox" } ],
                  "ports": { "inputs": [], "outputs": [] } },
                { "address": "/osc", "type_name": "oscillator",
                  "ports": { "inputs": [], "outputs": [] } }
            ]
        }))
    }

    #[test]
    fn a_picked_file_is_stored_relative_to_the_document_with_forward_slashes() {
        let dir = Path::new("/nowhere/lib");
        for (picked, stored) in [
            ("/nowhere/lib/samples/vox.wav", "samples/vox.wav"),
            ("/nowhere/lib/vox.wav", "vox.wav"),
            ("/nowhere/other/vox.wav", "../other/vox.wav"),
            ("/elsewhere/a/b.json", "../../elsewhere/a/b.json"),
        ] {
            assert_eq!(
                relative_source(dir, Path::new(picked)).as_deref(),
                Ok(stored),
                "{picked}"
            );
        }
        assert!(relative_source(dir, dir).is_err(), "a directory is no file");
    }

    /// Through a symlinked directory, the stored source names the picked file the way the resolver
    /// reads it: by joining it to the document's directory as spelled and folding each `..`.
    #[cfg(unix)]
    #[test]
    fn the_stored_source_resolves_to_the_picked_file_through_a_link() {
        let scratch = crate::session::test_support::Scratch::new("resources_link");
        let real = scratch.path("real/lib");
        std::fs::create_dir_all(real.join("samples")).unwrap();
        std::fs::create_dir_all(scratch.path("other")).unwrap();
        let link = scratch.path("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let document = link.join("x.json").display().to_string();
        for picked in [
            scratch.path("other/a.wav"),
            link.join("samples/b.wav"),
            real.join("samples/c.wav"),
        ] {
            std::fs::write(&picked, b"").unwrap();
            let stored = relative_source(&link, &picked).expect("a relative source");
            assert!(!stored.starts_with('/'), "{stored}");
            let read = FsResolver::for_document(&document).canonical(&stored, None);
            assert_eq!(Path::new(&read), lexical(&picked), "{stored}");
        }
        assert_eq!(
            relative_source(&link, &link.join("samples/b.wav")).as_deref(),
            Ok("samples/b.wav")
        );
    }

    #[test]
    fn a_node_offers_every_slot_its_operator_reads_through_even_one_left_unset() {
        let node: StructureNode = serde_json::from_value(json!({
            "address": "/cloud", "type_name": "granulator",
            "ports": { "inputs": [], "outputs": [] }
        }))
        .unwrap();
        assert_eq!(node_slots(&node), [("sample".to_string(), None)]);
        let node: StructureNode = serde_json::from_value(json!({
            "address": "/v", "type_name": "voicer",
            "resources": [ { "slot": "voice", "id": "lead" } ],
            "ports": { "inputs": [], "outputs": [] }
        }))
        .unwrap();
        assert_eq!(
            node_slots(&node),
            [("voice".to_string(), Some("lead".to_string()))]
        );
    }

    #[test]
    fn a_slot_offers_only_the_entries_of_the_kind_it_reads() {
        let mut top = top();
        top.resources.push(ResourceEntry {
            id: "room".into(),
            source: "patches/ROOM.JSON".into(),
            canonical: "/l/patches/ROOM.JSON".into(),
        });
        let ids = |slot| -> Vec<String> {
            (offered(&top, slot).into_iter())
                .map(|e| e.id.clone())
                .collect()
        };
        assert_eq!(ids("sample"), ["testvox", "testvox_2"]);
        assert_eq!(ids("patch"), ["room"]);
        assert_eq!(ids("voice"), ["room"]);
    }

    #[test]
    fn an_id_typed_for_one_document_is_not_added_to_another() {
        let mut draft = Draft::default();
        draft.viewing(Path::new("/nowhere/lib/a.json"));
        draft.id = "kick".into();
        draft.viewing(Path::new("/nowhere/lib/a.json"));
        assert_eq!(draft.id, "kick");
        draft.viewing(Path::new("/nowhere/lib/patches/b.json"));
        assert_eq!(draft.id, "");
    }

    #[test]
    fn a_new_entry_takes_the_typed_id_else_a_free_one_from_the_file_name() {
        let top = top();
        let dir = Path::new("/nowhere/lib");
        let mut draft = Draft::default();
        assert_eq!(
            draft.add(&top, dir, Path::new("/nowhere/lib/samples/testvox.wav")),
            Ok(ResourceEdit::Add {
                id: "testvox_3".into(),
                source: "samples/testvox.wav".into()
            })
        );
        assert_eq!(
            draft.add(&top, dir, Path::new("/nowhere/lib/my take (1).wav")),
            Ok(ResourceEdit::Add {
                id: "my_take__1_".into(),
                source: "my take (1).wav".into()
            })
        );
        draft.id = " second ".into();
        assert_eq!(
            draft.add(&top, dir, Path::new("/nowhere/lib/x.wav")),
            Ok(ResourceEdit::Add {
                id: "second".into(),
                source: "x.wav".into()
            })
        );
        draft.added();
        assert_eq!(draft.id, "");
    }

    #[test]
    fn a_repoint_keeps_the_id_and_names_the_picked_file() {
        assert_eq!(
            repoint(
                "space",
                Path::new("/nowhere/lib"),
                Path::new("/nowhere/lib/patches/room.json")
            ),
            Ok(ResourceEdit::Repoint {
                id: "space".into(),
                source: "patches/room.json".into()
            })
        );
    }

    #[test]
    fn the_picker_offers_the_kind_of_file_an_entry_already_names() {
        assert_eq!(Kind::of("patches/space.json"), Kind::Document);
        assert_eq!(Kind::of("samples/testvox.wav"), Kind::Sample);
        assert_eq!(Kind::of("samples/take"), Kind::Sample);
        assert_eq!(Kind::of("patches/ROOM.JSON"), Kind::Document);
    }

    /// Commit `edit` to the viewed document and wait until the engine plays what it wrote.
    fn landed(rig: &mut Rig, inspector: &mut Inspector, edit: ResourceEdit) {
        let channel = rig.channel_source();
        let edit = Edit::Resource(edit);
        let wrote = inspector.commit(edit.clone(), None, &mut rig.session, &channel);
        assert!(wrote, "{edit:?}: {:?}", rig.session.feedback());
        let hash = (rig.session.tree().top().hash())
            .expect("the top reads")
            .to_string();
        rig.poll_until("the engine playing the edit", |rig| rig.playing() == hash);
    }

    fn viewed(rig: &Rig) -> InstrumentStructure {
        (rig.session.viewed().read.clone()).expect("the viewed document reads")
    }

    fn entry(rig: &Rig, id: &str) -> ResourceEntry {
        (viewed(rig).resources.into_iter())
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id} is in the table"))
    }

    fn dir(rig: &Rig) -> PathBuf {
        let path = &rig.session.viewed().path;
        path.parent()
            .expect("a document has a directory")
            .to_path_buf()
    }

    /// The acceptance case: a second sample added to granular-cloud, read by its granulator, and
    /// played, with the path stored relative to the instrument.
    #[test]
    fn a_sample_added_to_granular_cloud_is_read_by_its_granulator_and_played() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("resources_add", "granular-cloud.json");
            let mut inspector = Inspector::default();
            let take = rig.lib.join("takes").join("second take.wav");
            std::fs::create_dir_all(take.parent().unwrap()).unwrap();
            std::fs::copy(rig.lib.join("samples/testvox.wav"), &take).unwrap();

            let add = (inspector.resource.add(&viewed(&rig), &dir(&rig), &take))
                .expect("a file in the library");
            assert_eq!(
                add,
                ResourceEdit::Add {
                    id: "second_take".into(),
                    source: "takes/second take.wav".into()
                }
            );
            landed(&mut rig, &mut inspector, add);
            let added = entry(&rig, "second_take");
            assert_eq!(added.source, "takes/second take.wav");
            assert_eq!(Path::new(&added.canonical), take);

            landed(
                &mut rig,
                &mut inspector,
                ResourceEdit::Slot {
                    address: "/cloud".into(),
                    slot: "sample".into(),
                    id: "second_take".into(),
                },
            );
            let top = viewed(&rig);
            assert_eq!(readers(&top, "second_take"), ["/cloud"]);
            assert!(readers(&top, "source").is_empty());
            assert!(top.report.ok, "{:?}", top.report);
            assert!(
                !(top.report.warnings.iter()).any(|w| w.node.as_deref() == Some("/cloud")),
                "the granulator reads its sample: {:?}",
                top.report
            );

            // The first sample, read by nothing now, goes; the take is repointed in one write.
            landed(
                &mut rig,
                &mut inspector,
                ResourceEdit::Remove("source".into()),
            );
            assert!(!viewed(&rig).resources.iter().any(|r| r.id == "source"));
            let before = rig.session.viewed().hash().map(str::to_string);
            let back = rig.lib.join("samples/testvox.wav");
            let edit = repoint("second_take", &dir(&rig), &back).expect("a library file");
            landed(&mut rig, &mut inspector, edit);
            assert_eq!(entry(&rig, "second_take").source, "samples/testvox.wav");
            let step = (rig.session.undo().next(Direction::Undo).cloned()).expect("a step");
            assert_eq!(Some(step.before), before, "a repoint is one step");
            rig.settle();
            assert_eq!(rig.status(), &Status::Current);
        });
    }

    /// Inside a subpatch the table is the patch's own, and a file is named from the patch's
    /// directory, which is where the patch's references resolve from.
    #[test]
    fn a_resource_added_inside_a_subpatch_is_named_from_the_patchs_directory() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("resources_patch", "granular-cloud.json");
            rig.session.enter("/verb").expect("/verb patches space");
            let space = rig.lib.join("patches").join("space.json");
            assert_eq!(rig.session.viewed().path, space);
            let mut inspector = Inspector::default();
            let sample = rig.lib.join("samples/testvox.wav");

            let add = (inspector.resource.add(&viewed(&rig), &dir(&rig), &sample))
                .expect("a file in the library");
            landed(&mut rig, &mut inspector, add);
            let added = entry(&rig, "testvox");
            assert_eq!(added.source, "../samples/testvox.wav");
            assert_eq!(Path::new(&added.canonical), sample);
            let top = (rig.session.tree().top().read.clone()).expect("the top reads");
            assert!(
                !top.resources.iter().any(|r| r.id == "testvox"),
                "the top-level table is untouched"
            );
        });
    }

    #[test]
    fn a_resource_id_already_in_the_table_is_refused_through_the_sessions_feedback() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("resources_taken", "granular-cloud.json");
            let mut inspector = Inspector::default();
            let channel = rig.channel_source();
            let before = rig.session.tree().clone();
            let wrote = inspector.commit(
                Edit::Resource(ResourceEdit::Add {
                    id: "source".into(),
                    source: "samples/testvox.wav".into(),
                }),
                None,
                &mut rig.session,
                &channel,
            );
            assert!(!wrote);
            assert_eq!(rig.session.tree(), &before);
            assert!(
                matches!(rig.session.feedback(), Some(Feedback::Refused { message, .. })
                    if message.contains("source")),
                "{:?}",
                rig.session.feedback()
            );
        });
    }

    #[test]
    fn each_entry_lists_the_nodes_that_read_it() {
        let top = top();
        assert_eq!(readers(&top, "testvox"), ["/cloud"]);
        assert!(readers(&top, "testvox_2").is_empty());
    }
}
