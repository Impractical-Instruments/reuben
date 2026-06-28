//! `#[derive(ArgValue)]` — integrate a shared *vocab* type with the central [`Arg`] enum
//! (ADR-0030).
//!
//! A vocab type's Rust name **is** its [`Arg`] variant: `Note` ↔ `Arg::Note`, `SnapTarget` ↔
//! `Arg::SnapTarget`. The derive generates the glue so a type is defined *once* and reused
//! everywhere (the divergence ADR-0030 fixes: per-operator enum duplication):
//!
//! - **structs** (`Note`, `Harmony`) get `From<T> for Arg` + `TryFrom<&Arg> for T`.
//! - **unit enums** (`SnapTarget`, `GateMode`) get the above *plus* the Enum-over-OSC table —
//!   `VARIANTS` / `DEFAULT_INDEX` / `from_index` / `to_index` / `from_symbol` / `resolve_arg`
//!   and an `enum_meta()` that builds the descriptor's [`EnumMeta`] from the same tokens, so
//!   the type and its descriptor metadata cannot drift.
//!
//! The OSC flat-multi-arg conversion for structs (`Note ↔ /note pitch vel`) is generated at
//! the boundary in phase 6, not here.
//!
//! [`Arg`]: ../../reuben_core/message/enum.Arg.html
//! [`EnumMeta`]: ../../reuben_core/descriptor/struct.EnumMeta.html

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields};

/// Expand `#[derive(ArgValue)]`. Over `proc_macro2` so it is unit-testable without a
/// proc-macro context.
pub fn expand(input: TokenStream) -> TokenStream {
    let ast: DeriveInput = match syn::parse2(input) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error(),
    };
    match &ast.data {
        Data::Enum(data) => expand_enum(&ast, data),
        Data::Struct(_) => expand_struct(&ast),
        Data::Union(_) => syn::Error::new_spanned(&ast.ident, "ArgValue cannot derive on a union")
            .to_compile_error(),
    }
}

/// `From<T> for Arg` + `TryFrom<&Arg> for T` — the integration every vocab type gets. The
/// type's name is the `Arg` variant name.
fn arg_conversions(name: &syn::Ident) -> TokenStream {
    quote! {
        impl ::core::convert::From<#name> for ::reuben_core::message::Arg {
            fn from(v: #name) -> Self {
                ::reuben_core::message::Arg::#name(v)
            }
        }

        impl ::core::convert::TryFrom<&::reuben_core::message::Arg> for #name {
            type Error = ();
            fn try_from(arg: &::reuben_core::message::Arg) -> ::core::result::Result<Self, ()> {
                match arg {
                    ::reuben_core::message::Arg::#name(v) => ::core::result::Result::Ok(::core::clone::Clone::clone(v)),
                    _ => ::core::result::Result::Err(()),
                }
            }
        }

        impl<'a> ::reuben_core::message::FromArg<'a> for #name {
            fn from_arg(arg: &'a ::reuben_core::message::Arg) -> ::core::option::Option<Self> {
                <Self as ::core::convert::TryFrom<&::reuben_core::message::Arg>>::try_from(arg).ok()
            }
        }
    }
}

/// A struct vocab type (`Note`, `Harmony`): only the `Arg` integration.
fn expand_struct(ast: &DeriveInput) -> TokenStream {
    arg_conversions(&ast.ident)
}

/// A unit-enum vocab type (`SnapTarget`, `GateMode`): `Arg` integration plus the
/// Enum-over-OSC table (symbol primary, index fallback — ADR-0030's binding, derive-generated).
fn expand_enum(ast: &DeriveInput, data: &syn::DataEnum) -> TokenStream {
    let name = &ast.ident;

    // Every variant must be a unit variant — a vocab enum is a closed set of named choices.
    for v in &data.variants {
        if !matches!(v.fields, Fields::Unit) {
            return syn::Error::new_spanned(
                v,
                "ArgValue enum variants must be unit variants (no fields)",
            )
            .to_compile_error();
        }
    }
    if data.variants.is_empty() {
        return syn::Error::new_spanned(name, "ArgValue enum needs at least one variant")
            .to_compile_error();
    }

    let idents: Vec<&syn::Ident> = data.variants.iter().map(|v| &v.ident).collect();
    let symbols: Vec<String> = idents.iter().map(|i| i.to_string()).collect();

    // The default variant: the one marked `#[default]`, else the first.
    let default_index = data
        .variants
        .iter()
        .position(|v| v.attrs.iter().any(|a| a.path().is_ident("default")))
        .unwrap_or(0);
    let default_ident = idents[default_index];
    let default_index_lit = proc_macro2::Literal::usize_unsuffixed(default_index);

    let from_index_arms = idents.iter().enumerate().map(|(i, v)| {
        let idx = proc_macro2::Literal::usize_unsuffixed(i);
        quote! { #idx => ::core::option::Option::Some(Self::#v) }
    });
    let to_index_arms = idents.iter().enumerate().map(|(i, v)| {
        let idx = proc_macro2::Literal::usize_unsuffixed(i);
        quote! { Self::#v => #idx }
    });
    let from_symbol_arms = idents.iter().zip(&symbols).map(|(v, s)| {
        quote! { #s => ::core::option::Option::Some(Self::#v) }
    });

    let conversions = arg_conversions(name);

    quote! {
        impl #name {
            /// The variant **symbols**, index-aligned with the enum — the Enum-over-OSC table.
            pub const VARIANTS: &'static [&'static str] = &[ #(#symbols),* ];
            /// Index of the unwired default variant (`#[default]` if marked, else the first).
            pub const DEFAULT_INDEX: usize = #default_index_lit;
            /// The unwired default variant.
            pub const DEFAULT: #name = #name::#default_ident;

            /// The variant at index `i`, or `None` if out of range.
            pub fn from_index(i: usize) -> ::core::option::Option<Self> {
                match i { #(#from_index_arms,)* _ => ::core::option::Option::None }
            }
            /// This variant's index — its on-wire integer form.
            pub fn to_index(self) -> usize {
                match self { #(#to_index_arms),* }
            }
            /// This variant's **symbol** — its on-wire string form (`VARIANTS[to_index()]`).
            pub fn symbol(self) -> &'static str {
                Self::VARIANTS[self.to_index()]
            }
            /// The variant whose symbol is `s`, or `None`.
            pub fn from_symbol(s: &str) -> ::core::option::Option<Self> {
                match s { #(#from_symbol_arms,)* _ => ::core::option::Option::None }
            }

            /// The descriptor [`EnumMeta`](::reuben_core::descriptor::EnumMeta) for an input of
            /// this enum named `name` — single-sourced with the type above so they cannot drift.
            /// Its `resolve` is a non-capturing closure over [`resolve_arg`](Self::resolve_arg),
            /// so routing can normalize an enum control message to this type's concrete `Arg`
            /// without knowing `Self`.
            pub fn enum_meta(name: &'static str) -> ::reuben_core::descriptor::EnumMeta {
                ::reuben_core::descriptor::EnumMeta {
                    name,
                    type_name: ::core::stringify!(#name),
                    variants: Self::VARIANTS,
                    default: Self::DEFAULT_INDEX,
                    resolve: |arg| Self::resolve_arg(arg).map(::reuben_core::message::Arg::from),
                }
            }

            /// Resolve an [`Arg`](::reuben_core::message::Arg) to this enum (ADR-0030 binding):
            /// the concrete variant first, then a **symbol** (`Str`), then an **index** fallback
            /// (`I32`/`F32`, in range). Allocation-free — the boundary/latch path.
            pub fn resolve_arg(arg: &::reuben_core::message::Arg) -> ::core::option::Option<Self> {
                use ::reuben_core::message::Arg;
                match arg {
                    Arg::#name(v) => ::core::option::Option::Some(*v),
                    Arg::Str(s) => Self::from_symbol(s.as_str()),
                    Arg::I32(i) => usize::try_from(*i).ok().and_then(Self::from_index),
                    Arg::F32(f) => usize::try_from(f.round() as i64).ok().and_then(Self::from_index),
                    _ => ::core::option::Option::None,
                }
            }
        }

        #conversions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str) -> String {
        let ts: TokenStream = src.parse().expect("token stream");
        expand(ts).to_string()
    }

    #[test]
    fn struct_gets_arg_conversions_only() {
        let out = render("struct Note { pitch: Pitch, velocity: f32 }");
        assert!(
            out.contains("impl :: core :: convert :: From < Note >"),
            "{out}"
        );
        assert!(out.contains("Arg :: Note (v)"), "{out}");
        assert!(out.contains("TryFrom"), "{out}");
        // No enum table for a struct.
        assert!(!out.contains("VARIANTS"), "{out}");
    }

    #[test]
    fn unit_enum_gets_table_and_conversions() {
        let out = render("enum SnapTarget { Scale, Chord, ChordThenScale }");
        assert!(out.contains("VARIANTS : & 'static [& 'static str] = & [\"Scale\" , \"Chord\" , \"ChordThenScale\"]"), "{out}");
        assert!(out.contains("DEFAULT_INDEX : usize = 0"), "{out}");
        assert!(
            out.contains("DEFAULT : SnapTarget = SnapTarget :: Scale"),
            "{out}"
        );
        assert!(out.contains("fn from_symbol"), "{out}");
        assert!(out.contains("fn resolve_arg"), "{out}");
        assert!(out.contains("fn enum_meta"), "{out}");
        assert!(out.contains("Arg :: SnapTarget (v)"), "{out}");
    }

    #[test]
    fn honors_default_attribute() {
        let out = render("enum SnapDir { Nearest, #[default] Up, Down }");
        assert!(out.contains("DEFAULT_INDEX : usize = 1"), "{out}");
        assert!(out.contains("DEFAULT : SnapDir = SnapDir :: Up"), "{out}");
    }

    #[test]
    fn data_enum_variant_is_rejected() {
        let out = render("enum Pitch { Degree(i32), Absolute(f32) }");
        assert!(out.contains("compile_error !"), "{out}");
        assert!(out.contains("unit variants"), "{out}");
    }

    #[test]
    fn union_is_rejected() {
        let out = render("union U { a: u32, b: f32 }");
        assert!(out.contains("compile_error !"), "{out}");
    }
}
