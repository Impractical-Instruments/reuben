//! The level meter on a node's box: which engine node and port each bar reads, and painting the
//! bars. The levels themselves come from the meter strip's poll.

use std::collections::HashSet;

use eframe::egui::{self, accesskit, Painter, Rect, Stroke, Visuals};
use reuben_api::authoring::WireSource;
use reuben_api::engine::{PortLevel, ProbeLevels, MAX_PROBES};

use super::graph::{Graph, Item, PortKind};
use crate::meter_strip::{self, fraction, SCALE_FLOOR_DBFS};
use crate::session::{Session, Tree};
use crate::theme;

const BAR_WIDTH: f32 = 4.0;
const BAR_GAP: f32 = 2.0;

/// How many patches deep a subpatch's output is followed to the node feeding it. A document tree
/// that nests deeper does not load, so this only stops a malformed one.
const DEEPEST: usize = 16;

/// What one bar of a box reads: the engine node at `path`, its output `port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tap {
    pub path: Vec<String>,
    pub port: String,
    /// The box's own output the bar stands for.
    pub output: String,
}

/// The width the bars of `bars` outputs take, in canvas units.
pub fn width(bars: usize) -> f32 {
    match bars {
        0 => 0.0,
        n => n as f32 * BAR_WIDTH + (n - 1) as f32 * BAR_GAP,
    }
}

/// What each of `graph`'s boxes meters, indexed as its boxes: a [`Tap`] per Signal output of a
/// node. A `subpatch` node is spliced away in the Plan, so its box reads the inner node feeding
/// each of its outputs instead, followed down through nested patches; an output fed straight from
/// one of the patch's own inputs has no node to read and no bar.
pub fn taps(graph: &Graph, session: &Session) -> Vec<Vec<Tap>> {
    let viewed = session.viewed();
    (graph.boxes.iter())
        .map(|b| {
            let Item::Node(address) = &b.item else {
                return Vec::new();
            };
            let at = session.probe_path(address);
            let patch = viewed.nested(address).filter(|_| !viewed.hosts(address));
            (b.outputs.iter())
                .filter(|output| output.kind == PortKind::Signal)
                .filter_map(|output| {
                    let (path, port) = match patch {
                        Some(source) => {
                            through(session.tree(), source, at.clone(), &output.name, DEEPEST)?
                        }
                        None => (at.clone(), output.name.clone()),
                    };
                    Some(Tap {
                        path,
                        port,
                        output: output.name.clone(),
                    })
                })
                .collect()
        })
        .collect()
}

/// The node and port feeding output `output` of the patch at `source`, spliced in at `at`.
fn through(
    tree: &Tree,
    source: &str,
    mut at: Vec<String>,
    output: &str,
    depth: usize,
) -> Option<(Vec<String>, String)> {
    let document = tree.document(source)?;
    let structure = document.read.as_ref().ok()?;
    let pipe = structure.outputs.iter().find(|pipe| pipe.name == output)?;
    let WireSource::NodeOutput { node, port } = &pipe.from else {
        return None;
    };
    at.last_mut()?.push_str(node);
    match document.nested(node) {
        Some(inner) if !document.hosts(node) => {
            through(tree, inner, at, port, depth.checked_sub(1)?)
        }
        _ => Some((at, port.clone())),
    }
}

/// The paths `taps` read, each once, in box order. Cut at [`MAX_PROBES`], since the engine refuses
/// a larger set whole: the boxes past the cut show no meter rather than every box showing none.
pub fn probe_set(taps: &[Vec<Tap>]) -> Vec<Vec<String>> {
    let mut seen = HashSet::new();
    (taps.iter().flatten())
        .filter(|tap| seen.insert(&tap.path))
        .map(|tap| tap.path.clone())
        .take(MAX_PROBES)
        .collect()
}

/// The level of each of `taps` that `probes` reports, named by the box's output it stands for.
pub fn read(taps: &[Tap], probes: &[ProbeLevels]) -> Vec<PortLevel> {
    (taps.iter())
        .filter_map(|tap| {
            let node = probes.iter().find(|p| p.path == tap.path)?;
            let port = node.ports.iter().find(|p| p.port == tap.port)?;
            Some(PortLevel {
                port: tap.output.clone(),
                level: port.level,
            })
        })
        .collect()
}

/// Where the bars of `ports` sit within a node box at `rect`, in its own units: in the header's
/// right-hand corner, `pad` in from the edge, `height` tall.
pub fn place(rect: Rect, ports: usize, pad: f32, height: f32) -> Rect {
    let right = rect.right() - pad;
    let top = rect.top() + pad / 2.0;
    Rect::from_min_max(
        egui::pos2(right - width(ports), top),
        egui::pos2(right, top + height),
    )
}

/// Paint one upright bar per port across `rect`, on screen: the RMS level filled over the peak
/// level faded, on the meter strip's scale.
pub fn paint(painter: &Painter, rect: Rect, ports: &[PortLevel], visuals: &Visuals) {
    let tick = Stroke::new(1.0, visuals.strong_text_color());
    for (column, port) in columns(rect, ports.len()).zip(ports) {
        painter.rect_filled(column, 0.0, visuals.extreme_bg_color);
        let up_to = |dbfs: f32| {
            let mut filled = column;
            filled.set_top(column.bottom() - column.height() * fraction(dbfs));
            filled
        };
        painter.rect_filled(
            up_to(port.level.peak_dbfs),
            0.0,
            theme::METER.gamma_multiply(0.4),
        );
        painter.rect_filled(up_to(port.level.rms_dbfs), 0.0, theme::METER);
        if port.level.peak_dbfs > SCALE_FLOOR_DBFS {
            let y = column.bottom() - column.height() * fraction(port.level.peak_dbfs);
            painter.hline(column.x_range(), y, tick);
        }
    }
}

/// Tell AccessKit about each bar of the box of the node at `address`, painted across `rect`: a
/// progress indicator labelled `"<address> level"`, or `"<address>.<output> level"` when `named`
/// (the box has more than one Signal output), whose value is its RMS level in dBFS.
pub fn describe(ctx: &egui::Context, rect: Rect, address: &str, ports: &[PortLevel], named: bool) {
    for (column, port) in columns(rect, ports.len()).zip(ports) {
        let label = match named {
            false => format!("{address} level"),
            true => format!("{address}.{} level", port.port),
        };
        let id = egui::Id::new(("node meter", address, &port.port));
        ctx.accesskit_node_builder(id, |node| {
            node.set_role(accesskit::Role::ProgressIndicator);
            node.set_label(label);
            node.set_value(format!(
                "peak {}, RMS {}",
                meter_strip::db(port.level.peak_dbfs),
                meter_strip::db(port.level.rms_dbfs)
            ));
            node.set_numeric_value(f64::from(port.level.rms_dbfs));
            node.set_min_numeric_value(f64::from(SCALE_FLOOR_DBFS));
            node.set_max_numeric_value(0.0);
            node.set_bounds(accesskit::Rect {
                x0: column.left().into(),
                y0: column.top().into(),
                x1: column.right().into(),
                y1: column.bottom().into(),
            });
        });
    }
}

/// The on-screen column of each of `bars` bars across `rect`, left to right, keeping the canvas's
/// proportion of bar to gap.
fn columns(rect: Rect, bars: usize) -> impl Iterator<Item = Rect> {
    let scale = rect.width() / width(bars).max(f32::EPSILON);
    (0..bars).map(move |i| {
        let left = rect.left() + i as f32 * (BAR_WIDTH + BAR_GAP) * scale;
        Rect::from_x_y_ranges(left..=left + BAR_WIDTH * scale, rect.y_range())
    })
}

#[cfg(test)]
mod tests {
    use reuben_api::engine::ChannelLevel;
    use reuben_native::test_support::{start_headless, within};

    use super::*;
    use crate::session::test_support::{probed, Scratch};

    fn path(segments: &[&str]) -> Vec<String> {
        segments.iter().map(|s| s.to_string()).collect()
    }

    /// Every box of the viewed document's meter taps, by the box's title.
    fn tapped(session: &Session) -> Vec<(String, Vec<Tap>)> {
        let viewed = session.viewed().read.as_ref().expect("the document reads");
        let graph = Graph::from_structure(viewed);
        (graph.boxes.iter().map(|b| b.title.clone()))
            .zip(taps(&graph, session))
            .filter(|(_, taps)| !taps.is_empty())
            .collect()
    }

    #[test]
    fn a_subpatch_box_meters_the_inner_node_feeding_each_signal_output() {
        let scratch = Scratch::new("node_taps");
        let top = probed(&scratch);
        let mut session = Session::open(&top, None, || {});
        let tap = |p: &[&str], port: &str, output: &str| Tap {
            path: path(p),
            port: port.to_string(),
            output: output.to_string(),
        };
        let top_level = tapped(&session);
        assert_eq!(
            top_level,
            [
                ("/osc".to_string(), vec![tap(&["/osc"], "audio", "audio")]),
                (
                    "/bass".to_string(),
                    vec![tap(&["/bass/filter"], "audio", "audio")]
                ),
                (
                    "/kick_v".to_string(),
                    vec![tap(&["/kick_v"], "audio", "audio")]
                ),
                ("/mix".to_string(), vec![tap(&["/mix"], "out", "out")]),
                (
                    "/wrap".to_string(),
                    vec![tap(&["/wrap/inner/filter"], "audio", "audio")]
                ),
                ("/mix2".to_string(), vec![tap(&["/mix2"], "out", "out")]),
            ]
        );
        session.enter("/kick_v").expect("a voicer");
        assert_eq!(
            tapped(&session),
            [(
                "/osc".to_string(),
                vec![tap(&["/kick_v", "/osc"], "audio", "audio")]
            )]
        );

        // Each names a node of the Plan the engine built from these files.
        let paths: Vec<Vec<String>> = (top_level.into_iter())
            .flat_map(|(_, taps)| taps)
            .map(|tap| tap.path)
            .collect();
        within(crate::engine::test_support::HANG_SECS, move || {
            let played = start_headless(crate::engine::test_support::config(top))
                .expect("the engine starts");
            let resolved = played.channel().set_probes(paths.clone());
            reuben_native::diagnostics::log_snapshot(&played.shutdown());
            assert_eq!(resolved.expect("set_probes"), paths);
        });
    }

    #[test]
    fn a_level_with_more_nodes_than_the_engine_takes_still_sends_a_set_it_takes() {
        let tap = |n: usize| Tap {
            path: path(&[&format!("/n{n}")]),
            port: "audio".to_string(),
            output: "audio".to_string(),
        };
        // Two outputs of one node read one path, which is sent once.
        let taps: Vec<Vec<Tap>> = (0..MAX_PROBES + 10).map(|n| vec![tap(n), tap(n)]).collect();
        let set = probe_set(&taps);
        assert_eq!(set.len(), MAX_PROBES);
        assert_eq!(set[0], path(&["/n0"]), "the first boxes keep theirs");
        assert_eq!(
            set[MAX_PROBES - 1],
            path(&[&format!("/n{}", MAX_PROBES - 1)])
        );
        within(crate::engine::test_support::HANG_SECS, move || {
            let scratch = Scratch::new("node_taps_cap");
            let played = start_headless(crate::engine::test_support::config(probed(&scratch)))
                .expect("the engine starts");
            let answer = played.channel().set_probes(set);
            reuben_native::diagnostics::log_snapshot(&played.shutdown());
            assert!(answer.is_ok(), "{answer:?}");
        });
    }

    #[test]
    fn a_playing_voicer_and_subpatch_report_a_level_on_the_port_their_bar_reads() {
        let scratch = Scratch::new("node_taps_levels");
        let top = probed(&scratch);
        let session = Session::open(&top, None, || {});
        let taps: Vec<(String, Vec<Tap>)> = (tapped(&session).into_iter())
            .filter(|(title, _)| ["/kick_v", "/bass", "/wrap"].contains(&title.as_str()))
            .collect();
        assert_eq!(taps.len(), 3);
        within(crate::engine::test_support::HANG_SECS, move || {
            let played = start_headless(crate::engine::test_support::config(top))
                .expect("the engine starts");
            let channel = played.channel();
            let all: Vec<Vec<Tap>> = taps.iter().map(|(_, taps)| taps.clone()).collect();
            channel.set_probes(probe_set(&all)).expect("set_probes");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let quiet = loop {
                let probes = channel.get_meters().expect("meters").probes;
                let quiet: Vec<String> = (taps.iter())
                    .filter(|(_, taps)| {
                        let levels = read(taps, &probes);
                        levels.len() != taps.len()
                            || levels.iter().any(|l| l.level.rms_dbfs <= SCALE_FLOOR_DBFS)
                    })
                    .map(|(title, _)| title.clone())
                    .collect();
                if quiet.is_empty() || std::time::Instant::now() >= deadline {
                    break quiet;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            reuben_native::diagnostics::log_snapshot(&played.shutdown());
            assert!(quiet.is_empty(), "no level on the bars of {quiet:?}");
        });
    }

    #[test]
    fn a_bar_reads_its_port_by_path_and_name_and_one_unreported_is_left_out() {
        let level = |peak_dbfs| ChannelLevel {
            peak_dbfs,
            rms_dbfs: peak_dbfs - 3.0,
        };
        let taps = [
            Tap {
                path: path(&["/bass/filter"]),
                port: "audio".to_string(),
                output: "out".to_string(),
            },
            Tap {
                path: path(&["/gone"]),
                port: "audio".to_string(),
                output: "other".to_string(),
            },
        ];
        let probes = [ProbeLevels {
            path: path(&["/bass/filter"]),
            ports: vec![PortLevel {
                port: "audio".to_string(),
                level: level(-6.0),
            }],
        }];
        assert_eq!(
            read(&taps, &probes),
            [PortLevel {
                port: "out".to_string(),
                level: level(-6.0),
            }]
        );
        assert!(read(&taps, &[]).is_empty());
    }
}
