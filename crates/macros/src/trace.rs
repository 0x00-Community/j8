//! `#[derive(Collect)]`: j8 堆类型的追踪 / 重定位（实现 paragc 的 `Trace`，经 `::j8framework::gc` 引用）。
//!
//! - 逐个字段调 `trace` / `relocate`；`NO_EDGES` 是参与字段的与
//! - `#[collect(require_static)]` 放在字段上：不是 GC 边的数据（`Arc<Shape>`、`Box<dyn Any + Send + Sync>` 之类），不追踪，
//!   也不要求实现 `Trace`；放在类型上：整个类型没有 GC 边
//! - `#[collect(no_drop)]`：只为兼容旧写法，什么都不做（下面这条检查总是开着）
//! - 有 GC 边的类型不许实现 `Drop`（析构顺序不定，`Drop` 里可能碰到已析构的对象），derive 会检查
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Attribute, Data, DeriveInput, Fields, Ident, Type, parse_quote};

/// 属性里有没有 `#[collect(require_static)]`
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
                Err(meta.error("只认 #[collect(require_static)] / #[collect(no_drop)]"))
            }
        })?;
    }
    Ok(found)
}

/// 一个 match 分支：模式 + 参与追踪的绑定名与类型
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

pub fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
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
        Data::Union(_) => return Err(syn::Error::new_spanned(name, "union 不能 derive(Collect)")),
    };
    let mut generics = input.generics.clone();
    let params: Vec<Ident> = generics.type_params().map(|p| p.ident.clone()).collect();
    let wc = generics.make_where_clause();
    for p in params {
        wc.predicates.push(parse_quote!(#p: ::j8framework::Trace));
    }
    let (ig, tg, wc) = generics.split_for_impl();
    let types: Vec<&Type> = arms.iter().flat_map(|a| a.traced.iter().map(|(_, t)| t)).collect();
    let trace_arms = arms.iter().map(|a| {
        let (p, b) = (&a.pattern, a.traced.iter().map(|(b, _)| b));
        quote!(#p => { #(::j8framework::Trace::trace(#b, __t);)* })
    });
    let relocate_arms = arms.iter().map(|a| {
        let (p, b) = (&a.pattern, a.traced.iter().map(|(b, _)| b));
        quote!(#p => { #(::j8framework::Trace::relocate(#b, __t);)* })
    });
    let body = |arms: Vec<TokenStream>| if leaf { quote!() } else { quote!(#[allow(unused_variables)] match self { #(#arms)* }) };
    let (trace_body, relocate_body) = (body(trace_arms.collect()), body(relocate_arms.collect()));
    // 没有边的类型可以有 Drop（析构里碰不到 Gc）
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
        // SAFETY: 逐个字段追踪 / 重定位（require_static 的字段与类型不含 GC 边）
        unsafe impl #ig ::j8framework::Trace for #name #tg #wc {
            const NO_EDGES: bool = true #(&& <#types as ::j8framework::Trace>::NO_EDGES)*;

            #[inline]
            fn trace(&self, __t: &mut ::j8framework::Tracer<'_>) {
                if <Self as ::j8framework::Trace>::NO_EDGES {
                    return;
                }
                #trace_body
            }

            #[inline]
            fn relocate(&mut self, __t: &mut ::j8framework::Tracer<'_>) {
                if <Self as ::j8framework::Trace>::NO_EDGES {
                    return;
                }
                #relocate_body
            }
        }
        #no_drop
    })
}
