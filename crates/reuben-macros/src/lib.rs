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
//!     inputs:  { freq: signal },
//!     outputs: { audio: signal },
//!     params:  { freq:     { 20.0..=20_000.0, default 440.0, "Hz", exp },
//!                waveform: { 0.0..=1.0,        default 0.0,   "",   lin } },
//!     lanes: inherit,
//! });
//! ```

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

/// How a port is declared — either the legacy carrier kind or an ADR-0028 shape.
enum PortShapeAst {
    /// `name: signal | message | context` (legacy carrier).
    Legacy(Ident),
    /// `name: float` (bare wire-in/output) or `name: float { .. }` (materialized default).
    Float(Option<FloatMetaAst>),
    /// `name: enum { A, B, .. }` (live-switchable named choice; default = first variant).
    Enum(Vec<Ident>),
}

struct PortAst {
    name: Ident,
    shape: PortShapeAst,
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
                    let (kind, shape, float, variants) = match &p.shape {
                        PortShapeAst::Legacy(k) => (k.to_string(), None, None, Vec::new()),
                        PortShapeAst::Float(m) => (
                            String::new(),
                            Some("float".to_string()),
                            m.as_ref().map(|m| FloatMeta {
                                min: m.min,
                                max: m.max,
                                default: m.default,
                                unit: m.unit.clone(),
                                curve: m.curve.clone(),
                            }),
                            Vec::new(),
                        ),
                        PortShapeAst::Enum(vs) => (
                            String::new(),
                            Some("enum".to_string()),
                            None,
                            vs.iter().map(Ident::to_string).collect(),
                        ),
                    };
                    PortSpec {
                        name: p.name.to_string(),
                        kind,
                        shape,
                        float,
                        variants,
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
                    match p.shape.as_deref() {
                        // Legacy carrier — `Port::signal/message/context`.
                        None => {
                            let ctor = match p.kind.as_str() {
                                "message" => quote! { message },
                                "context" => quote! { context },
                                _ => quote! { signal },
                            };
                            quote! { ::reuben_core::descriptor::Port::#ctor(#name) }
                        }
                        // Bare `float` is today's `signal` carrier (Float shape, no materialized
                        // default); `float { .. }` is a materialized Float input.
                        Some("float") => match &p.float {
                            None => quote! { ::reuben_core::descriptor::Port::signal(#name) },
                            Some(m) => {
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
                        },
                        // `enum { .. }` — reference the generated type's `VARIANTS`/`DEFAULT_INDEX`
                        // so the descriptor and the type are single-sourced.
                        Some("enum") => {
                            let ty = Ident::new(&naming::struct_name(&p.name), Span::call_site());
                            quote! {
                                ::reuben_core::descriptor::Port::enumerated(
                                    ::reuben_core::descriptor::EnumMeta {
                                        name: #name,
                                        variants: #ty::VARIANTS,
                                        default: #ty::DEFAULT_INDEX,
                                    }
                                )
                            }
                        }
                        Some(other) => {
                            // Unreachable: validate() restricts shapes to the SHAPES set.
                            let msg = format!("unsupported port shape {other:?}");
                            quote! { compile_error!(#msg) }
                        }
                    }
                })
                .collect()
        };
        let inputs = port_toks(&model.inputs);
        let outputs = port_toks(&model.outputs);

        // One generated `Enum` type per `enum { .. }` port (inputs/outputs), planted at module
        // scope beside the index consts (ADR-0028). Carries `VARIANTS`/`DEFAULT`/`from_index` etc.
        let enum_types = model
            .inputs
            .iter()
            .chain(&model.outputs)
            .filter(|p| p.shape.as_deref() == Some("enum"))
            .map(enum_type_toks);

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

            #(#enum_types)*

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

/// The `pub enum <Type> { .. }` + impl for one `enum { .. }` port (ADR-0028). The type name is the
/// port name PascalCased (`waveform` → `Waveform`); variants are emitted verbatim. `VARIANTS` is
/// index-aligned with the variants (the Enum-over-OSC symbol table), `DEFAULT` is the first.
/// `validate()` guarantees a non-empty, identifier-shaped, unique variant set before we get here.
fn enum_type_toks(port: &model::PortModel) -> TokenStream {
    let ty = Ident::new(&naming::struct_name(&port.name), Span::call_site());
    let variants: Vec<Ident> = port
        .variants
        .iter()
        .map(|v| Ident::new(v, Span::call_site()))
        .collect();
    let symbols = &port.variants;
    let first = &variants[0];
    let from_arms = variants.iter().enumerate().map(|(i, v)| {
        let idx = proc_macro2::Literal::usize_unsuffixed(i);
        quote! { #idx => ::core::option::Option::Some(Self::#v) }
    });
    let to_arms = variants.iter().enumerate().map(|(i, v)| {
        let idx = proc_macro2::Literal::usize_unsuffixed(i);
        quote! { Self::#v => #idx }
    });
    quote! {
        /// A generated `Enum`-shape choice (ADR-0028), single-sourced with its descriptor
        /// `EnumMeta` by `operator_contract!`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum #ty { #(#variants),* }

        impl #ty {
            /// The variant **symbols**, index-aligned with the enum — the Enum-over-OSC table.
            pub const VARIANTS: &'static [&'static str] = &[ #(#symbols),* ];
            /// Index of the unwired default variant (the first declared).
            pub const DEFAULT_INDEX: usize = 0;
            /// The unwired default variant (the first declared).
            pub const DEFAULT: #ty = #ty::#first;

            /// The variant at index `i`, or `None` if out of range.
            pub fn from_index(i: usize) -> ::core::option::Option<Self> {
                match i { #(#from_arms,)* _ => ::core::option::Option::None }
            }
            /// This variant's index — its on-wire integer form.
            pub fn to_index(self) -> usize { match self { #(#to_arms),* } }
        }

        impl ::core::default::Default for #ty {
            fn default() -> Self { Self::DEFAULT }
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

/// A brace-wrapped, comma-separated port list. Each entry is `name: <decl>` where `<decl>` is a
/// legacy carrier kind (`signal`/`message`/`context`), a `float`/`float { .. }` shape, or an
/// `enum { A, B }` shape (ADR-0028).
fn parse_ports(input: ParseStream) -> syn::Result<Vec<PortAst>> {
    let body;
    braced!(body in input);
    let mut out = Vec::new();
    while !body.is_empty() {
        // `parse_any` so a port may be named with a keyword, e.g. m2s's `in`.
        let name = Ident::parse_any(&body)?;
        body.parse::<Token![:]>()?;
        // `parse_any` so the shape keyword may be a reserved word (`enum`).
        let kw = Ident::parse_any(&body)?;
        let shape = match kw.to_string().as_str() {
            "float" => {
                let meta = if body.peek(syn::token::Brace) {
                    Some(parse_float_meta(&body)?)
                } else {
                    None
                };
                PortShapeAst::Float(meta)
            }
            "enum" => {
                let vars;
                braced!(vars in body);
                let mut variants = Vec::new();
                while !vars.is_empty() {
                    variants.push(Ident::parse_any(&vars)?);
                    if vars.peek(Token![,]) {
                        vars.parse::<Token![,]>()?;
                    }
                }
                PortShapeAst::Enum(variants)
            }
            // Anything else is a legacy carrier kind; `validate()` rejects an unknown one.
            _ => PortShapeAst::Legacy(kw),
        };
        out.push(PortAst { name, shape });
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
                inputs:  { freq: signal },
                outputs: { audio: signal },
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
                inputs:  { freq: signal, gate: signal },
                outputs: { audio: signal },
                resources: { sample },
            }"#,
        );
        assert!(out.contains("type_name : \"sample\""), "{out}");
        assert!(out.contains("ResourceSlot :: new (\"sample\")"), "{out}");
        assert!(out.contains("pub const IN_GATE : usize = 1"), "{out}");
    }

    #[test]
    fn per_kind_ordinals_match_the_voicer_footgun() {
        let out = render(
            r#"Voicer {
                inputs:  { notes: message, ctx: context },
                outputs: { freq: signal, gate: signal },
                params:  { voices: { 1.0..=32.0, default 8.0, "", lin } },
                lanes: from_param(voices),
            }"#,
        );
        assert!(out.contains("pub const IN_NOTES : usize = 0"), "{out}");
        assert!(out.contains("pub const IN_CTX : usize = 0"), "{out}");
        assert!(out.contains("LaneRule :: FromParam (P_VOICES)"), "{out}");
    }

    // Tracer step 4: a malformed contract is rejected *with a span*, in-band as a compile_error.
    #[test]
    fn duplicate_port_is_a_spanned_compile_error() {
        let out = render(
            r#"Bad {
                inputs: { a: signal, a: signal },
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

    // ADR-0028 shape surface: the filter target contract — bare float, float-with-meta (full and
    // unit/curve-omitted), an enum input, and sequential (not per-kind) input ordinals.
    #[test]
    fn emits_shape_based_filter_contract() {
        let out = render(
            r#"Filter {
                inputs:  { audio: float,
                           cutoff: float { 20.0..=20_000.0, default 1_000.0, "Hz", exp },
                           resonance: float { 0.0..=1.0, default 0.2 },
                           mode: enum { Lp, Hp, Bp } },
                outputs: { audio: float },
            }"#,
        );
        // Inputs number in declaration order, regardless of shape.
        assert!(out.contains("pub const IN_AUDIO : usize = 0"), "{out}");
        assert!(out.contains("pub const IN_CUTOFF : usize = 1"), "{out}");
        assert!(out.contains("pub const IN_RESONANCE : usize = 2"), "{out}");
        assert!(out.contains("pub const IN_MODE : usize = 3"), "{out}");
        assert!(out.contains("pub const OUT_AUDIO : usize = 0"), "{out}");
        // Bare `float` is the `signal` carrier; `float { .. }` is a materialized `Port::float`.
        assert!(out.contains("Port :: signal (\"audio\")"), "{out}");
        assert!(out.contains("Port :: float"), "{out}");
        assert!(out.contains("Curve :: Exponential"), "{out}");
        // Enum: a generated type plus a `Port::enumerated` single-sourced off its `VARIANTS`.
        assert!(out.contains("pub enum Mode"), "{out}");
        assert!(out.contains("Lp , Hp , Bp"), "{out}");
        assert!(out.contains("Port :: enumerated"), "{out}");
        assert!(out.contains("variants : Mode :: VARIANTS"), "{out}");
        assert!(out.contains("default : Mode :: DEFAULT_INDEX"), "{out}");
    }

    // The oscillator target contract: a materialized `freq` and a live-switchable `waveform` enum.
    #[test]
    fn emits_shape_based_oscillator_contract() {
        let out = render(
            r#"Oscillator {
                inputs:  { freq: float { 20.0..=20_000.0, default 440.0, "Hz", exp },
                           waveform: enum { Sine, Saw } },
                outputs: { audio: float },
            }"#,
        );
        assert!(out.contains("pub const IN_FREQ : usize = 0"), "{out}");
        assert!(out.contains("pub const IN_WAVEFORM : usize = 1"), "{out}");
        assert!(out.contains("pub enum Waveform"), "{out}");
        assert!(out.contains("Sine , Saw"), "{out}");
        assert!(out.contains("Waveform :: VARIANTS"), "{out}");
        assert!(out.contains("type_name : \"oscillator\""), "{out}");
    }

    // A malformed shape is rejected with a span, as a compile_error (not silently emitted).
    #[test]
    fn empty_enum_is_a_spanned_compile_error() {
        let out = render(r#"Bad { inputs: { mode: enum { } } }"#);
        assert!(out.contains("compile_error !"), "{out}");
        assert!(out.contains("at least one variant"), "{out}");
        assert!(!out.contains("pub enum Mode"), "{out}");
    }
}
