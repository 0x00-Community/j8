use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Fields, Ident, ItemStruct, Lifetime};

fn mutable(attrs: &mut Vec<syn::Attribute>, field: &Ident) -> syn::Result<Option<Ident>> {
    let mut found = None;
    let mut err = None;
    attrs.retain(|a| {
        if !a.path().is_ident("mutable") {
            return true;
        }
        let mut set = format_ident!("set_{}", field);
        if matches!(a.meta, syn::Meta::List(_)) {
            let r = a.parse_nested_meta(|m| {
                if m.path.is_ident("set") {
                    set = m.value()?.parse::<Ident>()?;
                    Ok(())
                } else {
                    Err(m.error("expected `set = name`"))
                }
            });
            if let Err(e) = r {
                err = Some(e);
            }
        }
        found = Some(set);
        false
    });
    match err {
        Some(e) => Err(e),
        None => Ok(found),
    }
}

pub fn expand(attr: &TokenStream, mut item: ItemStruct) -> syn::Result<TokenStream> {
    let with_new = match attr.to_string().as_str() {
        "" => false,
        "new" => true,
        _ => return Err(syn::Error::new(Span::call_site(), "#[j8::heap] takes nothing or `new`")),
    };
    let gc = item.generics.lifetimes().next().map_or_else(|| Lifetime::new("'static", Span::call_site()), |l| l.lifetime.clone());
    let Fields::Named(fields) = &mut item.fields else {
        return Err(syn::Error::new_spanned(&item.ident, "#[j8::heap] needs a struct with named fields"));
    };
    let mut accessors = Vec::new();
    let mut params = Vec::new();
    let mut inits = Vec::new();
    for f in &mut fields.named {
        let name = f.ident.clone().expect("named field");
        let ty = f.ty.clone();
        match mutable(&mut f.attrs, &name)? {
            Some(set) => {
                f.ty = syn::parse_quote!(<#ty as ::j8framework::Mutable<#gc>>::Cell);
                let doc = format!("Sets `{name}` (through the write barrier when it holds a GC pointer)");
                accessors.push(quote! {
                    #[inline]
                    pub fn #name(&self) -> #ty {
                        <#ty as ::j8framework::Mutable<#gc>>::load(&self.#name)
                    }

                    #[doc = #doc]
                    #[inline]
                    pub fn #set(&self, mc: &::j8framework::Mutation<#gc>, v: #ty) {
                        <#ty as ::j8framework::Mutable<#gc>>::store(&self.#name, mc, v);
                    }
                });
                inits.push(quote!(#name: <#ty as ::j8framework::Mutable<#gc>>::cell(#name)));
            }
            None => inits.push(quote!(#name)),
        }
        params.push(quote!(#name: #ty));
    }
    if !item.attrs.iter().any(|a| a.path().is_ident("derive") && a.to_token_stream_contains("Collect")) {
        item.attrs.push(syn::parse_quote!(#[derive(::j8framework::Collect)]));
    }
    let name = &item.ident;
    let (ig, tg, wc) = item.generics.split_for_impl();
    let new = with_new.then(|| {
        quote! {
            #[allow(clippy::too_many_arguments)]
            pub fn new(#(#params),*) -> Self {
                Self { #(#inits),* }
            }
        }
    });
    Ok(quote! {
        #item

        impl #ig #name #tg #wc {
            #new
            #(#accessors)*
        }
    })
}

trait ContainsWord {
    fn to_token_stream_contains(&self, word: &str) -> bool;
}

impl ContainsWord for syn::Attribute {
    fn to_token_stream_contains(&self, word: &str) -> bool {
        quote!(#self).to_string().split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == word)
    }
}
