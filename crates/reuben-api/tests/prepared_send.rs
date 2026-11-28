//! A live `send` to inputs that compile their text, through the engine channel's own `send`
//! handler: the strings are compiled before the batch reaches the host, text that does not compile
//! refuses the whole batch, and a value compiled for a Plan a swap has since replaced is dropped,
//! counted and named to subscribers rather than applied.
//!
//! The host here stands in for `reuben play`'s: it keeps each delivered batch until the test drains
//! it into a [`RenderSlot`], deciding each batch with [`control_fate`] as native's render callback
//! does.
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
    control_fate, dispatch, ChannelState, ControlArg, ControlFate, ControlMessage,
    DiagnosticsReport, EngineHost, Event, IngressClosed, PreparedMessage, Request, Response,
    SendRefusal, NOT_ONE_STRING, REFUSAL_TEXT_WINDOW,
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

/// A host whose ingress holds each delivered batch until [`Host::drain`] takes it, as a render
/// callback would. Its reclaim gives up at once unless a test installs `on_poll`, which then
/// answers each poll.
#[derive(Default)]
struct Host {
    delivered: Mutex<Vec<Vec<PreparedMessage>>>,
    dropped: Mutex<Vec<String>>,
    stale: AtomicU64,
    #[allow(clippy::type_complexity)]
    on_poll: Mutex<Option<Box<dyn FnMut() -> bool + Send>>>,
}

/// What native's drain does with one batch, without recording addresses: queue it, or count it
/// dropped, or leave it. Allocation-free.
fn take(slot: &mut RenderSlot, batch: &[PreparedMessage], stale: &AtomicU64) -> ControlFate {
    let fate = control_fate(slot, batch.len(), batch.iter().map(|m| m.against.as_ref()));
    match fate {
        ControlFate::Queue => {
            for m in batch {
                slot.queue_osc(&m.address, &m.args);
            }
        }
        ControlFate::Drop => {
            stale.fetch_add(batch.len() as u64, Ordering::SeqCst);
        }
        ControlFate::Wait => {}
    }
    fate
}

impl Host {
    /// Take each delivered batch in order at `slot`, as native's drain does: stop at one that must
    /// wait, and record the addresses of one dropped. Returns the batches taken, so the caller
    /// frees them where it chooses.
    fn drain(&self, slot: &mut RenderSlot) -> Vec<Vec<PreparedMessage>> {
        slot.latch_install();
        let mut delivered = self.delivered.lock().unwrap();
        let mut taken = Vec::new();
        while let Some(batch) = delivered.first() {
            match take(slot, batch, &self.stale) {
                ControlFate::Wait => break,
                ControlFate::Drop => self
                    .dropped
                    .lock()
                    .unwrap()
                    .extend(batch.iter().map(|m| m.address.clone())),
                ControlFate::Queue => {}
            }
            taken.push(delivered.remove(0));
        }
        taken
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
        Box::new(|| match self.on_poll.lock().unwrap().as_mut() {
            Some(poll) => poll(),
            None => true,
        })
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
            offset: 0,
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
/// input, a string at an address with no route (an enum symbol), several atoms at one, an address
/// that names nothing.
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
            address: "/abs/x".to_string(),
            args: vec![ControlArg::Str("1".into()), ControlArg::I32(2)],
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
    let udp = [PreparedMessage {
        address: "/direct/steps".to_string(),
        args: vec![Arg::Str("5".into())],
        against: None,
    }];
    assert_eq!(take(&mut r.slot, &udp, &r.host.stale), ControlFate::Queue);
    r.slot.fill(&mut r.out);
    let pairs: Vec<_> = r
        .slot
        .drain_outbound()
        .map(|m| (m.address, m.arg.as_f32().expect("a number")))
        .collect();
    assert_eq!(sent(&pairs, "/d_sum"), Vec::<f64>::new());
    assert_eq!(r.slot.take_dropped_unprepared(), 1);
}

/// A send compiled before a swap and drained after it is dropped whole, not applied in part to the
/// new Plan; every message is counted in the diagnostics, and a subscriber hears every address. A
/// send after the swap compiles against the new Plan and lands.
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
    assert!(
        !sent(&after, "/abs_out").contains(&6.0),
        "a message with nothing compiled in the same batch is dropped with it"
    );
    assert_eq!(
        r.slot.take_dropped_unprepared(),
        0,
        "dropped before routing"
    );

    match dispatch(&r.state, &Request::GetDiagnostics.to_ndjson()) {
        Response::Diagnostics(d) => assert_eq!(d.stale_sends_dropped, 2),
        other => panic!("expected diagnostics, got {other:?}"),
    }
    let named = Event::SendsDropped {
        addresses: vec!["/abs/x".to_string(), "/direct/steps".to_string()],
    };
    assert_eq!(r.state.publish_dropped_sends(), Some(named.clone()));
    assert_eq!(events.recv_timeout(Duration::from_secs(1)), Ok(named));
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
            slot.latch_install();
            for batch in &batches {
                take(slot, batch, &host.stale);
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

/// Anything but one string at an address whose input compiles its text refuses the batch, naming
/// the message, where it would otherwise be acked and then dropped unapplied at the input.
#[test]
fn anything_but_one_string_at_a_compiling_input_refuses_the_batch() {
    let r = rig(RIG);
    for args in [
        vec![ControlArg::Str("4".into()), ControlArg::I32(2)],
        vec![ControlArg::F32(2.5)],
        vec![],
    ] {
        let answer = r.send(vec![
            str_message("/direct/steps", "1"),
            ControlMessage {
                address: "/pat/in".to_string(),
                args: args.clone(),
            },
        ]);
        assert_eq!(
            answer,
            Response::SendRefused(SendRefusal {
                message: 1,
                address: "/pat/in".to_string(),
                text: String::new(),
                offset: 0,
                start: 0,
                end: 0,
                error: NOT_ONE_STRING.to_string(),
            }),
            "{args:?}"
        );
    }
    assert_eq!(r.host.delivered(), 0);
}

/// A refusal echoes at most a window of the text around the fault, so a request that fits the
/// channel's line cap never draws an answer that does not.
#[test]
fn a_refusal_of_a_near_cap_request_fits_under_the_line_cap() {
    use reuben_api::engine::MAX_LINE_BYTES;
    let r = rig(RIG);
    let request = |n: usize| Request::Send {
        messages: vec![str_message("/w/text", &format!("{}1", "a".repeat(n)))],
    };
    let base = request(0).to_ndjson().len();
    let line = request(MAX_LINE_BYTES - base - 1).to_ndjson();
    assert!(line.len() <= MAX_LINE_BYTES);
    let Response::SendRefused(refusal) = dispatch(&r.state, &line) else {
        panic!("refused");
    };
    assert!(refusal.text.len() <= REFUSAL_TEXT_WINDOW);
    assert_eq!(&refusal.text[refusal.start..refusal.end], "1");
    assert_eq!(
        refusal.offset + refusal.start,
        MAX_LINE_BYTES - base - 1,
        "offset and start locate the fault in the text sent"
    );
    assert!(Response::SendRefused(refusal).to_ndjson().len() < 2 * REFUSAL_TEXT_WINDOW);
}

/// A send compiled for the playing Plan and drained while a swap ramps down toward its install
/// waits for the installed Engine rather than being queued on the retiring one, where it would
/// leave unrendered with nothing said: once the swap installs it meets the new Plan, and is
/// dropped, counted and named.
#[test]
fn a_send_drained_during_the_install_ramp_waits_then_is_dropped_and_named() {
    let mut r = rig(RIG);
    r.block();
    let events = r.state.subscribe();
    let channels = r.slot.channels();
    let edge = r.slot.ramp_edge_frames();
    assert_ne!(
        (edge - 1) % BLOCK,
        0,
        "the old Engine has unread frames at the install"
    );

    assert_eq!(
        r.send(vec![str_message("/direct/steps", "50")]),
        Response::Sent
    );
    let open = Request::Open {
        name: "other".to_string(),
        expect: None,
    };
    assert!(matches!(
        dispatch(&r.state, &open.to_ndjson()),
        Response::SwapReport(ref report) if report.report.ok
    ));
    // All but one frame of the down-ramp: the old Engine renders its last block.
    let mut ramp = vec![0.0; (edge - 1) * channels];
    r.slot.fill(&mut ramp);
    assert!(r.slot.install_pending());
    assert!(r.host.drain(&mut r.slot).is_empty(), "the batch waits");
    // The rest of the old Engine's block: the install waits at zero for its boundary.
    let mut rest = vec![0.0; (edge.next_multiple_of(BLOCK) - (edge - 1)) * channels];
    r.slot.fill(&mut rest);
    assert!(!r.slot.install_pending(), "installed");
    drop(r.slot.drain_outbound());

    let after = r.block();
    assert_eq!(sent(&after, "/d_sum"), Vec::<f64>::new());
    assert_eq!(r.host.diagnostics().stale_sends_dropped, 1);
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)),
        Ok(Event::Opened { .. })
    ));
    r.state.publish_dropped_sends();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(1)),
        Ok(Event::SendsDropped {
            addresses: vec!["/direct/steps".to_string()],
        })
    );
}

/// A send made while a swap's reclaim is still waiting for the render side compiles for the Plan
/// being installed, waits out the ramp, and lands on it: the routes a send reads advance at the
/// commit, not after the reclaim.
#[test]
fn a_send_during_a_reclaim_that_succeeds_lands_on_the_new_plan() {
    let Rig {
        state,
        host,
        slot,
        mut out,
    } = rig(RIG);
    let slot = Arc::new(Mutex::new(slot));
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let render = {
        let (host, slot, outputs) = (Arc::clone(&host), Arc::clone(&slot), Arc::clone(&outputs));
        let mut buf = out.clone();
        move || {
            let mut slot = slot.lock().unwrap();
            drop(host.drain(&mut slot));
            slot.fill(&mut buf);
            let pairs: Vec<_> = slot
                .drain_outbound()
                .map(|m| (m.address, m.arg.as_f32().expect("a number")))
                .collect();
            outputs.lock().unwrap().extend(pairs);
        }
    };
    let mut render_in_poll = render.clone();
    let sender = state.clone();
    let mut polls = 0;
    *host.on_poll.lock().unwrap() = Some(Box::new(move || {
        polls += 1;
        if polls == 1 {
            let send = Request::Send {
                messages: vec![str_message("/direct/steps", "33")],
            };
            assert_eq!(dispatch(&sender, &send.to_ndjson()), Response::Sent);
        }
        render_in_poll();
        polls > 200
    }));
    {
        let mut slot = slot.lock().unwrap();
        slot.fill(&mut out);
        drop(slot.drain_outbound());
    }
    let open = Request::Open {
        name: "other".to_string(),
        expect: None,
    };
    assert!(matches!(
        dispatch(&state, &open.to_ndjson()),
        Response::SwapReport(ref report) if report.report.ok
    ));
    assert!(
        !slot.lock().unwrap().install_pending(),
        "the reclaim succeeded"
    );
    let mut render = render;
    for _ in 0..4 {
        render();
    }
    assert_eq!(host.stale.load(Ordering::SeqCst), 0, "not dropped");
    assert!(
        sent(&outputs.lock().unwrap(), "/d_sum").contains(&33.0),
        "applied to the new Plan"
    );
    host.on_poll.lock().unwrap().take();
}

/// A swap committed after the drain looked and before the same callback's fill does not begin its
/// ramp in that fill, even when the block is longer than a ramp edge and could ramp, install and
/// retire the Engine in one go: the batch the drain queued renders on the Engine it was queued on.
/// Whatever the timing, a send is applied audibly or dropped, counted and named, never lost.
#[test]
fn a_swap_committed_between_drain_and_fill_waits_for_the_next_callback() {
    let mut r = rig(RIG);
    r.block();
    let events = r.state.subscribe();
    let channels = r.slot.channels();
    let edge = r.slot.ramp_edge_frames();
    assert_eq!(
        r.send(vec![str_message("/direct/steps", "50")]),
        Response::Sent
    );
    // The render thread drains first, as the device callback does.
    assert_eq!(
        r.host.drain(&mut r.slot).len(),
        1,
        "queued on the live Engine"
    );
    // The open commits on the Coordinator's thread before the same callback's fill.
    let open = Request::Open {
        name: "other".to_string(),
        expect: None,
    };
    assert!(matches!(
        dispatch(&r.state, &open.to_ndjson()),
        Response::SwapReport(ref report) if report.report.ok
    ));
    let mut long = vec![0.0; (edge + 32) * channels];
    r.slot.fill(&mut long);
    assert!(
        r.slot.install_pending(),
        "the fill did not begin a ramp the drain did not see"
    );
    let first: Vec<_> = r
        .slot
        .drain_outbound()
        .map(|m| (m.address, m.arg.as_f32().expect("a number")))
        .collect();
    let mut seen = sent(&first, "/d_sum");
    for _ in 0..8 {
        seen.extend(sent(&r.block(), "/d_sum"));
    }
    assert!(!r.slot.install_pending(), "the next callbacks install it");
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)),
        Ok(Event::Opened { .. })
    ));
    let named = r.state.publish_dropped_sends();
    let dropped = r.host.diagnostics().stale_sends_dropped;
    assert!(
        seen.contains(&50.0)
            || (dropped == 1
                && named
                    == Some(Event::SendsDropped {
                        addresses: vec!["/direct/steps".to_string()],
                    })),
        "lost: rendered {seen:?}, dropped {dropped}, named {named:?}"
    );
}

/// A send compiled after a commit, for the Plan it installs, and drained in a callback whose latch
/// saw no swap waits for that Plan instead of being dropped as stale, then lands on it.
#[test]
fn a_send_for_a_plan_committed_after_the_latch_waits_for_it() {
    let mut r = rig(RIG);
    r.block();
    assert!(!r.slot.latch_install());
    let open = Request::Open {
        name: "other".to_string(),
        expect: None,
    };
    assert!(matches!(
        dispatch(&r.state, &open.to_ndjson()),
        Response::SwapReport(ref report) if report.report.ok
    ));
    assert_eq!(
        r.send(vec![str_message("/direct/steps", "77")]),
        Response::Sent
    );
    let batch = r.host.delivered.lock().unwrap()[0].clone();
    assert_eq!(take(&mut r.slot, &batch, &r.host.stale), ControlFate::Wait);
    let mut seen = Vec::new();
    for _ in 0..16 {
        seen.extend(sent(&r.block(), "/d_sum"));
    }
    assert!(!r.slot.install_pending());
    assert_eq!(r.host.stale.load(Ordering::SeqCst), 0, "not dropped");
    assert!(seen.contains(&77.0), "landed on the new Plan: {seen:?}");
}
