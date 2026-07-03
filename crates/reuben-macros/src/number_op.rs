//! `number_operator_contract!` — generate a family of pointwise number operators (ADR-0033).
//!
//! A *stateless pointwise* math op (output sample = fn of this sample's inputs only) whose operands
//! are numbers (and optionally held enum modes) is pure boilerplate apart from its scalar function
//! and its operand defaults. This macro takes that one scalar function plus an operand list and emits
//! the **whole** family across two axes:
//!
//! - `numbers` — the number type(s) the op supports (`f32` today; `i32`, … later). Each becomes a
//!   variant whose buffers/scalars are that concrete type.
//! - `carriers` — `value` (held scalars, output `set` once) and/or `signal` (per-sample buffers,
//!   output looped). Omit one for a single-form op.
//!
//! For each `numbers × carriers` pair it emits a submodule (isolating the `IN_`/`OUT_` consts) with
//! the contract, an empty stateless struct, the `Operator` impl whose `process` reads each operand
//! per the carrier and calls the shared scalar fn, `register_operator!`, and a contract-derived
//! `defaults_are_data` test. The struct is re-exported at the call site.
//!
//! ```ignore
//! number_operator_contract!(Add {
//!     numbers:  [f32],
//!     carriers: [value, signal],
//!     inputs:   { in_a: number { default 0.0 }, in_b: number { default 0.0 } },
//!     outputs:  { out },
//!     function: add_fn(in_a, in_b),
//! });
//! // -> add::AddF32Value (type_name "add_f32_value") + add::AddF32Signal ("add_f32_signal")
//! ```
//!
//! **Operand kinds.** A `number` operand follows the carrier (a per-sample buffer in `signal`, a held
//! scalar in `value`). An `enum(VocabType)` operand is *always held* (enums have no buffer form), in
//! both carriers. The scalar fn receives every operand by the names in the `function:` call-shape.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use reuben_contract::{naming, F32Meta, OperatorSpec, PortSpec, NUMBER_MAX, NUMBER_MIN};
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::{braced, bracketed, parenthesized, Error, Ident, Path, Token};

use crate::grammar::{parse_default_value, parse_float_or_sentinel, peek_range_sentinel};
use crate::model::{build, ContractModel};
use crate::render_contract;

/// The proc-macro body, over `proc_macro2` so it is unit-testable without a proc-macro context.
pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let parsed = match syn::parse2::<NumberOpInput>(input) {
        Ok(p) => p,
        Err(e) => return e.to_compile_error(),
    };
    parsed.render()
}

/// Which carrier a generated variant uses.
#[derive(Clone, Copy)]
enum Carrier {
    /// Held scalars in, one held scalar out (`set` once). Block-slicing keeps it sample-accurate.
    Value,
    /// Per-sample buffers in/out, the scalar fn looped over the block.
    Signal,
}

impl Carrier {
    /// `Value` -> `Value`, `Signal` -> `Signal` — the PascalCase fragment in the struct name.
    fn pascal(self) -> &'static str {
        match self {
            Carrier::Value => "Value",
            Carrier::Signal => "Signal",
        }
    }
}

/// One declared operand.
enum OperandKind {
    /// A number operand: follows the carrier. `default` falls back to the number type's zero; the
    /// range falls back to the type-wide [`NUMBER_MIN`]/[`NUMBER_MAX`].
    Number { min: f32, max: f32, default: f32 },
    /// A held enum mode operand, naming its shared `vocab` type. Always held, both carriers.
    Enum(Ident),
}

struct Operand {
    /// The operand name, un-raw'd (`in`, not `r#in`), so the contract/const names match.
    name: Ident,
    kind: OperandKind,
}

struct NumberOpInput {
    /// The op family base, e.g. `Add` — combined with number + carrier into `AddF32Value`.
    base: Ident,
    numbers: Vec<Ident>,
    carriers: Vec<Carrier>,
    inputs: Vec<Operand>,
    /// The single output's name (un-raw'd).
    output: Ident,
    /// The scalar fn path, e.g. `add_fn` or `std::ops::Add::add`.
    func: Path,
    /// The call-shape arg names, mapped to operand locals positionally into the fn.
    func_args: Vec<Ident>,
}

impl NumberOpInput {
    fn render(&self) -> TokenStream {
        let mut variants = Vec::new();
        for number in &self.numbers {
            for &carrier in &self.carriers {
                match self.render_variant(number, carrier) {
                    Ok(toks) => variants.push(toks),
                    Err(e) => return e.to_compile_error(),
                }
            }
        }
        quote! { #(#variants)* }
    }

    /// Build, validate, and render one `number × carrier` variant: its submodule + re-export.
    fn render_variant(&self, number: &Ident, carrier: Carrier) -> syn::Result<TokenStream> {
        let struct_name = format!(
            "{}{}{}",
            self.base,
            number.to_string().to_uppercase(),
            carrier.pascal()
        );
        let struct_ident = Ident::new(&struct_name, self.base.span());
        let type_name = naming::type_name_from_struct(&struct_name);
        let mod_ident = Ident::new(&type_name, self.base.span());

        let spec = self.to_spec(&type_name, carrier);
        if let Err(err) = reuben_contract::validate(&spec) {
            return Err(Error::new(self.base.span(), err.message));
        }
        let model = build(&spec);
        let contract = render_contract(&struct_ident, &model);

        let process = self.process_body(carrier, &model);
        let defaults_test = self.defaults_test(&struct_ident, &model);

        Ok(quote! {
            pub mod #mod_ident {
                use super::*;
                use ::reuben_core::descriptor::Descriptor;
                use ::reuben_core::operator::{Io, Operator};

                #contract

                /// A stateless pointwise number operator, generated by `number_operator_contract!`.
                #[derive(Default)]
                pub struct #struct_ident;

                impl #struct_ident {
                    pub fn new() -> Self {
                        Self
                    }
                }

                impl Operator for #struct_ident {
                    fn descriptor() -> Descriptor {
                        Self::contract()
                    }

                    fn process(&mut self, io: &mut Io) {
                        #process
                    }

                    fn spawn(&self) -> ::std::boxed::Box<dyn Operator> {
                        ::std::boxed::Box::new(Self::new())
                    }
                }

                crate::register_operator!(#struct_ident);

                #defaults_test
            }
            pub use #mod_ident::#struct_ident;
        })
    }

    /// The per-variant [`OperatorSpec`] (ports typed for this carrier), reusing the shared validator
    /// + builder so the contract is identical to a hand-written `operator_contract!`.
    fn to_spec(&self, type_name: &str, carrier: Carrier) -> OperatorSpec {
        let num_meta = |min: f32, max: f32, default: f32| F32Meta {
            min,
            max,
            default,
            unit: String::new(),
            curve: "linear".to_string(),
        };
        let inputs = self
            .inputs
            .iter()
            .map(|op| match &op.kind {
                OperandKind::Number { min, max, default } => {
                    let (ty, f32) = match carrier {
                        // Signal: a per-sample buffer carrying its scalar default + knob range.
                        Carrier::Signal => ("f32_buffer", Some(num_meta(*min, *max, *default))),
                        Carrier::Value => ("f32", Some(num_meta(*min, *max, *default))),
                    };
                    PortSpec {
                        name: op.name.to_string(),
                        ty: ty.to_string(),
                        f32,
                        i32: None,
                        vocab: None,
                    }
                }
                // Enum modes are held regardless of carrier.
                OperandKind::Enum(vocab) => PortSpec {
                    name: op.name.to_string(),
                    ty: "enum".to_string(),
                    f32: None,
                    i32: None,
                    vocab: Some(vocab.to_string()),
                },
            })
            .collect();
        let output = match carrier {
            // The signal output is a bare buffer; the value output is a held scalar with the
            // type-wide range so an unwired downstream still materialises a sane default.
            Carrier::Signal => PortSpec {
                name: self.output.to_string(),
                ty: "f32_buffer".to_string(),
                f32: None,
                i32: None,
                vocab: None,
            },
            Carrier::Value => PortSpec {
                name: self.output.to_string(),
                ty: "f32".to_string(),
                f32: Some(num_meta(NUMBER_MIN, NUMBER_MAX, 0.0)),
                i32: None,
                vocab: None,
            },
        };
        OperatorSpec {
            type_name: type_name.to_string(),
            inputs,
            outputs: vec![output],
            constants: Vec::new(),
            resources: Vec::new(),
        }
    }

    /// The `process` body for one carrier: read each operand through its typed handle (ADR-0037
    /// — a held read's default is the handle's declared default, a signal read is a length-n
    /// buffer indexed directly), call the shared scalar fn, write the output.
    fn process_body(&self, carrier: Carrier, model: &ContractModel) -> TokenStream {
        let out_const = Ident::new(&model.outputs[0].const_name, Span::call_site());
        let call = self.call();

        // Held reads (enum operands, always; number operands in the value carrier) come first.
        let mut held = Vec::new();
        let mut looped = Vec::new();
        for (op, port) in self.inputs.iter().zip(&model.inputs) {
            let local = raw(&op.name);
            let in_const = Ident::new(&port.const_name, Span::call_site());
            // The one per-sample read is a `number` operand in the signal carrier; enum modes
            // (always held) and value-carrier numbers alike are held reads of the handle default.
            match (&op.kind, carrier) {
                // The buffer-presence invariant (ADR-0037): a Signal input is always exactly
                // `frames` samples, so it indexes directly — no `.get(i).unwrap_or(..)`.
                (OperandKind::Number { .. }, Carrier::Signal) => looped.push(quote! {
                    let #local = io.read(#in_const)[i];
                }),
                _ => held.push(quote! {
                    let #local = io.read(#in_const);
                }),
            }
        }

        match carrier {
            Carrier::Value => quote! {
                #(#held)*
                io.write(#out_const).set(0, #call);
            },
            Carrier::Signal => quote! {
                let n = io.frames();
                #(#held)*
                for i in 0..n {
                    #(#looped)*
                    io.write(#out_const)[i] = #call;
                }
            },
        }
    }

    /// The scalar fn call, `func(arg, arg, ..)`, args referencing the operand locals (raw idents so
    /// a keyword-named operand like `in` is legal).
    fn call(&self) -> TokenStream {
        let func = &self.func;
        let args = self.func_args.iter().map(raw);
        quote! { #func( #(#args),* ) }
    }

    /// The contract-derived `defaults_are_data` test: every number operand's descriptor default
    /// equals its declared default. (A forgotten non-zero identity — e.g. a `mul` operand left at
    /// the zero fallback — fails here rather than silently zeroing patches.)
    fn defaults_test(&self, struct_ident: &Ident, model: &ContractModel) -> TokenStream {
        let checks = self
            .inputs
            .iter()
            .zip(&model.inputs)
            .filter_map(|(op, _)| match &op.kind {
                OperandKind::Number { default, .. } => {
                    let name = op.name.to_string();
                    let def = proc_macro2::Literal::f32_unsuffixed(*default);
                    Some(quote! {
                        let (_, meta) = d.settable_inputs()
                            .find(|(n, _)| *n == #name)
                            .expect(concat!(#name, " is a settable number operand"));
                        assert_eq!(meta.default, #def, #name);
                    })
                }
                OperandKind::Enum(_) => None,
            });
        quote! {
            #[cfg(test)]
            mod generated_defaults {
                use super::#struct_ident;
                use ::reuben_core::operator::Operator;
                #[test]
                fn defaults_are_data() {
                    let d = #struct_ident::descriptor();
                    #(#checks)*
                }
            }
        }
    }
}

/// A raw identifier for an operand-derived name, so a keyword operand (`in`) yields a legal local
/// (`r#in`). Raw form of a normal name (`in_a` -> `r#in_a`) refers to the same identifier.
fn raw(name: &Ident) -> Ident {
    Ident::new_raw(&name.unraw().to_string(), name.span())
}

// --- Parsing the macro grammar ---

impl Parse for NumberOpInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let base: Ident = input.parse()?;
        let body;
        braced!(body in input);

        let mut numbers = None;
        let mut carriers = None;
        let mut inputs = None;
        let mut output = None;
        let mut func = None;
        let mut func_args = None;

        while !body.is_empty() {
            let key: Ident = body.parse()?;
            body.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "numbers" => numbers = Some(parse_ident_array(&body)?),
                "carriers" => carriers = Some(parse_carriers(&body)?),
                "inputs" => inputs = Some(parse_operands(&body)?),
                "outputs" => output = Some(parse_single_output(&body)?),
                "function" => {
                    let (p, a) = parse_call_shape(&body)?;
                    func = Some(p);
                    func_args = Some(a);
                }
                other => {
                    return Err(Error::new(
                        key.span(),
                        format!("unknown field `{other}` (expected numbers/carriers/inputs/outputs/function)"),
                    ))
                }
            }
            if body.peek(Token![,]) {
                body.parse::<Token![,]>()?;
            }
        }

        let missing = |f: &str| Error::new(base.span(), format!("missing `{f}:` field"));
        Ok(NumberOpInput {
            base: base.clone(),
            numbers: numbers.ok_or_else(|| missing("numbers"))?,
            carriers: carriers.ok_or_else(|| missing("carriers"))?,
            inputs: inputs.ok_or_else(|| missing("inputs"))?,
            output: output.ok_or_else(|| missing("outputs"))?,
            func: func.ok_or_else(|| missing("function"))?,
            func_args: func_args.unwrap_or_default(),
        })
    }
}

/// `[f32, i32]` — a bracketed, comma-separated ident list.
fn parse_ident_array(input: ParseStream) -> syn::Result<Vec<Ident>> {
    let body;
    bracketed!(body in input);
    let mut out = Vec::new();
    while !body.is_empty() {
        out.push(body.parse::<Ident>()?);
        if body.peek(Token![,]) {
            body.parse::<Token![,]>()?;
        }
    }
    Ok(out)
}

/// `[value, signal]` — bracketed carrier keywords.
fn parse_carriers(input: ParseStream) -> syn::Result<Vec<Carrier>> {
    let body;
    bracketed!(body in input);
    let mut out = Vec::new();
    while !body.is_empty() {
        let kw: Ident = body.parse()?;
        out.push(match kw.to_string().as_str() {
            "value" => Carrier::Value,
            "signal" => Carrier::Signal,
            other => {
                return Err(Error::new(
                    kw.span(),
                    format!("carrier must be `value` or `signal`, got `{other}`"),
                ))
            }
        });
        if body.peek(Token![,]) {
            body.parse::<Token![,]>()?;
        }
    }
    Ok(out)
}

/// `{ in_a: number { default 0.0 }, curve: enum(MapCurve), .. }`. A `number` operand's `{ .. }`
/// meta is optional; inside it, `LO..=HI` and `default D` are each optional.
fn parse_operands(input: ParseStream) -> syn::Result<Vec<Operand>> {
    let body;
    braced!(body in input);
    let mut out = Vec::new();
    while !body.is_empty() {
        // `parse_any` so an operand may be a keyword, e.g. map's `in`.
        let name = Ident::parse_any(&body)?;
        body.parse::<Token![:]>()?;
        let kw = Ident::parse_any(&body)?;
        let kind = match kw.to_string().as_str() {
            "number" => parse_number_meta(&body)?,
            "enum" => {
                let inner;
                parenthesized!(inner in body);
                OperandKind::Enum(inner.parse::<Ident>()?)
            }
            other => {
                return Err(Error::new(
                    kw.span(),
                    format!("operand kind must be `number` or `enum(..)`, got `{other}`"),
                ))
            }
        };
        out.push(Operand { name, kind });
        if body.peek(Token![,]) {
            body.parse::<Token![,]>()?;
        }
    }
    Ok(out)
}

/// The optional `{ [LO..=HI,] [default D] }` on a `number` operand. Missing range -> type-wide
/// [`NUMBER_MIN`]/[`NUMBER_MAX`]; missing default -> `0.0` (the number type's zero). A range endpoint
/// may be written `min`/`max` (the type-wide sentinel), and `default` may be written `default max` /
/// `default min` to park the operand at its own range edge (issue #127) — e.g. `min`'s no-op `b`.
fn parse_number_meta(input: ParseStream) -> syn::Result<OperandKind> {
    let mut min = NUMBER_MIN;
    let mut max = NUMBER_MAX;
    let mut default = 0.0;
    if input.peek(syn::token::Brace) {
        let meta;
        braced!(meta in input);
        // Optional `LO..=HI` range. It starts with a number, `-`, or a `min`/`max` sentinel — but
        // not the `default` keyword, which introduces the default instead.
        if meta.peek(syn::LitInt)
            || meta.peek(syn::LitFloat)
            || meta.peek(Token![-])
            || peek_range_sentinel(&meta)
        {
            min = parse_float_or_sentinel(&meta)?;
            meta.parse::<Token![..=]>()?;
            max = parse_float_or_sentinel(&meta)?;
            if meta.peek(Token![,]) {
                meta.parse::<Token![,]>()?;
            }
        }
        // Optional `default D`, where `D` may be a literal or `max`/`min` (the operand's own range
        // extreme, resolved from the range parsed just above).
        if meta.peek(Ident) {
            let kw: Ident = meta.parse()?;
            if kw != "default" {
                return Err(Error::new(kw.span(), "expected `default <value>`"));
            }
            default = parse_default_value(&meta, min, max)?;
        }
        if meta.peek(Token![,]) {
            meta.parse::<Token![,]>()?;
        }
        if !meta.is_empty() {
            return Err(meta.error("unexpected tokens in number operand meta"));
        }
    }
    Ok(OperandKind::Number { min, max, default })
}

/// `{ out }` — a single output name (number, follows the carrier). More than one is rejected.
fn parse_single_output(input: ParseStream) -> syn::Result<Ident> {
    let body;
    braced!(body in input);
    let name = Ident::parse_any(&body)?;
    if body.peek(Token![,]) {
        body.parse::<Token![,]>()?;
    }
    if !body.is_empty() {
        return Err(body.error("number_operator_contract! supports a single output"));
    }
    Ok(name)
}

/// `add_fn(in_a, in_b)` — the scalar fn path plus its call-shape arg names.
fn parse_call_shape(input: ParseStream) -> syn::Result<(Path, Vec<Ident>)> {
    let func: Path = input.parse()?;
    let args;
    parenthesized!(args in input);
    let mut out = Vec::new();
    while !args.is_empty() {
        out.push(Ident::parse_any(&args)?);
        if args.peek(Token![,]) {
            args.parse::<Token![,]>()?;
        }
    }
    Ok((func, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str) -> String {
        let ts: TokenStream = src.parse().expect("token stream");
        expand(ts).to_string()
    }

    // A binary value+signal op expands to both variants: their modules, structs, snake type_names,
    // the shared scalar-fn call, and the re-exports.
    #[test]
    fn binary_op_emits_both_carriers() {
        let out = render(
            r#"Add {
                numbers:  [f32],
                carriers: [value, signal],
                inputs:   { in_a: number { default 0.0 }, in_b: number { default 0.0 } },
                outputs:  { out },
                function: add_fn(in_a, in_b),
            }"#,
        );
        assert!(out.contains("pub mod add_f32_value"), "{out}");
        assert!(out.contains("pub mod add_f32_signal"), "{out}");
        assert!(out.contains("pub struct AddF32Value"), "{out}");
        assert!(out.contains("pub struct AddF32Signal"), "{out}");
        assert!(out.contains("type_name : \"add_f32_value\""), "{out}");
        assert!(out.contains("type_name : \"add_f32_signal\""), "{out}");
        assert!(
            out.contains("pub use add_f32_value :: AddF32Value"),
            "{out}"
        );
        assert!(
            out.contains("pub use add_f32_signal :: AddF32Signal"),
            "{out}"
        );
        assert!(out.contains("add_fn (r#in_a , r#in_b)"), "{out}");
        assert!(out.contains("register_operator !"), "{out}");
    }

    // Value carrier reads held scalars and `set`s once; signal carrier loops per-sample buffers.
    #[test]
    fn carriers_read_and_write_per_their_kind() {
        let out = render(
            r#"Add {
                numbers:  [f32],
                carriers: [value, signal],
                inputs:   { in_a: number { default 0.0 }, in_b: number { default 0.0 } },
                outputs:  { out },
                function: add_fn(in_a, in_b),
            }"#,
        );
        // Value: held handle read (the declared default rides the handle) + a single set(0, ..).
        assert!(out.contains("let r#in_a = io . read (IN_IN_A) ;"), "{out}");
        assert!(
            out.contains("io . write (OUT_OUT) . set (0 ,")
                || out.contains("io . write (OUT_OUT) . set (0i32 ,"),
            "{out}"
        );
        // Signal: direct-indexed buffer read (the buffer-presence invariant — no
        // `.get(i).unwrap_or(..)` guard) + loop write through the handle.
        assert!(
            out.contains("let r#in_a = io . read (IN_IN_A) [i] ;"),
            "{out}"
        );
        assert!(out.contains("io . frames ()"), "{out}");
        assert!(out.contains("io . write (OUT_OUT) [i]"), "{out}");
        assert!(!out.contains("unwrap_or"), "{out}");
    }

    // An enum operand stays a held read in BOTH carriers (no buffer form), typed via its vocab.
    #[test]
    fn enum_operand_is_always_held() {
        let out = render(
            r#"Map {
                numbers:  [f32],
                carriers: [value, signal],
                inputs:   { in: number { default 0.0 }, curve: enum(MapCurve) },
                outputs:  { out },
                function: remap(in, curve),
            }"#,
        );
        // Held enum read appears in the signal variant too (no `[i]` indexing for the enum) —
        // the handle's `Held<MapCurve>` form makes it a held read in both carriers.
        assert!(
            out.contains("let r#curve = io . read (IN_CURVE) ;"),
            "{out}"
        );
        assert!(
            out.contains("form :: Held < :: reuben_core :: vocab :: MapCurve > >"),
            "{out}"
        );
        // The keyword operand `in` becomes a raw local `r#in`, passed to the fn.
        assert!(out.contains("r#in"), "{out}");
        assert!(out.contains("remap (r#in , r#curve)"), "{out}");
    }

    // `default max` / `default min` on a `number` operand park it at its own range edge — for a
    // range-less operand that is the type-wide ±1e6 sentinel (issue #127), so `min`/`max`'s no-op
    // `b` needs no raw literal.
    #[test]
    fn default_sentinel_parks_at_the_range_edge() {
        let out = render(
            r#"Min {
                numbers:  [f32],
                carriers: [value],
                inputs:   { a: number { default 0.0 }, b: number { default max } },
                outputs:  { out },
                function: min_fn(a, b),
            }"#,
        );
        // `b`'s default is the range maximum (1e6), materialized into its `f32` port meta and
        // carried by its typed handle (the held-read fallback — one datum, ADR-0037).
        assert!(out.contains("default : 1000000f32"), "{out}");
        assert!(out.contains("In :: new (1 , 1000000f32)"), "{out}");
    }

    // Omitting a carrier yields a single-form op (the stateful-op shape, e.g. value-less).
    #[test]
    fn single_carrier_emits_one_variant() {
        let out = render(
            r#"Thing {
                numbers:  [f32],
                carriers: [signal],
                inputs:   { in_a: number { default 0.0 } },
                outputs:  { out },
                function: id_fn(in_a),
            }"#,
        );
        assert!(out.contains("pub mod thing_f32_signal"), "{out}");
        assert!(!out.contains("thing_f32_value"), "{out}");
    }
}
