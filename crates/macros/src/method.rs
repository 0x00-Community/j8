use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, Ident, ImplItemFn, LitStr, ReturnType, Type};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Constructor,
    Method,
    Getter,
    Setter,
    Static,
    Skip,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Receiver {
    None,
    Shared,
    Mutable,
}

enum Param {
    Agent,
    This,
    Rest(usize),
    Arg(usize, Box<Type>),
    Str(usize),
}

pub(crate) struct Method {
    pub(crate) rust: Ident,
    pub(crate) js: String,
    pub(crate) role: Role,
    pub(crate) receiver: Receiver,
    params: Vec<Param>,
    pub(crate) length: u32,
    returns_result: bool,
    is_async: bool,
}

pub(crate) struct Names;

impl Names {
    pub(crate) fn last_segment(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        Type::Reference(r) => Self::last_segment(&r.elem),
        _ => None,
    }
    }

    pub(crate) fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in name.trim_start_matches("r#").chars() {
        if c == '_' {
            up = !out.is_empty();
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
    }
}

impl Method {
    pub(crate) fn parse(f: &mut ImplItemFn) -> syn::Result<Self> {
        Self::parse_parts(&mut f.attrs, &f.sig)
    }

    pub(crate) fn parse_parts(attrs: &mut Vec<syn::Attribute>, sig: &syn::Signature) -> syn::Result<Self> {
        let mut role = None;
        let mut name = None;
        let mut err = None;
        attrs.retain(|attr| {
            if !attr.path().is_ident("js") {
                return true;
            }
            let r = attr.parse_nested_meta(|m| {
                let kind = match m.path.get_ident().map(ToString::to_string).as_deref() {
                    Some("constructor") => Role::Constructor,
                    Some("getter") => Role::Getter,
                    Some("setter") => Role::Setter,
                    Some("static") => Role::Static,
                    Some("skip") => Role::Skip,
                    Some("name") => {
                        name = Some(m.value()?.parse::<LitStr>()?.value());
                        return Ok(());
                    }
                    _ => return Err(m.error("expected constructor / getter / setter / static / skip / name = \"…\"")),
                };
                role = Some(kind);
                Ok(())
            });
            if let Err(e) = r {
                err = Some(e);
            }
            false
        });
        if let Some(e) = err {
            return Err(e);
        }
        let receiver = match sig.receiver() {
            None => Receiver::None,
            Some(r) if r.reference.is_none() => return Err(syn::Error::new_spanned(r, "bound methods take &self or &mut self")),
            Some(r) if r.mutability.is_some() => Receiver::Mutable,
            Some(_) => Receiver::Shared,
        };
        let role = role.unwrap_or(if receiver == Receiver::None { Role::Static } else { Role::Method });
        let mut params = Vec::new();
        let mut index = 0;
        for input in &sig.inputs {
            let FnArg::Typed(t) = input else { continue };
            let is_str = matches!(&*t.ty, Type::Reference(r) if r.mutability.is_none() && matches!(&*r.elem, Type::Path(p) if p.path.is_ident("str")));
            let param = match Names::last_segment(&t.ty).as_deref() {
                Some("Agent") => Param::Agent,
                Some("This") => Param::This,
                Some("Rest") => Param::Rest(index),
                _ if is_str => {
                    index += 1;
                    Param::Str(index - 1)
                }
                _ => {
                    index += 1;
                    Param::Arg(index - 1, t.ty.clone())
                }
            };
            params.push(param);
        }
        let returns_result = matches!(&sig.output, ReturnType::Type(_, ty) if Names::last_segment(ty).as_deref() == Some("Result"));
        let rust = sig.ident.clone();
        let js = name.unwrap_or_else(|| Names::camel(&rust.to_string()));
        let is_async = sig.asyncness.is_some();
        if is_async && (receiver != Receiver::None || params.iter().any(|p| !matches!(p, Param::Arg(..)))) {
            return Err(syn::Error::new_spanned(&sig.ident, "an async binding takes owned parameters only (no self, &str, Agent, This or Rest): its future runs later on the event loop"));
        }
        Ok(Self { rust, js, role, receiver, params, length: u32::try_from(index).unwrap_or(u32::MAX), returns_result, is_async })
    }

    pub(crate) fn wrapper_name(&self) -> Ident {
        format_ident!("__j8_js_{}", self.rust)
    }

    pub(crate) fn wrapper(&self, class: &str, owner: &TokenStream) -> TokenStream {
        let name = self.wrapper_name();
        let rust = &self.rust;
        let mut prepare = Vec::new();
        let mut call_args = Vec::new();
        for (i, p) in self.params.iter().enumerate() {
            let var = format_ident!("__p{}", i);
            match p {
                Param::Agent => call_args.push(quote!(a)),
                Param::This => call_args.push(quote!(::j8::This(this))),
                Param::Rest(from) => call_args.push(quote!(::j8::Rest(args.get(#from..).unwrap_or(&[]).to_vec()))),
                Param::Arg(at, ty) => {
                    prepare.push(quote!(let #var = <#ty as ::j8::FromJs<'gc>>::from_js(a, ::j8::Glue::arg(args, #at))?;));
                    call_args.push(quote!(#var));
                }
                Param::Str(at) => {
                    prepare.push(quote!(let #var = <::j8::frame::JsString<'gc> as ::j8::FromJs<'gc>>::from_js(a, ::j8::Glue::arg(args, #at))?;));
                    call_args.push(quote!(&**#var));
                }
            }
        }
        let call = match self.receiver {
            Receiver::None => quote!(#owner::#rust(#(#call_args),*)),
            Receiver::Shared => quote! {{
                let __data = ::j8::Glue::host::<Self>(a, this)?;
                let __cell = ::j8::Glue::borrow(a, &__data, #class)?;
                let __self = ::j8::Glue::cast::<Self>(a, &__cell)?;
                __self.#rust(#(#call_args),*)
            }},
            Receiver::Mutable => quote! {{
                let __data = ::j8::Glue::host::<Self>(a, this)?;
                let mut __cell = ::j8::Glue::borrow_mut(a, &__data, #class)?;
                let __self = ::j8::Glue::cast_mut::<Self>(a, &mut __cell)?;
                __self.#rust(#(#call_args),*)
            }},
        };
        let body = if self.is_async {
            quote! {
                #(#prepare)*
                let __future = #call;
                ::j8::Glue::spawn(a, async move { ::j8::Glue::settle(__future.await) })
            }
        } else if self.role == Role::Constructor {
            let made = if self.returns_result { call } else { quote!(::core::result::Result::Ok(#call)) };
            quote! {
                ::j8::Glue::require_new(a, #class)?;
                #(#prepare)*
                let __made = #made;
                ::j8::Glue::construct::<Self>(a, __made)
            }
        } else {
            quote! {
                #(#prepare)*
                let __r = #call;
                ::j8::IntoJs::into_js(__r, a)
            }
        };
        quote! {
            #[doc(hidden)]
            #[allow(clippy::needless_question_mark, unused_variables, clippy::unnecessary_wraps)]
            fn #name<'gc>(a: &mut ::j8::frame::Agent<'gc, '_>, this: ::j8::frame::Value<'gc>, args: &[::j8::frame::Value<'gc>]) -> ::core::result::Result<::j8::frame::Value<'gc>, ::j8::frame::Error> {
                #body
            }
        }
    }
}

pub(crate) struct Bound {
    pub(crate) name: syn::LitStr,
    pub(crate) methods: Vec<Method>,
}

impl Bound {
    pub(crate) fn of_impl(attr: TokenStream, item: &mut syn::ItemImpl) -> syn::Result<Self> {
        let name = if attr.is_empty() {
            let n = Names::last_segment(&item.self_ty).ok_or_else(|| syn::Error::new_spanned(&item.self_ty, "cannot name this type"))?;
            LitStr::new(&n, proc_macro2::Span::call_site())
        } else {
            syn::parse2::<LitStr>(attr)?
        };
        let mut methods = Vec::new();
        for it in &mut item.items {
            if let syn::ImplItem::Fn(f) = it {
                let m = Method::parse(f)?;
                if m.role != Role::Skip {
                    methods.push(m);
                }
            }
        }
        Ok(Self { name, methods })
    }

    pub(crate) fn wrappers(&self) -> Vec<TokenStream> {
        let owner = quote!(Self);
        self.methods.iter().map(|m| m.wrapper(&self.name.value(), &owner)).collect()
    }

    pub(crate) fn members(&self) -> Vec<TokenStream> {
        let native = quote!(::j8::frame::NativeFn);
        let mut members = Vec::new();
        let mut accessors: Vec<(String, Option<Ident>, Option<Ident>)> = Vec::new();
        for m in &self.methods {
            let w = m.wrapper_name();
            let (js, len) = (&m.js, m.length);
            match m.role {
                Role::Method => members.push(quote!(::j8::Member::Method { name: #js, f: Self::#w as #native, length: #len })),
                Role::Static => members.push(quote!(::j8::Member::Static { name: #js, f: Self::#w as #native, length: #len })),
                Role::Getter | Role::Setter => {
                    let at = match accessors.iter().position(|(n, _, _)| n == js) {
                        Some(i) => i,
                        None => {
                            accessors.push((js.clone(), None, None));
                            accessors.len() - 1
                        }
                    };
                    if m.role == Role::Getter { accessors[at].1 = Some(w) } else { accessors[at].2 = Some(w) }
                }
                Role::Constructor | Role::Skip => {}
            }
        }
        for (name, get, set) in accessors {
            let get = get.map_or(quote!(::core::option::Option::None), |g| quote!(::core::option::Option::Some(Self::#g as #native)));
            let set = set.map_or(quote!(::core::option::Option::None), |s| quote!(::core::option::Option::Some(Self::#s as #native)));
            members.push(quote!(::j8::Member::Accessor { name: #name, get: #get, set: #set }));
        }
        members
    }
}
