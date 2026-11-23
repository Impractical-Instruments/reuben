//! Mini-notation text → [`Ast`].
//!
//! A hand-written recursive-descent parser over bytes. Every error carries the byte range of the
//! offending text. Recursion is bounded by [`MAX_NESTING`], so hostile input cannot exhaust the
//! stack.

use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;
use core::ops::Range;

use crate::ast::{Ast, Expr, ExprKind, Step};
use crate::error::Error;
use crate::time::Rational;

/// Deepest bracket nesting (`[`, `<`, `{`, counted together) the parser accepts.
pub const MAX_NESTING: u32 = 16;
/// Most modifiers that wrap one step (`*`, `/`, `?`, `(k,n,r)`, counted together).
pub const MAX_MODIFIERS: u32 = 8;
/// Largest count for `!`, a `..` range, or euclid steps: each expands to that many steps.
pub const MAX_EXPANSION: i64 = 1024;
/// Most digits after a decimal point; keeps a literal's denominator at most `10^9`.
const MAX_DECIMALS: u32 = 9;
/// Octave of a note name written without one: `c` is `c4`, MIDI 60.
pub const DEFAULT_OCTAVE: i64 = 4;

/// Longest pattern text [`parse`] accepts, in bytes. Parse memory grows with the text, so this
/// bounds it; 64 KiB is far beyond any pattern a person writes.
pub const MAX_INPUT: usize = 64 * 1024;

/// Parse a mini-notation string.
pub fn parse(src: &str) -> Result<Ast, Error> {
    if src.len() > MAX_INPUT {
        return Err(Error::new(
            MAX_INPUT..src.len(),
            "pattern text longer than 64 KiB",
        ));
    }
    let mut p = Parser {
        src: src.as_bytes(),
        text: src,
        pos: 0,
        depth: 0,
        expanded: 0,
    };
    let layers = p.layers(None, 0)?;
    let root = match combine(layers, 0..src.len(), false) {
        Some(e) => e,
        None => Expr {
            kind: ExprKind::Rest,
            span: 0..src.len(),
        },
    };
    Ok(Ast { root })
}

struct Parser<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    depth: u32,
    /// Steps produced by `..` ranges so far; the one expansion the AST holds in full.
    expanded: usize,
}

/// The layers of one body (top level or inside a bracket), and which separator joined them.
struct Layers {
    layers: Vec<(Vec<Step>, Range<usize>)>,
    sep: Option<u8>,
}

/// Turn a body's layers into one expression: a sequence (or alternation) per layer, stacked or
/// chosen between when there are several. `None` for an empty body.
fn combine(body: Layers, span: Range<usize>, alt: bool) -> Option<Expr> {
    let mut exprs: Vec<Expr> = body
        .layers
        .into_iter()
        .map(|(steps, span)| Expr {
            kind: if alt {
                ExprKind::Alt(steps)
            } else {
                ExprKind::Seq(steps)
            },
            span,
        })
        .collect();
    match exprs.len() {
        0 => None,
        1 => exprs.pop(),
        _ => Some(Expr {
            kind: if body.sep == Some(b'|') {
                ExprKind::Choice(exprs)
            } else {
                ExprKind::Stack(exprs)
            },
            span,
        }),
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'#'
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, off: usize) -> Option<u8> {
        self.src.get(self.pos + off).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    /// The byte range of the character at `at`, which may be multi-byte UTF-8.
    fn char_span(&self, at: usize) -> Range<usize> {
        match self.text[at..].chars().next() {
            Some(c) => at..at + c.len_utf8(),
            None => at..at,
        }
    }

    fn err_here(&self, message: impl Into<alloc::string::String>) -> Error {
        Error::new(self.char_span(self.pos), message)
    }

    /// Parse layers until `close` (or the end of input when `close` is `None`). Leaves `pos` on
    /// the closer.
    fn layers(&mut self, close: Option<u8>, open: usize) -> Result<Layers, Error> {
        let mut out = Layers {
            layers: Vec::new(),
            sep: None,
        };
        loop {
            let layer = self.layer()?;
            self.skip_ws();
            let c = self.peek();
            match c {
                Some(s @ (b',' | b'|')) => {
                    if let Some(prev) = out.sep {
                        if prev != s {
                            return Err(self.err_here(
                                "',' and '|' cannot share a group; wrap one side in [ ]",
                            ));
                        }
                    }
                    let Some(layer) = layer else {
                        return Err(self.err_here(format!("nothing before '{}'", s as char)));
                    };
                    out.sep = Some(s);
                    out.layers.push(layer);
                    self.pos += 1;
                }
                _ if c == close => {
                    match layer {
                        Some(layer) => out.layers.push(layer),
                        None if out.sep.is_some() => {
                            let s = out.sep.unwrap_or(b',') as char;
                            return Err(self.err_here(format!("nothing after '{s}'")));
                        }
                        None => {}
                    }
                    return Ok(out);
                }
                None => {
                    let opener = self.src[open] as char;
                    return Err(Error::new(open..open + 1, format!("unclosed '{opener}'")));
                }
                Some(b) => {
                    let msg = match close {
                        Some(want) => format!("expected '{}', found '{}'", want as char, b as char),
                        None => format!("unmatched '{}'", b as char),
                    };
                    return Err(self.err_here(msg));
                }
            }
        }
    }

    /// One layer: steps, optionally split into `.`-separated groups. `None` if it is empty.
    #[allow(clippy::type_complexity)]
    fn layer(&mut self) -> Result<Option<(Vec<Step>, Range<usize>)>, Error> {
        let mut groups: Vec<(Vec<Step>, Range<usize>)> = Vec::new();
        let mut cur: Vec<Step> = Vec::new();
        let mut cur_start = None;
        let mut dotted = false;
        loop {
            self.skip_ws();
            match self.peek() {
                None | Some(b',' | b'|' | b']' | b'>' | b'}') => break,
                Some(b'.') => {
                    if matches!(self.peek_at(1), Some(b'0'..=b'9')) {
                        return Err(self.err_here("write a leading zero, as in 0.5"));
                    }
                    if self.peek_at(1) == Some(b'.') {
                        return Err(Error::new(
                            self.pos..self.pos + 2,
                            "'..' needs a bare integer on each side",
                        ));
                    }
                    if cur.is_empty() {
                        return Err(self.err_here("empty group before '.'"));
                    }
                    let start = cur_start.take().unwrap_or(self.pos);
                    let end = cur.last().map_or(self.pos, |s| s.expr.span.end);
                    groups.push((core::mem::take(&mut cur), start..end));
                    dotted = true;
                    self.pos += 1;
                }
                Some(_) => {
                    if cur_start.is_none() {
                        cur_start = Some(self.pos);
                    }
                    self.step(&mut cur)?;
                }
            }
        }
        if !dotted {
            if cur.is_empty() {
                return Ok(None);
            }
            let start = cur_start.unwrap_or(self.pos);
            let end = cur.last().map_or(self.pos, |s| s.expr.span.end);
            return Ok(Some((cur, start..end)));
        }
        if cur.is_empty() {
            return Err(self.err_here("empty group after '.'"));
        }
        let start = cur_start.unwrap_or(self.pos);
        let end = cur.last().map_or(self.pos, |s| s.expr.span.end);
        groups.push((cur, start..end));
        let span = groups[0].1.start..end;
        let steps = groups
            .into_iter()
            .map(|(steps, span)| Step {
                expr: Expr {
                    kind: ExprKind::Seq(steps),
                    span,
                },
                weight: Rational::ONE,
                copies: 1,
            })
            .collect();
        Ok(Some((steps, span)))
    }

    /// One step (with its modifiers), or a `_` / bare `!` that amends the previous step.
    /// One step, which must end at whitespace, a separator, or a closing bracket: `1-2`, `0~1`
    /// and `[a][b]` are errors rather than two steps.
    fn step(&mut self, cur: &mut Vec<Step>) -> Result<(), Error> {
        self.step_inner(cur)?;
        match self.peek() {
            None | Some(b' ' | b'\t' | b'\n' | b'\r' | b',' | b'|' | b'.' | b']' | b'>' | b'}') => {
                Ok(())
            }
            Some(_) => Err(self.err_here("separate steps with whitespace")),
        }
    }

    fn step_inner(&mut self, cur: &mut Vec<Step>) -> Result<(), Error> {
        let start = self.pos;
        match self.peek() {
            Some(b'_') => {
                self.pos += 1;
                let Some(last) = cur.last_mut() else {
                    return Err(Error::new(start..start + 1, "'_' needs a step before it"));
                };
                last.weight = last
                    .weight
                    .checked_add(Rational::ONE)
                    .ok_or_else(|| Error::new(start..start + 1, "weight too large"))?;
                last.expr.span.end = last.expr.span.end.max(start + 1);
                return Ok(());
            }
            Some(b'!') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(Error::new(
                        start..start + 1,
                        "attach the count to the step, as in 'a!3'",
                    ));
                }
                let Some(last) = cur.last_mut() else {
                    return Err(Error::new(start..start + 1, "'!' needs a step before it"));
                };
                last.copies += 1;
                if i64::from(last.copies) > MAX_EXPANSION {
                    return Err(Error::new(start..start + 1, "too many repeats"));
                }
                return Ok(());
            }
            _ => {}
        }
        let term = self.term()?;
        let bare_int = matches!(term.kind, ExprKind::Number(n) if n.denom() == 1);
        let (expr, weight, copies, modified) = self.modifiers(term)?;
        if bare_int && !modified {
            if let Some(range) = self.range(&expr)? {
                cur.extend(range);
                return Ok(());
            }
        }
        cur.push(Step {
            expr,
            weight,
            copies,
        });
        Ok(())
    }

    /// If `..` follows the integer `left`, consume the range and return its steps.
    fn range(&mut self, left: &Expr) -> Result<Option<Vec<Step>>, Error> {
        let ExprKind::Number(a) = left.kind else {
            return Ok(None);
        };
        let save = self.pos;
        self.skip_ws();
        if !(self.peek() == Some(b'.') && self.peek_at(1) == Some(b'.')) {
            self.pos = save;
            return Ok(None);
        }
        let dots = self.pos;
        self.pos += 2;
        self.skip_ws();
        if !matches!(self.peek(), Some(b'0'..=b'9' | b'-')) {
            return Err(Error::new(
                dots..dots + 2,
                "'..' needs a bare integer on each side",
            ));
        }
        let (b, bspan) = self.number()?;
        if b.denom() != 1 {
            return Err(Error::new(bspan, "range ends must be integers"));
        }
        if matches!(
            self.peek(),
            Some(b'*' | b'/' | b'!' | b'@' | b'?' | b'(' | b':')
        ) {
            return Err(self.err_here("wrap a range in [ ] to modify it"));
        }
        let (a, b) = (a.numer(), b.numer());
        let span = left.span.start..bspan.end;
        let len = (i128::from(b) - i128::from(a)).unsigned_abs() + 1;
        if len > MAX_EXPANSION as u128 {
            return Err(Error::new(span, "range too long"));
        }
        self.expanded += len as usize;
        if self.expanded > crate::compile::MAX_SIZE {
            return Err(Error::new(span, "ranges expand to too many steps"));
        }
        let dir: i64 = if b >= a { 1 } else { -1 };
        let steps = (0..len as i64)
            .map(|i| Step {
                expr: Expr {
                    kind: ExprKind::Number(Rational::int(a + dir * i)),
                    span: span.clone(),
                },
                weight: Rational::ONE,
                copies: 1,
            })
            .collect();
        Ok(Some(steps))
    }

    fn term(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let Some(c) = self.peek() else {
            return Err(self.err_here("expected a step"));
        };
        let kind = match c {
            b'~' => {
                self.pos += 1;
                ExprKind::Rest
            }
            b'-' if !matches!(self.peek_at(1), Some(b'0'..=b'9')) => {
                self.pos += 1;
                ExprKind::Rest
            }
            b'-' | b'0'..=b'9' => ExprKind::Number(self.number()?.0),
            b'a'..=b'z' | b'A'..=b'Z' => ExprKind::Note(self.note()?),
            b'[' | b'<' | b'{' => return self.bracket(c),
            b'"' => {
                return Err(self.err_here(
                    "'\"' is reserved for the function layer; mini-notation has no quotes",
                ))
            }
            b':' => return Err(self.err_here("sample index ':' is not supported yet")),
            b'%' => return Err(self.err_here("'%' only follows a number or '{ }'")),
            _ => {
                let span = self.char_span(self.pos);
                let shown = &self.text[span.clone()];
                return Err(Error::new(span, format!("unexpected '{shown}'")));
            }
        };
        Ok(Expr {
            kind,
            span: start..self.pos,
        })
    }

    fn bracket(&mut self, open: u8) -> Result<Expr, Error> {
        let start = self.pos;
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(Error::new(start..start + 1, "brackets nested too deeply"));
        }
        let close = match open {
            b'[' => b']',
            b'<' => b'>',
            _ => b'}',
        };
        self.pos += 1;
        let body = self.layers(Some(close), start)?;
        self.pos += 1;
        self.depth -= 1;
        let span = start..self.pos;
        if body.layers.is_empty() {
            return Err(Error::new(span, "empty brackets"));
        }
        if open != b'{' {
            // The brackets belong to the expression, so a modifier after them starts after `]`.
            let mut expr = combine(body, span.clone(), open == b'<').expect("non-empty body");
            expr.span = span;
            return Ok(expr);
        }
        if body.sep == Some(b'|') {
            return Err(Error::new(span, "'|' is not allowed inside { }"));
        }
        let mut steps = None;
        if self.peek() == Some(b'%') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err_here("expected a step count after '%'"));
            }
            let (n, nspan) = self.number()?;
            if !n.is_positive() {
                return Err(Error::new(nspan, "step count must be greater than zero"));
            }
            steps = Some(n);
        }
        Ok(Expr {
            kind: ExprKind::Poly {
                layers: body.layers.into_iter().map(|(s, _)| s).collect(),
                steps,
            },
            span: start..self.pos,
        })
    }

    /// Modifiers attached to a term. Returns the wrapped expression, the step's weight and copy
    /// count, and whether any modifier was present.
    fn modifiers(&mut self, mut expr: Expr) -> Result<(Expr, Rational, u32, bool), Error> {
        let start = expr.span.start;
        let mut weight = Rational::ONE;
        let mut copies: u32 = 1;
        let mut wraps = 0;
        let mut any = false;
        loop {
            let op = self.pos;
            let Some(c) = self.peek() else { break };
            match c {
                b'*' | b'/' => {
                    self.pos += 1;
                    if matches!(self.peek(), Some(b'<' | b'[' | b'{')) {
                        return Err(
                            self.err_here("patterned speed is not supported yet; use a number")
                        );
                    }
                    let (f, fspan) = self.number_arg()?;
                    if !f.is_positive() {
                        return Err(Error::new(fspan, "speed must be greater than zero"));
                    }
                    let speed = if c == b'*' {
                        ExprKind::Fast
                    } else {
                        ExprKind::Slow
                    };
                    expr = self.wrap(expr, start, &mut wraps, |e| speed(e, f))?;
                }
                b'?' => {
                    self.pos += 1;
                    if self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(b'0'..=b'9')) {
                        return Err(self.err_here("write a leading zero, as in ?0.5"));
                    }
                    let p = if matches!(self.peek(), Some(b'0'..=b'9')) {
                        let (p, pspan) = self.number_arg()?;
                        if p < Rational::ZERO || p > Rational::ONE {
                            return Err(Error::new(pspan, "probability must be between 0 and 1"));
                        }
                        p
                    } else {
                        Rational::new(1, 2).expect("1/2")
                    };
                    expr = self.wrap(expr, start, &mut wraps, |e| ExprKind::Degrade(e, p))?;
                }
                b'(' => {
                    let (pulses, steps, rotation) = self.euclid_args()?;
                    expr = self.wrap(expr, start, &mut wraps, |e| ExprKind::Euclid {
                        expr: e,
                        pulses,
                        steps,
                        rotation,
                    })?;
                }
                b'!' => {
                    self.pos += 1;
                    let extra = if matches!(self.peek(), Some(b'0'..=b'9')) {
                        let (n, nspan) = self.number()?;
                        if n.denom() != 1 || n.numer() < 1 {
                            return Err(Error::new(
                                nspan,
                                "repeat count must be a whole number of at least 1",
                            ));
                        }
                        n.numer() - 1
                    } else {
                        1
                    };
                    let total = i64::from(copies) + extra;
                    if total > MAX_EXPANSION {
                        return Err(Error::new(op..self.pos, "too many repeats"));
                    }
                    copies = total as u32;
                }
                b'@' => {
                    self.pos += 1;
                    let (w, wspan) = self.number_arg()?;
                    if !w.is_positive() {
                        return Err(Error::new(wspan, "weight must be greater than zero"));
                    }
                    weight = w;
                }
                b':' => return Err(self.err_here("sample index ':' is not supported yet")),
                _ => break,
            }
            any = true;
        }
        Ok((expr, weight, copies, any))
    }

    fn wrap(
        &self,
        expr: Expr,
        start: usize,
        wraps: &mut u32,
        f: impl FnOnce(Box<Expr>) -> ExprKind,
    ) -> Result<Expr, Error> {
        *wraps += 1;
        if *wraps > MAX_MODIFIERS {
            return Err(Error::new(
                start..self.pos,
                "too many modifiers on one step",
            ));
        }
        Ok(Expr {
            kind: f(Box::new(expr)),
            span: start..self.pos,
        })
    }

    /// A modifier's numeric argument: a number literal, never a pattern.
    fn number_arg(&mut self) -> Result<(Rational, Range<usize>), Error> {
        match self.peek() {
            Some(b'0'..=b'9' | b'-') => self.number(),
            Some(b'<' | b'[' | b'{') => {
                Err(self.err_here("patterned arguments are not supported yet; use a number"))
            }
            _ => Err(self.err_here("expected a number")),
        }
    }

    /// A number literal: optional `-`, digits, optional `.digits`, optional `%ratio`.
    fn number(&mut self) -> Result<(Rational, Range<usize>), Error> {
        let start = self.pos;
        let v = self.decimal()?;
        let mut v = v;
        if self.peek() == Some(b'%') && matches!(self.peek_at(1), Some(b'0'..=b'9')) {
            self.pos += 1;
            let d = self.decimal()?;
            v = v.checked_div(d).ok_or_else(|| {
                Error::new(start..self.pos, "ratio denominator is zero or too large")
            })?;
        }
        if let Some(b) = self.peek() {
            if is_word_byte(b) {
                return Err(self.err_here(format!("unexpected '{}' after a number", b as char)));
            }
        }
        Ok((v, start..self.pos))
    }

    fn decimal(&mut self) -> Result<Rational, Error> {
        let start = self.pos;
        let neg = self.peek() == Some(b'-');
        if neg {
            self.pos += 1;
        }
        if !matches!(self.peek(), Some(b'0'..=b'9')) {
            return Err(self.err_here("expected a digit"));
        }
        let too_large = |p: &Self| Error::new(start..p.pos, "number too large");
        let mut num: i64 = 0;
        while let Some(d @ b'0'..=b'9') = self.peek() {
            self.pos += 1;
            num = num
                .checked_mul(10)
                .and_then(|n| n.checked_add(i64::from(d - b'0')))
                .ok_or_else(|| too_large(self))?;
        }
        let mut den: i64 = 1;
        if self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(b'0'..=b'9')) {
            self.pos += 1;
            let mut places = 0;
            while let Some(d @ b'0'..=b'9') = self.peek() {
                self.pos += 1;
                places += 1;
                if places > MAX_DECIMALS {
                    return Err(Error::new(start..self.pos, "too many decimal places"));
                }
                num = num
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(i64::from(d - b'0')))
                    .ok_or_else(|| too_large(self))?;
                den *= 10;
            }
        }
        if neg {
            num = -num;
        }
        Rational::new(num, den).ok_or_else(|| too_large(self))
    }

    /// A note name: letter `a`–`g`, accidentals (`#`/`s` sharp, `b`/`f` flat), optional octave.
    fn note(&mut self) -> Result<u8, Error> {
        let start = self.pos;
        while matches!(self.peek(), Some(b) if is_word_byte(b)) {
            self.pos += 1;
        }
        let span = start..self.pos;
        let word = &self.src[span.clone()];
        let unknown = || {
            Error::new(
                span.clone(),
                format!(
                    "unknown word '{}'; atoms are numbers and note names",
                    &self.text[span.clone()]
                ),
            )
        };
        let pc: i64 = match word[0].to_ascii_lowercase() {
            b'c' => 0,
            b'd' => 2,
            b'e' => 4,
            b'f' => 5,
            b'g' => 7,
            b'a' => 9,
            b'b' => 11,
            _ => return Err(unknown()),
        };
        let mut i = 1;
        let mut acc: i64 = 0;
        while i < word.len() {
            match word[i] {
                b'#' | b's' => acc += 1,
                b'b' | b'f' => acc -= 1,
                _ => break,
            }
            i += 1;
        }
        let octave = if i == word.len() {
            // `-1` directly after the name is the octave below 0, so MIDI 0-11 can be written.
            if self.peek() == Some(b'-') && self.peek_at(1) == Some(b'1') {
                self.pos += 2;
                -1
            } else {
                DEFAULT_OCTAVE
            }
        } else {
            let digits = &word[i..];
            if digits.len() > 2 || !digits.iter().all(u8::is_ascii_digit) {
                return Err(unknown());
            }
            digits.iter().fold(0, |o, d| o * 10 + i64::from(d - b'0'))
        };
        let midi = (octave + 1) * 12 + pc + acc;
        u8::try_from(midi)
            .ok()
            .filter(|m| *m <= 127)
            .ok_or_else(|| Error::new(start..self.pos, "note outside the MIDI range 0-127"))
    }

    fn euclid_args(&mut self) -> Result<(u32, u32, i64), Error> {
        let open = self.pos;
        self.pos += 1;
        let mut args: Vec<(i64, Range<usize>)> = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'0'..=b'9' | b'-') => {}
                Some(b'<' | b'[' | b'{' | b'~') => {
                    return Err(self.err_here("patterned euclid arguments are not supported yet"))
                }
                None => return Err(Error::new(open..open + 1, "unclosed '('")),
                _ => return Err(self.err_here("expected an integer")),
            }
            let (n, nspan) = self.number()?;
            if n.denom() != 1 {
                return Err(Error::new(nspan, "euclid arguments must be integers"));
            }
            args.push((n.numer(), nspan));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                None => return Err(Error::new(open..open + 1, "unclosed '('")),
                _ => return Err(self.err_here("expected ',' or ')'")),
            }
        }
        let span = open..self.pos;
        if !(2..=3).contains(&args.len()) {
            return Err(Error::new(
                span,
                "euclid takes (pulses,steps) or (pulses,steps,rotation)",
            ));
        }
        let (k, kspan) = args[0].clone();
        let (n, nspan) = args[1].clone();
        let r = args.get(2).map_or(0, |a| a.0);
        if !(1..=MAX_EXPANSION).contains(&n) {
            return Err(Error::new(nspan, "euclid steps must be between 1 and 1024"));
        }
        if k < 0 {
            return Err(Error::new(
                kspan,
                "negative euclid pulses are not supported",
            ));
        }
        if k > n {
            return Err(Error::new(kspan, "more euclid pulses than steps"));
        }
        Ok((k as u32, n as u32, r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(src: &str) -> ExprKind {
        parse(src).unwrap().root.kind
    }

    fn only(src: &str) -> ExprKind {
        let mut kinds = seq_kinds(kind(src));
        assert_eq!(kinds.len(), 1);
        kinds.remove(0)
    }

    fn err(src: &str) -> Error {
        parse(src).unwrap_err()
    }

    fn int(n: i64) -> ExprKind {
        ExprKind::Number(Rational::int(n))
    }

    fn seq_kinds(k: ExprKind) -> Vec<ExprKind> {
        match k {
            ExprKind::Seq(steps) => steps.into_iter().map(|s| s.expr.kind).collect(),
            other => panic!("not a seq: {other:?}"),
        }
    }

    #[test]
    fn empty_is_rest() {
        assert_eq!(kind(""), ExprKind::Rest);
        assert_eq!(kind("  \n"), ExprKind::Rest);
    }

    #[test]
    fn numbers_are_exact() {
        assert_eq!(
            seq_kinds(kind("1 -2 2.75 4%2")),
            vec![
                int(1),
                int(-2),
                ExprKind::Number(Rational::new(11, 4).unwrap()),
                int(2),
            ]
        );
    }

    #[test]
    fn dash_is_rest_unless_a_digit_follows() {
        assert_eq!(
            seq_kinds(kind("- ~ -1")),
            vec![ExprKind::Rest, ExprKind::Rest, int(-1)]
        );
    }

    #[test]
    fn note_names() {
        assert_eq!(
            seq_kinds(kind("c4 eb3 f#4 cs5 c")),
            vec![
                ExprKind::Note(60),
                ExprKind::Note(51),
                ExprKind::Note(66),
                ExprKind::Note(73),
                ExprKind::Note(60),
            ]
        );
        assert_eq!(
            seq_kinds(kind("bb2 bf2 C4")),
            vec![ExprKind::Note(46), ExprKind::Note(46), ExprKind::Note(60)]
        );
    }

    #[test]
    fn octave_minus_one() {
        assert_eq!(
            seq_kinds(kind("c-1 b-1 cs-1")),
            vec![ExprKind::Note(0), ExprKind::Note(11), ExprKind::Note(1)]
        );
        assert!(err("cb-1").message.contains("MIDI range"));
        assert!(err("c-2").message.contains("whitespace"));
    }

    #[test]
    fn adjacent_steps_need_whitespace() {
        for (src, at) in [
            ("1-2", 1),
            ("0~1", 1),
            ("c4-1", 2),
            ("[0][1]", 3),
            ("0 1_", 3),
        ] {
            let e = err(src);
            assert!(e.message.contains("whitespace"), "{src}: {e}");
            assert_eq!(e.span.start, at, "{src}");
        }
        assert_eq!(seq_kinds(kind("[0] [1]")).len(), 2);
        assert!(matches!(kind("0 1, 2"), ExprKind::Stack(_)));
        assert!(matches!(kind("0 1. 2"), ExprKind::Seq(_)));
    }

    #[test]
    fn leading_dot_numbers_are_errors() {
        assert!(err("a?.5 b").message.contains("?0.5"));
        assert!(err("a .5").message.contains("0.5"));
    }

    #[test]
    fn unknown_words_are_errors() {
        let e = err("bd sd");
        assert_eq!(e.span, 0..2);
        assert!(e.message.contains("unknown word 'bd'"));
    }

    #[test]
    fn sample_index_is_a_clear_error() {
        let e = err("c4:3");
        assert_eq!(e.span, 2..3);
        assert!(e.message.contains("sample index"));
    }

    #[test]
    fn range_expands_to_steps() {
        assert_eq!(
            seq_kinds(kind("0 .. 3")),
            vec![int(0), int(1), int(2), int(3)]
        );
        assert_eq!(seq_kinds(kind("2..0")), vec![int(2), int(1), int(0)]);
        assert!(err("0 .. 3*2").message.contains("wrap a range"));
        assert!(err("0*2 .. 3").message.contains("'..'"));
    }

    #[test]
    fn elongate_and_replicate_amend_the_previous_step() {
        let ExprKind::Seq(steps) = kind("a@3 _ b!3 c ! !") else {
            panic!()
        };
        assert_eq!(steps[0].weight, Rational::int(4));
        assert_eq!(steps[1].copies, 3);
        assert_eq!(steps[2].copies, 3);
        assert!(err("_ a").message.contains("'_'"));
        assert!(err("! a").message.contains("'!'"));
    }

    #[test]
    fn mixing_comma_and_bar_is_an_error() {
        assert!(err("[a, b | c]").message.contains("cannot share"));
    }

    #[test]
    fn unclosed_and_unmatched_brackets() {
        assert_eq!(err("[a b").span, 0..1);
        assert_eq!(err("a b]").span, 3..4);
        assert!(err("[a b>").message.contains("expected ']'"));
        assert!(err("[]").message.contains("empty"));
    }

    #[test]
    fn input_length_is_bounded() {
        let long = "0 ".repeat(MAX_INPUT / 2 + 1);
        let e = err(&long);
        assert_eq!(e.span, MAX_INPUT..long.len());
        assert!(parse(&"0 ".repeat(MAX_INPUT / 2)).is_ok());
    }

    #[test]
    fn nesting_is_bounded() {
        let deep = "[".repeat(100) + &"]".repeat(100);
        assert!(err(&deep).message.contains("nested too deeply"));
    }

    #[test]
    fn errors_span_multibyte_chars() {
        let e = err("a é");
        assert_eq!(e.span, 2..4);
    }

    #[test]
    fn euclid_arguments() {
        let ExprKind::Seq(steps) = kind("a(3,8,-2) b") else {
            panic!()
        };
        assert!(matches!(
            steps[0].expr.kind,
            ExprKind::Euclid {
                pulses: 3,
                steps: 8,
                rotation: -2,
                ..
            }
        ));
        assert!(err("a(3,<8 4>)").message.contains("patterned"));
        assert!(err("a(9,8)").message.contains("more euclid pulses"));
        assert!(err("a(3)").message.contains("euclid takes"));
    }

    #[test]
    fn patterned_speed_is_a_clear_error() {
        assert!(err("a*<2 3>").message.contains("patterned"));
    }

    #[test]
    fn quote_is_reserved() {
        assert!(err("\"a b\"").message.contains("reserved"));
    }

    #[test]
    fn degrade_probability_bounds() {
        assert!(
            matches!(only("a?0.3"), ExprKind::Degrade(_, p) if p == Rational::new(3, 10).unwrap())
        );
        assert!(err("a?2").message.contains("probability"));
    }

    #[test]
    fn polymeter_steps() {
        let ExprKind::Poly { layers, steps } = only("{c d e, f g}%8") else {
            panic!()
        };
        assert_eq!(layers.len(), 2);
        assert_eq!(steps, Some(Rational::int(8)));
        assert!(err("{c | d}").message.contains("'|'"));
    }
}
