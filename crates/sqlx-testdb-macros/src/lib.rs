//! `#[sqlx_testdb::test]`: runs an async sqlx test against its own throwaway database.

mod args;
mod config;

use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{ItemFn, parse_macro_input};

use crate::args::{Args, SchemaOverride};

#[proc_macro_attribute]
pub fn test(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as Args);
    let item = parse_macro_input!(item as ItemFn);
    expand(&args, item).unwrap_or_else(syn::Error::into_compile_error).into()
}

fn expand(args: &Args, mut item: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
    if item.sig.asyncness.is_none() {
        return Err(syn::Error::new(item.sig.fn_token.span(), "a database test is an `async fn`"));
    }
    let name = item.sig.ident.clone();
    let invoke = match item.sig.inputs.len() {
        1 => quote! { #name },
        2 => quote! { |(first, second): (_, _)| #name(first, second) },
        _ => {
            return Err(syn::Error::new(
                item.sig.inputs.span(),
                "a database test takes a pool, a connection, connect options, \
                 or pool options and connect options",
            ));
        }
    };
    let config = match &args.config {
        Some(path) => quote! { &#path },
        None => configured(args, item.sig.ident.span())?,
    };
    let fixtures = args.fixtures.iter().map(|path| {
        quote! { ::sqlx_testdb::Fixture { path: #path, sql: ::core::include_str!(#path) } }
    });
    let attrs = std::mem::take(&mut item.attrs);
    let vis = std::mem::replace(&mut item.vis, syn::Visibility::Inherited);
    Ok(quote! {
        #(#attrs)*
        #[::core::prelude::v1::test]
        #vis fn #name() {
            #item
            static FIXTURES: &[::sqlx_testdb::Fixture] = &[#(#fixtures),*];
            ::sqlx_testdb::run(
                #config,
                ::core::concat!(::core::module_path!(), "::", ::core::stringify!(#name)),
                FIXTURES,
                #invoke,
            );
        }
    })
}

fn configured(args: &Args, span: proc_macro2::Span) -> syn::Result<proc_macro2::TokenStream> {
    let file = config::File::find(span)?;
    let schema = match &args.schema {
        Some(SchemaOverride::None) => quote! { ::sqlx_testdb::Schema::None },
        Some(SchemaOverride::Migrator(path)) => quote! { ::sqlx_testdb::Schema::Migrator(&#path) },
        Some(SchemaOverride::Migrations(dir)) => {
            let dir = config::manifest_relative(dir)?;
            quote! { ::sqlx_testdb::Schema::Migrations(#dir) }
        }
        None => file.schema(),
    };
    let config = file.config(&schema);
    let track = file.track();
    Ok(quote! {{
        #track
        static CONFIG: ::sqlx_testdb::Config = #config;
        &CONFIG
    }})
}
