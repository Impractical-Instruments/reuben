//! Prepared `str` inputs: text compiled off the render thread into a payload the render thread
//! only reads.
//!
//! A `str` input is the boundary type: what a document default spells, what a live send carries,
//! and what an interface pipe declares. Inside the render thread the input holds a [`Prepared`]
//! instead, built by the [`Preparer`] its Operator declares (`pat: str(compile)` in
//! `operator_contract!`). The Plan prepares document defaults when it is built. Render delivers a
//! prepared value only to an input whose preparer made it ([`Prepared::kind`]), and drops anything
//! else — raw text included, so a live string reaches an input only once something off the render
//! thread has prepared it.

use alloc::borrow::Cow;
use alloc::sync::Arc;
use core::any::Any;
use core::fmt;
use core::ops::Range;

/// The signature of a preparer: compile `source` into a payload, or say which bytes are wrong.
///
/// `no_std`: it runs on whatever thread prepares, the Coordinator's or a producer's, and on the
/// bare-metal target too. It may allocate; it never runs on the render thread.
pub type PrepareFn = fn(&str) -> Result<Prepared, PrepareError>;

/// The text a preparer was given and the payload it built from it, type-erased, plus the kind tag
/// naming the preparer.
///
/// One `Arc`, so a clone on the render thread is a refcount bump and the last drop is one free,
/// which the render thread hands to the retire queue. Two values are equal when the same preparer
/// made them from the same text: a preparer is a pure function of its text, so their payloads are
/// equal too. Comparing by text is what lets a Swap tell an unchanged document default from a
/// changed one; [`ptr_eq`](Self::ptr_eq) is the cheap test for "the same send".
#[derive(Clone)]
pub struct Prepared(Arc<Inner<dyn Any + Send + Sync>>);

struct Inner<P: ?Sized> {
    /// [`Preparer::kind`] of the preparer that built this; empty until [`Preparer::prepare`]
    /// stamps it.
    kind: &'static str,
    /// The text the payload was built from: the external form, and what equality compares.
    source: Option<Arc<str>>,
    payload: P,
}

impl Prepared {
    /// Wrap a preparer's payload. A [`PrepareFn`] returns this; [`Preparer::prepare`] then records
    /// which preparer built it and from what text.
    pub fn new<P: Any + Send + Sync>(payload: P) -> Self {
        Prepared(Arc::new(Inner {
            kind: "",
            source: None,
            payload,
        }))
    }

    /// The kind tag of the preparer that built this value.
    pub fn kind(&self) -> &'static str {
        self.0.kind
    }

    /// The text this value was prepared from: its external OSC form.
    pub fn source(&self) -> &str {
        self.0.source.as_deref().unwrap_or("")
    }

    /// [`source`](Self::source) as the shared handle it arrived in, for a boundary that sends it
    /// back out as an `Arg::Str` without copying the text.
    pub fn source_arc(&self) -> Option<&Arc<str>> {
        self.0.source.as_ref()
    }

    /// The payload, if it is a `P`. The read an Operator makes in `process`: no allocation.
    pub fn payload<P: Any>(&self) -> Option<&P> {
        (&self.0.payload as &dyn Any).downcast_ref::<P>()
    }

    /// Whether `self` and `other` are the same allocation.
    pub fn ptr_eq(&self, other: &Prepared) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl PartialEq for Prepared {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other) || (self.kind() == other.kind() && self.source() == other.source())
    }
}

impl fmt::Debug for Prepared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Prepared")
            .field("kind", &self.kind())
            .field("source", &self.source())
            .finish()
    }
}

/// Why a preparer refused its text: the byte range at fault and a short message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareError {
    /// Byte offsets into the source, `start..end`; empty at a position where something is missing.
    pub span: Range<usize>,
    pub message: Cow<'static, str>,
}

impl PrepareError {
    pub fn new(span: Range<usize>, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "bytes {}..{}: {}",
            self.span.start, self.span.end, self.message
        )
    }
}

/// The preparer a `str` input declares: its function and the kind tag it stamps on what it builds.
///
/// `operator_contract!` derives the tag from the function's path: its crate-rooted path when the
/// declaration spells one (`str(crate::m::compile)`), else the declaring module's path plus the path
/// as written. Two functions never share a tag, but one function can have several: named
/// `super::compile` from one Operator and `crate::m::compile` from another, its values are two
/// kinds, and a kept pipe feeding both is refused. Name a shared preparer by its crate-rooted path
/// everywhere. Equality is by tag.
#[derive(Clone, Copy)]
pub struct Preparer {
    pub kind: &'static str,
    pub prepare: PrepareFn,
}

impl Preparer {
    pub const fn new(kind: &'static str, prepare: PrepareFn) -> Self {
        Self { kind, prepare }
    }

    /// Prepare `source`, stamping the result with this preparer's kind and the text it came from.
    /// Cold: it runs the preparer, which may allocate.
    pub fn prepare(&self, source: &Arc<str>) -> Result<Prepared, PrepareError> {
        let mut prepared = (self.prepare)(source)?;
        // A fresh `Prepared::new` is the only handle to its allocation. A preparer that hands back
        // a clone it kept would have this stamp land on every holder, so it is refused instead.
        let Some(inner) = Arc::get_mut(&mut prepared.0) else {
            return Err(PrepareError::new(
                0..source.len(),
                "the preparer returned a payload it also kept; return a fresh `Prepared::new`",
            ));
        };
        inner.kind = self.kind;
        inner.source = Some(Arc::clone(source));
        Ok(prepared)
    }
}

impl PartialEq for Preparer {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl Eq for Preparer {}

impl fmt::Debug for Preparer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Preparer").field(&self.kind).finish()
    }
}

/// Test preparers for the crate's own unit tests: one reads whitespace-separated numbers, the
/// other words with no digits, so a value one builds is the wrong kind for the other.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use alloc::vec::Vec;

    /// The parsed numbers.
    pub(crate) struct Steps(pub Vec<u32>);

    pub(crate) fn steps(source: &str) -> Result<Prepared, PrepareError> {
        let mut out = Vec::new();
        for (start, token) in tokens(source) {
            let n = token
                .parse::<u32>()
                .map_err(|_| PrepareError::new(start..start + token.len(), "not a number"))?;
            out.push(n);
        }
        Ok(Prepared::new(Steps(out)))
    }

    pub(crate) fn words(source: &str) -> Result<Prepared, PrepareError> {
        for (start, token) in tokens(source) {
            if let Some(at) = token.find(|c: char| c.is_ascii_digit()) {
                return Err(PrepareError::new(start + at..start + at + 1, "a digit"));
            }
        }
        Ok(Prepared::new(tokens(source).count()))
    }

    fn tokens(source: &str) -> impl Iterator<Item = (usize, &str)> {
        source
            .split(' ')
            .scan(0usize, |at, t| {
                let start = *at;
                *at += t.len() + 1;
                Some((start, t))
            })
            .filter(|(_, t)| !t.is_empty())
    }

    pub(crate) const STEPS: Preparer = Preparer::new("test::steps", steps);
    pub(crate) const WORDS: Preparer = Preparer::new("test::words", words);
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn prepare_stamps_the_kind_and_the_source() {
        let src: Arc<str> = "1 2 3".into();
        let p = STEPS.prepare(&src).expect("compiles");
        assert_eq!(p.kind(), STEPS.kind);
        assert_eq!(p.source(), "1 2 3");
        assert!(Arc::ptr_eq(p.source_arc().unwrap(), &src));
        assert_eq!(
            p.payload::<Steps>().map(|s| s.0.as_slice()),
            Some(&[1, 2, 3][..])
        );
        assert!(
            p.payload::<usize>().is_none(),
            "the wrong payload type reads nothing"
        );
    }

    #[test]
    fn a_refusal_carries_its_byte_span() {
        let err = STEPS.prepare(&"1 x2 3".into()).unwrap_err();
        assert_eq!(err.span, 2..4);
        assert_eq!(err.to_string(), "bytes 2..4: not a number");
    }

    #[test]
    fn equality_is_kind_and_text_and_ptr_eq_is_identity() {
        let a = STEPS.prepare(&"1 2".into()).unwrap();
        let b = STEPS.prepare(&"1 2".into()).unwrap();
        assert_eq!(a, b);
        assert!(!a.ptr_eq(&b));
        assert!(a.ptr_eq(&a.clone()));
        assert_ne!(a, STEPS.prepare(&"1 3".into()).unwrap());
        assert_ne!(
            WORDS.prepare(&"ab".into()).unwrap(),
            Preparer::new("other", words).prepare(&"ab".into()).unwrap()
        );
    }

    #[test]
    fn a_preparer_returning_a_kept_payload_is_refused() {
        fn kept(_: &str) -> Result<Prepared, PrepareError> {
            let p = Prepared::new(0u8);
            core::mem::forget(p.clone());
            Ok(p)
        }
        let err = Preparer::new("kept", kept)
            .prepare(&"x".into())
            .unwrap_err();
        assert_eq!(err.span, 0..1);
    }
}
