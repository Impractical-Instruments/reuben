//! Why the editor's last edit did not land, or landed leaving the document still invalid, held
//! until the next edit, and the banner that says so.
//!
//! An edit to a document that already has errors is written when it fixes one and adds none, and
//! the document is then still invalid: that is [`Feedback::StillInvalid`], so a write is never
//! read as a valid document. An edit that writes nothing is one of three things. The `expect` guard turned it away because
//! the file changed since the editor read it — `reuben-mcp`, a text editor — and the answer's hash
//! is what is on disk now, not the one passed. The verb rejected it as leaving the document
//! invalid, and the hash is the one passed. Or the verb could not run at all.

use std::path::Path;

use eframe::egui;
use reuben_api::authoring::{Diag, EditResult, Refusal};

#[derive(Debug, Clone, PartialEq)]
pub enum Feedback {
    /// The file `source` was written by someone else since the editor read it. Nothing of the
    /// edit was written, and the session has read the file again.
    Conflict { source: String },
    /// The edit would have left `source` invalid; `errors` names the node and port at fault.
    /// `hash` is the document they were found in, which they describe only while it is on disk.
    Rejected {
        source: String,
        hash: String,
        /// The edit's own errors, then those the document already had before it: one slice, so
        /// all of them pin where they are.
        errors: Vec<Diag>,
        /// How many of `errors`, at its end, the document already had before the edit.
        pre_existing: usize,
        notes: Vec<String>,
    },
    /// The verb could not run on `source`.
    Refused { source: String, message: String },
    /// The edit was written as a repair: it fixed at least one of `source`'s errors and added none,
    /// and `errors` are the ones left. `hash` is what was written, which they describe only while
    /// it is on disk.
    StillInvalid {
        source: String,
        hash: String,
        errors: Vec<Diag>,
        notes: Vec<String>,
    },
}

impl Feedback {
    /// What to hold after an edit of `source` written against `expect` answered `result`: `None`
    /// when it landed and left the document valid.
    pub fn of(
        source: &str,
        expect: Option<&str>,
        result: &Result<EditResult, Refusal>,
    ) -> Option<Self> {
        let source = source.to_string();
        match result {
            Ok(edit) if edit.written && edit.report.ok => None,
            Ok(edit) if edit.written => Some(Self::StillInvalid {
                source,
                hash: edit.hash.clone(),
                errors: edit.report.errors.clone(),
                notes: edit.notes.clone(),
            }),
            Ok(edit) if expect.is_some_and(|expect| expect != edit.hash) => {
                Some(Self::Conflict { source })
            }
            Ok(edit) => Some(Self::Rejected {
                source,
                hash: edit.hash.clone(),
                errors: (edit.report.errors.iter())
                    .chain(&edit.pre_existing)
                    .cloned()
                    .collect(),
                pre_existing: edit.pre_existing.len(),
                notes: edit.notes.clone(),
            }),
            Err(refusal) => Some(Self::Refused {
                source,
                message: refusal.message.clone(),
            }),
        }
    }

    /// The errors a rejected edit of `source` was turned away with; empty for any other.
    pub fn rejected(&self, source: &str) -> &[Diag] {
        match self {
            Self::Rejected {
                source: of, errors, ..
            } if of == source => errors,
            _ => &[],
        }
    }

    /// Whether this is a rejection or a repair of a document that has changed since, `hash_of`
    /// giving each source's content hash as read now: its errors may name nodes that are gone or
    /// fixed.
    pub fn outlived<'a>(&self, hash_of: impl Fn(&str) -> Option<&'a str>) -> bool {
        match self {
            Self::Rejected { source, hash, .. } | Self::StillInvalid { source, hash, .. } => {
                hash_of(source) != Some(hash.as_str())
            }
            Self::Conflict { .. } | Self::Refused { .. } => false,
        }
    }

    /// The one line the banner leads with.
    pub fn headline(&self) -> String {
        match self {
            Self::Conflict { source } => format!(
                "Your edit did not land: {} changed since the edit began, by reuben-mcp, another \
                 program or an undo. It is shown as it is now; make the edit again to apply it.",
                file_name(source)
            ),
            Self::Rejected {
                source,
                errors,
                pre_existing,
                ..
            } if *pre_existing > 0 && *pre_existing == errors.len() => format!(
                "Your edit was not written: {} was already invalid before it, and until it is \
                 valid only an edit that fixes an error and adds none is written. Fix what is \
                 below first. The file is unchanged.",
                file_name(source)
            ),
            Self::Rejected { source, .. } => format!(
                "Your edit was not written: it would leave {} invalid. The file is unchanged.",
                file_name(source)
            ),
            Self::Refused { source, message } => {
                format!(
                    "Your edit to {} could not be made: {message}",
                    file_name(source)
                )
            }
            Self::StillInvalid { source, errors, .. } => format!(
                "Your edit was written, but {} is still invalid: it fixed an error and added \
                 none, and {} error(s) still keep it from loading. Fix what is below next.",
                file_name(source),
                errors.len()
            ),
        }
    }

    /// The lines under the headline: each error of a rejected edit, or each one a repair left, at
    /// the node and port it names and marked when a rejected edit's document already had it, then
    /// what the verb noted.
    pub fn details(&self) -> Vec<String> {
        let (errors, pre_existing, notes) = match self {
            Self::Rejected {
                errors,
                pre_existing,
                notes,
                ..
            } => (errors, *pre_existing, notes),
            Self::StillInvalid { errors, notes, .. } => (errors, 0, notes),
            Self::Conflict { .. } | Self::Refused { .. } => return Vec::new(),
        };
        let (own, already) = errors.split_at(errors.len() - pre_existing);
        (own.iter().map(located))
            .chain(
                already
                    .iter()
                    .map(|d| format!("already there: {}", located(d))),
            )
            .chain(notes.iter().cloned())
            .collect()
    }
}

/// `diag`'s message, led by the node and port it names.
pub fn located(diag: &Diag) -> String {
    match (&diag.node, &diag.port) {
        (Some(node), Some(port)) => format!("{node}.{port}: {}", diag.message),
        (Some(at), None) | (None, Some(at)) => format!("{at}: {}", diag.message),
        (None, None) => diag.message.clone(),
    }
}

fn file_name(source: &str) -> String {
    Path::new(source).file_name().map_or_else(
        || source.to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Draw the banner for `feedback`, when there is one, with a button that lets it go.
pub fn show(ui: &mut egui::Ui, feedback: &mut Option<Feedback>) {
    let Some(shown) = feedback.as_ref() else {
        return;
    };
    let color = match shown {
        Feedback::Conflict { .. } | Feedback::StillInvalid { .. } => ui.visuals().warn_fg_color,
        Feedback::Rejected { .. } | Feedback::Refused { .. } => ui.visuals().error_fg_color,
    };
    let mut dismissed = false;
    egui::Frame::group(ui.style())
        .stroke(egui::Stroke::new(1.0, color))
        .show(ui, |ui| {
            // Not beside the headline: a horizontal row does not wrap, and the headline would push
            // the central panel over the inspector.
            ui.colored_label(color, shown.headline());
            for line in shown.details() {
                ui.colored_label(color, line);
            }
            dismissed = ui.small_button("Dismiss").clicked();
        });
    if dismissed {
        *feedback = None;
    }
}

#[cfg(test)]
mod tests {
    use reuben_api::authoring::Report;

    use super::*;

    fn answered(written: bool, hash: &str, errors: Vec<Diag>) -> Result<EditResult, Refusal> {
        answered_with(written, hash, errors, Vec::new())
    }

    fn answered_with(
        written: bool,
        hash: &str,
        errors: Vec<Diag>,
        pre_existing: Vec<Diag>,
    ) -> Result<EditResult, Refusal> {
        Ok(EditResult {
            report: Report {
                ok: written,
                errors,
                warnings: Vec::new(),
            },
            pre_existing,
            written,
            hash: hash.to_string(),
            notes: Vec::new(),
            zoom: String::new(),
        })
    }

    fn diag(node: &str, port: &str) -> Diag {
        Diag {
            node: Some(node.to_string()),
            port: Some(port.to_string()),
            message: "no".to_string(),
        }
    }

    #[test]
    fn a_landed_edit_holds_nothing() {
        assert_eq!(
            Feedback::of("a.json", Some("h1"), &answered(true, "h2", vec![])),
            None
        );
    }

    #[test]
    fn a_hash_other_than_the_one_passed_is_a_conflict() {
        let guard = vec![Diag {
            node: None,
            port: None,
            message: "expect guard".to_string(),
        }];
        let held = Feedback::of(
            "dir/a.json",
            Some("mine"),
            &answered(false, "theirs", guard),
        );
        assert_eq!(
            held,
            Some(Feedback::Conflict {
                source: "dir/a.json".to_string()
            })
        );
        let held = held.unwrap();
        assert!(held.headline().contains("a.json"), "{}", held.headline());
        assert!(held.rejected("dir/a.json").is_empty());
    }

    #[test]
    fn the_hash_passed_back_unchanged_is_a_rejection_with_its_errors() {
        let held = Feedback::of(
            "a.json",
            Some("h"),
            &answered(false, "h", vec![diag("/filter", "cutoff")]),
        )
        .expect("held");
        assert_eq!(held.rejected("a.json"), &[diag("/filter", "cutoff")]);
        assert!(held.rejected("b.json").is_empty());
        assert_eq!(held.details(), vec!["/filter.cutoff: no".to_string()]);
        assert!(!held.outlived(|_| Some("h")));
        assert!(held.outlived(|_| Some("moved on")));
        assert!(held.outlived(|_| None));
    }

    #[test]
    fn a_rejection_for_what_the_document_already_had_says_so_and_pins_it() {
        let held = Feedback::of(
            "a.json",
            None,
            &answered_with(false, "h", vec![], vec![diag("/gone", "in")]),
        )
        .expect("held");
        assert!(
            held.headline().contains("already invalid before it"),
            "{}",
            held.headline()
        );
        assert_eq!(
            held.details(),
            vec!["already there: /gone.in: no".to_string()]
        );
        assert_eq!(held.rejected("a.json"), &[diag("/gone", "in")]);

        let both = Feedback::of(
            "a.json",
            None,
            &answered_with(
                false,
                "h",
                vec![diag("/osc", "freq")],
                vec![diag("/gone", "in")],
            ),
        )
        .expect("held");
        assert!(
            both.headline().contains("it would leave a.json invalid"),
            "{}",
            both.headline()
        );
        assert_eq!(
            both.details(),
            vec![
                "/osc.freq: no".to_string(),
                "already there: /gone.in: no".to_string()
            ]
        );
        assert_eq!(both.rejected("a.json").len(), 2);
    }

    #[test]
    fn a_repair_written_while_still_invalid_says_so_and_lists_what_is_left() {
        let mut repaired = answered(true, "h2", vec![diag("/amp2", "a")]);
        if let Ok(edit) = &mut repaired {
            edit.report.ok = false;
        }
        let held = Feedback::of("a.json", Some("h1"), &repaired).expect("held");
        assert!(matches!(held, Feedback::StillInvalid { .. }), "{held:?}");
        assert!(
            held.headline()
                .contains("written, but a.json is still invalid"),
            "{}",
            held.headline()
        );
        assert_eq!(held.details(), vec!["/amp2.a: no".to_string()]);
        assert!(held.rejected("a.json").is_empty());
        assert!(
            !held.outlived(|_| Some("h2")),
            "the write it describes is on disk"
        );
        assert!(held.outlived(|_| Some("h3")));
    }

    #[test]
    fn an_unguarded_edit_that_writes_nothing_is_a_rejection() {
        let held = Feedback::of("a.json", None, &answered(false, "h", vec![])).expect("held");
        assert!(matches!(held, Feedback::Rejected { .. }), "{held:?}");
    }

    #[test]
    fn a_verb_that_could_not_run_is_refused() {
        let held = Feedback::of(
            "a.json",
            Some("h"),
            &Err(Refusal {
                message: "unreadable".to_string(),
            }),
        )
        .expect("held");
        assert!(
            held.headline().contains("unreadable"),
            "{}",
            held.headline()
        );
    }
}
