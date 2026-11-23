use proc_macro2::TokenStream;
use quote::quote;
use syn::{ItemFn, LitStr};

use crate::method::Method;

pub fn expand(attr: TokenStream, mut item: ItemFn) -> syn::Result<TokenStream> {
    let mut m = Method::parse_parts(&mut item.attrs, &item.sig)?;
    if !attr.is_empty() {
        m.js = syn::parse2::<LitStr>(attr)?.value();
    }
    let owner = quote!(super);
    let wrapper = m.wrapper(&m.js, &owner);
    let (rust, js, length, w) = (&m.rust, &m.js, m.length, m.wrapper_name());
    let vis = &item.vis;
    Ok(quote! {
        #item

        #[doc(hidden)]
        #[allow(non_snake_case, clippy::module_name_repetitions)]
        #vis mod #rust {
            #[allow(unused_imports)]
            use super::*;

            #wrapper

            pub const NATIVE: ::j8::frame::NativeFn = #w;

            pub fn install<'scope>(a: &mut ::j8::frame::Agent<'scope, '_>, target: ::j8::frame::JsObject<'scope>) -> ::j8::frame::JsObject<'scope> {
                ::j8::Glue::function(a, target, #js, NATIVE, #length)
            }
        }
    })
}
