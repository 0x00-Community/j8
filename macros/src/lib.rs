//! r8gc 的过程宏：`#[derive(Collect)]`、`Rootable!` 用的 `__unelide_lifetimes!`。
//!
//! 派生宏的模式与属性沿用 gc-arena-derive（MIT），多生成一个 `relocate`。
use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::visit_mut::VisitMut;
use synstructure::{AddBounds, BindStyle, Structure, decl_derive};

/// `Drop` 怎么处理。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 类型是 `'static`：里面没有 `Gc`，什么都不用做
    RequireStatic,
    /// 禁止实现 `Drop`（析构时 `Gc` 可能已经没了）
    NoDrop,
    /// 允许 `Drop`，由实现者保证不碰 `Gc`
    UnsafeDrop,
}

/// 类型上的 `#[collect(...)]`。
#[derive(Default)]
struct Options {
    mode: Option<Mode>,
    bound: Option<syn::LitStr>,
    gc_lifetime: Option<syn::Lifetime>,
}

impl Options {
    fn parse(attrs: &[syn::Attribute]) -> syn::Result<Self> {
        let mut o = Self::default();
        let Some(attr) = Derive::attr(attrs)? else {
            return Ok(o);
        };
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("bound") {
                o.bound = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("gc_lifetime") {
                o.gc_lifetime = Some(meta.value()?.parse()?);
            } else {
                let mode = [("require_static", Mode::RequireStatic), ("no_drop", Mode::NoDrop), ("unsafe_drop", Mode::UnsafeDrop)]
                    .into_iter()
                    .find(|(name, _)| meta.path.is_ident(name))
                    .map(|(_, m)| m)
                    .ok_or_else(|| meta.error("expected `require_static`, `no_drop`, `unsafe_drop`, `bound = \"…\"` or `gc_lifetime = …`"))?;
                if o.mode.replace(mode).is_some() {
                    return Err(meta.error("multiple modes specified"));
                }
            }
            Ok(())
        })?;
        Ok(o)
    }
}

/// 一次 `#[derive(Collect)]`。
struct Derive<'a> {
    s: Structure<'a>,
    options: Options,
    errors: Vec<syn::Error>,
}

impl Derive<'_> {
    /// 至多一个 `#[collect]`
    fn attr(attrs: &[syn::Attribute]) -> syn::Result<Option<&syn::Attribute>> {
        let mut found = attrs.iter().filter(|a| a.path().is_ident("collect"));
        let first = found.next();
        match found.next() {
            Some(dup) => Err(syn::Error::new_spanned(dup.path(), "multiple `#[collect]` attributes; merge them")),
            None => Ok(first),
        }
    }

    /// 不标 `require_static` 的字段才要遍历；标了的字段加 `'static` 约束
    fn filter_static_fields(&mut self) {
        let mut statics = Vec::new();
        let errors = &mut self.errors;
        self.s.filter(|b| match Self::attr(&b.ast().attrs) {
            Ok(Some(attr)) => {
                let mut is_static = false;
                let r = attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("require_static") && meta.input.is_empty() {
                        is_static = true;
                        statics.push(b.ast().ty.clone());
                        Ok(())
                    } else {
                        Err(meta.error("only `#[collect(require_static)]` is supported on a field"))
                    }
                });
                errors.extend(r.err());
                !is_static
            }
            Ok(None) => true,
            Err(e) => {
                errors.push(e);
                true
            }
        });
        for ty in statics {
            self.s.add_where_predicate(syn::parse_quote! { #ty: 'static });
        }
    }

    /// 类型只有一个生命周期参数时就是 `'gc`
    fn gc_lifetime(&self) -> syn::Result<Option<syn::Lifetime>> {
        if let Some(lt) = &self.options.gc_lifetime {
            return Ok(Some(lt.clone()));
        }
        let mut lts = self.s.ast().generics.lifetimes().map(|l| l.lifetime.clone());
        match (lts.next(), lts.next()) {
            (first, None) => Ok(first),
            (Some(_), Some(_)) => Err(syn::Error::new(Span::call_site(), "multiple lifetimes: specify `#[collect(gc_lifetime = 'gc)]`")),
            (None, Some(_)) => unreachable!(),
        }
    }

    fn collect_impl(&mut self) -> syn::Result<TokenStream> {
        if self.options.mode == Some(Mode::RequireStatic) {
            let mut s = self.s.clone();
            s.add_bounds(AddBounds::None);
            return Ok(s.gen_impl(quote! {
                gen unsafe impl<'gc> ::r8gc::Collect<'gc> for @Self where Self: 'static {
                    const NEEDS_TRACE: bool = false;
                }
            }));
        }
        self.filter_static_fields();
        if let syn::Data::Enum(_) = self.s.ast().data {
            for v in self.s.variants() {
                if let Some(a) = v.ast().attrs.iter().find(|a| a.path().is_ident("collect")) {
                    self.errors.push(syn::Error::new_spanned(a.path(), "`#[collect]` is not supported on enum variants"));
                }
            }
        }
        let mut needs_trace = quote!(false);
        for b in self.s.variants().iter().flat_map(synstructure::VariantInfo::bindings) {
            let ty = &b.ast().ty;
            let span = b.ast().span().resolved_at(Span::call_site());
            quote_spanned!(span=> || <#ty as ::r8gc::Collect>::NEEDS_TRACE).to_tokens(&mut needs_trace);
        }
        let trace = self.s.each(|b| quote! { t.trace(#b); });
        let mut mutable = self.s.clone();
        mutable.bind_with(|_| BindStyle::RefMut);
        let relocate = mutable.each(|b| quote! { r.relocate(#b); });
        let gc = self.gc_lifetime()?;
        let mut s = self.s.clone();
        s.add_bounds(if self.options.bound.is_some() { AddBounds::None } else { AddBounds::Generics });
        let bound: TokenStream = match &self.options.bound {
            Some(b) => b.parse()?,
            None => TokenStream::new(),
        };
        let (generics, gc) = match gc {
            Some(lt) => (TokenStream::new(), lt.into_token_stream()),
            None => (quote!(<'gc>), quote!('gc)),
        };
        Ok(s.gen_impl(quote! {
            gen unsafe impl #generics ::r8gc::Collect<#gc> for @Self #bound {
                const NEEDS_TRACE: bool = #needs_trace;

                #[inline]
                fn trace(&self, t: &mut ::r8gc::Trace<'_>) {
                    match *self { #trace }
                }

                #[inline]
                fn relocate(&mut self, r: &mut ::r8gc::Relocate<'_>) {
                    match *self { #relocate }
                }
            }
        }))
    }

    fn expand(mut self) -> TokenStream {
        let Some(mode) = self.options.mode else {
            return syn::Error::new(
                Span::call_site(),
                "deriving `Collect` requires `#[collect(no_drop)]`, `#[collect(require_static)]` or `#[collect(unsafe_drop)]`",
            )
            .to_compile_error();
        };
        let collect = self.collect_impl().unwrap_or_else(syn::Error::into_compile_error);
        let no_drop = (mode == Mode::NoDrop).then(|| {
            let mut s = self.s.clone();
            s.add_bounds(AddBounds::None).gen_impl(quote! { gen impl ::r8gc::__MustNotImplDrop for @Self {} })
        });
        let errors = self.errors.into_iter().map(syn::Error::into_compile_error);
        quote! { #collect #no_drop #(#errors)* }
    }
}

fn collect_derive(s: Structure<'_>) -> TokenStream {
    match Options::parse(&s.ast().attrs) {
        Ok(options) => Derive { s, options, errors: Vec::new() }.expand(),
        Err(e) => e.into_compile_error(),
    }
}

decl_derive! {
    [Collect, attributes(collect)] =>
    /// 派生 `r8gc::Collect`（`trace` + `relocate`）。
    ///
    /// 类型上必须标一种模式：
    /// - `#[collect(no_drop)]`：常用；禁止类型实现 `Drop`
    /// - `#[collect(require_static)]`：类型是 `'static`，什么都不遍历
    /// - `#[collect(unsafe_drop)]`：允许 `Drop`，实现者保证析构时不碰 `Gc`
    ///
    /// 可选：`bound = "where …"` 换掉自动加的约束；`gc_lifetime = 'gc` 指定哪个生命周期是品牌。
    /// 字段上可标 `#[collect(require_static)]`：加 `'static` 约束、不遍历它。
    collect_derive
}

/// `Rootable!` 用：把类型里的 `'_` 都换成给定的生命周期。写法 `__unelide_lifetimes!('lt; Type)`。
#[doc(hidden)]
#[proc_macro]
pub fn __unelide_lifetimes(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    /// 输入：生命周期; 类型
    struct Input {
        lt: syn::Lifetime,
        ty: syn::Type,
    }

    impl Parse for Input {
        fn parse(input: ParseStream) -> syn::Result<Self> {
            let lt = input.parse()?;
            input.parse::<syn::Token![;]>()?;
            Ok(Self { lt, ty: input.parse()? })
        }
    }

    /// 把 `'_` 换成 `self.0`
    struct Unelide(syn::Lifetime);

    impl VisitMut for Unelide {
        fn visit_lifetime_mut(&mut self, i: &mut syn::Lifetime) {
            if i.ident == "_" {
                *i = self.0.clone();
            }
        }
    }

    let mut input = syn::parse_macro_input!(input as Input);
    Unelide(input.lt).visit_type_mut(&mut input.ty);
    input.ty.to_token_stream().into()
}
