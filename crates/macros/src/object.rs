//! `#[j8::object]`: placed on an `impl` block containing only associated functions; exports it as a plain
//! object (like `console` / `process`): the functions are the object's methods, and `#[js(getter)]` /
//! `#[js(setter)]` are the object's accessors. Generates an `j8::Object` implementation.
use proc_macro2::TokenStream;
use quote::quote;
use syn::ItemImpl;

use crate::method::{Bound, Receiver, Role};

pub fn expand(attr: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let b = Bound::of_impl(attr, &mut item)?;
    if let Some(m) = b.methods.iter().find(|m| m.receiver != Receiver::None || m.role == Role::Constructor) {
        return Err(syn::Error::new(m.rust.span(), "#[j8::object] takes associated functions only (no self, no constructor)"));
    }
    let (name, members, wrappers, self_ty) = (&b.name, b.members(), b.wrappers(), &item.self_ty);
    let (impl_generics, _, where_clause) = item.generics.split_for_impl();
    Ok(quote! {
        #item

        impl #impl_generics #self_ty #where_clause {
            #(#wrappers)*
        }

        impl #impl_generics ::j8::Object for #self_ty #where_clause {
            const NAME: &'static str = #name;
            const MEMBERS: &'static [::j8::Member] = &[#(#members),*];
        }

        impl #impl_generics ::j8::Install for #self_ty #where_clause {
            fn install_into<'gc>(a: &mut ::j8::frame::Agent<'gc, '_>, target: ::j8::frame::JsObject<'gc>) {
                let _ = <Self as ::j8::Object>::install(a, target);
            }
        }
    })
}

/// `#[j8::module("node:fs", "fs")]`: an object of functions exported as a native module under these specifiers (the
/// first is canonical): `import fs from "node:fs"` / `import { readFileSync } from "fs"` / `require("fs")` in a runtime
/// built with `j8::runtime!`.
pub fn expand_module(attr: TokenStream, item: ItemImpl) -> syn::Result<TokenStream> {
    let specs = syn::parse::Parser::parse2(syn::punctuated::Punctuated::<syn::LitStr, syn::Token![,]>::parse_terminated, attr)?;
    let Some(first) = specs.first() else {
        return Err(syn::Error::new(proc_macro2::Span::call_site(), "#[j8::module] needs at least one specifier, e.g. #[j8::module(\"node:fs\", \"fs\")]"));
    };
    let first = first.clone();
    let self_ty = item.self_ty.clone();
    let (impl_generics, _, where_clause) = item.generics.split_for_impl();
    let (impl_generics, where_clause) = (quote!(#impl_generics), quote!(#where_clause));
    let specs = specs.iter();
    let object = expand(quote!(#first), item)?;
    Ok(quote! {
        #object

        impl #impl_generics ::j8::Module for #self_ty #where_clause {
            const SPECIFIERS: &'static [&'static str] = &[#(#specs),*];
        }
    })
}
