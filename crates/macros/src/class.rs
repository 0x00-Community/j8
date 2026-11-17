//! `#[j8::class]`: placed on an `impl` block; methods are exported as members of a JS class (constructor,
//! prototype methods, accessors, static methods). Generates an `j8::Class` implementation.
//!
//! `#[j8::class(extends = Base)]` (with an optional name first): the class's prototype chain goes through `Base`'s, and
//! `Base`'s methods work on instances through `AsRef<Base>` / `AsMut<Base>` (the type must implement both).
use proc_macro2::TokenStream;
use quote::quote;
use syn::ItemImpl;

use crate::method::{Bound, Role};

/// The attribute: an optional JS name and an optional `extends = Base`
struct Attr {
    name: Option<syn::LitStr>,
    extends: Option<syn::Type>,
}

impl syn::parse::Parse for Attr {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut attr = Self { name: None, extends: None };
        while !input.is_empty() {
            if input.peek(syn::LitStr) {
                attr.name = Some(input.parse()?);
            } else {
                let key: syn::Ident = input.parse()?;
                if key != "extends" {
                    return Err(syn::Error::new_spanned(key, "expected a name string or `extends = Base`"));
                }
                input.parse::<syn::Token![=]>()?;
                attr.extends = Some(input.parse()?);
            }
            if !input.is_empty() {
                input.parse::<syn::Token![,]>()?;
            }
        }
        Ok(attr)
    }
}

pub fn expand(attr: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let Attr { name, extends } = syn::parse2(attr)?;
    let b = Bound::of_impl(name.map(|n| quote!(#n)).unwrap_or_default(), &mut item)?;
    // `extends = Base`: the prototype chain, and the base's methods see `AsRef<Base>` / `AsMut<Base>` of this value
    let inherit = extends.map(|base| {
        quote! {
            fn parent<'gc>(a: &mut ::j8::frame::Agent<'gc, '_>, target: ::j8::frame::JsObject<'gc>) -> ::core::option::Option<::j8::ClassHandle<'gc>> {
                ::core::option::Option::Some(::j8::Glue::parent::<#base>(a, target))
            }

            fn upcast(this: &dyn ::core::any::Any, ty: ::core::any::TypeId) -> ::core::option::Option<&dyn ::core::any::Any> {
                let me = this.downcast_ref::<Self>()?;
                if ty == ::core::any::TypeId::of::<Self>() {
                    return ::core::option::Option::Some(me);
                }
                <#base as ::j8::Class>::upcast(<Self as ::core::convert::AsRef<#base>>::as_ref(me), ty)
            }

            fn upcast_mut(this: &mut dyn ::core::any::Any, ty: ::core::any::TypeId) -> ::core::option::Option<&mut dyn ::core::any::Any> {
                let me = this.downcast_mut::<Self>()?;
                if ty == ::core::any::TypeId::of::<Self>() {
                    return ::core::option::Option::Some(me);
                }
                <#base as ::j8::Class>::upcast_mut(<Self as ::core::convert::AsMut<#base>>::as_mut(me), ty)
            }
        }
    });
    let native = quote!(::j8::frame::NativeFn);
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

        impl #impl_generics ::j8::Class for #self_ty #where_clause {
            const NAME: &'static str = #name;
            const CONSTRUCTOR: ::core::option::Option<(#native, u32)> = #ctor;
            const MEMBERS: &'static [::j8::Member] = &[#(#members),*];
            #inherit
        }

        impl #impl_generics ::j8::Install for #self_ty #where_clause {
            fn install_into<'gc>(a: &mut ::j8::frame::Agent<'gc, '_>, target: ::j8::frame::JsObject<'gc>) {
                let _ = <Self as ::j8::Class>::install(a, target);
            }
        }
    })
}
