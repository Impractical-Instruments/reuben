//! Keeping the router's table in step with what `play` plays: rebuilt from the open instrument's
//! interface inputs and its MIDI map whenever either changes.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use reuben_api::authoring::{Boundary, DescribeBoundary};
use reuben_api::engine::{call_tool, Channel};
use reuben_api::tools::names;

use super::router::SharedRouter;
use super::routes::{build_routes, RouteTable};
use crate::engine_channel::locate_surface;
use crate::surface::SurfaceDoc;

/// How often the watch asks what plays and looks at the map file: the most a swap or a map edit
/// waits to reach the router. Each tick is one in-process `get_document` and one `stat`.
const POLL: Duration = Duration::from_millis(250);

/// What a rebuild is a function of: the instrument that plays, by source and content hash, and
/// the MIDI map file, by path and modification stamp.
#[derive(Debug, Clone, PartialEq)]
struct Inputs {
    source: Option<String>,
    content_hash: String,
    map: Option<(PathBuf, Option<(SystemTime, u64)>)>,
}

/// A thread that rebuilds the router's table at start, after every swap, and (with
/// `follow_files`) after every change to the MIDI map file. It polls rather than subscribing, so
/// it stops promptly without a subscription to wake. Dropping it stops and joins the thread.
pub(crate) struct RouteWatch {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl RouteWatch {
    pub(crate) fn spawn(
        channel: Channel,
        router: SharedRouter,
        follow_files: bool,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("midi-routes".to_string())
            .spawn(move || {
                let mut last: Option<Inputs> = None;
                while !thread_stop.load(Ordering::SeqCst) {
                    if let Some(now) = inputs(&channel, follow_files) {
                        if last.as_ref() != Some(&now) {
                            rebuild(&channel, &router, &now);
                            last = Some(now);
                        }
                    }
                    std::thread::park_timeout(POLL);
                }
            })?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for RouteWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

/// `None` when the engine does not answer (it is shutting down): nothing to rebuild against.
fn inputs(channel: &Channel, follow_files: bool) -> Option<Inputs> {
    let document = channel.get_document().ok()?;
    let map = document
        .source
        .as_deref()
        .and_then(|source| locate_surface(Path::new(source), ".midi"))
        .map(|path| {
            // Without file following, a map is read when the instrument changes, not when it does.
            let stamp = follow_files.then(|| stamp(&path)).flatten();
            (path, stamp)
        });
    Some(Inputs {
        source: document.source,
        content_hash: document.content_hash,
        map,
    })
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Installs the table `now` describes. A map that exists and cannot be used keeps the table that
/// was working, as an instrument that stops loading keeps playing; anything else that leaves no
/// map (none beside the instrument, nothing open, an instrument that does not load) installs an
/// empty one, releasing every held note.
fn rebuild(channel: &Channel, router: &SharedRouter, now: &Inputs) {
    let install = |table: RouteTable| {
        router
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .install(table)
    };
    let (Some(source), Some((path, _))) = (&now.source, &now.map) else {
        install(RouteTable::default());
        return;
    };
    let shown = path.display();
    let doc = match std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|text| SurfaceDoc::parse(&text).map_err(|e| e.to_string()))
    {
        Ok(doc) => doc,
        Err(why) => {
            eprintln!("midi map: {shown} {why}; keeping the routes it had");
            return;
        }
    };
    let boundary = match boundary(channel, source) {
        Ok(boundary) => boundary,
        Err(why) => {
            eprintln!("midi map: {shown} not used: {why}");
            install(RouteTable::default());
            return;
        }
    };
    let (table, warnings) = build_routes(&doc, &boundary);
    for warning in warnings {
        eprintln!("midi map: {shown}: {warning}");
    }
    install(table);
}

/// The interface inputs of the instrument at `source`, as the editor reads them.
fn boundary(channel: &Channel, source: &str) -> Result<Boundary, String> {
    let args = DescribeBoundary {
        source: source.to_string(),
    };
    let answer = call_tool(names::DESCRIBE_BOUNDARY, &args, channel).map_err(|r| r.message)?;
    serde_json::from_value(answer.output).map_err(|e| e.to_string())
}
