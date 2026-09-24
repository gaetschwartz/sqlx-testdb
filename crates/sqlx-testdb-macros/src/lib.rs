//! `#[sqlx_testdb::test]`: runs an async sqlx test against its own throwaway database.

mod args;

use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{ItemFn, parse_macro_input};

use crate::args::{Args, SchemaOverride};

const MANIFEST_DIR_VAR: &str = "CARGO_MANIFEST_DIR";

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
    let config = args.config.as_ref().map_or_else(
        || quote! { ::core::option::Option::None },
        |path| quote! { ::core::option::Option::Some(#path as fn() -> ::sqlx_testdb::Config) },
    );
    let schema = match &args.schema {
        None => quote! { ::core::option::Option::None },
        Some(SchemaOverride::None) => {
            quote! { ::core::option::Option::Some(::sqlx_testdb::SchemaOverride::None) }
        }
        Some(SchemaOverride::Migrator(path)) => quote! {
            ::core::option::Option::Some(::sqlx_testdb::SchemaOverride::Migrator(&#path))
        },
        Some(SchemaOverride::Migrations(dir)) => quote! {
            ::core::option::Option::Some(::sqlx_testdb::SchemaOverride::Migrations(#dir))
        },
    };
    let source_dir = source_dir().unwrap_or_default();
    let fixtures = &args.fixtures;
    let attrs = std::mem::take(&mut item.attrs);
    let vis = std::mem::replace(&mut item.vis, syn::Visibility::Inherited);
    Ok(quote! {
        #(#attrs)*
        #[::core::prelude::v1::test]
        #vis fn #name() {
            #item
            static SPEC: ::sqlx_testdb::TestSpec = ::sqlx_testdb::TestSpec {
                manifest_dir: ::core::env!("CARGO_MANIFEST_DIR"),
                source_dir: #source_dir,
                config: #config,
                schema: #schema,
                fixtures: &[#(#fixtures),*],
            };
            ::sqlx_testdb::run(
                &SPEC,
                ::core::concat!(::core::module_path!(), "::", ::core::stringify!(#name)),
                #invoke,
            );
        }
    })
}

fn source_dir() -> Option<String> {
    let manifest_dir = std::env::var_os(MANIFEST_DIR_VAR)?;
    let file = std::env::current_dir().ok()?.join(proc_macro::Span::call_site().local_file()?);
    let dir = file.parent()?.strip_prefix(manifest_dir).ok()?;
    dir.to_str().map(str::to_owned)
}
