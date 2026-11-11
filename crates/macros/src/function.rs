//! `#[r8::function]` / `#[r8::function("名字")]`：写在函数上，原函数照留，旁边生成同名模块，
//! `名字::install(&mut agent, target)` 把它装成 `target` 上的 JS 函数（`名字::NATIVE` 是宿主函数本身）。
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

            /// 宿主函数本身
            pub const NATIVE: ::r8::frame::NativeFn = #w;

            /// 装成 `target` 上的 JS 函数，给出函数对象。
            pub fn install<'gc>(a: &mut ::r8::frame::Agent<'gc, '_>, target: ::r8::frame::JsObject<'gc>) -> ::r8::frame::JsObject<'gc> {
                ::r8::Glue::function(a, target, #js, NATIVE, #length)
            }
        }
    })
}
