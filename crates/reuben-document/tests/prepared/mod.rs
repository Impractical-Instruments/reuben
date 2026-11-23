//! Test-only Operators with prepared `str` inputs, registered beside the built-ins by
//! [`registry`]. No built-in Operator prepares its text yet.
//!
//! `steps` compiles whitespace-separated whole numbers and reports their sum; `words` compiles
//! words with no digits and reports how many; the `nonempty` family refuses empty text. Each refuses bad text with the byte span at fault,
//! and a value one prepares is the wrong kind for the other.
#![allow(dead_code)] // not every including binary uses every helper.

use std::sync::Arc;

use reuben_core::descriptor::Descriptor;
use reuben_core::message::Arg;
use reuben_core::operator::{Io, Operator};
use reuben_core::prepare::{PrepareError, Prepared, Preparer};
use reuben_core::Registry;

/// Whitespace-separated tokens of `source`, each with its byte offset.
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

/// What `steps` compiles its text to.
pub struct Compiled(pub Vec<u32>);

pub fn parse_steps(source: &str) -> Result<Prepared, PrepareError> {
    let mut out = Vec::new();
    for (start, token) in tokens(source) {
        let n = token.parse::<u32>().map_err(|_| {
            PrepareError::new(start..start + token.len(), "expected a whole number")
        })?;
        out.push(n);
    }
    Ok(Prepared::new(Compiled(out)))
}

pub fn parse_words(source: &str) -> Result<Prepared, PrepareError> {
    for (start, token) in tokens(source) {
        if let Some(at) = token.find(|c: char| c.is_ascii_digit()) {
            return Err(PrepareError::new(
                start + at..start + at + 1,
                "a word has no digits",
            ));
        }
    }
    Ok(Prepared::new(tokens(source).count()))
}

pub mod steps {
    use super::*;

    /// The last sum sent, so an unchanged payload re-sends nothing.
    pub struct Steps(pub Option<f32>);
    reuben_core::operator_contract!(Steps {
        type_name: "steps",
        inputs: { steps: str(parse_steps) },
        outputs: { total: f32 { 0.0..=1_000_000.0, default 0.0 } },
    });

    impl Operator for Steps {
        fn descriptor() -> Descriptor {
            Self::contract()
        }
        fn process(&mut self, io: &mut Io) {
            let total = io
                .read(IN_STEPS)
                .and_then(|p| p.payload::<Compiled>())
                .map_or(-1.0, |c| c.0.iter().sum::<u32>() as f32);
            if self.0 != Some(total) {
                io.write(OUT_TOTAL).set(0, total);
                self.0 = Some(total);
            }
        }
        fn spawn(&self) -> Box<dyn Operator> {
            Box::new(Steps(None))
        }
    }
}

pub mod words {
    use super::*;

    /// The last count sent.
    pub struct Words(pub Option<f32>);
    reuben_core::operator_contract!(Words {
        type_name: "words",
        inputs: { text: str(parse_words) },
        outputs: { count: f32 { 0.0..=1_000_000.0, default 0.0 } },
    });

    impl Operator for Words {
        fn descriptor() -> Descriptor {
            Self::contract()
        }
        fn process(&mut self, io: &mut Io) {
            let count = io
                .read(IN_TEXT)
                .and_then(|p| p.payload::<usize>())
                .map_or(-1.0, |n| *n as f32);
            if self.0 != Some(count) {
                io.write(OUT_COUNT).set(0, count);
                self.0 = Some(count);
            }
        }
        fn spawn(&self) -> Box<dyn Operator> {
            Box::new(Words(None))
        }
    }
}

/// Refuses empty text, so an input whose `""` is compiled fails to plan.
pub fn parse_nonempty(source: &str) -> Result<Prepared, PrepareError> {
    if source.is_empty() {
        return Err(PrepareError::new(0..0, "empty"));
    }
    Ok(Prepared::new(source.len()))
}

/// An Operator with no outputs whose input refuses `""`, its preparer named by `$path`.
macro_rules! nonempty_op {
    ($module:ident, $ty:ident, $name:literal, $path:path) => {
        pub mod $module {
            use super::*;
            pub struct $ty;
            reuben_core::operator_contract!($ty {
                type_name: $name,
                inputs: { text: str($path) },
                outputs: { },
            });
            impl Operator for $ty {
                fn descriptor() -> Descriptor {
                    Self::contract()
                }
                fn process(&mut self, _io: &mut Io) {}
                fn spawn(&self) -> Box<dyn Operator> {
                    Box::new($ty)
                }
            }
        }
    };
}

// Two Operators naming one preparer by its crate-rooted path, which gives them one kind; a third
// reaching it through `super::`, which gives it another.
nonempty_op!(
    nonempty,
    NonEmpty,
    "nonempty",
    crate::prepared::parse_nonempty
);
nonempty_op!(
    nonempty_too,
    NonEmptyToo,
    "nonempty_too",
    crate::prepared::parse_nonempty
);
nonempty_op!(
    nonempty_super,
    NonEmptySuper,
    "nonempty_super",
    super::parse_nonempty
);

/// The built-ins plus `steps`, `words` and the three `nonempty` Operators.
pub fn registry() -> Registry {
    let mut r = Registry::builtin();
    r.register(
        || Box::new(steps::Steps(None)),
        <steps::Steps as Operator>::descriptor(),
    );
    r.register(
        || Box::new(words::Words(None)),
        <words::Words as Operator>::descriptor(),
    );
    r.register(
        || Box::new(nonempty::NonEmpty),
        <nonempty::NonEmpty as Operator>::descriptor(),
    );
    r.register(
        || Box::new(nonempty_too::NonEmptyToo),
        <nonempty_too::NonEmptyToo as Operator>::descriptor(),
    );
    r.register(
        || Box::new(nonempty_super::NonEmptySuper),
        <nonempty_super::NonEmptySuper as Operator>::descriptor(),
    );
    r
}

/// `steps`'s preparer, as its descriptor declares it.
pub fn steps_preparer() -> Preparer {
    *<steps::Steps as Operator>::descriptor().inputs[steps::IN_STEPS.index()]
        .preparer()
        .expect("steps prepares its text")
}

/// `words`'s preparer, as its descriptor declares it.
pub fn words_preparer() -> Preparer {
    *<words::Words as Operator>::descriptor().inputs[words::IN_TEXT.index()]
        .preparer()
        .expect("words prepares its text")
}

/// `text` prepared for a `steps` input, as a producer hands it to the engine.
pub fn steps_arg(text: &str) -> Arg {
    prepared(steps_preparer(), text)
}

/// `text` prepared for a `words` input.
pub fn words_arg(text: &str) -> Arg {
    prepared(words_preparer(), text)
}

pub fn prepared(preparer: Preparer, text: &str) -> Arg {
    let source: Arc<str> = text.into();
    Arg::Prepared(preparer.prepare(&source).expect("test text compiles"))
}
