//! Procedural macros for j8gc: `#[derive(Collect)]` and `__unelide_lifetimes!` (used by `Rootable!`).
//!
//! The derive macro's modes and attributes follow gc-arena-derive (MIT), and it additionally generates `relocate`.
use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::visit_mut::VisitMut;
use synstructure::{AddBounds, BindStyle, Structure, decl_derive};

/// How `Drop` is handled.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The type is `'static`: it contains no `Gc`, so nothing needs to be done
    RequireStatic,
    /// Implementing `Drop` is forbidden (a `Gc` may already be gone when the destructor runs)
    NoDrop,
    /// `Drop` is allowed; the implementer guarantees it does not touch any `Gc`
    UnsafeDrop,
}

/// `#[collect(...)]` on the type.
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

/// One `#[derive(Collect)]` invocation.
struct Derive<'a> {
    s: Structure<'a>,
    options: Options,
    errors: Vec<syn::Error>,
}

impl Derive<'_> {
    /// At most one `#[collect]`
    fn attr(attrs: &[syn::Attribute]) -> syn::Result<Option<&syn::Attribute>> {
        let mut found = attrs.iter().filter(|a| a.path().is_ident("collect"));
        let first = found.next();
        match found.next() {
            Some(dup) => Err(syn::Error::new_spanned(dup.path(), "multiple `#[collect]` attributes; merge them")),
            None => Ok(first),
        }
    }

    /// Only fields not marked `require_static` are traced; marked fields get a `'static` bound. Returns whether any
    /// such field exists
    fn filter_static_fields(&mut self) -> bool {
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
        let any = !statics.is_empty();
        for ty in statics {
            self.s.add_where_predicate(syn::parse_quote! { #ty: 'static });
        }
        any
    }

    /// If the type has exactly one lifetime parameter, it is `'gc`
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
                gen unsafe impl<'gc> ::j8gc::Collect<'gc> for @Self where Self: 'static {
                    const NEEDS_TRACE: bool = false;
                }
            }));
        }
        let has_static = self.filter_static_fields();
        if let syn::Data::Enum(_) = self.s.ast().data {
            for v in self.s.variants() {
                if let Some(a) = v.ast().attrs.iter().find(|a| a.path().is_ident("collect")) {
                    self.errors.push(syn::Error::new_spanned(a.path(), "`#[collect]` is not supported on enum variants"));
                }
            }
        }
        let mut needs_trace = quote!(false);
        // Can drop in parallel: no custom Drop (no_drop), no opaque 'static fields, and every field can
        let mut parallel_drop = if self.options.mode == Some(Mode::NoDrop) && !has_static { quote!(true) } else { quote!(false) };
        for b in self.s.variants().iter().flat_map(synstructure::VariantInfo::bindings) {
            let ty = &b.ast().ty;
            let span = b.ast().span().resolved_at(Span::call_site());
            quote_spanned!(span=> || <#ty as ::j8gc::Collect>::NEEDS_TRACE).to_tokens(&mut needs_trace);
            quote_spanned!(span=> && <#ty as ::j8gc::Collect>::PARALLEL_DROP).to_tokens(&mut parallel_drop);
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
            gen unsafe impl #generics ::j8gc::Collect<#gc> for @Self #bound {
                const NEEDS_TRACE: bool = #needs_trace;
                const PARALLEL_DROP: bool = #parallel_drop;

                #[inline]
                fn trace(&self, t: &mut ::j8gc::Trace<'_>) {
                    match *self { #trace }
                }

                #[inline]
                fn relocate(&mut self, r: &mut ::j8gc::Relocate<'_>) {
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
            s.add_bounds(AddBounds::None).gen_impl(quote! { gen impl ::j8gc::__MustNotImplDrop for @Self {} })
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
    /// Derives `j8gc::Collect` (`trace` + `relocate`).
    ///
    /// The type must be marked with one mode:
    /// - `#[collect(no_drop)]`: the common case; the type may not implement `Drop`
    /// - `#[collect(require_static)]`: the type is `'static`; nothing is traced
    /// - `#[collect(unsafe_drop)]`: `Drop` is allowed; the implementer guarantees the destructor does not touch
    ///   any `Gc`
    ///
    /// Optional: `bound = "where …"` replaces the automatically added bounds; `gc_lifetime = 'gc` specifies which
    /// lifetime is the brand. Fields may be marked `#[collect(require_static)]`: adds a `'static` bound and does
    /// not trace the field.
    collect_derive
}

/// Used by `Rootable!`: replaces every `'_` in a type with the given lifetime. Syntax:
/// `__unelide_lifetimes!('lt; Type)`.
#[doc(hidden)]
#[proc_macro]
pub fn __unelide_lifetimes(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    /// Input: lifetime; type
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

    /// Replaces `'_` with `self.0`
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
