// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use proc_macro::TokenStream;
use quote::quote;
use syn::ext::IdentExt;
use syn::{Data, DeriveInput, Field, Fields, FieldsNamed, LitStr, Token, parse_macro_input};

/// Generates `UciSectionSchema` from a settings struct.
///
/// Every named field is mapped one-to-one:
///
/// ```text
/// Rust field name -> UCI option name
/// Rust field name -> config-rs field name
/// ```
#[proc_macro_derive(UciSection)]
pub fn derive_uci_section(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match expand_uci_section(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Generates `UciConfigSchema` from the root settings struct.
///
/// Required container attribute:
///
/// ```ignore
/// #[uci(package = "sqm-autorate-rust")]
/// ```
///
/// Every field must specify its UCI section:
///
/// ```ignore
/// #[uci(section)]
/// network: NetworkSettings,
///
/// #[uci(section = "advanced_settings")]
/// advanced: AdvancedSettings,
/// ```
#[proc_macro_derive(UciConfig, attributes(uci))]
pub fn derive_uci_config(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match expand_uci_config(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn expand_uci_section(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let struct_name = &input.ident;
    let fields = require_named_fields(input)?;

    let mappings = fields
        .named
        .iter()
        .map(|field| {
            let field_ident = field
                .ident
                .as_ref()
                .expect("named fields always have identifiers");

            let field_name = field_ident.unraw().to_string();
            let field_name = LitStr::new(&field_name, field_ident.span());

            quote! {
                crate::settings::uci_schema::UciOptionMapping {
                    uci_option: #field_name,
                    config_field: #field_name,
                }
            }
        })
        .collect::<Vec<_>>();

    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics
            crate::settings::uci_schema::UciSectionSchema
            for #struct_name #type_generics
            #where_clause
        {
            const UCI_OPTIONS: &'static [
                crate::settings::uci_schema::UciOptionMapping
            ] = &[
                #(#mappings),*
            ];
        }
    })
}

fn expand_uci_config(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let struct_name = &input.ident;
    let fields = require_named_fields(input)?;
    let package = parse_package_name(input)?;

    let mut sections = Vec::with_capacity(fields.named.len());

    for field in &fields.named {
        let field_ident = field
            .ident
            .as_ref()
            .expect("named fields always have identifiers");

        let field_type = &field.ty;

        let config_section_name = field_ident.unraw().to_string();
        let config_section_name = LitStr::new(&config_section_name, field_ident.span());

        let uci_section_name = parse_section_name(field)?;

        sections.push(quote! {
            crate::settings::uci_schema::UciSectionMapping {
                uci_section: #uci_section_name,
                config_section: #config_section_name,
                options: <
                    #field_type as
                    crate::settings::uci_schema::UciSectionSchema
                >::UCI_OPTIONS,
            }
        });
    }

    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics
            crate::settings::uci_schema::UciConfigSchema
            for #struct_name #type_generics
            #where_clause
        {
            const UCI_PACKAGE: &'static str = #package;

            const UCI_SECTIONS: &'static [
                crate::settings::uci_schema::UciSectionMapping
            ] = &[
                #(#sections),*
            ];
        }
    })
}

fn require_named_fields(input: &DeriveInput) -> syn::Result<&FieldsNamed> {
    match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => Ok(fields),

            Fields::Unnamed(_) | Fields::Unit => Err(syn::Error::new_spanned(
                input,
                "this derive only supports structs with named fields",
            )),
        },

        Data::Enum(_) | Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "this derive only supports structs",
        )),
    }
}

fn parse_package_name(input: &DeriveInput) -> syn::Result<LitStr> {
    let mut package = None;

    for attribute in &input.attrs {
        if !attribute.path().is_ident("uci") {
            continue;
        }

        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("package") {
                if package.is_some() {
                    return Err(meta.error("duplicate `package` attribute"));
                }

                let value: LitStr = meta.value()?.parse()?;

                if value.value().is_empty() {
                    return Err(syn::Error::new(value.span(), "`package` cannot be empty"));
                }

                package = Some(value);
                return Ok(());
            }

            Err(meta.error("expected `package = \"...\"`"))
        })?;
    }

    package.ok_or_else(|| {
        syn::Error::new_spanned(
            &input.ident,
            "UciConfig requires `#[uci(package = \"...\")]`",
        )
    })
}

fn parse_section_name(field: &Field) -> syn::Result<LitStr> {
    let field_ident = field
        .ident
        .as_ref()
        .expect("named fields always have identifiers");

    let default_name = field_ident.unraw().to_string();
    let default_name = LitStr::new(&default_name, field_ident.span());

    let mut section = None;

    for attribute in &field.attrs {
        if !attribute.path().is_ident("uci") {
            continue;
        }

        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("section") {
                if section.is_some() {
                    return Err(meta.error("duplicate `section` attribute"));
                }

                if meta.input.peek(Token![=]) {
                    let value: LitStr = meta.value()?.parse()?;

                    if value.value().is_empty() {
                        return Err(syn::Error::new(value.span(), "`section` cannot be empty"));
                    }

                    section = Some(value);
                } else {
                    section = Some(default_name.clone());
                }

                return Ok(());
            }

            Err(meta.error("expected `section` or `section = \"...\"`"))
        })?;
    }

    section.ok_or_else(|| {
        syn::Error::new_spanned(
            field,
            "UciConfig fields require `#[uci(section)]` \
             or `#[uci(section = \"...\")]`",
        )
    })
}
