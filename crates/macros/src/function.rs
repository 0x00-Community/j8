//! `#[r8::function]` / `#[r8::function("name")]`: placed on a function; the original function is kept and a
//! module of the same name is generated next to it. `name::install(&mut agent, target)` installs it as a JS
//! function on `target` (`name::NATIVE` is the host function itself).
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

            /// The host function itself
            pub const NATIVE: ::r8::frame::NativeFn = #w;

            /// Installs it as a JS function on `target` and returns the function object.
            pub fn install<'gc>(a: &mut ::r8::frame::Agent<'gc, '_>, target: ::r8::frame::JsObject<'gc>) -> ::r8::frame::JsObject<'gc> {
                ::r8::Glue::function(a, target, #js, NATIVE, #length)
            }
        }
    })
}
