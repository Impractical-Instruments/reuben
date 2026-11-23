//! The text-editor clients: `reuben send` sends one string to an input that compiles its text, and
//! `reuben live` sends a set file's lines each time it is saved. Both reach the running
//! `reuben play` through the engine channel's `send` verb and nothing else.
//!
//! Every problem prints as `<file>:<line>:<col>: error: <message>`, 1-based, the column counted in
//! characters, followed by the refused text with its span marked: the one shape VS Code problem
//! matchers, Vim's `errorformat` and Emacs's compilation mode all read.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher};
use reuben_api::engine::{
    Channel, ChannelError, ControlArg, ControlMessage, Event, SendOutcome, SendRefusal,
};

/// The read budget for a `send`: it queues and compiles one line of text, and answers at once.
pub const SEND_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the set file must go untouched before it is read. One save is several events — a
/// temporary file, a rename, sometimes a truncate before the bytes.
const QUIET: Duration = Duration::from_millis(50);

/// The longest a save waits to be read while later touches keep arriving.
const LONGEST_WAIT: Duration = Duration::from_millis(500);

/// Send `text` to `address` as one string.
pub fn send_text(
    channel: &Channel,
    address: &str,
    text: &str,
) -> Result<SendOutcome, ChannelError> {
    channel.send(vec![ControlMessage {
        address: address.to_string(),
        args: vec![ControlArg::Str(text.to_string())],
    }])
}

/// The 0-based line and character column of byte `at` in `text`, moved back onto a `char`
/// boundary.
fn locate(text: &str, at: usize) -> (usize, usize) {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let before = &text[..at];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (
        before.matches('\n').count(),
        before[line_start..].chars().count(),
    )
}

/// The report of `refusal` of `text`, whose first character sits at 1-based `line` and `column` of
/// `file`: the location line, then the refused text with its span marked.
pub fn refusal_report(
    file: &str,
    line: usize,
    column: usize,
    text: &str,
    refusal: &SendRefusal,
) -> String {
    let (down, across) = locate(text, refusal.offset + refusal.start);
    let column = if down == 0 {
        column + across
    } else {
        across + 1
    };
    let mut report = format!("{file}:{}:{column}: error: {}", line + down, refusal.error);
    if !refusal.text.is_empty() {
        report.push('\n');
        report.push_str(&refusal.marked());
    }
    report
}

/// One line of a set file to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetLine {
    /// 1-based.
    pub line: usize,
    /// 1-based, in characters: where the text starts.
    pub column: usize,
    pub address: String,
    pub text: String,
}

/// What one non-blank, non-comment line of a set file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Send(SetLine),
    /// A line that is not an address, whitespace, then text; or one naming an address an earlier
    /// line already sets, which would make "changed since its last send" mean two things.
    Malformed {
        line: usize,
        column: usize,
        message: String,
    },
}

/// The entries of a set file: one per line that is neither blank nor a `#` comment, in order.
/// Trailing whitespace is not part of a line's text.
pub fn parse_set(source: &str) -> Vec<Entry> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut entries = Vec::new();
    for (i, raw) in source.lines().enumerate() {
        let line = i + 1;
        let content = raw.trim_end();
        let body = content.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let lead = content.len() - body.len();
        let column_at = |byte: usize| content[..byte].chars().count() + 1;
        let address_end = body.find(char::is_whitespace).unwrap_or(body.len());
        let address = &body[..address_end];
        if !address.starts_with('/') {
            entries.push(Entry::Malformed {
                line,
                column: column_at(lead),
                message: format!("expected an address starting with `/`, found `{address}`"),
            });
            continue;
        }
        let rest = &body[address_end..];
        let text = rest.trim_start();
        let text_start = lead + address_end + (rest.len() - text.len());
        if text.is_empty() {
            entries.push(Entry::Malformed {
                line,
                column: column_at(text_start),
                message: format!("expected a pattern after `{address}`"),
            });
            continue;
        }
        if let Some(first) = seen.get(address) {
            entries.push(Entry::Malformed {
                line,
                column: column_at(lead),
                message: format!("`{address}` is already set on line {first}"),
            });
            continue;
        }
        seen.insert(address, line);
        entries.push(Entry::Send(SetLine {
            line,
            column: column_at(text_start),
            address: address.to_string(),
            text: text.to_string(),
        }));
    }
    entries
}

/// What `reuben live` remembers between saves of one set file.
#[derive(Debug)]
pub struct Live {
    /// The file as the user named it, the spelling every report uses.
    file: String,
    /// Each address's text the engine last queued.
    sent: BTreeMap<String, String>,
    /// The addresses whose last send was refused.
    failing: BTreeSet<String>,
    /// Where each address sits in the file as last read.
    at: BTreeMap<String, (usize, usize)>,
}

impl Live {
    pub fn new(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            sent: BTreeMap::new(),
            failing: BTreeSet::new(),
            at: BTreeMap::new(),
        }
    }

    /// Send each line of `source` whose text differs from what its address last queued, or whose
    /// last send was refused, one `send` per line, and report each between a
    /// `reuben live: sending <file>` line and a `reuben live: done` line. A refused line leaves its
    /// address playing what it played. An engine that stops answering ends the batch early; what
    /// was not sent is sent on the next save.
    pub fn save(
        &mut self,
        source: &str,
        channel: &Channel,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let file = &self.file;
        writeln!(out, "reuben live: sending {file}")?;
        let entries = parse_set(source);
        self.at = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Send(l) => Some((l.address.clone(), (l.line, l.column))),
                Entry::Malformed { .. } => None,
            })
            .collect();
        self.failing.retain(|address| self.at.contains_key(address));
        let (mut queued, mut errors) = (0, 0);
        for entry in entries {
            let l = match entry {
                Entry::Send(l) => l,
                Entry::Malformed {
                    line,
                    column,
                    message,
                } => {
                    errors += 1;
                    writeln!(out, "{file}:{line}:{column}: error: {message}")?;
                    continue;
                }
            };
            if !self.failing.contains(&l.address) && self.sent.get(&l.address) == Some(&l.text) {
                continue;
            }
            match send_text(channel, &l.address, &l.text) {
                Ok(SendOutcome::Queued) => {
                    queued += 1;
                    let fixed = if self.failing.remove(&l.address) {
                        "fixed, "
                    } else {
                        ""
                    };
                    writeln!(
                        out,
                        "{file}:{}:{}: {fixed}queued {}",
                        l.line, l.column, l.address
                    )?;
                    self.sent.insert(l.address, l.text);
                }
                Ok(SendOutcome::Refused(refusal)) => {
                    errors += 1;
                    writeln!(
                        out,
                        "{}",
                        refusal_report(file, l.line, l.column, &l.text, &refusal)
                    )?;
                    self.failing.insert(l.address);
                }
                Err(e) => {
                    writeln!(out, "reuben live: {e}")?;
                    break;
                }
            }
        }
        writeln!(out, "reuben live: done ({queued} queued, {errors} errors)")?;
        out.flush()
    }

    /// React to one of the engine's events: a send a Swap made stale is named so the performer
    /// resends it, and any address whose live value the engine let go is sent again on the next
    /// save even if its line is unchanged.
    pub fn event(&mut self, event: &Event, out: &mut impl Write) -> io::Result<()> {
        let file = &self.file;
        match event {
            Event::SendsDropped { addresses } => {
                for address in addresses {
                    let Some((line, column)) = self.at.get(address) else {
                        continue;
                    };
                    self.sent.remove(address);
                    writeln!(
                        out,
                        "{file}:{line}:{column}: warning: dropped: instrument changed, resend \
                         {address}"
                    )?;
                }
            }
            Event::Opened { name, .. } => {
                if !self.sent.is_empty() {
                    writeln!(
                        out,
                        "reuben live: {name} was opened; every line is sent on the next save"
                    )?;
                }
                self.sent.clear();
            }
            Event::Changed { reset, .. } => {
                let gone: Vec<String> = match reset {
                    Some(reset) => reset
                        .iter()
                        .filter(|a| self.sent.contains_key(*a))
                        .cloned()
                        .collect(),
                    None => self.sent.keys().cloned().collect(),
                };
                for address in gone {
                    self.sent.remove(&address);
                    writeln!(
                        out,
                        "reuben live: an edit reset {address}; it is sent on the next save"
                    )?;
                }
            }
            Event::Invalid { .. } => {}
        }
        out.flush()
    }

    /// Forget every send, so the next save sends every line: the engine may have restarted.
    pub fn forget(&mut self) {
        self.sent.clear();
    }
}

/// Why `reuben live` stopped.
#[derive(Debug)]
pub enum LiveError {
    /// The engine's event stream could not be opened.
    Subscribe(ChannelError),
    /// The set file's directory could not be watched.
    Watch(notify::Error),
    Io(io::Error),
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LiveError::Subscribe(e) => write!(f, "could not follow the engine's events: {e}"),
            LiveError::Watch(e) => write!(f, "could not watch the set file: {e}"),
            LiveError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LiveError {}

impl From<io::Error> for LiveError {
    fn from(e: io::Error) -> Self {
        LiveError::Io(e)
    }
}

/// What wakes the live loop.
enum Wake {
    Touched,
    Event(Event),
    /// The engine closed the event stream.
    Lost,
}

/// Follow the engine's events on a thread of its own, forwarding each to `wake`.
fn follow(channel: &Channel, wake: Sender<Wake>) -> Result<(), ChannelError> {
    let events = channel.subscribe()?;
    std::thread::Builder::new()
        .name("live-events".to_string())
        .spawn(move || {
            for event in events {
                if wake.send(Wake::Event(event)).is_err() {
                    return;
                }
            }
            let _ = wake.send(Wake::Lost);
        })
        .map_err(|e| ChannelError::Channel(e.to_string()))?;
    Ok(())
}

/// Send `path`'s lines now and after every save until the process ends, reporting to `out` under
/// the name `shown`.
///
/// The directory is watched, not the file: an editor that saves by renaming a new file over the
/// old one replaces the file a watch on it would follow.
pub fn run(
    path: &Path,
    shown: &str,
    channel: &Channel,
    out: &mut impl Write,
) -> Result<(), LiveError> {
    let path: PathBuf = std::path::absolute(path)?;
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let name = path.file_name().map(ToOwned::to_owned);

    let (wake, woken) = mpsc::channel();
    follow(channel, wake.clone()).map_err(LiveError::Subscribe)?;
    let touched = wake.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let ours = match event {
            // Reading the file is an event too on Linux, and every save is read.
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => false,
            Ok(event) if event.need_rescan() => true,
            Err(_) => true,
            Ok(event) => (event.paths.iter()).any(|p| p.file_name() == name.as_deref()),
        };
        if ours {
            let _ = touched.send(Wake::Touched);
        }
    })
    .map_err(LiveError::Watch)?;
    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .map_err(LiveError::Watch)?;

    let mut live = Live::new(shown);
    let mut subscribed = true;
    let mut first: Option<Instant> = None;
    let mut due = Some(Instant::now());
    loop {
        let woke = match due {
            Some(due) => woken.recv_timeout(due.saturating_duration_since(Instant::now())),
            None => woken.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match woke {
            Ok(Wake::Touched) => {
                let now = Instant::now();
                let first = *first.get_or_insert(now);
                due = Some((now + QUIET).min(first + LONGEST_WAIT));
                continue;
            }
            Ok(Wake::Event(event)) => {
                live.event(&event, out)?;
                continue;
            }
            Ok(Wake::Lost) => {
                subscribed = false;
                writeln!(
                    out,
                    "reuben live: lost the engine's events; following them again on the next save"
                )?;
                out.flush()?;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if due.is_none_or(|due| Instant::now() < due) {
            continue;
        }
        (first, due) = (None, None);
        if !subscribed && follow(channel, wake.clone()).is_ok() {
            subscribed = true;
            live.forget();
        }
        match std::fs::read_to_string(&path) {
            Ok(source) => live.save(&source, channel, out)?,
            Err(e) => {
                writeln!(out, "reuben live: cannot read {shown}: {e}")?;
                out.flush()?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(text: &str, start: usize, end: usize) -> SendRefusal {
        SendRefusal {
            message: 0,
            address: "/p".to_string(),
            text: text.to_string(),
            offset: 0,
            start,
            end,
            error: "bad".to_string(),
        }
    }

    #[test]
    fn a_set_file_is_an_address_whitespace_then_text_per_line() {
        let source = "# comment\n\n  # indented comment\n/a  bd ~ [sd sd] ~  \r\n\t/b\tx\n";
        assert_eq!(
            parse_set(source),
            vec![
                Entry::Send(SetLine {
                    line: 4,
                    column: 5,
                    address: "/a".to_string(),
                    text: "bd ~ [sd sd] ~".to_string(),
                }),
                Entry::Send(SetLine {
                    line: 5,
                    column: 5,
                    address: "/b".to_string(),
                    text: "x".to_string(),
                }),
            ]
        );
    }

    #[test]
    fn a_malformed_line_is_an_error_at_its_place() {
        assert_eq!(
            parse_set("drums bd\n/a   \n/b x\n /b y\n"),
            vec![
                Entry::Malformed {
                    line: 1,
                    column: 1,
                    message: "expected an address starting with `/`, found `drums`".to_string(),
                },
                Entry::Malformed {
                    line: 2,
                    column: 3,
                    message: "expected a pattern after `/a`".to_string(),
                },
                Entry::Send(SetLine {
                    line: 3,
                    column: 4,
                    address: "/b".to_string(),
                    text: "x".to_string(),
                }),
                Entry::Malformed {
                    line: 4,
                    column: 2,
                    message: "`/b` is already set on line 3".to_string(),
                },
            ]
        );
    }

    #[test]
    fn a_report_locates_the_span_in_characters_from_where_the_text_starts() {
        assert_eq!(
            refusal_report("f.set", 7, 10, "é x", &refusal("é x", 3, 4)),
            "f.set:7:12: error: bad\né x\n  ^"
        );
        assert_eq!(
            refusal_report("<stdin>", 1, 1, "a\nbc d", &refusal("a\nbc d", 5, 6)),
            "<stdin>:2:4: error: bad\na\nbc d\n   ^"
        );
        let windowed = SendRefusal {
            offset: 2,
            ..refusal("cd", 1, 2)
        };
        assert_eq!(
            refusal_report("<arg>", 1, 1, "abcd", &windowed),
            "<arg>:1:4: error: bad\ncd\n ^"
        );
        let not_text = refusal("", 0, 0);
        assert_eq!(
            refusal_report("<arg>", 1, 1, "x", &not_text),
            "<arg>:1:1: error: bad"
        );
    }

    fn shown(live: &mut Live, event: Event) -> String {
        let mut out = Vec::new();
        live.event(&event, &mut out).expect("write");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn a_dropped_send_is_named_at_its_line_and_sent_again_on_the_next_save() {
        let mut live = Live::new("s.set");
        live.at.insert("/a".to_string(), (3, 4));
        live.sent.insert("/a".to_string(), "1".to_string());
        live.sent.insert("/b".to_string(), "2".to_string());
        assert_eq!(
            shown(
                &mut live,
                Event::SendsDropped {
                    addresses: vec!["/a".to_string(), "/elsewhere".to_string()],
                }
            ),
            "s.set:3:4: warning: dropped: instrument changed, resend /a\n"
        );
        assert!(!live.sent.contains_key("/a"), "the next save resends it");
        assert!(live.sent.contains_key("/b"));
    }

    #[test]
    fn a_value_the_engine_let_go_is_sent_again_on_the_next_save() {
        let mut live = Live::new("s.set");
        live.sent.insert("/a/in".to_string(), "1".to_string());
        live.sent.insert("/b/in".to_string(), "2".to_string());
        let changed = |reset| Event::Changed {
            hash: "h".to_string(),
            by: reuben_api::engine::ChangedBy::Client,
            reset,
        };
        assert_eq!(
            shown(&mut live, changed(Some(vec!["/a/in".to_string()]))),
            "reuben live: an edit reset /a/in; it is sent on the next save\n"
        );
        assert_eq!(live.sent.keys().collect::<Vec<_>>(), ["/b/in"]);
        shown(
            &mut live,
            Event::Opened {
                name: "x".to_string(),
                hash: "h".to_string(),
            },
        );
        assert!(live.sent.is_empty());
    }
}
