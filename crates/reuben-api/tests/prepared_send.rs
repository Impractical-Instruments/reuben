//! A live `send` to inputs that compile their text, through the engine channel's own `send`
//! handler: the strings are compiled before the batch reaches the host, text that does not compile
//! refuses the whole batch, and a value compiled for a Plan a swap has since replaced is dropped,
//! counted and named to subscribers rather than applied.
//!
//! The host here stands in for `reuben play`'s: it keeps each delivered batch until the test drains
//! it into a [`RenderSlot`] through [`queue_control`], the call native's render callback makes.
//! No built-in Operator compiles its text, so `steps` and `words` (`prepared/mod.rs`) are
//! registered here.

#[path = "../../reuben-document/tests/prepared/mod.rs"]
mod prepared;
#[path = "../../reuben-document/tests/rt_alloc/mod.rs"]
mod rt_alloc;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reuben_api::authoring::{DocumentHistory, ResolveError, Resources, SampleBuffer};
use reuben_api::engine::{
    dispatch, queue_control, ChannelState, ControlArg, ControlMessage, DiagnosticsReport,
    EngineHost, Event, IngressClosed, PreparedMessage, Request, Response, SendRefusal,
};
use reuben_api::render::{Arg, AudioConfig, RenderSlot};
use reuben_api::staged::Staged;
use reuben_core::signal::AudioSample;
use rt_alloc::{assert_counter_is_live, measure, Counting};

#[global_allocator]
static GLOBAL: Counting = Counting;

const BLOCK: usize = 128;

/// A kept `str` pipe fanning out to two `steps` inputs, an unwired `steps` input, an unwired
/// `words` input, and a number input, each total reported through an `osc_out` sink.
const RIG: &str = r#"{ "format_version": 3, "instrument": "live",
  "interface": { "inputs": { "pat": { "type": "str", "default": "1" } } },
  "nodes": [
    { "type": "steps", "address": "/a", "inputs": { "steps": { "from": "/pat" } } },
    { "type": "steps", "address": "/b", "inputs": { "steps": { "from": "/pat" } } },
    { "type": "steps", "address": "/direct", "inputs": { "steps": "3" } },
    { "type": "words", "address": "/w", "inputs": { "text": "x" } },
    { "type": "abs_i32_value", "address": "/abs" },
    { "type": "osc_out", "address": "/a_sum", "inputs": { "in": { "from": "/a.total" } } },
    { "type": "osc_out", "address": "/b_sum", "inputs": { "in": { "from": "/b.total" } } },
    { "type": "osc_out", "address": "/d_sum", "inputs": { "in": { "from": "/direct.total" } } },
    { "type": "osc_out", "address": "/w_n", "inputs": { "in": { "from": "/w.count" } } },
    { "type": "osc_out", "address": "/abs_out", "inputs": { "in": { "from": "/abs.out" } } } ] }"#;

/// Documents an `open` reads, by name.
struct Texts(BTreeMap<String, String>);

impl Resources for Texts {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        self.0
            .get(source)
            .cloned()
            .ok_or_else(|| ResolveError::NotFound(source.to_string()))
    }
}

/// A host whose ingress holds each delivered batch until [`Host::drain`] queues it, as a render
/// callback would, and whose reclaim gives up at once: the test drives the render side itself.
#[derive(Default)]
struct Host {
    delivered: Mutex<Vec<Vec<PreparedMessage>>>,
    dropped: Mutex<Vec<String>>,
    stale: AtomicU64,
}

impl Host {
    /// Queue every delivered batch at `slot` through [`queue_control`], recording what it drops.
    /// Returns the batches, so the caller frees them where it chooses.
    fn drain(&self, slot: &mut RenderSlot) -> Vec<Vec<PreparedMessage>> {
        let batches = std::mem::take(&mut *self.delivered.lock().unwrap());
        let mut dropped = self.dropped.lock().unwrap();
        for m in batches.iter().flatten() {
            if !queue_control(slot, &m.address, &m.args, m.against.as_ref()) {
                dropped.push(m.address.clone());
                self.stale.fetch_add(1, Ordering::SeqCst);
            }
        }
        batches
    }

    fn delivered(&self) -> usize {
        self.delivered.lock().unwrap().len()
    }
}

impl EngineHost for Host {
    fn deliver_control(&self, messages: Vec<PreparedMessage>) -> Result<(), IngressClosed> {
        self.delivered.lock().unwrap().push(messages);
        Ok(())
    }

    fn take_dropped_sends(&self) -> Vec<String> {
        let mut dropped = std::mem::take(&mut *self.dropped.lock().unwrap());
        dropped.sort();
        dropped.dedup();
        dropped
    }

    fn diagnostics(&self) -> DiagnosticsReport {
        DiagnosticsReport {
            stale_sends_dropped: self.stale.load(Ordering::SeqCst),
            ..DiagnosticsReport::default()
        }
    }

    fn publish_render_config(&self, _: usize, _: usize) -> Vec<reuben_api::authoring::Diag> {
        Vec::new()
    }

    fn retire_poll(&self) -> Box<dyn FnMut() -> bool + Send + '_> {
        Box::new(|| true)
    }
}

struct Rig {
    state: ChannelState,
    host: Arc<Host>,
    slot: RenderSlot,
    out: Vec<AudioSample>,
}

/// [`RIG`] with no `osc_out` sinks, which allocate to send: the rig the heap is measured on.
const QUIET_RIG: &str = r#"{ "format_version": 3, "instrument": "quiet",
  "interface": { "inputs": { "pat": { "type": "str", "default": "1" } } },
  "nodes": [
    { "type": "steps", "address": "/a", "inputs": { "steps": { "from": "/pat" } } },
    { "type": "steps", "address": "/b", "inputs": { "steps": { "from": "/pat" } } },
    { "type": "steps", "address": "/direct", "inputs": { "steps": "3" } } ] }"#;

/// [`RIG`] installed, with `other` offered to `open` under that name.
fn rig(other: &str) -> Rig {
    rig_of(RIG, other)
}

/// `installed` installed, with `other` offered to `open` under that name.
fn rig_of(installed: &str, other: &str) -> Rig {
    let (coordinator, side, _) = reuben_api::engine::Coordinator::install_initial(
        installed,
        prepared::registry(),
        Box::new(reuben_document::MemoryResolver::new()),
        AudioConfig::new(48_000.0, BLOCK),
    )
    .expect("initial install");
    let texts = Texts(BTreeMap::from([("other".to_string(), other.to_string())]));
    let store = Arc::new(DocumentHistory::new(Staged::new(
        Box::new(texts) as Box<dyn Resources + Send + Sync>
    )));
    let host = Arc::new(Host::default());
    let state = ChannelState::new(coordinator, Arc::clone(&host) as Arc<dyn EngineHost>, store);
    let slot = RenderSlot::new(side);
    let out = vec![0.0f32; BLOCK * slot.channels()];
    Rig {
        state,
        host,
        slot,
        out,
    }
}

impl Rig {
    fn send(&self, messages: Vec<ControlMessage>) -> Response {
        dispatch(&self.state, &Request::Send { messages }.to_ndjson())
    }

    /// Drain what was delivered and render one block; the `(sink, value)` pairs it sent.
    fn block(&mut self) -> Vec<(String, f32)> {
        drop(self.host.drain(&mut self.slot));
        self.slot.fill(&mut self.out);
        self.slot
            .drain_outbound()
            .map(|m| (m.address, m.arg.as_f32().expect("a number")))
            .collect()
    }

    /// Open `other`, then render until the render side has installed it.
    fn swap(&mut self) {
        match dispatch(
            &self.state,
            &Request::Open {
                name: "other".to_string(),
                expect: None,
            }
            .to_ndjson(),
        ) {
            Response::SwapReport(report) => assert!(report.report.ok, "{report:?}"),
            other => panic!("expected a swap report, got {other:?}"),
        }
        self.render_until_installed();
    }

    fn render_until_installed(&mut self) {
        for _ in 0..32 {
            self.slot.fill(&mut self.out);
        }
        assert!(!self.slot.is_ramping(), "the swap installed");
        drop(self.slot.drain_outbound());
    }
}

fn str_message(address: &str, text: &str) -> ControlMessage {
    ControlMessage {
        address: address.to_string(),
        args: vec![ControlArg::Str(text.to_string())],
    }
}

/// The values `sink` sent, widened so they read as control values rather than audio.
fn sent(pairs: &[(String, f32)], sink: &str) -> Vec<f64> {
    pairs
        .iter()
        .filter(|(a, _)| a == sink)
        .map(|(_, v)| f64::from(*v))
        .collect()
}

#[test]
fn a_good_send_arrives_prepared_and_is_applied() {
    let mut r = rig(RIG);
    let first = r.block();
    assert_eq!(
        sent(&first, "/d_sum"),
        vec![3.0],
        "the document default plays"
    );

    assert_eq!(
        r.send(vec![str_message("/direct/steps", "4 5")]),
        Response::Sent
    );
    let batch = r.host.delivered.lock().unwrap()[0].clone();
    let [m] = <[PreparedMessage; 1]>::try_from(batch).expect("one message");
    match m.args.as_slice() {
        [Arg::Prepared(p)] => {
            assert_eq!(p.kind(), prepared::steps_preparer().kind);
            assert_eq!(p.source(), "4 5");
        }
        other => panic!("not prepared: {other:?}"),
    }
    assert!(m.against.is_some(), "stamped with the Plan it compiled for");

    let live = r.block();
    assert_eq!(sent(&live, "/d_sum"), vec![9.0]);
    assert_eq!(r.slot.take_dropped_unprepared(), 0);
}

/// A kept `str` interface pipe in front of two preparing inputs takes their preparer, so a send to
/// it is compiled with that preparer and forwarded compiled to both.
#[test]
fn a_send_to_a_kept_str_pipe_arrives_prepared_at_every_input_it_feeds() {
    let mut r = rig(RIG);
    let first = r.block();
    assert_eq!(
        (sent(&first, "/a_sum"), sent(&first, "/b_sum")),
        (vec![1.0], vec![1.0])
    );
    assert_eq!(r.send(vec![str_message("/pat/in", "2 2")]), Response::Sent);
    let live = r.block();
    assert_eq!(
        (sent(&live, "/a_sum"), sent(&live, "/b_sum")),
        (vec![4.0], vec![4.0])
    );
    assert_eq!(r.slot.take_dropped_unprepared(), 0);
}

/// Text that does not compile refuses the batch with its span; the good messages beside it, before
/// and after, are not applied either, and nothing reaches the host.
#[test]
fn a_bad_value_refuses_the_whole_batch_with_its_span() {
    let mut r = rig(RIG);
    r.block();
    let answer = r.send(vec![
        str_message("/direct/steps", "7"),
        ControlMessage {
            address: "/abs/x".to_string(),
            args: vec![ControlArg::I32(-5)],
        },
        str_message("/w/text", "ab c3d"),
        str_message("/pat/in", "8"),
    ]);
    assert_eq!(
        answer,
        Response::SendRefused(SendRefusal {
            message: 2,
            address: "/w/text".to_string(),
            text: "ab c3d".to_string(),
            start: 4,
            end: 5,
            error: "a word has no digits".to_string(),
        })
    );
    assert_eq!(r.host.delivered(), 0, "nothing was delivered");
    let after = r.block();
    for sink in ["/d_sum", "/w_n", "/a_sum", "/b_sum"] {
        assert_eq!(sent(&after, sink), Vec::<f64>::new(), "{sink} changed");
    }
    assert!(!sent(&after, "/abs_out").contains(&5.0), "/abs/x changed");
}

/// What prepare leaves alone reaches the host exactly as it did before: a number at a number
/// input, a string at an address with no route (an enum symbol), several atoms, an address that
/// names nothing.
#[test]
fn sends_that_compile_nothing_pass_through_unchanged() {
    let r = rig(RIG);
    let messages = vec![
        ControlMessage {
            address: "/abs/x".to_string(),
            args: vec![ControlArg::I32(-5)],
        },
        str_message("/nowhere/at", "1 x"),
        ControlMessage {
            address: "/direct/steps".to_string(),
            args: vec![ControlArg::Str("1".into()), ControlArg::I32(2)],
        },
        ControlMessage {
            address: "/direct/steps".to_string(),
            args: vec![ControlArg::F32(2.5)],
        },
    ];
    let expected: Vec<Vec<Arg>> = messages
        .iter()
        .map(|m| m.args.iter().cloned().map(Arg::from).collect())
        .collect();
    assert_eq!(r.send(messages), Response::Sent);
    let batch = r.host.delivered.lock().unwrap()[0].clone();
    assert_eq!(
        batch.iter().map(|m| m.args.clone()).collect::<Vec<_>>(),
        expected
    );
    assert!(batch.iter().all(|m| m.against.is_none()));
}

/// Raw text that reaches a preparing input without passing through `send` (an OSC datagram, which
/// queues with no Plan named) is dropped at the input and counted, as before.
#[test]
fn raw_text_from_udp_is_still_dropped_and_counted() {
    let mut r = rig(RIG);
    r.block();
    assert!(queue_control(
        &mut r.slot,
        "/direct/steps",
        &[Arg::Str("5".into())],
        None
    ));
    r.slot.fill(&mut r.out);
    let pairs: Vec<_> = r
        .slot
        .drain_outbound()
        .map(|m| (m.address, m.arg.as_f32().expect("a number")))
        .collect();
    assert_eq!(sent(&pairs, "/d_sum"), Vec::<f64>::new());
    assert_eq!(r.slot.take_dropped_unprepared(), 1);
}

/// A send compiled before a swap and drained after it is dropped, not applied to the new Plan;
/// it is counted in the diagnostics, and a subscriber hears its address. A send after the swap
/// compiles against the new Plan and lands.
#[test]
fn a_swap_between_prepare_and_apply_drops_counts_and_names_the_send() {
    // The same document again: the address still reaches a `steps` input of the same kind, the
    // case the kind check alone would wrongly let through.
    let mut r = rig(RIG);
    r.block();
    let events = r.state.subscribe();

    assert_eq!(
        r.send(vec![
            str_message("/direct/steps", "50"),
            ControlMessage {
                address: "/abs/x".to_string(),
                args: vec![ControlArg::I32(-6)],
            },
        ]),
        Response::Sent
    );
    r.swap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)),
        Ok(Event::Opened { .. })
    ));

    let after = r.block();
    assert_eq!(
        sent(&after, "/d_sum"),
        Vec::<f64>::new(),
        "the stale value was not applied"
    );
    assert_eq!(
        sent(&after, "/abs_out"),
        vec![6.0],
        "a message with nothing compiled in the same batch is applied as before"
    );
    assert_eq!(
        r.slot.take_dropped_unprepared(),
        0,
        "dropped before routing"
    );

    match dispatch(&r.state, &Request::GetDiagnostics.to_ndjson()) {
        Response::Diagnostics(d) => assert_eq!(d.stale_sends_dropped, 1),
        other => panic!("expected diagnostics, got {other:?}"),
    }
    assert_eq!(
        r.state.publish_dropped_sends(),
        Some(Event::SendsDropped {
            addresses: vec!["/direct/steps".to_string()],
        })
    );
    assert_eq!(
        events.recv_timeout(Duration::from_secs(1)),
        Ok(Event::SendsDropped {
            addresses: vec!["/direct/steps".to_string()],
        })
    );
    assert_eq!(r.state.publish_dropped_sends(), None, "named once");

    assert_eq!(
        r.send(vec![str_message("/direct/steps", "20")]),
        Response::Sent
    );
    assert_eq!(sent(&r.block(), "/d_sum"), vec![20.0]);
}

/// A send compiled against the Plan that plays and drained before a pending swap installs is
/// applied to that Plan: only crossing the install drops it.
#[test]
fn a_send_drained_before_the_install_is_applied() {
    let mut r = rig(RIG);
    r.block();
    assert_eq!(
        r.send(vec![str_message("/direct/steps", "11")]),
        Response::Sent
    );
    let applied = r.block();
    assert_eq!(sent(&applied, "/d_sum"), vec![11.0]);
    r.swap();
    assert_eq!(r.host.diagnostics().stale_sends_dropped, 0);
}

/// The render side of a send, from the queue through the block that applies it, touches no heap:
/// in steady state, for the first send after a swap, and for a send the swap made stale.
#[test]
fn prepared_sends_through_the_send_path_touch_no_heap_on_the_render_thread() {
    let mut r = rig_of(QUIET_RIG, QUIET_RIG);
    let mut texts = (0..).map(|i| format!("{i} {i}"));
    let measured = |r: &mut Rig, what: &str| {
        let batches = std::mem::take(&mut *r.host.delivered.lock().unwrap());
        let host = Arc::clone(&r.host);
        let (slot, out) = (&mut r.slot, &mut r.out);
        let c = measure(|| {
            for m in batches.iter().flatten() {
                if !queue_control(slot, &m.address, &m.args, m.against.as_ref()) {
                    host.stale.fetch_add(1, Ordering::SeqCst);
                }
            }
            slot.fill(out);
        });
        drop(batches);
        drop(r.slot.drain_outbound());
        assert_eq!((c.allocs, c.frees), (0, 0), "{what}");
    };
    for _ in 0..4 {
        r.send(vec![
            str_message("/direct/steps", &texts.next().unwrap()),
            str_message("/pat/in", &texts.next().unwrap()),
        ]);
        r.block();
    }
    // Each `send` drains the retire queue before it delivers, which is where the values the
    // render thread replaced are freed.
    assert_counter_is_live();
    for i in 0..8 {
        r.send(vec![
            str_message("/direct/steps", &texts.next().unwrap()),
            str_message("/pat/in", &texts.next().unwrap()),
        ]);
        measured(&mut r, &format!("steady send {i}"));
    }

    r.send(vec![str_message("/direct/steps", "9")]);
    r.swap();
    measured(&mut r, "a send the swap made stale");
    assert_eq!(r.host.stale.load(Ordering::SeqCst), 1);

    for i in 0..4 {
        r.send(vec![
            str_message("/direct/steps", &texts.next().unwrap()),
            str_message("/pat/in", &texts.next().unwrap()),
        ]);
        measured(&mut r, &format!("send {i} after the swap"));
    }
}
