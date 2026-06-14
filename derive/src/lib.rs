//! Derive macros for the SimplicityHL compiler.
//!
//! This crate is internal to `simplicityhl`: the generated code references the
//! crate-private paths `crate::unstable::{RequireFeature, FeatureRequirement,
//! UnstableFeature}`, so the derive only expands correctly inside the
//! `simplicityhl` crate itself. It is published only because crates.io requires
//! proc-macro dependencies of published crates to be published.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DataEnum, DataStruct, DeriveInput, Fields, Ident};

/// Derive `RequireFeature` by recursing into every field.
///
/// Structs are destructured exhaustively and enums are matched exhaustively, so
/// the generated impl always covers the type's current shape. Every field and
/// every enum variant payload must itself implement `RequireFeature`; a field
/// whose type lacks an impl is a compile error, which forces a decision about
/// the feature requirements of any type newly reachable from the AST.
///
/// # Attributes
///
/// - `#[require_feature(requires(FeatureName))]` on a struct additionally
///   pushes `UnstableFeature::FeatureName`, attached to the struct's `span`
///   field, before recursing into children. This marks the node as the owner
///   of feature-gated syntax (a "gate node").
/// - `#[require_feature(skip)]` on a field or enum variant excludes it from
///   recursion. This is an unchecked promise that the field can never contain
///   feature-gated syntax; prefer giving the field's type a (possibly empty)
///   `RequireFeature` impl instead, so the decision is recorded once per type.
#[proc_macro_derive(RequireFeature, attributes(require_feature))]
pub fn derive_require_feature(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let required = requires_attr(&input.attrs)?;

    let body = match &input.data {
        Data::Struct(data) => expand_struct(name, data, required.as_ref())?,
        Data::Enum(data) => {
            if let Some(feature) = &required {
                return Err(syn::Error::new(
                    feature.span(),
                    "`requires(...)` is only supported on structs with a `span` field",
                ));
            }
            expand_enum(name, data)?
        }
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                name,
                "RequireFeature cannot be derived for unions",
            ))
        }
    };

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics crate::unstable::RequireFeature for #name #ty_generics #where_clause {
            fn feature_requirements(
                &self,
                __out: &mut Vec<crate::unstable::FeatureRequirement>,
            ) {
                #body
            }
        }
    })
}

fn expand_struct(
    name: &Ident,
    data: &DataStruct,
    required: Option<&Ident>,
) -> syn::Result<TokenStream2> {
    let fields = match &data.fields {
        Fields::Named(fields) => fields,
        Fields::Unnamed(fields) => {
            if let Some(feature) = required {
                return Err(requires_needs_span(feature));
            }
            let (pats, calls) = positional_bindings(fields, false)?;
            return Ok(quote! {
                let #name(#(#pats),*) = self;
                #(#calls)*
            });
        }
        Fields::Unit => {
            if let Some(feature) = required {
                return Err(requires_needs_span(feature));
            }
            return Ok(quote!());
        }
    };

    let mut pats = Vec::new();
    let mut calls = Vec::new();
    let mut has_span = false;
    for field in &fields.named {
        let ident = field.ident.as_ref().expect("named field");
        let is_span = ident == "span";
        has_span |= is_span;
        // The `span` binding stays live when `requires(...)` needs it,
        // even if the field is excluded from recursion.
        if skip_attr(&field.attrs)? && !(is_span && required.is_some()) {
            pats.push(quote!(#ident: _));
        } else {
            pats.push(quote!(#ident));
            calls.push(recurse_call(&quote!(#ident)));
        }
    }

    let push = match required {
        None => quote!(),
        Some(feature) => {
            if !has_span {
                return Err(requires_needs_span(feature));
            }
            quote! {
                __out.push(crate::unstable::FeatureRequirement::new(
                    crate::unstable::UnstableFeature::#feature,
                    *span,
                ));
            }
        }
    };

    Ok(quote! {
        let #name { #(#pats),* } = self;
        #push
        #(#calls)*
    })
}

fn expand_enum(name: &Ident, data: &DataEnum) -> syn::Result<TokenStream2> {
    let mut arms = Vec::new();
    for variant in &data.variants {
        let vname = &variant.ident;
        let skip_all = skip_attr(&variant.attrs)?;
        let arm = match &variant.fields {
            Fields::Unit => quote!(#name::#vname => {}),
            Fields::Unnamed(fields) => {
                let (pats, calls) = positional_bindings(fields, skip_all)?;
                quote!(#name::#vname(#(#pats),*) => { #(#calls)* })
            }
            Fields::Named(fields) => {
                let mut pats = Vec::new();
                let mut calls = Vec::new();
                for field in &fields.named {
                    let ident = field.ident.as_ref().expect("named field");
                    if skip_all || skip_attr(&field.attrs)? {
                        pats.push(quote!(#ident: _));
                    } else {
                        pats.push(quote!(#ident));
                        calls.push(recurse_call(&quote!(#ident)));
                    }
                }
                quote!(#name::#vname { #(#pats),* } => { #(#calls)* })
            }
        };
        arms.push(arm);
    }
    Ok(quote! {
        match self {
            #(#arms)*
        }
    })
}

fn positional_bindings(
    fields: &syn::FieldsUnnamed,
    skip_all: bool,
) -> syn::Result<(Vec<TokenStream2>, Vec<TokenStream2>)> {
    let mut pats = Vec::new();
    let mut calls = Vec::new();
    for (index, field) in fields.unnamed.iter().enumerate() {
        if skip_all || skip_attr(&field.attrs)? {
            pats.push(quote!(_));
        } else {
            let binding = format_ident!("__field_{}", index);
            pats.push(quote!(#binding));
            calls.push(recurse_call(&quote!(#binding)));
        }
    }
    Ok((pats, calls))
}

/// Generate a fully qualified trait call so that an inherent method named
/// `feature_requirements` on the field's type can never shadow the trait.
fn recurse_call(binding: &TokenStream2) -> TokenStream2 {
    quote! {
        crate::unstable::RequireFeature::feature_requirements(#binding, __out);
    }
}

fn requires_needs_span(feature: &Ident) -> syn::Error {
    syn::Error::new(
        feature.span(),
        "`requires(...)` needs a field named `span` to attach the requirement to",
    )
}

/// Parse `#[require_feature(skip)]` on a field or enum variant.
fn skip_attr(attrs: &[syn::Attribute]) -> syn::Result<bool> {
    let mut skip = false;
    for attr in attrs {
        if !attr.path().is_ident("require_feature") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                skip = true;
                Ok(())
            } else {
                Err(meta.error("expected `skip`"))
            }
        })?;
    }
    Ok(skip)
}

/// Parse `#[require_feature(requires(FeatureName))]` on a type.
fn requires_attr(attrs: &[syn::Attribute]) -> syn::Result<Option<Ident>> {
    let mut feature = None;
    for attr in attrs {
        if !attr.path().is_ident("require_feature") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("requires") {
                let content;
                syn::parenthesized!(content in meta.input);
                feature = Some(content.parse::<Ident>()?);
                Ok(())
            } else {
                Err(meta.error("expected `requires(FeatureName)`"))
            }
        })?;
    }
    Ok(feature)
}
