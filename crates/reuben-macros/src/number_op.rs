//! `number_operator_contract!` — generate a family of pointwise number operators.
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
//! the contract, a stateless op carrier (`AddF32SignalOp`) whose `ValueOp`/`SignalOp` impl names the
//! contract's handle consts and the shared scalar fn, a `pub type` aliasing that carrier to the
//! carrier's **shell**, `register_operator!`, and a contract-derived `defaults_are_data` test. The
//! alias is re-exported at the call site.
//!
//! (Plain code spans, not intra-doc links: this crate cannot depend on `reuben-core`, where
//! `operator::shell` lives — the dependency runs the other way.)
//!
//! **`process` is not emitted.** It belongs to the shell, written once per carrier — so the
//! per-sample loop exists in one place, where hoisting each operand's slice out of it lets LLVM
//! vectorize every signal-carrier op (issue #556).
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
use reuben_contract::{
    naming, Curve, F32Meta, OperatorSpec, PortSpec, PortTy, NUMBER_MAX, NUMBER_MIN,
};
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

        // The contract goes on the op carrier — the op's identity and arithmetic — not on the
        // public struct, which is now a shell alias; `AddF32SignalOp::contract()` is what the
        // shell's `descriptor()` returns. Named per variant rather than a bare `Op` because it is
        // the shell's type parameter, and so the thing that distinguishes one of the 26 monomorphic
        // `process` bodies from another: under `-Csymbol-mangling-version=v0` the symbol reads
        // `SignalShell<..::add_f32_signal::AddF32SignalOp>::process`. (The default legacy mangling
        // prints the *parameter* name, `SignalShell<Op>`, for every instantiation alike — so pass
        // v0 when disassembling or profiling these.)
        let op_ident = Ident::new(&format!("{struct_name}Op"), self.base.span());
        let contract = render_contract(&op_ident, &model);
        let op_impl = self.op_impl(&op_ident, number, carrier, &model);
        let shell = match carrier {
            Carrier::Value => quote! { ::reuben_core::operator::shell::ValueShell },
            Carrier::Signal => quote! { ::reuben_core::operator::shell::SignalShell },
        };

        let defaults_test = self.defaults_test(&struct_ident, &model);

        Ok(quote! {
            pub mod #mod_ident {
                use super::*;
                use ::reuben_core::descriptor::Descriptor;
                // `Operator` is in scope for `register_operator!`'s `<T>::descriptor`; `process`
                // and its `Io` are the shell's now, so neither is named here.
                use ::reuben_core::operator::Operator;

                #contract

                /// The op itself: which ports it reads and what arithmetic it runs. `process` is
                /// the shell's — see [`reuben_core::operator::shell`].
                pub struct #op_ident;

                #op_impl

                /// A stateless pointwise number operator, generated by
                /// `number_operator_contract!`: the op's arithmetic in the carrier's shell.
                pub type #struct_ident = #shell<#op_ident>;

                crate::register_operator!(#struct_ident);

                #defaults_test
            }
            pub use #mod_ident::#struct_ident;
        })
    }

    /// The `impl ValueOp` / `impl SignalOp` that binds the contract's handle consts and the
    /// declared scalar fn to the carrier's shell.
    ///
    /// `HANDLES` **is** the contract-emitted const tuple — `(IN_A, IN_B)` — so the port index the
    /// shell reads and the one the descriptor publishes are the same datum, as they were when
    /// `process` was emitted next to them.
    fn op_impl(
        &self,
        op_ident: &Ident,
        number: &Ident,
        carrier: Carrier,
        model: &ContractModel,
    ) -> TokenStream {
        let handle_tys = self.inputs.iter().map(|op| match (&op.kind, carrier) {
            (OperandKind::Number { .. }, Carrier::Signal) => quote! {
                ::reuben_core::operator::In<::reuben_core::operator::form::SignalF32>
            },
            (OperandKind::Number { .. }, Carrier::Value) => quote! {
                ::reuben_core::operator::In<::reuben_core::operator::form::Held<#number>>
            },
            // Enum modes are held regardless of carrier — enums have no buffer form.
            (OperandKind::Enum(vocab), _) => quote! {
                ::reuben_core::operator::In<
                    ::reuben_core::operator::form::Held<::reuben_core::vocab::#vocab>
                >
            },
        });
        let handles = model
            .inputs
            .iter()
            .map(|p| Ident::new(&p.const_name, Span::call_site()));
        let out_const = Ident::new(&model.outputs[0].const_name, Span::call_site());
        // The scalar fn's args, bound positionally out of the shell's value tuple.
        let locals = self.inputs.iter().map(|op| raw(&op.name));
        let call = self.call();

        let (trait_path, out_ty, extra) = match carrier {
            Carrier::Value => (
                quote! { ::reuben_core::operator::shell::ValueOp },
                quote! {
                    ::reuben_core::operator::Out<::reuben_core::operator::form::Held<#number>>
                },
                quote! { type Value = #number; },
            ),
            Carrier::Signal => (
                quote! { ::reuben_core::operator::shell::SignalOp },
                quote! {
                    ::reuben_core::operator::Out<::reuben_core::operator::form::SignalF32>
                },
                quote! {},
            ),
        };
        let ret = match carrier {
            Carrier::Value => quote! { #number },
            Carrier::Signal => quote! { f32 },
        };

        quote! {
            impl #trait_path for #op_ident {
                type Handles = ( #(#handle_tys,)* );
                #extra

                const HANDLES: Self::Handles = ( #(#handles,)* );
                const OUT: #out_ty = #out_const;

                #[inline]
                fn apply(
                    ( #(#locals,)* ): <Self::Handles as
                        ::reuben_core::operator::shell::Operands>::Vals
                ) -> #ret {
                    #call
                }

                fn descriptor() -> Descriptor {
                    Self::contract()
                }
            }
        }
    }

    /// The per-variant [`OperatorSpec`] (ports typed for this carrier), reusing the shared validator
    /// + builder so the contract is identical to a hand-written `operator_contract!`.
    fn to_spec(&self, type_name: &str, carrier: Carrier) -> OperatorSpec {
        let num_meta = |min: f32, max: f32, default: f32| F32Meta {
            min,
            max,
            default,
            unit: String::new(),
            curve: Curve::Linear,
        };
        let inputs = self
            .inputs
            .iter()
            .map(|op| {
                let ty = match &op.kind {
                    OperandKind::Number { min, max, default } => match carrier {
                        // Signal: a per-sample buffer carrying its scalar default + knob range.
                        Carrier::Signal => PortTy::F32Buffer(Some(num_meta(*min, *max, *default))),
                        Carrier::Value => PortTy::F32(num_meta(*min, *max, *default)),
                    },
                    // Enum modes are held regardless of carrier.
                    OperandKind::Enum(vocab) => PortTy::Enum(vocab.to_string()),
                };
                PortSpec {
                    name: op.name.to_string(),
                    ty,
                }
            })
            .collect();
        let output = match carrier {
            // The signal output is a bare buffer; the value output is a held scalar with the
            // type-wide range so an unwired downstream still materialises a sane default.
            Carrier::Signal => PortSpec {
                name: self.output.to_string(),
                ty: PortTy::F32Buffer(None),
            },
            Carrier::Value => PortSpec {
                name: self.output.to_string(),
                ty: PortTy::F32(num_meta(NUMBER_MIN, NUMBER_MAX, 0.0)),
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

    /// Just one variant's module, so a carrier-specific assertion cannot be satisfied by the
    /// *other* carrier's module — both are in the same expansion, and their emitted shapes differ
    /// only in the handle forms and the shell.
    fn variant<'a>(out: &'a str, type_name: &str) -> &'a str {
        let start = out
            .find(&format!("pub mod {type_name} "))
            .unwrap_or_else(|| panic!("no `pub mod {type_name}` in {out}"));
        let rest = &out[start..];
        let end = rest
            .find(&format!("pub use {type_name} ::"))
            .unwrap_or_else(|| panic!("no re-export terminating {type_name} in {out}"));
        &rest[..end]
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
        // The public type is the carrier's shell wrapping the module's `Op` — `process` lives on
        // the shell, so no `Operator` impl is emitted here at all.
        assert!(
            out.contains(
                "pub type AddF32Value = :: reuben_core :: operator :: shell :: ValueShell < AddF32ValueOp >"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "pub type AddF32Signal = :: reuben_core :: operator :: shell :: SignalShell < AddF32SignalOp >"
            ),
            "{out}"
        );
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

    // The carriers differ only in their operands' declared *form* and the shell that reads them:
    // a value operand is a held scalar, a signal operand a per-sample buffer. Neither variant
    // emits a `process` — that is the shell's, written once (issue #556).
    #[test]
    fn carriers_differ_only_in_form_and_shell() {
        let out = render(
            r#"Add {
                numbers:  [f32],
                carriers: [value, signal],
                inputs:   { in_a: number { default 0.0 }, in_b: number { default 0.0 } },
                outputs:  { out },
                function: add_fn(in_a, in_b),
            }"#,
        );
        let value = variant(&out, "add_f32_value");
        let signal = variant(&out, "add_f32_signal");

        // Value: held `f32` operands, held `f32` out, in the value shell.
        assert!(
            value.contains("form :: Held < f32 > >") && value.contains("ValueOp for AddF32ValueOp"),
            "{value}"
        );
        // Signal: buffer operands, buffer out, in the signal shell.
        assert!(
            signal.contains("form :: SignalF32 >")
                && signal.contains("SignalOp for AddF32SignalOp"),
            "{signal}"
        );
        assert!(!signal.contains("form :: Held"), "{signal}");

        // No emitted `process`: the whole body the macro used to write is gone.
        assert!(!out.contains("fn process"), "{out}");
        assert!(!out.contains("io . read"), "{out}");
        assert!(!out.contains("io . write"), "{out}");
    }

    // The shell reads through the contract's own handle consts, so the port index it reads and
    // the one the descriptor publishes are a single datum — the drift `operator_contract!` exists
    // to prevent, preserved now that `process` no longer sits next to the consts.
    #[test]
    fn handles_are_the_contract_consts() {
        let out = render(
            r#"Add {
                numbers:  [f32],
                carriers: [value],
                inputs:   { in_a: number { default 0.0 }, in_b: number { default 0.0 } },
                outputs:  { out },
                function: add_fn(in_a, in_b),
            }"#,
        );
        assert!(
            out.contains("const HANDLES : Self :: Handles = (IN_IN_A , IN_IN_B ,)"),
            "{out}"
        );
        assert!(
            out.contains("const OUT :") && out.contains("= OUT_OUT ;"),
            "{out}"
        );
        // No bare port ordinals in the op impl — the indices live only in the contract.
        assert!(!out.contains("In :: new (0 , 0f32) ,"), "{out}");
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
        // The enum operand is `Held<MapCurve>` in BOTH carriers — the value variant alone would
        // satisfy a bare `contains`, so pin the *signal* variant, where the enum sits in the same
        // operand tuple as a buffer. `ReadOperand` projects the two forms to one value tuple, so
        // no per-carrier grammar is needed to keep the enum held.
        let signal = variant(&out, "map_f32_signal");
        assert!(
            signal.contains("form :: Held < :: reuben_core :: vocab :: MapCurve > >"),
            "{signal}"
        );
        assert!(signal.contains("form :: SignalF32 >"), "{signal}");
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
        // carried by its typed handle (the held-read fallback — one datum).
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
