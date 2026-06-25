//! `operator_contract!` — the single-source operator contract macro (ADR-0025).
//!
//! An operator declares its ports/params **once**, inside `operator_contract!`. The macro plants,
//! at module scope, the `IN_`/`OUT_`/`P_` index consts *and* an inherent `fn contract() ->
//! Descriptor`; the author's `impl Operator` delegates `fn descriptor()` to it with a one-liner.
//! Because the consts and the descriptor come from the **same tokens**, name↔slot drift is
//! impossible by construction — the disease this macro exists to cure (ADR-0010/0021).
//!
//! Shape A (delegate), forced by Rust: `descriptor()` and `process()` are both required methods of
//! one `impl Operator` block, and a macro can't inject a method into a hand-written impl. So the
//! macro emits an *inherent* `impl T { fn contract() }` at module scope, and the trait impl reads
//! `fn descriptor() -> Descriptor { Self::contract() }`.
//!
//! ```ignore
//! operator_contract!(Oscillator {
//!     inputs:  { freq: float { 20.0..=20_000.0, default 440.0, "Hz", exp },
//!                waveform: enum(Waveform) },
//!     outputs: { audio: buffer },
//!     lanes: inherit,
//! });
//! ```

mod argvalue;
mod model;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use reuben_contract::{
    naming, ContractError, FloatMeta, LaneSpec, Locus, OperatorSpec, ParamSpec, PortSpec,
};
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::{braced, parenthesized, Error, Ident, Lit, LitStr, Token};

use model::{build, ContractModel, LaneModel};

/// Emit an operator's index consts + `fn contract()` from its one declaration. See the crate docs.
#[proc_macro]
pub fn operator_contract(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    expand(input.into()).into()
}

/// Integrate a shared *vocab* type with the central `Arg` enum (ADR-0030): `From`/`TryFrom`
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

/// The `float { LO..=HI, default D, "unit", curve }` block (ADR-0028). `unit`/`curve` are
/// optional; an omitted curve defaults to `linear`.
struct FloatMetaAst {
    min: f32,
    max: f32,
    default: f32,
    unit: String,
    curve: String,
}

/// How a port is declared — its [`Arg`] type (ADR-0030).
enum PortTypeAst {
    /// `name: buffer` — a dense per-sample signal (audio / control buffer).
    Buffer,
    /// `name: float { .. }` — a materialized scalar control with its default/range meta.
    Float(FloatMetaAst),
    /// `name: enum(VocabType)` — a held vocab enum, naming its shared `vocab` type.
    Enum(Ident),
    /// `name: note` — a `Note` event port.
    Note,
    /// `name: harmony` — a `Harmony` held port.
    Harmony,
}

struct PortAst {
    name: Ident,
    ty: PortTypeAst,
}

struct ParamAst {
    name: Ident,
    min: f32,
    max: f32,
    default: f32,
    unit: String,
    curve: String,
}

enum LaneAst {
    Inherit,
    FromParam(Ident),
}

struct ContractInput {
    struct_ident: Ident,
    type_name: Option<LitStr>,
    inputs: Vec<PortAst>,
    outputs: Vec<PortAst>,
    params: Vec<ParamAst>,
    resources: Vec<Ident>,
    lanes: LaneAst,
    lanes_span: Span,
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
                .map(|p| {
                    let (ty, float, vocab) = match &p.ty {
                        PortTypeAst::Buffer => ("buffer", None, None),
                        PortTypeAst::Float(m) => (
                            "float",
                            Some(FloatMeta {
                                min: m.min,
                                max: m.max,
                                default: m.default,
                                unit: m.unit.clone(),
                                curve: m.curve.clone(),
                            }),
                            None,
                        ),
                        PortTypeAst::Enum(t) => ("enum", None, Some(t.to_string())),
                        PortTypeAst::Note => ("note", None, None),
                        PortTypeAst::Harmony => ("harmony", None, None),
                    };
                    PortSpec {
                        name: p.name.to_string(),
                        ty: ty.to_string(),
                        float,
                        vocab,
                    }
                })
                .collect()
        };
        OperatorSpec {
            type_name,
            inputs: ports(&self.inputs),
            outputs: ports(&self.outputs),
            params: self
                .params
                .iter()
                .map(|p| ParamSpec {
                    name: p.name.to_string(),
                    min: p.min,
                    max: p.max,
                    default: p.default,
                    unit: p.unit.clone(),
                    curve: p.curve.clone(),
                })
                .collect(),
            resources: self.resources.iter().map(Ident::to_string).collect(),
            lanes: match &self.lanes {
                LaneAst::Inherit => LaneSpec::Inherit,
                LaneAst::FromParam(p) => LaneSpec::FromParam(p.to_string()),
            },
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
            Locus::Param(i) => self.params[i].name.span(),
            Locus::Lanes => self.lanes_span,
        };
        Error::new(span, &err.message)
    }

    /// Render the resolved model to the const block + inherent `fn contract()`.
    fn render(&self, model: &ContractModel) -> TokenStream {
        let struct_ident = &self.struct_ident;

        let consts = model
            .inputs
            .iter()
            .chain(&model.outputs)
            .map(|p| (p.const_name.as_str(), p.ordinal))
            .chain(
                model
                    .params
                    .iter()
                    .map(|p| (p.const_name.as_str(), p.index)),
            )
            .map(|(name, value)| {
                let ident = Ident::new(name, Span::call_site());
                let val = proc_macro2::Literal::usize_unsuffixed(value);
                quote! { pub const #ident: usize = #val; }
            });

        let port_toks = |ports: &[model::PortModel]| -> Vec<TokenStream> {
            ports
                .iter()
                .map(|p| {
                    let name = &p.name;
                    match p.ty.as_str() {
                        // A dense per-sample signal — `Port::buffer`.
                        "buffer" => quote! { ::reuben_core::descriptor::Port::buffer(#name) },
                        // A `Note` event port — `Port::note`.
                        "note" => quote! { ::reuben_core::descriptor::Port::note(#name) },
                        // A `Harmony` held port — `Port::harmony`.
                        "harmony" => quote! { ::reuben_core::descriptor::Port::harmony(#name) },
                        // A materialized scalar control — `Port::float` with its meta.
                        "float" => {
                            let m = p.float.as_ref().expect("validate() guarantees float meta");
                            let (min, max, default, unit) = (m.min, m.max, m.default, &m.unit);
                            let curve = if m.curve == "exponential" {
                                quote! { ::reuben_core::descriptor::Curve::Exponential }
                            } else {
                                quote! { ::reuben_core::descriptor::Curve::Linear }
                            };
                            quote! {
                                ::reuben_core::descriptor::Port::float(
                                    ::reuben_core::descriptor::ParamMeta {
                                        name: #name, min: #min, max: #max,
                                        default: #default, unit: #unit, curve: #curve,
                                    }
                                )
                            }
                        }
                        // A held vocab enum — `Port::enumerated` off the shared type's
                        // `enum_meta`, so the descriptor and the type are single-sourced (ADR-0030).
                        "enum" => {
                            let vocab = p.vocab.as_ref().expect("validate() guarantees enum vocab");
                            let ty = Ident::new(vocab, Span::call_site());
                            quote! {
                                ::reuben_core::descriptor::Port::enumerated(
                                    ::reuben_core::vocab::#ty::enum_meta(#name)
                                )
                            }
                        }
                        other => {
                            // Unreachable: validate() restricts ty to the PORT_TYPES set.
                            let msg = format!("unsupported port type {other:?}");
                            quote! { compile_error!(#msg) }
                        }
                    }
                })
                .collect()
        };
        let inputs = port_toks(&model.inputs);
        let outputs = port_toks(&model.outputs);

        let params = model.params.iter().map(|p| {
            let (name, min, max, default, unit) = (&p.name, p.min, p.max, p.default, &p.unit);
            let curve = if p.curve == "exponential" {
                quote! { ::reuben_core::descriptor::Curve::Exponential }
            } else {
                quote! { ::reuben_core::descriptor::Curve::Linear }
            };
            quote! {
                ::reuben_core::descriptor::ParamMeta {
                    name: #name, min: #min, max: #max, default: #default, unit: #unit, curve: #curve,
                }
            }
        });

        let resources = model.resources.iter().map(|r| {
            quote! { ::reuben_core::descriptor::ResourceSlot::new(#r) }
        });

        let lanes = match &model.lanes {
            LaneModel::Inherit => quote! { ::reuben_core::descriptor::LaneRule::Inherit },
            LaneModel::FromParam(const_name) => {
                let ident = Ident::new(const_name, Span::call_site());
                quote! { ::reuben_core::descriptor::LaneRule::FromParam(#ident) }
            }
        };

        let type_name = &model.type_name;
        quote! {
            #(#consts)*

            impl #struct_ident {
                /// The operator's [`Descriptor`](::reuben_core::descriptor::Descriptor),
                /// single-sourced with the index consts above by `operator_contract!` (ADR-0025).
                pub fn contract() -> ::reuben_core::descriptor::Descriptor {
                    ::reuben_core::descriptor::Descriptor {
                        type_name: #type_name,
                        inputs: ::std::vec![ #(#inputs),* ],
                        outputs: ::std::vec![ #(#outputs),* ],
                        params: ::std::vec![ #(#params),* ],
                        resources: ::std::vec![ #(#resources),* ],
                        lanes: #lanes,
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
            params: Vec::new(),
            resources: Vec::new(),
            lanes: LaneAst::Inherit,
            lanes_span: struct_ident.span(),
        };

        while !body.is_empty() {
            let key: Ident = body.parse()?;
            body.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "type_name" => ci.type_name = Some(body.parse()?),
                "inputs" => ci.inputs = parse_ports(&body)?,
                "outputs" => ci.outputs = parse_ports(&body)?,
                "params" => ci.params = parse_params(&body)?,
                "resources" => ci.resources = parse_resources(&body)?,
                "lanes" => {
                    let (lanes, span) = parse_lanes(&body)?;
                    ci.lanes = lanes;
                    ci.lanes_span = span;
                }
                other => {
                    return Err(Error::new(
                        key.span(),
                        format!("unknown contract field `{other}` (expected inputs/outputs/params/resources/lanes/type_name)"),
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
/// port's [`Arg`] type (ADR-0030): `buffer`, `float { .. }`, `enum(VocabType)`, `note`, or
/// `harmony`. `validate()` rejects an unknown type.
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
            "buffer" => PortTypeAst::Buffer,
            "note" => PortTypeAst::Note,
            "harmony" => PortTypeAst::Harmony,
            "float" => PortTypeAst::Float(parse_float_meta(&body)?),
            "enum" => {
                let inner;
                parenthesized!(inner in body);
                PortTypeAst::Enum(inner.parse::<Ident>()?)
            }
            other => {
                return Err(Error::new(
                    kw.span(),
                    format!("port type must be `buffer`, `float`, `enum(..)`, `note`, or `harmony`, got `{other}`"),
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

/// `{ LO..=HI, default D [, "unit"] [, curve] }` — the meta on a `float { .. }` port. `unit` and
/// `curve` are each optional (an omitted curve defaults to `linear`), unlike the all-required
/// legacy `params` block.
fn parse_float_meta(input: ParseStream) -> syn::Result<FloatMetaAst> {
    let meta;
    braced!(meta in input);

    let min = parse_signed_float(&meta)?;
    meta.parse::<Token![..=]>()?;
    let max = parse_signed_float(&meta)?;
    meta.parse::<Token![,]>()?;

    let default_kw: Ident = meta.parse()?;
    if default_kw != "default" {
        return Err(Error::new(default_kw.span(), "expected `default <value>`"));
    }
    let default = parse_signed_float(&meta)?;

    let mut unit = String::new();
    let mut curve = "linear".to_string();
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
        return Err(meta.error("unexpected tokens in `float { .. }` meta"));
    }
    Ok(FloatMetaAst {
        min,
        max,
        default,
        unit,
        curve,
    })
}

/// `lin`/`linear` → `"linear"`, `exp`/`exponential` → `"exponential"`; anything else is an error.
fn parse_curve_ident(input: ParseStream) -> syn::Result<String> {
    let curve_ident: Ident = input.parse()?;
    match curve_ident.to_string().as_str() {
        "lin" | "linear" => Ok("linear".to_string()),
        "exp" | "exponential" => Ok("exponential".to_string()),
        other => Err(Error::new(
            curve_ident.span(),
            format!("curve must be `lin` or `exp`, got `{other}`"),
        )),
    }
}

/// `{ name: { LO..=HI, default D, "unit", curve }, .. }`.
fn parse_params(input: ParseStream) -> syn::Result<Vec<ParamAst>> {
    let body;
    braced!(body in input);
    let mut out = Vec::new();
    while !body.is_empty() {
        // `parse_any` so a param may be named with a keyword, e.g. m2s's `default`.
        let name = Ident::parse_any(&body)?;
        body.parse::<Token![:]>()?;
        let meta;
        braced!(meta in body);

        let min = parse_signed_float(&meta)?;
        meta.parse::<Token![..=]>()?;
        let max = parse_signed_float(&meta)?;
        meta.parse::<Token![,]>()?;

        let default_kw: Ident = meta.parse()?;
        if default_kw != "default" {
            return Err(Error::new(default_kw.span(), "expected `default <value>`"));
        }
        let default = parse_signed_float(&meta)?;
        meta.parse::<Token![,]>()?;

        let unit: LitStr = meta.parse()?;
        meta.parse::<Token![,]>()?;

        let curve = parse_curve_ident(&meta)?;
        if meta.peek(Token![,]) {
            meta.parse::<Token![,]>()?;
        }

        out.push(ParamAst {
            name,
            min,
            max,
            default,
            unit: unit.value(),
            curve,
        });
        if body.peek(Token![,]) {
            body.parse::<Token![,]>()?;
        }
    }
    Ok(out)
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

/// `inherit` | `from_param(<param>)`.
fn parse_lanes(input: ParseStream) -> syn::Result<(LaneAst, Span)> {
    let kw: Ident = input.parse()?;
    let span = kw.span();
    match kw.to_string().as_str() {
        "inherit" => Ok((LaneAst::Inherit, span)),
        "from_param" => {
            let arg;
            parenthesized!(arg in input);
            let param: Ident = arg.parse()?;
            Ok((LaneAst::FromParam(param), span))
        }
        other => Err(Error::new(
            span,
            format!("lanes must be `inherit` or `from_param(<param>)`, got `{other}`"),
        )),
    }
}

/// A numeric literal with an optional leading `-` (param bounds and defaults can be negative).
fn parse_signed_float(input: ParseStream) -> syn::Result<f32> {
    let neg = input.peek(Token![-]);
    if neg {
        input.parse::<Token![-]>()?;
    }
    let lit: Lit = input.parse()?;
    let val = match lit {
        Lit::Float(f) => f.base10_parse::<f32>()?,
        Lit::Int(i) => i.base10_parse::<f32>()?,
        other => return Err(Error::new(other.span(), "expected a numeric literal")),
    };
    Ok(if neg { -val } else { val })
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
                inputs:  { freq: buffer },
                outputs: { audio: buffer },
                params:  { freq:     { 20.0..=20_000.0, default 440.0, "Hz", exp },
                           waveform: { 0.0..=1.0,        default 0.0,   "",   lin } },
                lanes: inherit,
            }"#,
        );
        assert!(out.contains("pub const IN_FREQ : usize = 0"), "{out}");
        assert!(out.contains("pub const OUT_AUDIO : usize = 0"), "{out}");
        assert!(out.contains("pub const P_FREQ : usize = 0"), "{out}");
        assert!(out.contains("pub const P_WAVEFORM : usize = 1"), "{out}");
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
                inputs:  { freq: buffer, gate: buffer },
                outputs: { audio: buffer },
                resources: { sample },
            }"#,
        );
        assert!(out.contains("type_name : \"sample\""), "{out}");
        assert!(out.contains("ResourceSlot :: new (\"sample\")"), "{out}");
        assert!(out.contains("pub const IN_GATE : usize = 1"), "{out}");
    }

    // Ports number sequentially in declaration order (ADR-0030) — a note input and a harmony input
    // are 0 and 1, not split per kind.
    #[test]
    fn ports_number_sequentially_and_lane_resolves() {
        let out = render(
            r#"Voicer {
                inputs:  { notes: note, ctx: harmony },
                outputs: { freq: buffer, gate: buffer },
                params:  { voices: { 1.0..=32.0, default 8.0, "", lin } },
                lanes: from_param(voices),
            }"#,
        );
        assert!(out.contains("pub const IN_NOTES : usize = 0"), "{out}");
        assert!(out.contains("pub const IN_CTX : usize = 1"), "{out}");
        assert!(out.contains("Port :: note (\"notes\")"), "{out}");
        assert!(out.contains("Port :: harmony (\"ctx\")"), "{out}");
        assert!(out.contains("LaneRule :: FromParam (P_VOICES)"), "{out}");
    }

    // A malformed contract is rejected *with a span*, in-band as a compile_error.
    #[test]
    fn duplicate_port_is_a_spanned_compile_error() {
        let out = render(
            r#"Bad {
                inputs: { a: buffer, a: buffer },
            }"#,
        );
        assert!(out.contains("compile_error !"), "{out}");
        assert!(out.contains("duplicate input port name"), "{out}");
        // The success path is NOT emitted alongside the error.
        assert!(!out.contains("fn contract"), "{out}");
    }

    #[test]
    fn bad_curve_keyword_is_rejected() {
        let out = render(r#"Bad { params: { a: { 0.0..=1.0, default 0.0, "", log } } }"#);
        assert!(out.contains("compile_error !"), "{out}");
    }

    // The filter target contract (ADR-0030): a buffer audio in/out, float-with-meta controls, and
    // an `enum(FilterMode)` naming its shared vocab type — sequential input ordinals.
    #[test]
    fn emits_filter_contract() {
        let out = render(
            r#"Filter {
                inputs:  { audio: buffer,
                           cutoff: float { 20.0..=20_000.0, default 1_000.0, "Hz", exp },
                           resonance: float { 0.0..=1.0, default 0.2 },
                           mode: enum(FilterMode) },
                outputs: { audio: buffer },
            }"#,
        );
        assert!(out.contains("pub const IN_AUDIO : usize = 0"), "{out}");
        assert!(out.contains("pub const IN_CUTOFF : usize = 1"), "{out}");
        assert!(out.contains("pub const IN_RESONANCE : usize = 2"), "{out}");
        assert!(out.contains("pub const IN_MODE : usize = 3"), "{out}");
        assert!(out.contains("pub const OUT_AUDIO : usize = 0"), "{out}");
        assert!(out.contains("Port :: buffer (\"audio\")"), "{out}");
        assert!(out.contains("Port :: float"), "{out}");
        assert!(out.contains("Curve :: Exponential"), "{out}");
        // Enum: `Port::enumerated` single-sourced off the shared vocab type's `enum_meta`.
        assert!(out.contains("Port :: enumerated"), "{out}");
        assert!(
            out.contains("vocab :: FilterMode :: enum_meta (\"mode\")"),
            "{out}"
        );
        // No locally-generated enum type any more — vocab types are shared (ADR-0030).
        assert!(!out.contains("pub enum"), "{out}");
    }

    // The oscillator target contract: a materialized `freq` and a `waveform` vocab enum.
    #[test]
    fn emits_oscillator_contract() {
        let out = render(
            r#"Oscillator {
                inputs:  { freq: float { 20.0..=20_000.0, default 440.0, "Hz", exp },
                           waveform: enum(Waveform) },
                outputs: { audio: buffer },
            }"#,
        );
        assert!(out.contains("pub const IN_FREQ : usize = 0"), "{out}");
        assert!(out.contains("pub const IN_WAVEFORM : usize = 1"), "{out}");
        assert!(
            out.contains("vocab :: Waveform :: enum_meta (\"waveform\")"),
            "{out}"
        );
        assert!(out.contains("type_name : \"oscillator\""), "{out}");
    }

    // An unknown port type is rejected with a span, as a compile_error.
    #[test]
    fn unknown_port_type_is_a_spanned_error() {
        let out = render(r#"Bad { inputs: { mode: signal } }"#);
        assert!(out.contains("compile_error !"), "{out}");
        assert!(out.contains("port type must be"), "{out}");
    }
}
