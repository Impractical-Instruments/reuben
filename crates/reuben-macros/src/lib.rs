//! `operator_contract!` — the single-source operator contract macro.
//!
//! An operator declares its ports/params **once**, inside `operator_contract!`. The macro plants,
//! at module scope, the `IN_`/`OUT_`/`P_` index consts *and* an inherent `fn contract() ->
//! Descriptor`; the author's `impl Operator` delegates `fn descriptor()` to it with a one-liner.
//! Because the consts and the descriptor come from the **same tokens**, name↔slot drift is
//! impossible by construction — the disease this macro exists to cure.
//!
//! Shape A (delegate), forced by Rust: `descriptor()` and `process()` are both required methods of
//! one `impl Operator` block, and a macro can't inject a method into a hand-written impl. So the
//! macro emits an *inherent* `impl T { fn contract() }` at module scope, and the trait impl reads
//! `fn descriptor() -> Descriptor { Self::contract() }`.
//!
//! ```ignore
//! operator_contract!(Oscillator {
//!     inputs:  { freq: f32 { 20.0..=20_000.0, default 440.0, "Hz", exp },
//!                waveform: enum(Waveform) },
//!     outputs: { audio: f32_buffer },
//! });
//! ```

mod argvalue;
mod grammar;
mod model;
mod number_op;
mod unpack;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use reuben_contract::{
    naming, ContractError, Curve, F32Meta, I32Meta, Locus, OperatorSpec, PortSpec, PortTy,
};
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::{braced, parenthesized, Error, Ident, Lit, LitStr, Token};

use grammar::{parse_default_value, parse_float_or_sentinel};
use model::{build, ContractModel};

/// Emit an operator's index consts + `fn contract()` from its one declaration. See the crate docs.
#[proc_macro]
pub fn operator_contract(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    expand(input.into()).into()
}

/// Generate a family of stateless pointwise number operators from one scalar fn. See
/// the [`number_op`] module docs for the grammar and what each `numbers × carriers` variant emits.
#[proc_macro]
pub fn number_operator_contract(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    number_op::expand(input.into()).into()
}

/// Generate an `unpack_<type>` operator that destructures a product vocab type into its held
/// fields. See the [`unpack`] module docs for the grammar and what it emits.
#[proc_macro]
pub fn unpack_op(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    unpack::expand(input.into()).into()
}

/// Integrate a shared *vocab* type with the central `Arg` enum: `From`/`TryFrom`
/// for every type, plus the Enum-over-OSC table for unit enums. See [`argvalue`].
#[proc_macro_derive(ArgValue)]
pub fn derive_arg_value(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    argvalue::expand(input.into()).into()
}

/// The proc-macro body, over `proc_macro2` so it is unit-testable without a proc-macro context.
fn expand(input: TokenStream) -> TokenStream {
    let parsed = match syn::parse2::<ContractInput>(input) {
        Ok(p) => p,
        Err(e) => return e.to_compile_error(),
    };
    let spec = parsed.to_spec();
    if let Err(err) = reuben_contract::validate(&spec) {
        return parsed.error_at(&err).to_compile_error();
    }
    parsed.render(&build(&spec))
}

// --- Parsed AST (spans retained so validation errors point at the offending token) ---

/// One parsed port: the grammar's `name: <ty>` entry. The type parses **straight into the
/// shared contract [`PortTy`]** (issue #217) — there is no AST-side taxonomy or meta copy to
/// plumb field-by-field. The struct survives only because [`ContractInput::error_at`] needs the
/// name's `Span` to underline the offending port.
struct PortAst {
    name: Ident,
    ty: PortTy,
}

struct ContractInput {
    struct_ident: Ident,
    type_name: Option<LitStr>,
    inputs: Vec<PortAst>,
    outputs: Vec<PortAst>,
    /// Instantiate-time `Constant` ports — declared like inputs, in their own block.
    constants: Vec<PortAst>,
    resources: Vec<Ident>,
}

impl ContractInput {
    /// The flat [`OperatorSpec`] the shared validator/builder operate on.
    fn to_spec(&self) -> OperatorSpec {
        let type_name = self
            .type_name
            .as_ref()
            .map(LitStr::value)
            .unwrap_or_else(|| naming::type_name_from_struct(&self.struct_ident.to_string()));
        let ports = |ps: &[PortAst]| {
            ps.iter()
                .map(|p| PortSpec {
                    name: p.name.to_string(),
                    ty: p.ty.clone(),
                })
                .collect()
        };
        OperatorSpec {
            type_name,
            inputs: ports(&self.inputs),
            outputs: ports(&self.outputs),
            constants: ports(&self.constants),
            resources: self.resources.iter().map(Ident::to_string).collect(),
        }
    }

    /// Re-attach a validation error to the source token it concerns, so the compiler underlines it.
    fn error_at(&self, err: &ContractError) -> Error {
        let span = match err.locus {
            Locus::TypeName => self
                .type_name
                .as_ref()
                .map(LitStr::span)
                .unwrap_or_else(|| self.struct_ident.span()),
            Locus::Input(i) => self.inputs[i].name.span(),
            Locus::Output(i) => self.outputs[i].name.span(),
            Locus::Constant(i) => self.constants[i].name.span(),
        };
        Error::new(span, &err.message)
    }

    /// Render the resolved model to the const block + inherent `fn contract()`.
    fn render(&self, model: &ContractModel) -> TokenStream {
        render_contract(&self.struct_ident, model)
    }
}

/// The emitted `::reuben_core::descriptor::Curve::*` path for a declared [`Curve`] — the runtime
/// descriptor re-exports the contract's enum, so the variant maps one-to-one.
fn curve_tokens(c: Curve) -> TokenStream {
    match c {
        Curve::Linear => quote! { ::reuben_core::descriptor::Curve::Linear },
        Curve::Exponential => quote! { ::reuben_core::descriptor::Curve::Exponential },
    }
}

/// The [`form`] marker type for a port's declared type — the single source of the port-type →
/// form-marker mapping, shared by the input and output handle emitters so the two can't drift.
/// (`arg` maps to `Raw`; the validator rejects `arg` *outputs*, so [`output_handle`] never reaches
/// that arm.) Exhaustive over [`PortTy`] — no unreachable fallback arm (issue #217).
fn port_form(p: &model::PortModel) -> TokenStream {
    match &p.spec.ty {
        PortTy::F32Buffer(_) => quote! { SignalF32 },
        PortTy::F32(_) => quote! { Held<f32> },
        PortTy::I32(_) => quote! { Held<i32> },
        PortTy::Enum(vocab) => {
            let ty = Ident::new(vocab, Span::call_site());
            quote! { Held<::reuben_core::vocab::#ty> }
        }
        PortTy::Note => quote! { Event<::reuben_core::vocab::pitch::Note> },
        PortTy::Harmony => quote! { Held<::reuben_core::vocab::Harmony> },
        PortTy::Pitch => quote! { Held<::reuben_core::vocab::pitch::Pitch> },
        PortTy::Arg => quote! { Raw },
    }
}

/// The typed-handle const for one **input** port: its [`form`] marker type comes from
/// the declared port type ([`port_form`]), its stored default from the same declaration — so the
/// descriptor default and the held-read fallback are one datum. `In::new` takes `()` for
/// defaultless forms (events, the raw pass-through).
fn input_handle(p: &model::PortModel) -> TokenStream {
    let ident = Ident::new(&p.const_name, Span::call_site());
    let idx = proc_macro2::Literal::usize_unsuffixed(p.ordinal);
    let form = port_form(p);
    let default = match &p.spec.ty {
        // A Signal handle carries the declared scalar default as data (a bare audio buffer's is
        // 0.0 — literally what an unwired bare input materializes, the buffer-presence invariant).
        PortTy::F32Buffer(m) => {
            let d = m.as_ref().map(|m| m.default).unwrap_or(0.0);
            quote! { #d }
        }
        PortTy::F32(m) => {
            let d = m.default;
            quote! { #d }
        }
        PortTy::I32(m) => {
            let d = m.default;
            quote! { #d }
        }
        PortTy::Enum(vocab) => {
            let ty = Ident::new(vocab, Span::call_site());
            quote! { ::reuben_core::vocab::#ty::DEFAULT }
        }
        PortTy::Harmony => quote! { ::reuben_core::vocab::Harmony::DEFAULT },
        // A held `Pitch` leaf carries its declared default (tonic, `Degree(0)`) as data, exactly
        // like `Harmony` above — the read fallback for a `pitch` input (`resolve`, #523).
        PortTy::Pitch => quote! { ::reuben_core::vocab::pitch::Pitch::DEFAULT },
        // Defaultless forms (events, the raw pass-through) store `()`.
        PortTy::Note | PortTy::Arg => quote! { () },
    };
    quote! {
        pub const #ident: ::reuben_core::operator::In<::reuben_core::operator::form::#form> =
            ::reuben_core::operator::In::new(#idx, #default);
    }
}

/// The typed-handle const for one **output** port. Outputs carry no default — the
/// handle is index + form ([`port_form`]) only.
fn output_handle(p: &model::PortModel) -> TokenStream {
    let ident = Ident::new(&p.const_name, Span::call_site());
    let idx = proc_macro2::Literal::usize_unsuffixed(p.ordinal);
    let form = port_form(p);
    quote! {
        pub const #ident: ::reuben_core::operator::Out<::reuben_core::operator::form::#form> =
            ::reuben_core::operator::Out::new(#idx);
    }
}

/// Render a resolved [`ContractModel`] to the `IN_`/`OUT_`/`C_` const block + the inherent
/// `impl T { fn contract() }`. Free function so both `operator_contract!` and the
/// derived `number_operator_contract!` variants emit the identical contract from the same code.
///
/// Inputs/outputs emit **typed handles** (`In<form>`/`Out<form>`) whose type fixes the
/// `io.read`/`io.write` shape and whose value carries the declared default. Constants stay bare
/// `usize` ordinals: a `C_*` is instantiate-time config, never read in `process`, so it gets no
/// handle.
pub(crate) fn render_contract(struct_ident: &Ident, model: &ContractModel) -> TokenStream {
    {
        let in_handles = model.inputs.iter().map(input_handle);
        let out_handles = model.outputs.iter().map(output_handle);
        let const_ordinals = model.constants.iter().map(|p| {
            let ident = Ident::new(&p.const_name, Span::call_site());
            let val = proc_macro2::Literal::usize_unsuffixed(p.ordinal);
            quote! { pub const #ident: usize = #val; }
        });
        let consts = in_handles.chain(out_handles).chain(const_ordinals);

        // The emitted `F32Meta { .. }` literal for a meta-carrying port — one emitter for the
        // `f32` and `f32_buffer { .. }` arms below.
        let f32_meta_toks = |m: &F32Meta| -> TokenStream {
            let (min, max, default, unit) = (m.min, m.max, m.default, &m.unit);
            let curve = curve_tokens(m.curve);
            quote! {
                ::reuben_core::descriptor::F32Meta {
                    min: #min, max: #max,
                    default: #default, unit: #unit.into(), curve: #curve,
                }
            }
        };
        let port_toks = |ports: &[model::PortModel]| -> Vec<TokenStream> {
            ports
                .iter()
                .map(|p| {
                    let name = &p.spec.name;
                    // Exhaustive over [`PortTy`] with the payload in hand — the stringly-era
                    // `expect("validate() guarantees…")` calls and the unreachable
                    // `compile_error!` fallback are gone (issue #217).
                    match &p.spec.ty {
                        // A dense per-sample signal — `Port::f32_buffer`, or `f32_buffer_meta`
                        // when it carries a scalar default + knob.
                        PortTy::F32Buffer(None) => {
                            quote! { ::reuben_core::descriptor::Port::f32_buffer(#name) }
                        }
                        PortTy::F32Buffer(Some(m)) => {
                            let meta = f32_meta_toks(m);
                            quote! {
                                ::reuben_core::descriptor::Port::f32_buffer_meta(#name, #meta)
                            }
                        }
                        // A `Note` event port — `Port::note`.
                        PortTy::Note => quote! { ::reuben_core::descriptor::Port::note(#name) },
                        // A `Harmony` held port — `Port::harmony`.
                        PortTy::Harmony => {
                            quote! { ::reuben_core::descriptor::Port::harmony(#name) }
                        }
                        // A held `Pitch` leaf port — `Port::pitch`.
                        PortTy::Pitch => {
                            quote! { ::reuben_core::descriptor::Port::pitch(#name) }
                        }
                        // A type-agnostic pass-through — `Port::arg` (issue #141).
                        PortTy::Arg => quote! { ::reuben_core::descriptor::Port::arg(#name) },
                        // A materialized scalar control — `Port::f32` with its meta.
                        PortTy::F32(m) => {
                            let meta = f32_meta_toks(m);
                            quote! { ::reuben_core::descriptor::Port::f32(#name, #meta) }
                        }
                        // A bounded integer control / constant — `Port::i32` with its meta.
                        PortTy::I32(m) => {
                            let (min, max, default) = (m.min, m.max, m.default);
                            quote! {
                                ::reuben_core::descriptor::Port::i32(
                                    #name,
                                    ::reuben_core::descriptor::I32Meta {
                                        min: #min, max: #max, default: #default,
                                    }
                                )
                            }
                        }
                        // A held vocab enum — `Port::enumerated` off the shared type's
                        // `enum_meta`, so the descriptor and the type are single-sourced.
                        PortTy::Enum(vocab) => {
                            let ty = Ident::new(vocab, Span::call_site());
                            quote! {
                                ::reuben_core::descriptor::Port::enumerated(
                                    ::reuben_core::vocab::#ty::enum_meta(#name)
                                )
                            }
                        }
                    }
                })
                .collect()
        };
        let inputs = port_toks(&model.inputs);
        let outputs = port_toks(&model.outputs);
        let constants = port_toks(&model.constants);

        let resources = model.resources.iter().map(|r| {
            quote! { ::reuben_core::descriptor::ResourceSlot::new(#r) }
        });

        let type_name = &model.type_name;
        quote! {
            #(#consts)*

            impl #struct_ident {
                /// The operator's [`Descriptor`](::reuben_core::descriptor::Descriptor),
                /// single-sourced with the index consts above by `operator_contract!`.
                pub fn contract() -> ::reuben_core::descriptor::Descriptor {
                    ::reuben_core::descriptor::Descriptor {
                        type_name: #type_name,
                        inputs: ::std::vec![ #(#inputs),* ],
                        outputs: ::std::vec![ #(#outputs),* ],
                        constants: ::std::vec![ #(#constants),* ],
                        resources: ::std::vec![ #(#resources),* ],
                    }
                }
            }
        }
    }
}

// --- Parsing the macro grammar ---

impl Parse for ContractInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let struct_ident: Ident = input.parse()?;
        let body;
        braced!(body in input);

        let mut ci = ContractInput {
            struct_ident: struct_ident.clone(),
            type_name: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            constants: Vec::new(),
            resources: Vec::new(),
        };

        while !body.is_empty() {
            let key: Ident = body.parse()?;
            body.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "type_name" => ci.type_name = Some(body.parse()?),
                "inputs" => ci.inputs = parse_ports(&body)?,
                "outputs" => ci.outputs = parse_ports(&body)?,
                "constants" => ci.constants = parse_ports(&body)?,
                "resources" => ci.resources = parse_resources(&body)?,
                other => {
                    return Err(Error::new(
                        key.span(),
                        format!("unknown contract field `{other}` (expected inputs/outputs/constants/resources/type_name)"),
                    ))
                }
            }
            if body.peek(Token![,]) {
                body.parse::<Token![,]>()?;
            }
        }
        Ok(ci)
    }
}

/// A brace-wrapped, comma-separated port list. Each entry is `name: <ty>` where `<ty>` is the
/// port's [`Arg`] type: `f32_buffer`, `f32 { .. }`, `enum(VocabType)`, `note`,
/// `harmony`, or `arg` — parsed straight into the shared [`PortTy`] (issue #217), so an unknown
/// type keyword is a parse error here and unrepresentable past this point.
fn parse_ports(input: ParseStream) -> syn::Result<Vec<PortAst>> {
    let body;
    braced!(body in input);
    let mut out = Vec::new();
    while !body.is_empty() {
        // `parse_any` so a port may be named with a keyword, e.g. m2s's `in`.
        let name = Ident::parse_any(&body)?;
        body.parse::<Token![:]>()?;
        // `parse_any` so the type keyword may be a reserved word (`enum`).
        let kw = Ident::parse_any(&body)?;
        let ty = match kw.to_string().as_str() {
            // A bare `f32_buffer` is a pure signal; an optional `{ .. }` meta block gives it a
            // scalar default + knob range.
            "f32_buffer" => {
                let meta = if body.peek(syn::token::Brace) {
                    Some(parse_f32_meta(&body)?)
                } else {
                    None
                };
                PortTy::F32Buffer(meta)
            }
            "note" => PortTy::Note,
            "harmony" => PortTy::Harmony,
            "pitch" => PortTy::Pitch,
            "arg" => PortTy::Arg,
            "f32" => PortTy::F32(parse_f32_meta(&body)?),
            "i32" => PortTy::I32(parse_i32_meta(&body)?),
            "enum" => {
                let inner;
                parenthesized!(inner in body);
                // A syn `Ident` is a valid Rust identifier by construction, so the vocab name
                // needs no further shape check on this path (the JSON path gets `is_ident`).
                PortTy::Enum(inner.parse::<Ident>()?.to_string())
            }
            other => {
                return Err(Error::new(
                    kw.span(),
                    format!("port type must be `f32_buffer`, `f32`, `i32`, `enum(..)`, `note`, `harmony`, `pitch`, or `arg`, got `{other}`"),
                ))
            }
        };
        out.push(PortAst { name, ty });
        if body.peek(Token![,]) {
            body.parse::<Token![,]>()?;
        }
    }
    Ok(out)
}

/// `{ LO..=HI, default D [, "unit"] [, curve] }` — the meta on a `f32 { .. }` port. `unit` and
/// `curve` are each optional (an omitted curve defaults to `linear`), unlike the all-required
/// legacy `params` block. A range endpoint may be the `min`/`max` sentinel (the type-wide `±1e6`
/// bound), and `default` may be `default max` / `default min` (the port's own range edge) — so the
/// sentinel is never a raw literal (issue #127). Parses straight into the shared contract
/// [`F32Meta`] (issue #217).
fn parse_f32_meta(input: ParseStream) -> syn::Result<F32Meta> {
    let meta;
    braced!(meta in input);

    let min = parse_float_or_sentinel(&meta)?;
    meta.parse::<Token![..=]>()?;
    let max = parse_float_or_sentinel(&meta)?;
    meta.parse::<Token![,]>()?;

    let default_kw: Ident = meta.parse()?;
    if default_kw != "default" {
        return Err(Error::new(default_kw.span(), "expected `default <value>`"));
    }
    let default = parse_default_value(&meta, min, max)?;

    let mut unit = String::new();
    let mut curve = Curve::Linear;
    // Optional `, "unit"` then optional `, curve` — either may be omitted.
    if meta.peek(Token![,]) {
        meta.parse::<Token![,]>()?;
        if meta.peek(LitStr) {
            unit = meta.parse::<LitStr>()?.value();
            if meta.peek(Token![,]) {
                meta.parse::<Token![,]>()?;
                if meta.peek(Ident) {
                    curve = parse_curve_ident(&meta)?;
                }
            }
        } else if meta.peek(Ident) {
            curve = parse_curve_ident(&meta)?;
        }
    }
    if meta.peek(Token![,]) {
        meta.parse::<Token![,]>()?;
    }
    if !meta.is_empty() {
        return Err(meta.error("unexpected tokens in `f32 { .. }` meta"));
    }
    Ok(F32Meta {
        min,
        max,
        default,
        unit,
        curve,
    })
}

/// `lin`/`linear` → [`Curve::Linear`], `exp`/`exponential` → [`Curve::Exponential`]; anything
/// else is an error — a bad curve never survives the parse (issue #217).
fn parse_curve_ident(input: ParseStream) -> syn::Result<Curve> {
    let curve_ident: Ident = input.parse()?;
    match curve_ident.to_string().as_str() {
        "lin" | "linear" => Ok(Curve::Linear),
        "exp" | "exponential" => Ok(Curve::Exponential),
        other => Err(Error::new(
            curve_ident.span(),
            format!("curve must be `lin` or `exp`, got `{other}`"),
        )),
    }
}

/// `{ LO..=HI, default D }` — the meta on an `i32 { .. }` port / constant. Integer
/// bounds + default; no unit/curve (a count is not a swept knob). Parses straight into the
/// shared contract [`I32Meta`] (issue #217).
fn parse_i32_meta(input: ParseStream) -> syn::Result<I32Meta> {
    let meta;
    braced!(meta in input);

    let min = parse_signed_int(&meta)?;
    meta.parse::<Token![..=]>()?;
    let max = parse_signed_int(&meta)?;
    meta.parse::<Token![,]>()?;

    let default_kw: Ident = meta.parse()?;
    if default_kw != "default" {
        return Err(Error::new(default_kw.span(), "expected `default <value>`"));
    }
    let default = parse_signed_int(&meta)?;
    if meta.peek(Token![,]) {
        meta.parse::<Token![,]>()?;
    }
    if !meta.is_empty() {
        return Err(meta.error("unexpected tokens in `i32 { .. }` meta"));
    }
    Ok(I32Meta { min, max, default })
}

/// A signed integer literal (an `i32` port's bounds/default may be negative).
fn parse_signed_int(input: ParseStream) -> syn::Result<i32> {
    let neg = input.peek(Token![-]);
    if neg {
        input.parse::<Token![-]>()?;
    }
    let lit: Lit = input.parse()?;
    let val = match lit {
        Lit::Int(i) => i.base10_parse::<i32>()?,
        other => return Err(Error::new(other.span(), "expected an integer literal")),
    };
    Ok(if neg { -val } else { val })
}

/// `{ name, name }` — a brace-wrapped, comma-separated resource-slot list.
fn parse_resources(input: ParseStream) -> syn::Result<Vec<Ident>> {
    let body;
    braced!(body in input);
    let mut out = Vec::new();
    while !body.is_empty() {
        out.push(body.parse::<Ident>()?);
        if body.peek(Token![,]) {
            body.parse::<Token![,]>()?;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str) -> String {
        let ts: TokenStream = src.parse().expect("token stream");
        expand(ts).to_string()
    }

    #[test]
    fn emits_consts_and_contract_fn() {
        let out = render(
            r#"Oscillator {
                inputs:  { freq:     f32 { 20.0..=20_000.0, default 440.0, "Hz", exp },
                           waveform: f32 { 0.0..=1.0,        default 0.0,   "",   lin } },
                outputs: { audio: f32_buffer },
            }"#,
        );
        // Typed handles: the const's type carries the form, its value the ordinal +
        // declared default.
        assert!(
            out.contains("pub const IN_FREQ : :: reuben_core :: operator :: In < :: reuben_core :: operator :: form :: Held < f32 > >"),
            "{out}"
        );
        assert!(out.contains("In :: new (0 , 440f32)"), "{out}");
        assert!(out.contains("pub const IN_WAVEFORM :"), "{out}");
        assert!(out.contains("In :: new (1 , 0f32)"), "{out}");
        assert!(
            out.contains("pub const OUT_AUDIO : :: reuben_core :: operator :: Out < :: reuben_core :: operator :: form :: SignalF32 > = :: reuben_core :: operator :: Out :: new (0)"),
            "{out}"
        );
        assert!(out.contains("impl Oscillator"), "{out}");
        assert!(out.contains("fn contract"), "{out}");
        assert!(out.contains("type_name : \"oscillator\""), "{out}");
        assert!(out.contains("Curve :: Exponential"), "{out}");
    }

    #[test]
    fn explicit_type_name_overrides_struct_default() {
        let out = render(
            r#"SamplePlayer {
                type_name: "sample",
                inputs:  { freq: f32_buffer, gate: f32_buffer },
                outputs: { audio: f32_buffer },
                resources: { sample },
            }"#,
        );
        assert!(out.contains("type_name : \"sample\""), "{out}");
        assert!(out.contains("ResourceSlot :: new (\"sample\")"), "{out}");
        // A bare `f32_buffer` handle carries 0.0 — literally what an unwired bare input
        // materializes (the buffer-presence invariant).
        assert!(
            out.contains("pub const IN_GATE : :: reuben_core :: operator :: In < :: reuben_core :: operator :: form :: SignalF32 > = :: reuben_core :: operator :: In :: new (1 , 0f32)"),
            "{out}"
        );
    }

    // Ports number sequentially in declaration order — a note input and a harmony input
    // are 0 and 1, not split per kind. A `constants:` block renders as immutable ports
    // with their own `C_` index consts and an `i32` meta.
    #[test]
    fn ports_number_sequentially_and_constant_renders_as_port() {
        let out = render(
            r#"Voicer {
                inputs:  { notes: note, ctx: harmony },
                outputs: { audio: f32_buffer },
                constants: { voices: i32 { 1..=32, default 8 } },
            }"#,
        );
        // An Event handle stores no default (`()`); a held `Harmony` carries the const C-major
        // default. Constants stay bare `usize` ordinals (no handle — never read in `process`).
        assert!(
            out.contains("pub const IN_NOTES : :: reuben_core :: operator :: In < :: reuben_core :: operator :: form :: Event < :: reuben_core :: vocab :: pitch :: Note > > = :: reuben_core :: operator :: In :: new (0 , ())"),
            "{out}"
        );
        assert!(
            out.contains("pub const IN_CTX : :: reuben_core :: operator :: In < :: reuben_core :: operator :: form :: Held < :: reuben_core :: vocab :: Harmony > > = :: reuben_core :: operator :: In :: new (1 , :: reuben_core :: vocab :: Harmony :: DEFAULT)"),
            "{out}"
        );
        assert!(out.contains("pub const C_VOICES : usize = 0"), "{out}");
        assert!(out.contains("Port :: note (\"notes\")"), "{out}");
        assert!(out.contains("Port :: harmony (\"ctx\")"), "{out}");
        assert!(out.contains("Port :: i32"), "{out}");
        assert!(out.contains("I32Meta"), "{out}");
    }

    // A malformed contract is rejected *with a span*, in-band as a compile_error.
    #[test]
    fn duplicate_port_is_a_spanned_compile_error() {
        let out = render(
            r#"Bad {
                inputs: { a: f32_buffer, a: f32_buffer },
            }"#,
        );
        assert!(out.contains("compile_error !"), "{out}");
        assert!(out.contains("duplicate input port name"), "{out}");
        // The success path is NOT emitted alongside the error.
        assert!(!out.contains("fn contract"), "{out}");
    }

    // An unknown curve keyword is rejected by `parse_curve_ident` itself, not by some earlier
    // grammar failure — the input is otherwise a fully valid contract, so the only thing wrong
    // with it is the curve, and the assertion pins the curve-specific message.
    #[test]
    fn bad_curve_keyword_is_rejected() {
        let out = render(
            r#"Bad {
                inputs:  { a: f32 { 0.0..=1.0, default 0.0, "", log } },
                outputs: { out: f32_buffer },
            }"#,
        );
        assert!(out.contains("compile_error !"), "{out}");
        assert!(out.contains("curve must be `lin` or `exp`"), "{out}");
    }

    // `unit` and `curve` are each independently optional in the `f32 { .. }` meta: the
    // curve-without-unit form `{ LO..=HI, default D, exp }` still parses (the `Ident` peek after
    // the comma, not the `LitStr` one) and keeps the curve exponential rather than silently
    // falling back to linear, with the omitted unit defaulting to the empty string. Every real
    // operator contract writes a unit before its curve, so nothing else exercises this branch.
    #[test]
    fn curve_without_unit_is_accepted() {
        let out = render(
            r#"Osc {
                inputs:  { freq: f32 { 20.0..=20_000.0, default 440.0, exp } },
                outputs: { audio: f32_buffer },
            }"#,
        );
        assert!(!out.contains("compile_error !"), "{out}");
        assert!(out.contains("Curve :: Exponential"), "{out}");
        // The omitted unit defaults to the empty string.
        assert!(out.contains("unit : \"\""), "{out}");
    }

    // The filter target contract: a buffer audio in/out, float-with-meta controls, and
    // an `enum(FilterMode)` naming its shared vocab type — sequential input ordinals.
    #[test]
    fn emits_filter_contract() {
        let out = render(
            r#"Filter {
                inputs:  { audio: f32_buffer,
                           cutoff: f32 { 20.0..=20_000.0, default 1_000.0, "Hz", exp },
                           resonance: f32 { 0.0..=1.0, default 0.2 },
                           mode: enum(FilterMode) },
                outputs: { audio: f32_buffer },
            }"#,
        );
        assert!(out.contains("pub const IN_AUDIO :"), "{out}");
        assert!(
            out.contains(
                "form :: SignalF32 > = :: reuben_core :: operator :: In :: new (0 , 0f32)"
            ),
            "{out}"
        );
        assert!(out.contains("pub const IN_CUTOFF :"), "{out}");
        assert!(
            out.contains(
                "form :: Held < f32 > > = :: reuben_core :: operator :: In :: new (1 , 1000f32)"
            ),
            "{out}"
        );
        assert!(out.contains("pub const IN_RESONANCE :"), "{out}");
        assert!(out.contains("In :: new (2 , 0.2f32)"), "{out}");
        // A held enum handle defaults to the shared type's derive-generated `DEFAULT`.
        assert!(
            out.contains("pub const IN_MODE : :: reuben_core :: operator :: In < :: reuben_core :: operator :: form :: Held < :: reuben_core :: vocab :: FilterMode > > = :: reuben_core :: operator :: In :: new (3 , :: reuben_core :: vocab :: FilterMode :: DEFAULT)"),
            "{out}"
        );
        assert!(out.contains("pub const OUT_AUDIO :"), "{out}");
        assert!(out.contains("Out :: new (0)"), "{out}");
        assert!(out.contains("Port :: f32_buffer (\"audio\")"), "{out}");
        assert!(out.contains("Port :: f32"), "{out}");
        assert!(out.contains("Curve :: Exponential"), "{out}");
        // Enum: `Port::enumerated` single-sourced off the shared vocab type's `enum_meta`.
        assert!(out.contains("Port :: enumerated"), "{out}");
        assert!(
            out.contains("vocab :: FilterMode :: enum_meta (\"mode\")"),
            "{out}"
        );
        // No locally-generated enum type any more — vocab types are shared.
        assert!(!out.contains("pub enum"), "{out}");
    }

    // The oscillator target contract: a materialized `freq` and a `waveform` vocab enum.
    #[test]
    fn emits_oscillator_contract() {
        let out = render(
            r#"Oscillator {
                inputs:  { freq: f32 { 20.0..=20_000.0, default 440.0, "Hz", exp },
                           waveform: enum(Waveform) },
                outputs: { audio: f32_buffer },
            }"#,
        );
        assert!(out.contains("pub const IN_FREQ :"), "{out}");
        assert!(out.contains("In :: new (0 , 440f32)"), "{out}");
        assert!(out.contains("pub const IN_WAVEFORM :"), "{out}");
        assert!(
            out.contains("In :: new (1 , :: reuben_core :: vocab :: Waveform :: DEFAULT)"),
            "{out}"
        );
        assert!(
            out.contains("vocab :: Waveform :: enum_meta (\"waveform\")"),
            "{out}"
        );
        assert!(out.contains("type_name : \"oscillator\""), "{out}");
    }

    // A signal control with a scalar default: `f32_buffer { .. }` carries
    // its meta yet stays a buffer port, so it emits `Port::f32_buffer_meta` — distinct from the
    // bare `Port::f32_buffer` and from a Value `Port::f32`.
    #[test]
    fn f32_buffer_with_meta_emits_f32_buffer_meta() {
        let out = render(
            r#"Osc {
                inputs:  { freq: f32_buffer { 20.0..=20_000.0, default 440.0, "Hz", exp } },
                outputs: { audio: f32_buffer },
            }"#,
        );
        assert!(out.contains("Port :: f32_buffer_meta (\"freq\""), "{out}");
        assert!(out.contains("default : 440"), "{out}");
        assert!(out.contains("Curve :: Exponential"), "{out}");
        // Not the bare-buffer ctor and not the Value `f32` ctor.
        assert!(!out.contains("Port :: f32_buffer (\"freq\")"), "{out}");
        assert!(!out.contains("Port :: f32 ("), "{out}");
    }

    // A bare `f32_buffer` (no meta) still emits the plain ctor — the meta block is optional.
    #[test]
    fn bare_f32_buffer_emits_plain_ctor() {
        let out =
            render(r#"Sig { inputs: { audio: f32_buffer }, outputs: { audio: f32_buffer } }"#);
        assert!(out.contains("Port :: f32_buffer (\"audio\")"), "{out}");
        assert!(!out.contains("f32_buffer_meta"), "{out}");
    }

    // The `min`/`max` range sentinels resolve to the type-wide ±1e6 bound, and `default max` /
    // `default min` to the port's own range edge — no raw literal in the contract (issue #127). A
    // half-sentinel range (`0.0..=max`, m2s's `rate`) keeps its real lower bound.
    #[test]
    fn min_max_sentinels_resolve_to_type_wide_bounds() {
        let out = render(
            r#"Op {
                inputs:  { in:   f32 { min..=max, default 0.0 },
                           rate: f32 { 0.0..=max, default 1_000.0 },
                           ceil: f32 { min..=max, default max },
                           floor: f32 { min..=max, default min } },
                outputs: { out: f32_buffer },
            }"#,
        );
        // Type-wide bounds materialize as the shared ±1e6 sentinel.
        assert!(out.contains("min : - 1000000f32"), "{out}");
        assert!(out.contains("max : 1000000f32"), "{out}");
        // `rate` keeps its real 0.0 floor next to the `max` sentinel ceiling.
        assert!(out.contains("min : 0f32 , max : 1000000f32"), "{out}");
        // `default max` parks at the ceiling, `default min` at the floor.
        assert!(out.contains("default : 1000000f32"), "{out}");
        assert!(out.contains("default : - 1000000f32"), "{out}");
    }

    // The remaining handle forms, one assertion per PortType: a raw `arg`
    // pass-through input, a bounded `i32` input, and the message-output handles (`f32`, `note`,
    // `harmony`) — every port form gets a typed handle, full coverage.
    #[test]
    fn emits_typed_handles_for_every_remaining_port_form() {
        let out = render(
            r#"Kitchen {
                inputs:  { passthru: arg,
                           count: i32 { 1..=8, default 3 } },
                outputs: { cv: f32_buffer,
                           active: f32 { 0.0..=1.0, default 0.0 },
                           degrees: note,
                           ctx: harmony },
            }"#,
        );
        // Raw pass-through: no default to carry.
        assert!(
            out.contains("pub const IN_PASSTHRU : :: reuben_core :: operator :: In < :: reuben_core :: operator :: form :: Raw > = :: reuben_core :: operator :: In :: new (0 , ())"),
            "{out}"
        );
        // Held i32: the declared integer default rides the handle.
        assert!(
            out.contains("pub const IN_COUNT : :: reuben_core :: operator :: In < :: reuben_core :: operator :: form :: Held < i32 > > = :: reuben_core :: operator :: In :: new (1 , 3i32)"),
            "{out}"
        );
        // Outputs: form-typed, index-only (all-outputs index — signal then message).
        assert!(
            out.contains("pub const OUT_CV : :: reuben_core :: operator :: Out < :: reuben_core :: operator :: form :: SignalF32 > = :: reuben_core :: operator :: Out :: new (0)"),
            "{out}"
        );
        assert!(
            out.contains("pub const OUT_ACTIVE : :: reuben_core :: operator :: Out < :: reuben_core :: operator :: form :: Held < f32 > > = :: reuben_core :: operator :: Out :: new (1)"),
            "{out}"
        );
        assert!(
            out.contains("pub const OUT_DEGREES : :: reuben_core :: operator :: Out < :: reuben_core :: operator :: form :: Event < :: reuben_core :: vocab :: pitch :: Note > > = :: reuben_core :: operator :: Out :: new (2)"),
            "{out}"
        );
        assert!(
            out.contains("pub const OUT_CTX : :: reuben_core :: operator :: Out < :: reuben_core :: operator :: form :: Held < :: reuben_core :: vocab :: Harmony > > = :: reuben_core :: operator :: Out :: new (3)"),
            "{out}"
        );
    }

    // An unknown port type is rejected with a span, as a compile_error.
    #[test]
    fn unknown_port_type_is_a_spanned_error() {
        let out = render(r#"Bad { inputs: { mode: signal } }"#);
        assert!(out.contains("compile_error !"), "{out}");
        assert!(out.contains("port type must be"), "{out}");
    }
}
