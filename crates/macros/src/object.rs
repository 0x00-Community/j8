//! `#[r8::object]`：写在只有关联函数的 `impl` 块上，导出成一个普通对象（`console` / `process` 这类）：
//! 函数是对象的方法，`#[js(getter)]` / `#[js(setter)]` 是对象的访问器。生成 `r8::Object` 实现。
use proc_macro2::TokenStream;
use quote::quote;
use syn::ItemImpl;

use crate::method::{Bound, Receiver, Role};

pub fn expand(attr: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let b = Bound::of_impl(attr, &mut item)?;
    if let Some(m) = b.methods.iter().find(|m| m.receiver != Receiver::None || m.role == Role::Constructor) {
        return Err(syn::Error::new(m.rust.span(), "#[r8::object] takes associated functions only (no self, no constructor)"));
    }
    let (name, members, wrappers, self_ty) = (&b.name, b.members(), b.wrappers(), &item.self_ty);
    let (impl_generics, _, where_clause) = item.generics.split_for_impl();
    Ok(quote! {
        #item

        impl #impl_generics #self_ty #where_clause {
            #(#wrappers)*
        }

        impl #impl_generics ::r8::Object for #self_ty #where_clause {
            const NAME: &'static str = #name;
            const MEMBERS: &'static [::r8::Member] = &[#(#members),*];
        }
    })
}
