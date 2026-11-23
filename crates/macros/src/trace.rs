use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Attribute, Data, DeriveInput, Fields, Ident, Type, parse_quote};

fn is_static(attrs: &[Attribute]) -> syn::Result<bool> {
    let mut found = false;
    for attr in attrs.iter().filter(|a| a.path().is_ident("collect")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("require_static") {
                found = true;
                Ok(())
            } else if meta.path.is_ident("no_drop") {
                Ok(())
            } else {
                Err(meta.error("expected #[collect(require_static)] or #[collect(no_drop)]"))
            }
        })?;
    }
    Ok(found)
}

struct Arm {
    pattern: TokenStream,
    traced: Vec<(Ident, Type)>,
}

impl Arm {
    fn of(path: &TokenStream, fields: &Fields) -> syn::Result<Self> {
        let mut traced = Vec::new();
        let mut binds = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            let b = format_ident!("__f{i}");
            if !is_static(&f.attrs)? {
                traced.push((b.clone(), f.ty.clone()));
            }
            binds.push(b);
        }
        let pattern = match fields {
            Fields::Named(n) => {
                let names = n.named.iter().map(|f| f.ident.as_ref());
                quote!(#path { #(#names: #binds),* })
            }
            Fields::Unnamed(_) => quote!(#path(#(#binds),*)),
            Fields::Unit => quote!(#path),
        };
        Ok(Self { pattern, traced })
    }
}

pub fn expand(input: &DeriveInput, krate: &TokenStream) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let leaf = is_static(&input.attrs)?;
    let arms: Vec<Arm> = match &input.data {
        _ if leaf => Vec::new(),
        Data::Struct(s) => vec![Arm::of(&quote!(Self), &s.fields)?],
        Data::Enum(e) => e
            .variants
            .iter()
            .map(|v| {
                let n = &v.ident;
                Arm::of(&quote!(Self::#n), &v.fields)
            })
            .collect::<syn::Result<_>>()?,
        Data::Union(_) => return Err(syn::Error::new_spanned(name, "a union cannot derive(Collect)")),
    };
    let mut generics = input.generics.clone();
    let params: Vec<Ident> = generics.type_params().map(|p| p.ident.clone()).collect();
    let wc = generics.make_where_clause();
    for p in params {
        wc.predicates.push(parse_quote!(#p: #krate::Trace));
    }
    let (ig, tg, wc) = generics.split_for_impl();
    let types: Vec<&Type> = arms.iter().flat_map(|a| a.traced.iter().map(|(_, t)| t)).collect();
    let trace_arms = arms.iter().map(|a| {
        let (p, b) = (&a.pattern, a.traced.iter().map(|(b, _)| b));
        quote!(#p => { #(#krate::Trace::trace(#b, __t);)* })
    });
    let relocate_arms = arms.iter().map(|a| {
        let (p, b) = (&a.pattern, a.traced.iter().map(|(b, _)| b));
        quote!(#p => { #(#krate::Trace::relocate(#b, __t);)* })
    });
    let body = |arms: Vec<TokenStream>| if leaf { quote!() } else { quote!(#[allow(unused_variables)] match self { #(#arms)* }) };
    let (trace_body, relocate_body) = (body(trace_arms.collect()), body(relocate_arms.collect()));
    let no_drop = (!types.is_empty()).then(|| {
        quote! {
            const _: () = {
                trait MustNotImplDrop {}
                #[allow(drop_bounds)]
                impl<T: ::core::ops::Drop> MustNotImplDrop for T {}
                impl #ig MustNotImplDrop for #name #tg #wc {}
            };
        }
    });
    Ok(quote! {
        unsafe impl #ig #krate::Trace for #name #tg #wc {
            const NO_EDGES: bool = true #(&& <#types as #krate::Trace>::NO_EDGES)*;

            #[inline]
            fn trace(&self, __t: &mut #krate::Tracer<'_>) {
                if <Self as #krate::Trace>::NO_EDGES {
                    return;
                }
                #trace_body
            }

            #[inline]
            fn relocate(&mut self, __t: &mut #krate::Tracer<'_>) {
                if <Self as #krate::Trace>::NO_EDGES {
                    return;
                }
                #relocate_body
            }
        }
        #no_drop
    })
}
