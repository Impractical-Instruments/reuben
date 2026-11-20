//! The level meter on a node's box: which engine node and port each bar reads, and painting the
//! bars. The levels themselves come from the meter strip's poll.

use eframe::egui::{self, accesskit, Painter, Rect, Stroke, Visuals};
use reuben_api::authoring::WireSource;
use reuben_api::engine::{PortLevel, ProbeLevels};

use super::graph::{Graph, Item, PortKind};
use crate::meter_strip::{self, fraction, SCALE_FLOOR_DBFS};
use crate::session::{Session, Tree};
use crate::theme;

/// A bar's width, in canvas units.
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
    let Some(bar) = bar_width(rect, ports.len()) else {
        return;
    };
    let gap = (rect.width() - bar * ports.len() as f32) / (ports.len() - 1).max(1) as f32;
    let tick = Stroke::new(1.0, visuals.strong_text_color());
    for (i, port) in ports.iter().enumerate() {
        let left = rect.left() + i as f32 * (bar + gap);
        let column = Rect::from_x_y_ranges(left..=left + bar, rect.y_range());
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
/// progress indicator labelled `"<address> level"`, or `"<address>.<output> level"` when the box
/// has more than one, whose value is its RMS level in dBFS.
pub fn describe(ctx: &egui::Context, rect: Rect, address: &str, ports: &[PortLevel]) {
    let Some(bar) = bar_width(rect, ports.len()) else {
        return;
    };
    let gap = (rect.width() - bar * ports.len() as f32) / (ports.len() - 1).max(1) as f32;
    for (i, port) in ports.iter().enumerate() {
        let label = match ports.len() {
            1 => format!("{address} level"),
            _ => format!("{address}.{} level", port.port),
        };
        let left = rect.left() + i as f32 * (bar + gap);
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
                x0: left.into(),
                y0: rect.top().into(),
                x1: (left + bar).into(),
                y1: rect.bottom().into(),
            });
        });
    }
}

/// The on-screen width of each of `bars` bars across `rect`, keeping the canvas's proportion of
/// bar to gap; `None` when there are none to draw.
fn bar_width(rect: Rect, bars: usize) -> Option<f32> {
    (bars > 0).then(|| rect.width() * BAR_WIDTH / width(bars))
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
