//! The engine verbs: audition a control gesture, ask whether the engine is there, open an
//! instrument, list what can be opened, read what is playing, read the counters.
//!
//! Each is act-then-map — the exchange itself reports a dead engine, so there is no separate probe
//! and no window between checking and acting. What a door still owns is its transport and how it
//! renders an [`Answer`] or a [`Refusal`]; the classification between them is decided here, once.
//!
//! The split is the same one the authoring half draws: a verb that *ran* and reported a problem is
//! an [`Answer`] (a rejected open, an `expect` conflict — the guard guarding), and only a verb that
//! could not do its job at all is a [`Refusal`]. [`get_engine_status`] cannot refuse, because
//! answering "is it there?" is its whole job.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::authoring::{Answer, DocumentView, Refusal};

use super::args::{ListInstruments, OpenInstrument, SendLiveControls};
use super::channel::{CallOutcome, Channel, ChannelError, SendOutcome, SwapOutcome};
use super::prose::ENGINE_UNREACHABLE_GUIDANCE;
use super::result::{
    CurrentInstrument, EngineStatus, InstrumentList, SendOutput, SidecarInfo, StatusEndpoints,
    SwapResult,
};
use super::wire::{
    over_long_batch_refusal, ChannelLevel, ControlArg, ControlMessage, DiagnosticsReport,
    DocumentSnapshot, ListWhat, MetersReport, SwapReport, EMPTY_BATCH_REFUSAL, MAX_SEND_BATCH,
};

/// Audition a batch of control values on the running engine.
///
/// The batch is converted **before** anything reaches the engine, so a bad argument is a refusal
/// the caller can act on even when the engine is down.
pub fn send_live_controls(
    args: &SendLiveControls,
    channel: &Channel,
) -> Result<Answer<SendOutput>, Refusal> {
    // Belt-and-braces against a client that skips schema validation: refusing here costs no round
    // trip. The server enforces both bounds too, in the same words — it cannot trust that a client
    // checked.
    if args.messages.is_empty() {
        return Err(Refusal::new(EMPTY_BATCH_REFUSAL));
    }
    if args.messages.len() > MAX_SEND_BATCH {
        return Err(Refusal::new(over_long_batch_refusal(args.messages.len())));
    }
    let mut messages = Vec::with_capacity(args.messages.len());
    for (i, message) in args.messages.iter().enumerate() {
        let converted = control_args(&message.args).map_err(|why| {
            Refusal::new(format!(
                "message {i} (`{}`) has an unsupported argument: {why}",
                message.address
            ))
        })?;
        messages.push(ControlMessage {
            address: message.address.clone(),
            args: converted,
        });
    }

    let sent = messages.len();
    match channel.send(messages) {
        Ok(SendOutcome::Queued) => Ok(Answer {
            output: SendOutput { sent },
            summary: format!("the engine queued {sent} control message(s)"),
        }),
        Ok(SendOutcome::Refused(refusal)) => {
            Err(Refusal::new(format!("{refusal}\n{}", refusal.marked())))
        }
        Err(why) => Err(refuse(
            "the engine is reachable but the control messages were not accepted",
            why,
        )),
    }
}

/// Is there an engine, and where? **Never a refusal**: a dead engine is the answer, and the
/// guidance beside it is what the caller acts on.
///
/// `door_version` is the caller's own version string — the one identity the window cannot know,
/// since it is the door that is running.
pub fn get_engine_status(channel: &Channel, door_version: &str) -> Answer<EngineStatus> {
    let reachable = channel.ping().is_ok();
    let output = EngineStatus {
        reachable,
        endpoints: StatusEndpoints {
            engine: channel.endpoint().to_string(),
        },
        sidecar: SidecarInfo {
            version: door_version.to_string(),
            format_version: reuben_document::format::FORMAT_VERSION,
        },
        guidance: if reachable {
            None
        } else {
            Some(ENGINE_UNREACHABLE_GUIDANCE.to_string())
        },
    };
    let summary = if reachable {
        format!("engine reachable on {}", output.endpoints.engine)
    } else {
        "engine not reachable — start `reuben play`".to_string()
    };
    Answer { output, summary }
}

/// Stop playing the current instrument and play the one `name` names, read through the engine's
/// store.
///
/// An `ok: false` load report, an `expect` conflict and a held open are all ordinary answers: the
/// channel worked, and the report is the deliverable.
pub fn open_instrument(
    args: &OpenInstrument,
    channel: &Channel,
) -> Result<Answer<SwapResult>, Refusal> {
    open_answer(channel.open(args.name.clone(), args.expect.clone()))
}

/// List the instrument or sample names the engine offers.
pub fn list_instruments(
    args: &ListInstruments,
    channel: &Channel,
) -> Result<Answer<InstrumentList>, Refusal> {
    let names = channel
        .list(args.what)
        .map_err(|why| refuse("the list could not be completed", why))?;
    let kind = match (args.what, names.len()) {
        (ListWhat::Instruments, 1) => "instrument",
        (ListWhat::Instruments, _) => "instruments",
        (ListWhat::Samples, 1) => "sample",
        (ListWhat::Samples, _) => "samples",
    };
    let summary = format!("{} {kind} offered", names.len());
    Ok(Answer {
        output: InstrumentList { names },
        summary,
    })
}

/// Map an open's channel outcome to the verb's answer.
fn open_answer(outcome: Result<SwapOutcome, ChannelError>) -> Result<Answer<SwapResult>, Refusal> {
    match outcome {
        Ok(SwapOutcome::Installed(report)) => {
            let summary = open_summary(&report);
            Ok(Answer {
                output: SwapResult::installed(report),
                summary,
            })
        }
        Ok(SwapOutcome::Conflict(conflict)) => {
            let playing = if conflict.actual.is_empty() {
                "nothing (the open instrument does not load)".to_string()
            } else {
                conflict.actual.clone()
            };
            let summary = format!(
                "open rejected by the expect guard: the engine is playing {playing}, not the \
                 expected {} — re-read with get_current_instrument and reconcile",
                conflict.expected
            );
            Ok(Answer {
                output: SwapResult::conflict(conflict),
                summary,
            })
        }
        Ok(SwapOutcome::Held(held)) => {
            let playing = held
                .playing
                .clone()
                .unwrap_or_else(|| format!("content_hash {}", held.content_hash));
            let summary = format!(
                "not installed yet: the engine is embedded in reuben-editor, whose user has been \
                 asked whether to open {requested}; {playing} keeps playing. Not a failure and not \
                 worth retrying — check get_current_instrument later: its source becomes \
                 {requested} if they accept",
                requested = held.requested
            );
            Ok(Answer {
                output: SwapResult::held(held),
                summary,
            })
        }
        Ok(SwapOutcome::Refused(message)) => Err(Refusal::new(message)),
        Err(why) => Err(refuse("the open could not be completed", why)),
    }
}

/// Run one non-engine roster verb on the engine's server, which owns the files it reads and writes.
/// The verb's own output comes back as JSON; a refusal carries the server's message verbatim.
pub fn call_tool<A: serde::Serialize>(
    tool: &str,
    args: &A,
    channel: &Channel,
) -> Result<Answer<serde_json::Value>, Refusal> {
    let args = serde_json::to_value(args)
        .map_err(|e| Refusal::new(format!("`{tool}` arguments could not be encoded: {e}")))?;
    match channel.call(tool, args) {
        Ok(CallOutcome::Called { output, summary }) => Ok(Answer { output, summary }),
        Ok(CallOutcome::Refused(message)) => Err(Refusal::new(message)),
        Err(why) => Err(refuse(&format!("`{tool}` could not be run"), why)),
    }
}

/// What is the engine playing? The source and hash the server names, and the server's
/// `describe_instrument` index of that file, so the caller needs no store of its own and never
/// holds the document. A projection that cannot be had is a note in its place, never an error: the
/// hash is the load-bearing half.
pub fn current_instrument(channel: &Channel) -> Result<Answer<CurrentInstrument>, Refusal> {
    let snapshot = channel
        .get_document()
        .map_err(|why| refuse("could not read the current instrument", why))?;
    let summary = playing_summary(&snapshot);
    let projection = match &snapshot.source {
        None => "(projection unavailable: no source to describe)".to_string(),
        Some(source) if snapshot.content_hash.is_empty() => {
            format!("(nothing plays: {source} does not load)")
        }
        Some(source) => {
            let args = serde_json::json!({ "source": source, "view": "index" });
            match call_tool("describe_instrument", &args, channel) {
                Ok(answer) => match serde_json::from_value::<DocumentView>(answer.output) {
                    Ok(view) => view.text,
                    Err(e) => format!("(projection unavailable: {e})"),
                },
                Err(refusal) => format!("(projection unavailable: {})", refusal.message),
            }
        }
    };
    Ok(Answer {
        output: CurrentInstrument {
            projection,
            source: snapshot.source,
            content_hash: snapshot.content_hash,
        },
        summary,
    })
}

/// What `get_current_instrument` says plays. An empty content hash is the server's word that
/// nothing plays: the open instrument does not load, or nothing is open.
fn playing_summary(snapshot: &DocumentSnapshot) -> String {
    match &snapshot.source {
        Some(source) if snapshot.content_hash.is_empty() => format!(
            "{source} is open but does not load, so nothing plays until an edit makes it load"
        ),
        Some(source) => format!("playing {source} (content_hash {})", snapshot.content_hash),
        None if snapshot.content_hash.is_empty() => "nothing is open, so nothing plays".to_string(),
        None => format!(
            "playing the built-in default rig, which has no file (content_hash {})",
            snapshot.content_hash
        ),
    }
}

/// Read the engine's running counters.
pub fn get_engine_diagnostics(channel: &Channel) -> Result<Answer<DiagnosticsReport>, Refusal> {
    let report = channel
        .get_diagnostics()
        .map_err(|why| refuse("could not read the engine diagnostics", why))?;
    let summary = format!(
        "output_xruns={} input_ring_underruns={} input_ring_overruns={} \
         input_ring_producer_drops={} control_batches_dropped_full={} wired_writes_dropped={} \
         retire_queue_dropped_full={} unprepared_writes_dropped={}",
        report.output_xruns,
        report.input_ring_underruns,
        report.input_ring_overruns,
        report.input_ring_producer_drops,
        report.control_batches_dropped_full,
        report.wired_writes_dropped,
        report.retire_queue_dropped_full,
        report.unprepared_writes_dropped
    );
    Ok(Answer {
        output: report,
        summary,
    })
}

/// Read the engine's levels at the device.
pub fn get_engine_meters(channel: &Channel) -> Result<Answer<MetersReport>, Refusal> {
    let mut report = channel
        .get_meters()
        .map_err(|why| refuse("could not read the engine meters", why))?;
    // The tool is device-only; node levels are for a client that chose them with `SetProbes`.
    report.probes.clear();
    let summary = format!(
        "peak/rms dBFS per channel; outputs: {}; inputs: {}",
        levels(&report.outputs),
        levels(&report.inputs)
    );
    Ok(Answer {
        output: report,
        summary,
    })
}

/// `-0.5/-3.5, -120/-120`, or `none` for a direction with no channels.
fn levels(channels: &[ChannelLevel]) -> String {
    if channels.is_empty() {
        return "none".to_string();
    }
    channels
        .iter()
        .map(|level| format!("{:.1}/{:.1}", level.peak_dbfs, level.rms_dbfs))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The one place a failed exchange becomes a refusal, so no two verbs classify it differently:
/// unreachable answers with the shared guidance, an unanswered request with its re-read guidance
/// (the request may have run, so `context`'s "could not" would be false), and anything else names
/// *which* call died.
fn refuse(context: &str, why: ChannelError) -> Refusal {
    match why {
        ChannelError::Unreachable(_) => Refusal::new(ENGINE_UNREACHABLE_GUIDANCE),
        ChannelError::NoAnswer(guidance) => Refusal::new(guidance),
        why => Refusal::new(format!("{context}: {why}")),
    }
}

/// Convert a `send` message's JSON args into the wire's flat control atoms.
///
/// Hand-written rather than typing the argument as the wire atom directly, so a non-scalar argument
/// is a refusal naming the offending value rather than a deserialization failure the caller cannot
/// act on.
fn control_args(args: &[serde_json::Value]) -> Result<Vec<ControlArg>, String> {
    args.iter()
        .map(|value| match value {
            serde_json::Value::String(s) => Ok(ControlArg::Str(s.clone())),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    if let Ok(i) = i32::try_from(i) {
                        return Ok(ControlArg::I32(i));
                    }
                }
                // Guard the f32 narrowing, not just the JSON type: a magnitude past f32 range
                // saturates to infinity, which serde_json writes as `null` — a value no control
                // atom accepts, so the engine would reject the whole batch with an opaque
                // "did not match any variant".
                match n.as_f64() {
                    Some(f) if (f as f32).is_finite() => Ok(ControlArg::F32(f as f32)),
                    Some(_) => Err(format!(
                        "number {value} is outside the range a control value can carry (f32)"
                    )),
                    None => Err(format!("number {value} is not a usable control value")),
                }
            }
            other => Err(format!(
                "{other} is not a control argument (expected a number or a string)"
            )),
        })
        .collect()
}

/// One-line gloss of an open's outcome: what installed (with diff counts) or why nothing did.
fn open_summary(report: &SwapReport) -> String {
    if report.report.ok {
        match &report.diff {
            Some(diff) => format!(
                "opened (content_hash {}): {} survived, {} state-reset, {} added, {} removed",
                report.content_hash,
                diff.survived,
                diff.state_reset.len(),
                diff.added.len(),
                diff.removed.len()
            ),
            None => format!("opened (content_hash {})", report.content_hash),
        }
    } else if report.content_hash.is_empty() {
        // Nothing plays. The server's warnings say which instrument is open and whether this
        // request opened it; the first error says what was refused.
        let first = report
            .report
            .errors
            .first()
            .map_or("", |error| error.message.as_str());
        let open = report
            .report
            .warnings
            .iter()
            .map(|warning| warning.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        let open = if open.is_empty() {
            "nothing plays".to_string()
        } else {
            open
        };
        format!(
            "open did not install: {} error(s), the first: {first} — {open}",
            report.report.errors.len(),
        )
    } else {
        format!(
            "open rejected: {} error(s), {} warning(s) — nothing installed; {} keeps playing",
            report.report.errors.len(),
            report.report.warnings.len(),
            report.content_hash
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::super::args::ControlSendMessage;
    use super::super::channel::Transport;
    use super::*;
    use std::io;
    use std::time::Duration;

    /// A transport with no engine behind it: every exchange fails the way a refused connect does.
    #[derive(Debug)]
    struct DeadEngine;

    impl Transport for DeadEngine {
        fn round_trip(&self, _line: &str, _read_timeout: Duration) -> io::Result<String> {
            Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "connection refused",
            ))
        }

        fn endpoint(&self) -> &str {
            "127.0.0.1:9124"
        }
    }

    fn dead() -> Channel {
        Channel::new(DeadEngine)
    }

    /// Every acting verb answers a dead engine the same way, with the one message that says what to
    /// do about it. The split is the window's now, so a second door cannot classify it differently
    /// — and a per-verb wording would be a per-verb opinion about whose fault it is.
    #[test]
    fn a_dead_engine_refuses_every_acting_verb_with_the_same_guidance() {
        let channel = dead();
        let refusals = [
            send_live_controls(
                &SendLiveControls {
                    messages: vec![ControlSendMessage {
                        address: "/filt/cutoff".to_string(),
                        args: vec![serde_json::json!(800.0)],
                    }],
                },
                &channel,
            )
            .expect_err("no engine to send to"),
            open_instrument(
                &OpenInstrument {
                    name: "inst.json".to_string(),
                    expect: None,
                },
                &channel,
            )
            .expect_err("no engine to install into"),
            current_instrument(&channel).expect_err("nothing is playing"),
            get_engine_diagnostics(&channel).expect_err("no counters to read"),
            get_engine_meters(&channel).expect_err("no levels to read"),
        ];
        for refusal in refusals {
            assert_eq!(refusal.message, ENGINE_UNREACHABLE_GUIDANCE);
        }
    }

    /// A transport whose engine took the request and never answered.
    #[derive(Debug)]
    struct SlowEngine;

    impl Transport for SlowEngine {
        fn round_trip(&self, _line: &str, _read_timeout: Duration) -> io::Result<String> {
            Err(super::super::channel::unanswered(io::Error::new(
                io::ErrorKind::WouldBlock,
                "read timed out",
            )))
        }

        fn endpoint(&self) -> &str {
            "127.0.0.1:9124"
        }
    }

    /// A slow answer is not a dead engine: telling the caller to start one would send it to retry
    /// a request the engine may already have applied.
    #[test]
    fn an_unanswered_request_says_it_may_have_been_applied() {
        let channel = Channel::new(SlowEngine);
        let refusals = [
            call_tool("add_instrument_node", &serde_json::json!({}), &channel)
                .expect_err("no answer"),
            open_instrument(
                &OpenInstrument {
                    name: "a.json".to_string(),
                    expect: None,
                },
                &channel,
            )
            .expect_err("no answer"),
        ];
        for refusal in refusals {
            assert_ne!(refusal.message, ENGINE_UNREACHABLE_GUIDANCE);
            assert!(
                !refusal.message.contains("could not"),
                "it may have been: {refusal}"
            );
            assert!(
                refusal.message.contains("may have been applied")
                    && refusal.message.contains("get_current_instrument")
                    && refusal.message.contains("describe_instrument"),
                "{refusal}"
            );
        }
    }

    /// The one verb that must not refuse: asking whether the engine is there is answerable when it
    /// is not, and the guidance rides in the payload rather than replacing it.
    #[test]
    fn engine_status_answers_a_dead_engine_instead_of_refusing_it() {
        let answer = get_engine_status(&dead(), "9.9.9");
        assert!(!answer.output.reachable);
        assert_eq!(
            answer.output.guidance.as_deref(),
            Some(ENGINE_UNREACHABLE_GUIDANCE)
        );
        assert_eq!(answer.output.endpoints.engine, "127.0.0.1:9124");
        // The door names itself; the window supplies the format it speaks.
        assert_eq!(answer.output.sidecar.version, "9.9.9");
        assert_eq!(
            answer.output.sidecar.format_version,
            crate::authoring::FORMAT_VERSION
        );
    }

    /// A batch the engine could never accept is refused before the engine is consulted, so the
    /// caller gets the argument's name rather than a dead-engine message that hides it.
    #[test]
    fn a_bad_control_argument_is_refused_before_the_engine_is_reached() {
        let refusal = send_live_controls(
            &SendLiveControls {
                messages: vec![ControlSendMessage {
                    address: "/filt/cutoff".to_string(),
                    args: vec![serde_json::json!({ "not": "a scalar" })],
                }],
            },
            &dead(),
        )
        .expect_err("an object is not a control argument");
        assert!(
            refusal.message.contains("/filt/cutoff"),
            "the refusal names the offending message: {refusal}"
        );
        assert_ne!(refusal.message, ENGINE_UNREACHABLE_GUIDANCE);
    }

    /// A transport that answers each request with the next scripted line and records what it was
    /// asked.
    #[derive(Debug)]
    struct Scripted {
        replies: std::sync::Mutex<alloc::collections::VecDeque<String>>,
        asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Transport for Scripted {
        fn round_trip(&self, line: &str, _read_timeout: Duration) -> io::Result<String> {
            self.asked.lock().unwrap().push(line.to_string());
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("a scripted reply"))
        }

        fn endpoint(&self) -> &str {
            "scripted"
        }
    }

    fn scripted(replies: &[&str]) -> (Channel, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let transport = Scripted {
            replies: std::sync::Mutex::new(replies.iter().map(|r| r.to_string()).collect()),
            asked: asked.clone(),
        };
        (Channel::new(transport), asked)
    }

    #[test]
    fn call_tool_unreachable_carries_the_start_reuben_play_guidance() {
        let refusal = call_tool("describe_instrument", &serde_json::json!({}), &dead())
            .expect_err("no engine to call");
        assert!(
            refusal.message.contains(ENGINE_UNREACHABLE_GUIDANCE),
            "{refusal}"
        );
    }

    #[test]
    fn current_instrument_with_no_source_answers_unavailable_not_error() {
        let (channel, asked) =
            scripted(&[r#"{"reply":"document","document":{},"content_hash":"h"}"#]);
        let answer = current_instrument(&channel).expect("an answer, not an error");
        assert_eq!(
            answer.output.projection,
            "(projection unavailable: no source to describe)"
        );
        assert_eq!(answer.output.content_hash, "h");
        assert_eq!(asked.lock().unwrap().len(), 1);
    }

    #[test]
    fn current_instrument_projects_through_describe() {
        let (channel, asked) = scripted(&[
            r#"{"reply":"document","document":{},"content_hash":"h","source":"a.json"}"#,
            r#"{"reply":"called","output":{"view":"index","text":"/osc sine"},"summary":"s"}"#,
        ]);
        let answer = current_instrument(&channel).expect("an answer");
        assert_eq!(answer.output.projection, "/osc sine");
        assert_eq!(answer.output.source.as_deref(), Some("a.json"));
        let asked = asked.lock().unwrap();
        let second = super::super::wire::Request::from_ndjson(&asked[1]).unwrap();
        assert_eq!(
            second,
            super::super::wire::Request::Call {
                tool: "describe_instrument".to_string(),
                args: serde_json::json!({"source": "a.json", "view": "index"}),
            }
        );
    }

    #[test]
    fn an_open_that_does_not_load_says_nothing_plays() {
        let (channel, _) = scripted(&[
            r#"{"reply":"swap_report","ok":false,"errors":[{"message":"bad"}],"warnings":[{"message":"b.json is open but does not load, so nothing plays until an edit makes it load"}],"content_hash":""}"#,
        ]);
        let answer = open_instrument(
            &OpenInstrument {
                name: "b.json".to_string(),
                expect: None,
            },
            &channel,
        )
        .expect("an answer, not a refusal");
        assert!(
            answer
                .summary
                .contains("b.json is open but does not load, so nothing plays until an edit"),
            "{}",
            answer.summary
        );
        assert!(
            !answer.summary.contains("keeps playing"),
            "{}",
            answer.summary
        );
    }

    #[test]
    fn a_refused_open_while_nothing_plays_names_the_refused_name_and_what_is_open() {
        let (channel, _) = scripted(&[
            r#"{"reply":"swap_report","ok":false,"errors":[{"message":"open missing.json: not found"}],"warnings":[{"message":"nothing was opened; b.json is still open but does not load, so nothing plays"}],"content_hash":""}"#,
        ]);
        let answer = open_instrument(
            &OpenInstrument {
                name: "missing.json".to_string(),
                expect: None,
            },
            &channel,
        )
        .expect("an answer");
        for expected in [
            "open missing.json: not found",
            "nothing was opened; b.json is still open",
        ] {
            assert!(
                answer.summary.contains(expected),
                "{expected:?} missing from {}",
                answer.summary
            );
        }
    }

    #[test]
    fn current_instrument_says_an_open_instrument_that_does_not_load_plays_nothing() {
        let (channel, asked) = scripted(&[
            r#"{"reply":"document","document":null,"content_hash":"","source":"b.json"}"#,
        ]);
        let answer = current_instrument(&channel).expect("an answer");
        assert_eq!(
            answer.summary,
            "b.json is open but does not load, so nothing plays until an edit makes it load"
        );
        assert_eq!(answer.output.content_hash, "");
        assert_eq!(
            answer.output.projection,
            "(nothing plays: b.json does not load)"
        );
        assert_eq!(
            asked.lock().unwrap().len(),
            1,
            "no describe while nothing plays"
        );
    }

    #[test]
    fn current_instrument_notes_a_refused_describe_in_the_projection() {
        let (channel, _) = scripted(&[
            r#"{"reply":"document","document":{},"content_hash":"h","source":"a.json"}"#,
            r#"{"reply":"refused","message":"nope"}"#,
        ]);
        let answer = current_instrument(&channel).expect("an answer");
        assert_eq!(answer.output.projection, "(projection unavailable: nope)");
    }

    #[test]
    fn open_instrument_relays_a_refusal() {
        let (channel, asked) = scripted(&[r#"{"reply":"refused","message":"no such instrument"}"#]);
        let refusal = open_instrument(
            &OpenInstrument {
                name: "nope".to_string(),
                expect: None,
            },
            &channel,
        )
        .expect_err("a refused open is a refusal");
        assert_eq!(refusal.message, "no such instrument");
        let asked = asked.lock().unwrap();
        assert_eq!(
            super::super::wire::Request::from_ndjson(&asked[0]).unwrap(),
            super::super::wire::Request::Open {
                name: "nope".to_string(),
                expect: None,
            }
        );
    }

    #[test]
    fn open_instrument_held_reads_as_not_yet_installed() {
        let (channel, _) = scripted(&[
            r#"{"reply":"held","requested":"groovebox","playing":"acid-techno","content_hash":"00c0ffee"}"#,
        ]);
        let answer = open_instrument(
            &OpenInstrument {
                name: "groovebox".to_string(),
                expect: None,
            },
            &channel,
        )
        .expect("held is an answer");
        let held = answer.output.held.as_ref().expect("held payload");
        assert_eq!(held.requested, "groovebox");
        assert!(!answer.output.report.report.ok, "nothing installed");
        for expected in [
            "not installed yet",
            "acid-techno keeps playing",
            "get_current_instrument",
        ] {
            assert!(
                answer.summary.contains(expected),
                "{expected:?} missing from {}",
                answer.summary
            );
        }
    }

    #[test]
    fn list_instruments_summarizes_the_count() {
        let (channel, _) = scripted(&[
            r#"{"reply":"listed","names":["a","b"]}"#,
            r#"{"reply":"listed","names":["kick"]}"#,
            r#"{"reply":"listed","names":["a"]}"#,
            r#"{"reply":"listed","names":["kick","snare"]}"#,
        ]);
        let instruments = list_instruments(
            &ListInstruments {
                what: ListWhat::Instruments,
            },
            &channel,
        )
        .expect("an answer");
        assert_eq!(instruments.output.names, ["a", "b"]);
        assert_eq!(instruments.summary, "2 instruments offered");
        let samples = list_instruments(
            &ListInstruments {
                what: ListWhat::Samples,
            },
            &channel,
        )
        .expect("an answer");
        assert_eq!(samples.summary, "1 sample offered");
        let one = list_instruments(
            &ListInstruments {
                what: ListWhat::Instruments,
            },
            &channel,
        )
        .expect("an answer");
        assert_eq!(one.summary, "1 instrument offered");
        let two = list_instruments(
            &ListInstruments {
                what: ListWhat::Samples,
            },
            &channel,
        )
        .expect("an answer");
        assert_eq!(two.summary, "2 samples offered");
    }

    fn open_summary(reply: &str) -> String {
        let (channel, _) = scripted(&[reply]);
        open_instrument(
            &OpenInstrument {
                name: "x".to_string(),
                expect: Some("e".to_string()),
            },
            &channel,
        )
        .expect("an answer")
        .summary
    }

    #[test]
    fn open_instrument_conflict_says_open_not_swap() {
        let summary = open_summary(r#"{"reply":"conflict","expected":"e","actual":"a"}"#);
        assert!(summary.starts_with("open rejected"), "{summary}");
        assert!(!summary.contains("swap"), "{summary}");
    }

    #[test]
    fn open_instrument_failed_report_says_open_not_swap() {
        let summary = open_summary(
            r#"{"reply":"swap_report","ok":false,"errors":[],"warnings":[],"content_hash":"h"}"#,
        );
        assert!(summary.starts_with("open rejected"), "{summary}");
        assert!(!summary.contains("swap"), "{summary}");
    }
}
