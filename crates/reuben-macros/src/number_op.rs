//! `number_operator_contract!` — generate a family of pointwise number operators.
//!
//! A *stateless pointwise* math op (output sample = fn of this sample's inputs only) whose operands
//! are numbers (and optionally held enum modes) is pure boilerplate apart from its scalar function
//! and its operand defaults. This macro takes that one scalar function plus an operand list and emits
//! the **whole** family from one `variants:` list.
//!
//! Each entry is `<number type> [-> <number type>] <carrier>` and names exactly one operator to
//! emit:
//!
//! - the **number type** (`f32`, `i32`) fixes the ports' scalar type and the type the scalar fn is
//!   instantiated at,
//! - the optional **`-> <number type>`** gives the *output* type where it differs from the input's
//!   — a **converter** (`f32 -> i32`), the one shape a single-type entry cannot express. Omitting
//!   it means "out is in", so no op whose arithmetic stays in one type writes one,
//! - the **carrier** is `value` (held scalars, output `set` once) or `signal` (per-sample buffers,
//!   output looped).
//!
//! It is a written list rather than a `numbers × carriers` cross product because the product is
//! **not** full: `i32` has no dense buffer form (`PortTy` has `F32Buffer` and no `I32Buffer` —
//! issue #560), so `i32 signal` does not exist and is rejected. A cross product would have to carry
//! a per-number carrier table to say so, and could not name a converter at all — a converter is not
//! a cell of `numbers × carriers` but a *pair* of number types. Listing the instantiations says
//! both directly, and a missing entry is a missing operator rather than a silently-skipped cell.
//!
//! The same rule rejects a bufferless type in **either** position, which is one statement covering
//! two facts: integer operators are value-only, and so is every converter that produces one.
//!
//! For each entry it emits a submodule (isolating the `IN_`/`OUT_` consts) with the contract, a
//! stateless op carrier (`AddF32SignalOp`) whose `ValueOp`/`SignalOp` impl names the contract's
//! handle consts and the shared scalar fn, a `pub type` aliasing that carrier to the carrier's
//! **shell**, `register_operator!`, and a contract-derived `defaults_are_data` test. The alias is
//! re-exported at the call site.
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
//!     variants: [f32 value, f32 signal, i32 value],
//!     inputs:   { in_a: number { default 0 }, in_b: number { default 0 } },
//!     outputs:  { out },
//!     function: add_fn(in_a, in_b),
//! });
//! // -> add::AddF32Value ("add_f32_value") + add::AddF32Signal ("add_f32_signal")
//! //  + add::AddI32Value ("add_i32_value")
//!
//! number_operator_contract!(Round {
//!     variants: [f32 value, f32 signal, f32 -> i32 value],
//!     inputs:   { x: number { default 0 } },
//!     outputs:  { out },
//!     function: round_fn(x),
//! });
//! // -> round::RoundF32Value + round::RoundF32Signal
//! //  + round::RoundF32I32Value ("round_f32_i32_value")
//! ```
//!
//! The out fragment appears in the name **only where the types differ**, so `add_f32_value` does
//! not become `add_f32_f32_value`. The type name is the operator's identity on the wire; restating
//! the matching case would migrate every instrument document in existence.
//!
//! **Operand kinds.** A `number` operand follows the carrier (a per-sample buffer in `signal`, a held
//! scalar in `value`). An `enum(VocabType)` operand is *always held* (enums have no buffer form), in
//! both carriers. The scalar fn receives every operand by the names in the `function:` call-shape.
//!
//! **One operand declaration serves every variant.** A declared range or `default` is written
//! type-neutrally and projected per number type, so an operand's `default 1` is `1.0` in the `f32`
//! instantiations and `1` in the `i32` ones. A value that cannot survive the projection — a
//! fractional `default 0.5` on an op that also lists an `i32` variant — is a compile error at the
//! operand, not a silent truncation.
//!
//! **What restricts an op to a subset of the number types is the scalar fn's own bounds.** `power`
//! lists only `f32` entries because its `shape` is a concrete `f32` fn; listing `i32 value` for it
//! fails to compile at the call, which is the point (issue #556). The same holds across the arrow:
//! a converter entry is legal only where the fn can *produce* the output type, so `round`'s
//! `f32 -> i32` compiles because `RoundInto<i32> for f32` exists, and an unimplemented pairing is
//! a missing-impl error rather than a wrongly-typed operator.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use reuben_contract::{
    naming, Curve, F32Meta, I32Meta, OperatorSpec, PortSpec, PortTy, NUMBER_MAX, NUMBER_MAX_I32,
    NUMBER_MIN, NUMBER_MIN_I32,
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
#[derive(Clone, Copy, PartialEq, Eq)]
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

/// The number type a variant instantiates at — the ports' scalar type and the type the shared
/// scalar fn is called at.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NumKind {
    F32,
    I32,
}

impl NumKind {
    /// The PascalCase fragment in the struct name — `AddF32Value`'s `F32`.
    fn pascal(self) -> &'static str {
        match self {
            NumKind::F32 => "F32",
            NumKind::I32 => "I32",
        }
    }

    /// The Rust type the handles and the scalar fn are written at.
    fn ty(self) -> TokenStream {
        match self {
            NumKind::F32 => quote! { f32 },
            NumKind::I32 => quote! { i32 },
        }
    }

    /// Whether this type has a dense per-sample buffer form, and so a `signal` carrier. Only `f32`
    /// does: [`PortTy`] has `F32Buffer` and no integer counterpart (issue #560).
    fn has_buffer(self) -> bool {
        matches!(self, NumKind::F32)
    }

    /// The type-wide default range — the `min`/`max` grammar sentinel resolved at this type.
    fn type_wide_range(self) -> (f64, f64) {
        match self {
            NumKind::F32 => (f64::from(NUMBER_MIN), f64::from(NUMBER_MAX)),
            NumKind::I32 => (f64::from(NUMBER_MIN_I32), f64::from(NUMBER_MAX_I32)),
        }
    }

    /// Project a type-neutral operand bound/default onto this number type's port meta, as one
    /// `(min, max, default)` triple. `Err` when the declared value has no exact representation
    /// here — a fractional literal in an `i32` instantiation, which `as i32` would silently
    /// truncate. `at` locates the error on the operand that declared it.
    fn meta(self, at: Span, (min, max, default): (f64, f64, f64)) -> syn::Result<PortTy> {
        Ok(match self {
            NumKind::F32 => PortTy::F32(F32Meta {
                min: min as f32,
                max: max as f32,
                default: default as f32,
                unit: String::new(),
                curve: Curve::Linear,
            }),
            NumKind::I32 => PortTy::I32(I32Meta {
                min: exact_i32(at, min)?,
                max: exact_i32(at, max)?,
                default: exact_i32(at, default)?,
            }),
        })
    }
}

/// A declared operand bound/default, narrowed to `i32` only if it lands exactly on one. The
/// alternative — `as i32` — turns a `default 0.5` into `0` with no diagnostic, which is precisely
/// the class of silent miscompile the `numbers:` axis used to produce (issue #556).
fn exact_i32(at: Span, v: f64) -> syn::Result<i32> {
    if v.fract() != 0.0 || v < f64::from(i32::MIN) || v > f64::from(i32::MAX) {
        return Err(Error::new(
            at,
            format!(
                "`{v}` is not an `i32`: this operand's bound/default must be a whole number \
                 within i32 for the op's `i32` variants (drop the `i32` entries from `variants:` \
                 if the op is float-only)"
            ),
        ));
    }
    Ok(v as i32)
}

/// One entry of `variants:` — the operator to emit, as `<number type> [-> <number type>]
/// <carrier>`.
#[derive(Clone, Copy)]
struct Variant {
    /// The type the input ports carry, and the type the scalar fn's operands are read at.
    num_in: NumKind,
    /// The type the output port carries, and so the type the scalar fn is instantiated to
    /// return. Equal to `num_in` for every op whose arithmetic stays in one type; different only
    /// for a **converter** (`f32 -> i32`), which is the case a single-type entry cannot express.
    num_out: NumKind,
    carrier: Carrier,
}

impl Variant {
    /// The `<in><out?>` fragment pair in the struct name — `F32` where the types match,
    /// `F32I32` where they differ.
    ///
    /// The out fragment is **omitted** when the types are equal, so `add_f32_value` does not
    /// become `add_f32_f32_value`. That is not cosmetic: the type name is the operator's identity
    /// on the wire, so restating it would migrate every instrument document in existence.
    fn name_fragment(self) -> String {
        if self.num_in == self.num_out {
            self.num_in.pascal().to_string()
        } else {
            format!("{}{}", self.num_in.pascal(), self.num_out.pascal())
        }
    }
}

/// One declared operand.
enum OperandKind {
    /// A number operand: follows the carrier. `default` falls back to the number type's zero; the
    /// range falls back to the type-wide sentinel ([`NUMBER_MIN`]/[`NUMBER_MAX`] at `f32`).
    ///
    /// Held as `f64` — **type-neutral**, because one operand declaration serves every entry in
    /// `variants:`, which may instantiate at more than one number type. The projection onto a
    /// concrete type happens per variant, in [`NumKind::meta`], where a value that does not fit
    /// that type is an error rather than a silent cast. `f64` is exact for every `f32` literal and
    /// every `i32`, so the neutral form loses nothing on the way through.
    Number { min: f64, max: f64, default: f64 },
    /// A held enum mode operand, naming its shared `vocab` type. Always held, both carriers.
    Enum(Ident),
}

struct Operand {
    /// The operand name, un-raw'd (`in`, not `r#in`), so the contract/const names match.
    name: Ident,
    kind: OperandKind,
}

struct NumberOpInput {
    /// The op family base, e.g. `Add` — combined with each variant's number + carrier into
    /// `AddF32Value`.
    base: Ident,
    /// The operators to emit, one per entry — see the module doc on why this is a written list
    /// rather than a `numbers × carriers` product.
    variants: Vec<Variant>,
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
        let mut rendered = Vec::new();
        for &v in &self.variants {
            match self.render_variant(v) {
                Ok(toks) => rendered.push(toks),
                Err(e) => return e.to_compile_error(),
            }
        }
        quote! { #(#rendered)* }
    }

    /// Build, validate, and render one variant: its submodule + re-export.
    fn render_variant(&self, v: Variant) -> syn::Result<TokenStream> {
        let struct_name = format!("{}{}{}", self.base, v.name_fragment(), v.carrier.pascal());
        let struct_ident = Ident::new(&struct_name, self.base.span());
        let type_name = naming::type_name_from_struct(&struct_name);
        let mod_ident = Ident::new(&type_name, self.base.span());

        let spec = self.to_spec(&type_name, v)?;
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
        let op_impl = self.op_impl(&op_ident, v, &model);
        let shell = match v.carrier {
            Carrier::Value => quote! { ::reuben_core::operator::shell::ValueShell },
            Carrier::Signal => quote! { ::reuben_core::operator::shell::SignalShell },
        };

        let defaults_test = self.defaults_test(&struct_ident);

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
    fn op_impl(&self, op_ident: &Ident, v: Variant, model: &ContractModel) -> TokenStream {
        // The operands are read at the input type and the result produced at the output type.
        // For every op but a converter these are the same token.
        let number = v.num_in.ty();
        let number_out = v.num_out.ty();
        let handle_tys = self.inputs.iter().map(|op| match (&op.kind, v.carrier) {
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

        let (trait_path, out_ty, extra) = match v.carrier {
            Carrier::Value => (
                quote! { ::reuben_core::operator::shell::ValueOp },
                quote! {
                    ::reuben_core::operator::Out<::reuben_core::operator::form::Held<#number_out>>
                },
                quote! { type Value = #number_out; },
            ),
            Carrier::Signal => (
                quote! { ::reuben_core::operator::shell::SignalOp },
                quote! {
                    ::reuben_core::operator::Out<::reuben_core::operator::form::SignalF32>
                },
                quote! {},
            ),
        };
        // The signal carrier is `f32` on both sides by construction — it is the only type with a
        // buffer form, which is why `variants:` rejects a bufferless type in either position.
        let ret = match v.carrier {
            Carrier::Value => number_out.clone(),
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

    /// The per-variant [`OperatorSpec`] — ports typed for **this variant's number type and
    /// carrier** — reusing the shared validator + builder so the contract is identical to a
    /// hand-written `operator_contract!`.
    ///
    /// This is where the declared number type reaches the ports. It used to hardcode `F32Meta` /
    /// `PortTy::F32` / `PortTy::F32Buffer` and never read the declared type at all, so a
    /// non-`f32` entry generated an operator *named* for that type whose every port was `f32` —
    /// a well-formed contract, wrong on the wire, caught by nothing (issue #556).
    fn to_spec(&self, type_name: &str, v: Variant) -> syn::Result<OperatorSpec> {
        let inputs = self
            .inputs
            .iter()
            .map(|op| {
                let ty = match &op.kind {
                    OperandKind::Number { min, max, default } => match v.carrier {
                        // Signal: a per-sample buffer carrying its scalar default + knob range.
                        // `f32` by construction — `variants:` rejects a bufferless type here.
                        Carrier::Signal => PortTy::F32Buffer(Some(F32Meta {
                            min: *min as f32,
                            max: *max as f32,
                            default: *default as f32,
                            unit: String::new(),
                            curve: Curve::Linear,
                        })),
                        Carrier::Value => v.num_in.meta(op.name.span(), (*min, *max, *default))?,
                    },
                    // Enum modes are held regardless of carrier.
                    OperandKind::Enum(vocab) => PortTy::Enum(vocab.to_string()),
                };
                Ok(PortSpec {
                    name: op.name.to_string(),
                    ty,
                })
            })
            .collect::<syn::Result<Vec<_>>>()?;
        // The signal output is a bare buffer; the value output is a held scalar with the type-wide
        // range so an unwired downstream still materialises a sane default. Zero is the identity
        // the range is centred on at either number type.
        let output = match v.carrier {
            Carrier::Signal => PortSpec {
                name: self.output.to_string(),
                ty: PortTy::F32Buffer(None),
            },
            Carrier::Value => {
                let (min, max) = v.num_out.type_wide_range();
                PortSpec {
                    name: self.output.to_string(),
                    ty: v.num_out.meta(self.output.span(), (min, max, 0.0))?,
                }
            }
        };
        Ok(OperatorSpec {
            type_name: type_name.to_string(),
            inputs,
            outputs: vec![output],
            constants: Vec::new(),
            resources: Vec::new(),
        })
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
    ///
    /// Asserted through [`Port::number_default`], which reads whichever meta slot this variant's
    /// number type uses — so the one test body serves every variant, and an operand whose declared
    /// default did *not* survive the projection onto the port would fail here as a mismatch even if
    /// [`NumKind::meta`] had let it through.
    fn defaults_test(&self, struct_ident: &Ident) -> TokenStream {
        let checks = self.inputs.iter().filter_map(|op| match &op.kind {
            OperandKind::Number { default, .. } => {
                let name = op.name.to_string();
                // The declared default in the neutral `f64` the accessor answers in, so the
                // comparison does not depend on which number type this variant instantiated at.
                let def = proc_macro2::Literal::f64_unsuffixed(*default);
                Some(quote! {
                    let p = d.inputs.iter()
                        .find(|p| p.name == #name)
                        .expect(concat!(#name, " is a declared input"));
                    assert_eq!(
                        p.number_default(),
                        Some(#def),
                        concat!(#name, " default is contract data"),
                    );
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

        let mut variants = None;
        let mut inputs = None;
        let mut output = None;
        let mut func = None;
        let mut func_args = None;

        while !body.is_empty() {
            let key: Ident = body.parse()?;
            body.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "variants" => variants = Some(parse_variants(&body)?),
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
                        format!(
                            "unknown field `{other}` (expected variants/inputs/outputs/function)"
                        ),
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
            variants: variants.ok_or_else(|| missing("variants"))?,
            inputs: inputs.ok_or_else(|| missing("inputs"))?,
            output: output.ok_or_else(|| missing("outputs"))?,
            func: func.ok_or_else(|| missing("function"))?,
            func_args: func_args.unwrap_or_default(),
        })
    }
}

/// `[f32 value, f32 signal, i32 value, f32 -> i32 value]` — the operators to emit, one per entry.
///
/// Each entry is `<number type> [-> <number type>] <carrier>`. The arrow gives the **output**
/// type where it differs from the input's — a converter. Omitting it means "out is in", which is
/// every op whose arithmetic stays in one type, so no shipping declaration has to write one and
/// no generated name changes.
///
/// An entry naming a `signal` carrier for a type with no buffer form is rejected here rather than
/// emitted: that is the sparsity the old `numbers × carriers` product could not express, and
/// emitting it would produce an operator whose name promises an integer signal the engine has no
/// way to carry. The check reads **both** positions, which is the same statement saying integer
/// operators are value-only and converters are value-only.
fn parse_variants(input: ParseStream) -> syn::Result<Vec<Variant>> {
    let body;
    bracketed!(body in input);
    let mut out: Vec<Variant> = Vec::new();
    while !body.is_empty() {
        let (num_in, in_kw) = parse_num_kind(&body)?;
        // `-> <type>` marks a converter; without it the output type is the input's.
        let (num_out, out_kw) = if body.peek(Token![->]) {
            body.parse::<Token![->]>()?;
            parse_num_kind(&body)?
        } else {
            (num_in, in_kw.clone())
        };
        let car_kw: Ident = body.parse()?;
        let carrier = match car_kw.to_string().as_str() {
            "value" => Carrier::Value,
            "signal" => Carrier::Signal,
            other => {
                return Err(Error::new(
                    car_kw.span(),
                    format!("carrier must be `value` or `signal`, got `{other}`"),
                ))
            }
        };
        // The signal carrier is per-sample buffers on *both* sides, so both types need a dense
        // buffer form. Reported against the offending type's own token, so a converter's error
        // points at the half that has no buffer rather than at the entry as a whole.
        if matches!(carrier, Carrier::Signal) {
            for (num, kw) in [(num_in, &in_kw), (num_out, &out_kw)] {
                if !num.has_buffer() {
                    return Err(Error::new(
                        kw.span(),
                        format!(
                            "`{kw}` has no `signal` carrier: it has no dense buffer form, so \
                             there is nothing for a per-sample port to carry (issue #560). \
                             Integer operators — and every converter producing one — are \
                             value-only; drop this entry and keep the `value` one."
                        ),
                    ));
                }
            }
        }
        // Two entries for the same operator would emit the same module and `register_operator!`
        // twice — a duplicate-symbol error far from its cause.
        if out
            .iter()
            .any(|v| v.num_in == num_in && v.num_out == num_out && v.carrier == carrier)
        {
            return Err(Error::new(
                in_kw.span(),
                format!("duplicate variant `{in_kw} {car_kw}`"),
            ));
        }
        out.push(Variant {
            num_in,
            num_out,
            carrier,
        });
        if body.peek(Token![,]) {
            body.parse::<Token![,]>()?;
        }
    }
    if out.is_empty() {
        return Err(input.error("`variants:` must name at least one operator to emit"));
    }
    Ok(out)
}

/// One number-type keyword of a `variants:` entry, returned with its token so an error about the
/// type can be spanned to the position that wrote it (input or output).
fn parse_num_kind(input: ParseStream) -> syn::Result<(NumKind, Ident)> {
    let kw: Ident = input.parse()?;
    let kind = match kw.to_string().as_str() {
        "f32" => NumKind::F32,
        "i32" => NumKind::I32,
        other => {
            return Err(Error::new(
                kw.span(),
                format!("number type must be `f32` or `i32`, got `{other}`"),
            ))
        }
    };
    Ok((kind, kw))
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
/// [`NUMBER_MIN`]/[`NUMBER_MAX`]; missing default -> `0` (the number type's zero). A range endpoint
/// may be written `min`/`max` (the type-wide sentinel), and `default` may be written `default max` /
/// `default min` to park the operand at its own range edge (issue #127) — e.g. `min`'s no-op `b`.
///
/// Answers in the type-neutral `f64`: this one declaration serves every entry in `variants:`, so
/// the number type is not known here. An omitted range falls back to the `f32` sentinel and stays
/// correct at every type, because [`NUMBER_MIN_I32`] is *derived from* [`NUMBER_MIN`] — projecting
/// the fallback onto `i32` lands exactly on the integer sentinel rather than merely near it.
fn parse_number_meta(input: ParseStream) -> syn::Result<OperandKind> {
    let mut min = f64::from(NUMBER_MIN);
    let mut max = f64::from(NUMBER_MAX);
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
                variants: [f32 value, f32 signal],
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
                variants: [f32 value, f32 signal],
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
                variants: [f32 value],
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
                variants: [f32 value, f32 signal],
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
                variants: [f32 value],
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
                variants: [f32 signal],
                inputs:   { in_a: number { default 0.0 } },
                outputs:  { out },
                function: id_fn(in_a),
            }"#,
        );
        assert!(out.contains("pub mod thing_f32_signal"), "{out}");
        assert!(!out.contains("thing_f32_value"), "{out}");
    }

    // **The bug this macro had**: the declared number type reached the struct name and nothing
    // else, so an `i32` entry emitted an operator *named* `add_i32_value` whose every port was
    // `f32` — a well-formed contract, wrong on the wire, caught by nothing (issue #556). Pin the
    // whole path from the declaration to the ports: the port type, the handle form, the emitted
    // default literal, and the type the scalar fn is called at.
    #[test]
    fn i32_variant_types_its_ports_i32() {
        let out = render(
            r#"Add {
                variants: [f32 value, i32 value],
                inputs:   { a: number { default 0 }, b: number { default 0 } },
                outputs:  { out },
                function: add_fn(a, b),
            }"#,
        );
        let i32v = variant(&out, "add_i32_value");

        // Ports: `Port::i32` with an `I32Meta`, and nowhere an `F32Meta` — the specific
        // miscompile was an i32-named operator publishing f32 ports.
        assert!(i32v.contains("Port :: i32 (\"a\""), "{i32v}");
        assert!(i32v.contains("I32Meta"), "{i32v}");
        assert!(!i32v.contains("F32Meta"), "{i32v}");
        assert!(!i32v.contains("Port :: f32"), "{i32v}");

        // Handles: held `i32`, carrying an `i32`-suffixed default literal (not `0f32`).
        assert!(i32v.contains("form :: Held < i32 > >"), "{i32v}");
        assert!(i32v.contains("In :: new (0 , 0i32)"), "{i32v}");
        // The scalar fn is instantiated at i32 through the output type, so a fn whose bounds
        // exclude i32 fails to compile at the call site rather than silently running at f32.
        assert!(i32v.contains("type Value = i32 ;"), "{i32v}");

        // The f32 sibling in the same expansion is untouched — one operand declaration, projected
        // per variant.
        let f32v = variant(&out, "add_f32_value");
        assert!(f32v.contains("form :: Held < f32 > >"), "{f32v}");
        assert!(f32v.contains("In :: new (0 , 0f32)"), "{f32v}");
    }

    // `i32` has no dense buffer form, so `i32 signal` names an operator the engine could not
    // carry. Rejected at the parse — the sparsity a `numbers × carriers` product could not
    // express without a per-number carrier table.
    #[test]
    fn i32_signal_is_rejected() {
        let out = render(
            r#"Add {
                variants: [i32 signal],
                inputs:   { a: number { default 0 }, b: number { default 0 } },
                outputs:  { out },
                function: add_fn(a, b),
            }"#,
        );
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("has no `signal` carrier"), "{out}");
    }

    // A declared bound/default that is not a whole number cannot be an `i32` port's. Erroring
    // beats `as i32`, which would turn `default 0.5` into `0` with no diagnostic — the same class
    // of silent miscompile the axis used to produce.
    #[test]
    fn fractional_default_is_rejected_for_an_i32_variant() {
        let out = render(
            r#"Add {
                variants: [f32 value, i32 value],
                inputs:   { a: number { default 0.5 }, b: number { default 0 } },
                outputs:  { out },
                function: add_fn(a, b),
            }"#,
        );
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("is not an `i32`"), "{out}");
    }

    // The same operand declaration serves every variant, so a range written once lands on both
    // types — and the `min`/`max` sentinel resolves to each type's own type-wide bound.
    #[test]
    fn one_operand_declaration_projects_onto_both_types() {
        let out = render(
            r#"Clamp {
                variants: [f32 value, i32 value],
                inputs:   { x: number { default 0 }, lo: number { default min }, hi: number { default max } },
                outputs:  { out },
                function: clamp_fn(x, lo, hi),
            }"#,
        );
        let f32v = variant(&out, "clamp_f32_value");
        let i32v = variant(&out, "clamp_i32_value");
        // The type-wide sentinel at each type: `±1e6` as an f32 literal, `±1_000_000` as an i32.
        assert!(f32v.contains("In :: new (2 , 1000000f32)"), "{f32v}");
        assert!(i32v.contains("In :: new (2 , 1000000i32)"), "{i32v}");
        assert!(f32v.contains("In :: new (1 , - 1000000f32)"), "{f32v}");
        assert!(i32v.contains("In :: new (1 , - 1000000i32)"), "{i32v}");
    }

    // A **converter** variant: the input ports carry one type and the output another, which no
    // single-type entry can express. The output type reaches only the output port and the
    // instantiated return type — the operands stay at the input type.
    #[test]
    fn converter_variant_types_its_output_separately() {
        let out = render(
            r#"Round {
                variants: [f32 value, f32 signal, f32 -> i32 value],
                inputs:   { x: number { default 0 } },
                outputs:  { out },
                function: round_fn(x),
            }"#,
        );
        let conv = variant(&out, "round_f32_i32_value");

        // Input: an `f32` held operand, carrying an `f32` default literal.
        assert!(conv.contains("form :: Held < f32 > >"), "{conv}");
        assert!(conv.contains("In :: new (0 , 0f32)"), "{conv}");
        assert!(conv.contains("Port :: f32 (\"x\""), "{conv}");
        // Output: an `i32` port, and the scalar fn instantiated to return `i32` — so a fn whose
        // bounds cannot produce the output type fails to compile at the call.
        assert!(conv.contains("Port :: i32 (\"out\""), "{conv}");
        assert!(conv.contains("type Value = i32 ;"), "{conv}");

        // The same-type siblings in the same expansion keep their two-fragment names and are
        // untouched — the out fragment appears only where the types actually differ.
        assert!(out.contains("pub mod round_f32_value"), "{out}");
        assert!(out.contains("pub mod round_f32_signal"), "{out}");
        assert!(!out.contains("round_f32_f32"), "{out}");
    }

    // Writing the output type explicitly when it equals the input type is the same operator, by
    // the same name — the sugar is the omission, not a different emission. This is what keeps
    // every shipping `*_f32_value` name (and so every instrument document) unchanged.
    #[test]
    fn an_explicit_matching_output_type_is_the_plain_variant() {
        let sugared = render(
            r#"Add {
                variants: [f32 value],
                inputs:   { a: number { default 0 } },
                outputs:  { out },
                function: add_fn(a),
            }"#,
        );
        let explicit = render(
            r#"Add {
                variants: [f32 -> f32 value],
                inputs:   { a: number { default 0 } },
                outputs:  { out },
                function: add_fn(a),
            }"#,
        );
        assert_eq!(sugared, explicit);
    }

    // A converter has no `signal` carrier: the carrier is per-sample buffers on *both* sides, and
    // `i32` has no dense buffer form. The check reads both types, so it rejects a bufferless
    // output for the same reason — and by the same message — as a bufferless input.
    #[test]
    fn converter_signal_carrier_is_rejected() {
        let out = render(
            r#"Round {
                variants: [f32 -> i32 signal],
                inputs:   { x: number { default 0 } },
                outputs:  { out },
                function: round_fn(x),
            }"#,
        );
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("has no `signal` carrier"), "{out}");
    }

    // Two entries naming the same operator would emit the same module and `register_operator!`
    // twice — caught here rather than as a duplicate-symbol error far from its cause.
    #[test]
    fn duplicate_variant_is_rejected() {
        let out = render(
            r#"Add {
                variants: [f32 value, f32 value],
                inputs:   { a: number { default 0 }, b: number { default 0 } },
                outputs:  { out },
                function: add_fn(a, b),
            }"#,
        );
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("duplicate variant"), "{out}");
    }
}
