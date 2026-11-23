mod bytecodes;
mod class;
mod function;
mod heap;
mod method;
mod object;
mod trace;

use proc_macro::TokenStream;
use syn::{DeriveInput, LitStr, parse_macro_input};

#[proc_macro_attribute]
pub fn class(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemImpl);
    class::expand(attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

#[proc_macro_attribute]
pub fn object(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemImpl);
    object::expand(attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

#[proc_macro_attribute]
pub fn module(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemImpl);
    object::expand_module(attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

#[proc_macro_attribute]
pub fn function(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemFn);
    function::expand(attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

#[proc_macro_derive(Collect, attributes(collect))]
pub fn derive_trace(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    trace::expand(&input, &quote::quote!(::j8framework)).unwrap_or_else(syn::Error::into_compile_error).into()
}

#[proc_macro_derive(Trace, attributes(collect))]
pub fn derive_j8_trace(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    trace::expand(&input, &quote::quote!(::j8::frame)).unwrap_or_else(syn::Error::into_compile_error).into()
}

#[proc_macro_attribute]
pub fn heap(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemStruct);
    heap::expand(&attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

#[proc_macro]
pub fn bytecodes(input: TokenStream) -> TokenStream {
    let rel = parse_macro_input!(input as LitStr);
    bytecodes::Tables::read(&rel).and_then(|t| t.expand(&rel)).unwrap_or_else(syn::Error::into_compile_error).into()
}
