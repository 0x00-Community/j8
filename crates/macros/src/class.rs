//! `#[r8::class]`：写在 `impl` 块上，方法导出成 JS 类的成员（构造器、原型方法、访问器、静态方法），生成 `r8::Class` 实现。
use proc_macro2::TokenStream;
use quote::quote;
use syn::ItemImpl;

use crate::method::{Bound, Role};

pub fn expand(attr: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let b = Bound::of_impl(attr, &mut item)?;
    let native = quote!(::r8::frame::NativeFn);
    let ctor = match b.methods.iter().find(|m| m.role == Role::Constructor) {
        Some(m) => {
            let w = m.wrapper_name();
            let len = m.length;
            quote!(::core::option::Option::Some((Self::#w as #native, #len)))
        }
        None => quote!(::core::option::Option::None),
    };
    let (name, members, wrappers, self_ty) = (&b.name, b.members(), b.wrappers(), &item.self_ty);
    let (impl_generics, _, where_clause) = item.generics.split_for_impl();
    Ok(quote! {
        #item

        impl #impl_generics #self_ty #where_clause {
            #(#wrappers)*
        }

        impl #impl_generics ::r8::Class for #self_ty #where_clause {
            const NAME: &'static str = #name;
            const CONSTRUCTOR: ::core::option::Option<(#native, u32)> = #ctor;
            const MEMBERS: &'static [::r8::Member] = &[#(#members),*];
        }
    })
}
