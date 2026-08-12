//! The five engine verbs: audition a control gesture, ask whether the engine is there, install a
//! document, read what is playing, read the counters.
//!
//! Each is act-then-map — the exchange itself reports a dead engine, so there is no separate probe
//! and no window between checking and acting. What a door still owns is its transport and how it
//! renders an [`Answer`] or a [`Refusal`]; the classification between them is decided here, once.
//!
//! The split is the same one the authoring half draws: a verb that *ran* and reported a problem is
//! an [`Answer`] (a rejected swap, an `expect` conflict — the guard guarding), and only a verb that
//! could not do its job at all is a [`Refusal`]. [`get_engine_status`] cannot refuse, because
//! answering "is it there?" is its whole job. see rules: agent-mcp

use reuben_core::Registry;
use reuben_document::projection::Projector;

use crate::authoring::{Answer, Refusal, Resources};

use super::args::{SendLiveControls, SwapInstrument};
use super::channel::{Channel, ChannelError, SwapOutcome};
use super::prose::ENGINE_UNREACHABLE_GUIDANCE;
use super::result::{
    CurrentInstrument, EngineStatus, SendOutput, SidecarInfo, StatusEndpoints, SwapResult,
};
use super::wire::{
    over_long_batch_refusal, ControlArg, ControlMessage, DiagnosticsReport, DocSource,
    DocumentSnapshot, SwapReport, EMPTY_BATCH_REFUSAL, MAX_SEND_BATCH,
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
        Ok(()) => Ok(Answer {
            output: SendOutput { sent },
            summary: format!("the engine queued {sent} control message(s)"),
        }),
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
            structure: channel.endpoint().to_string(),
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
        format!("engine reachable on {}", output.endpoints.structure)
    } else {
        "engine not reachable — start `reuben play`".to_string()
    };
    Answer { output, summary }
}

/// Install a document from a path as the playing engine.
///
/// An `ok: false` load report and an `expect` conflict are both ordinary answers: the channel
/// worked, and the report is the deliverable.
pub fn swap_instrument(
    args: &SwapInstrument,
    channel: &Channel,
) -> Result<Answer<SwapResult>, Refusal> {
    match channel.swap(DocSource::Path(args.path.clone()), args.expect.clone()) {
        Ok(SwapOutcome::Installed(report)) => {
            let summary = swap_summary(&report);
            Ok(Answer {
                output: SwapResult::installed(report),
                summary,
            })
        }
        Ok(SwapOutcome::Conflict(conflict)) => {
            let summary = format!(
                "swap rejected by the expect guard: the engine is playing {}, not the \
                 expected {} — re-read with get_current_instrument and reconcile",
                conflict.actual, conflict.expected
            );
            Ok(Answer {
                output: SwapResult::conflict(conflict),
                summary,
            })
        }
        Err(why) => Err(refuse("the swap could not be completed", why)),
    }
}

/// What is the engine playing? Answered as a projection of the **installed** graph — the snapshot
/// the channel hands back reaches the window and stops here, never a document in a model's context.
///
/// `store_for` opens the caller's store against the source the engine names, because which document
/// is playing (and so what its relative references resolve against) is the answer, not the question.
/// A projection failure degrades to a note in place of the view rather than failing the call: the
/// hash is the load-bearing half.
pub fn get_current_instrument<S: Resources>(
    channel: &Channel,
    store_for: impl Fn(Option<&str>) -> S,
) -> Result<Answer<CurrentInstrument>, Refusal> {
    let snapshot = channel
        .get_document()
        .map_err(|why| refuse("could not read the current instrument", why))?;
    let summary = match &snapshot.source {
        Some(source) => format!("playing {source} (content_hash {})", snapshot.content_hash),
        None => format!(
            "playing an instrument installed by value (content_hash {})",
            snapshot.content_hash
        ),
    };
    let projection = project_installed(&snapshot, &store_for(snapshot.source.as_deref()));
    Ok(Answer {
        output: CurrentInstrument {
            projection,
            source: snapshot.source,
            content_hash: snapshot.content_hash,
        },
        summary,
    })
}

/// Read the engine's running counters.
pub fn get_engine_diagnostics(channel: &Channel) -> Result<Answer<DiagnosticsReport>, Refusal> {
    let report = channel
        .get_diagnostics()
        .map_err(|why| refuse("could not read the engine diagnostics", why))?;
    let summary = format!(
        "output_xruns={} input_ring_underruns={} input_ring_overruns={} \
         input_ring_producer_drops={}",
        report.output_xruns,
        report.input_ring_underruns,
        report.input_ring_overruns,
        report.input_ring_producer_drops
    );
    Ok(Answer {
        output: report,
        summary,
    })
}

/// The one place a failed exchange becomes a refusal, so no two verbs classify it differently:
/// unreachable answers with the shared guidance, anything else names *which* call died.
fn refuse(context: &str, why: ChannelError) -> Refusal {
    if why.is_unreachable() {
        return Refusal::new(ENGINE_UNREACHABLE_GUIDANCE);
    }
    Refusal::new(format!("{context}: {why}"))
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

/// The node index of what the engine is actually playing, cut from the snapshot the channel handed
/// back.
fn project_installed(snapshot: &DocumentSnapshot, store: &dyn Resources) -> String {
    let json = match serde_json::to_string(&snapshot.document) {
        Ok(json) => json,
        Err(e) => return format!("(projection unavailable: {e})"),
    };
    match Projector::new(
        &json,
        &Registry::builtin(),
        &crate::resources::Adapter(store),
    ) {
        Ok(p) => p.index().render(),
        Err(e) => format!("(projection unavailable: {e})"),
    }
}

/// One-line gloss of a swap outcome: what installed (with diff counts) or why nothing did.
fn swap_summary(report: &SwapReport) -> String {
    if report.report.ok {
        match &report.diff {
            Some(diff) => format!(
                "swapped (content_hash {}): {} survived, {} state-reset, {} added, {} removed",
                report.content_hash,
                diff.survived,
                diff.state_reset.len(),
                diff.added.len(),
                diff.removed.len()
            ),
            None => format!("swapped (content_hash {})", report.content_hash),
        }
    } else {
        format!(
            "swap rejected: {} error(s), {} warning(s) — nothing installed; {} keeps playing",
            report.report.errors.len(),
            report.report.warnings.len(),
            report.content_hash
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::args::ControlSendMessage;
    use super::super::channel::Transport;
    use super::*;
    use crate::authoring::{ResolveError, Resources, SampleBuffer};
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

    /// A store that holds nothing — `get_current_instrument` never reaches it against a dead engine.
    struct NoStore;

    impl Resources for NoStore {
        fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(source.to_string()))
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
            swap_instrument(
                &SwapInstrument {
                    path: "inst.json".to_string(),
                    expect: None,
                },
                &channel,
            )
            .expect_err("no engine to install into"),
            get_current_instrument(&channel, |_| NoStore).expect_err("nothing is playing"),
            get_engine_diagnostics(&channel).expect_err("no counters to read"),
        ];
        for refusal in refusals {
            assert_eq!(refusal.message, ENGINE_UNREACHABLE_GUIDANCE);
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
        assert_eq!(answer.output.endpoints.structure, "127.0.0.1:9124");
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
}
