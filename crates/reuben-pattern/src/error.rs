use alloc::string::String;
use core::fmt;
use core::ops::Range;

/// A parse or compile failure: where in the source, and a short human message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Byte range in the source text. Always on `char` boundaries, and non-empty unless the error
    /// is at the end of the input (`start == end == len`).
    pub span: Range<usize>,
    /// One line, lowercase, no trailing period; written for a live-coding UI to show beside `span`.
    pub message: String,
}

impl Error {
    pub(crate) fn new(span: Range<usize>, message: impl Into<String>) -> Error {
        Error {
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}..{}: {}",
            self.span.start, self.span.end, self.message
        )
    }
}

impl core::error::Error for Error {}
