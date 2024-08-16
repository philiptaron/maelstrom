#![allow(clippy::manual_unwrap_or_default)]

mod config;
mod into_proto_buf;
mod into_result;
mod pocket_definition;
mod remote_derive;
mod try_from_proto_buf;

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Item};

#[proc_macro_derive(IntoResult)]
pub fn into_result(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match into_result::main(input) {
        Err(e) => e.into_compile_error().into(),
        Ok(v) => quote!(#v).into(),
    }
}

#[proc_macro_derive(IntoProtoBuf, attributes(proto))]
pub fn into_proto_buf(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match into_proto_buf::main(input) {
        Err(e) => e.into_compile_error().into(),
        Ok(v) => quote!(#v).into(),
    }
}

#[proc_macro_derive(TryFromProtoBuf, attributes(proto))]
pub fn try_from_proto_buf(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match try_from_proto_buf::main(input) {
        Err(e) => e.into_compile_error().into(),
        Ok(v) => quote!(#v).into(),
    }
}

#[proc_macro_derive(Config, attributes(config))]
pub fn config(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match config::main(input) {
        Err(e) => e.into_compile_error().into(),
        Ok(v) => quote!(#v).into(),
    }
}

#[proc_macro_attribute]
pub fn pocket_definition(attrs: TokenStream, input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as Item);
    match pocket_definition::main(&input, attrs.into()) {
        Err(e) => e.into_compile_error().into(),
        Ok(v) => quote! {
            #input
            #v
        }
        .into(),
    }
}

#[proc_macro]
pub fn remote_derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as remote_derive::Arguments);
    match remote_derive::main(input) {
        Err(e) => e.into_compile_error().into(),
        Ok(v) => quote!(#v).into(),
    }
}
