use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::ItemImpl;

use crate::method::{Bound, Role};

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

struct Relabel(syn::Lifetime);

impl syn::visit_mut::VisitMut for Relabel {
    fn visit_lifetime_mut(&mut self, l: &mut syn::Lifetime) {
        *l = self.0.clone();
    }
}

impl Relabel {
    fn ty(name: &str, ty: &syn::Type) -> syn::Type {
        let mut ty = ty.clone();
        syn::visit_mut::VisitMut::visit_type_mut(&mut Self(syn::Lifetime::new(name, proc_macro2::Span::call_site())), &mut ty);
        ty
    }
}

pub fn expand(attr: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let Attr { name, extends } = syn::parse2(attr)?;
    if let Some(p) = item.generics.type_params().next() {
        return Err(syn::Error::new_spanned(p, "a #[j8::class] type takes lifetimes only (its brand is one type)"));
    }
    let b = Bound::of_impl(name.map(|n| quote!(#n)).unwrap_or_default(), &mut item)?;
    let ty = Relabel::ty("'scope", &item.self_ty);
    let anon = Relabel::ty("'_", &item.self_ty);
    let brand = format_ident!("__J8Brand{}", b.name.value().replace(|c: char| !c.is_alphanumeric(), "_"));
    let inherit = extends.map(|base| {
        let base = Relabel::ty("'scope", &base);
        quote! {
            fn parent(a: &mut ::j8::frame::Scope<'scope, '_>, target: ::j8::frame::JsObject<'scope>) -> ::core::option::Option<::j8::ClassHandle<'scope>> {
                ::core::option::Option::Some(::j8::Glue::parent::<#base>(a, target))
            }

            fn find(&self, brand: ::core::any::TypeId) -> ::core::option::Option<::core::ptr::NonNull<()>> {
                if brand == <Self as ::j8::Class<'scope>>::brand() {
                    return ::core::option::Option::Some(::core::ptr::NonNull::from(self).cast());
                }
                <#base as ::j8::Class<'scope>>::find(<Self as ::core::convert::AsRef<#base>>::as_ref(self), brand)
            }

            fn find_mut(&mut self, brand: ::core::any::TypeId) -> ::core::option::Option<::core::ptr::NonNull<()>> {
                if brand == <Self as ::j8::Class<'scope>>::brand() {
                    return ::core::option::Option::Some(::core::ptr::NonNull::from(self).cast());
                }
                <#base as ::j8::Class<'scope>>::find_mut(<Self as ::core::convert::AsMut<#base>>::as_mut(self), brand)
            }
        }
    });
    let native = quote!(::j8::frame::NativeFn);
    let ctor = match b.methods.iter().find(|m| m.role == Role::Constructor) {
        Some(m) => {
            let w = m.wrapper_name();
            let len = m.length;
            quote!(::core::option::Option::Some((#brand::#w as #native, #len)))
        }
        None => quote!(::core::option::Option::None),
    };
    let (owner, ty_tokens) = (quote!(#brand), quote!(#ty));
    let (name, members, wrappers) = (&b.name, b.members(&owner), b.wrappers(&ty_tokens, &quote!(<#ty>)));
    Ok(quote! {
        #item

        #[doc(hidden)]
        struct #brand;

        impl #brand {
            #(#wrappers)*
        }

        unsafe impl<'scope> ::j8::Class<'scope> for #ty {
            fn brand() -> ::core::any::TypeId {
                ::core::any::TypeId::of::<#brand>()
            }

            const NAME: &'static str = #name;
            const CONSTRUCTOR: ::core::option::Option<(#native, u32)> = #ctor;
            const MEMBERS: &'static [::j8::Member] = &[#(#members),*];
            #inherit
        }

        impl ::j8::Install for #anon {
            fn install_into<'scope>(a: &mut ::j8::frame::Scope<'scope, '_>, target: ::j8::frame::JsObject<'scope>) {
                let _ = <#ty as ::j8::Class<'scope>>::install(a, target);
            }
        }
    })
}
