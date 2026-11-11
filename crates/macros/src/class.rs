//! `#[r8binding::class]`：写在 `impl` 块上，把里面的方法包成 JS 宿主函数，生成 `r8binding::Class` 实现。
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, LitStr, ReturnType, Type};

/// 方法在 JS 里是什么。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Constructor,
    Method,
    Getter,
    Setter,
    Static,
    Skip,
}

/// 接收者。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Receiver {
    None,
    Shared,
    Mutable,
}

/// 一个形参从哪来。
enum Param {
    /// `&mut Agent`
    Agent,
    /// `This`：原样的 `this`
    This,
    /// `Rest`：第 `usize` 个起的全部实参
    Rest(usize),
    /// 第 `usize` 个实参，按 `FromJs` 转
    Arg(usize, Box<Type>),
    /// 第 `usize` 个实参：ToString 后借出 `&str`
    Str(usize),
}

/// 解析好的一个方法。
struct Method {
    rust: Ident,
    js: String,
    role: Role,
    receiver: Receiver,
    params: Vec<Param>,
    length: u32,
    returns_result: bool,
}

/// 名字工具。
struct Names;

impl Names {
    /// 类型路径的最后一段名字。
    fn last_segment(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        Type::Reference(r) => Self::last_segment(&r.elem),
        _ => None,
    }
    }

    /// `snake_case` ⇒ `camelCase`。
    fn camel(name: &str) -> String {
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
    /// 读 `#[js(…)]`（读完从方法上去掉），分析签名。
    fn parse(f: &mut ImplItemFn) -> syn::Result<Self> {
        let mut role = None;
        let mut name = None;
        let mut err = None;
        f.attrs.retain(|attr| {
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
        let receiver = match f.sig.receiver() {
            None => Receiver::None,
            Some(r) if r.reference.is_none() => return Err(syn::Error::new_spanned(r, "bound methods take &self or &mut self")),
            Some(r) if r.mutability.is_some() => Receiver::Mutable,
            Some(_) => Receiver::Shared,
        };
        let role = role.unwrap_or(if receiver == Receiver::None { Role::Static } else { Role::Method });
        let mut params = Vec::new();
        let mut index = 0;
        for input in &f.sig.inputs {
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
        let returns_result = matches!(&f.sig.output, ReturnType::Type(_, ty) if Names::last_segment(ty).as_deref() == Some("Result"));
        let rust = f.sig.ident.clone();
        let js = name.unwrap_or_else(|| Names::camel(&rust.to_string()));
        Ok(Self { rust, js, role, receiver, params, length: u32::try_from(index).unwrap_or(u32::MAX), returns_result })
    }

    fn wrapper_name(&self) -> Ident {
        format_ident!("__r8_js_{}", self.rust)
    }

    /// 转换外壳：`fn(agent, this, args) -> Result<Value>`。
    fn wrapper(&self, class: &str) -> TokenStream {
        let name = self.wrapper_name();
        let rust = &self.rust;
        let mut prepare = Vec::new();
        let mut call_args = Vec::new();
        for (i, p) in self.params.iter().enumerate() {
            let var = format_ident!("__p{}", i);
            match p {
                Param::Agent => call_args.push(quote!(a)),
                Param::This => call_args.push(quote!(::r8binding::This(this))),
                Param::Rest(from) => call_args.push(quote!(::r8binding::Rest(args.get(#from..).unwrap_or(&[]).to_vec()))),
                Param::Arg(at, ty) => {
                    prepare.push(quote!(let #var = <#ty as ::r8binding::FromJs<'gc>>::from_js(a, ::r8binding::Glue::arg(args, #at))?;));
                    call_args.push(quote!(#var));
                }
                Param::Str(at) => {
                    prepare.push(quote!(let #var = <::r8binding::frame::JsString<'gc> as ::r8binding::FromJs<'gc>>::from_js(a, ::r8binding::Glue::arg(args, #at))?;));
                    call_args.push(quote!(&**#var));
                }
            }
        }
        let call = match self.receiver {
            Receiver::None => quote!(Self::#rust(#(#call_args),*)),
            Receiver::Shared => quote! {{
                let __data = ::r8binding::Glue::host::<Self>(a, this)?;
                let __cell = ::r8binding::Glue::borrow(a, &__data, #class)?;
                let __self = ::r8binding::Glue::cast::<Self>(a, &__cell)?;
                __self.#rust(#(#call_args),*)
            }},
            Receiver::Mutable => quote! {{
                let __data = ::r8binding::Glue::host::<Self>(a, this)?;
                let mut __cell = ::r8binding::Glue::borrow_mut(a, &__data, #class)?;
                let __self = ::r8binding::Glue::cast_mut::<Self>(a, &mut __cell)?;
                __self.#rust(#(#call_args),*)
            }},
        };
        let body = if self.role == Role::Constructor {
            let made = if self.returns_result { call } else { quote!(::core::result::Result::Ok(#call)) };
            quote! {
                ::r8binding::Glue::require_new(a, #class)?;
                #(#prepare)*
                ::r8binding::Glue::construct::<Self>(a, #made)
            }
        } else {
            quote! {
                #(#prepare)*
                let __r = #call;
                ::r8binding::IntoJs::into_js(__r, a)
            }
        };
        quote! {
            #[doc(hidden)]
            #[allow(clippy::needless_question_mark, unused_variables, clippy::unnecessary_wraps)]
            fn #name<'gc>(a: &mut ::r8binding::frame::Agent<'gc, '_>, this: ::r8binding::frame::Value<'gc>, args: &[::r8binding::frame::Value<'gc>]) -> ::core::result::Result<::r8binding::frame::Value<'gc>, ::r8binding::frame::Error> {
                #body
            }
        }
    }
}

pub fn expand(attr: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let self_ty = item.self_ty.clone();
    let class = if attr.is_empty() {
        Names::last_segment(&self_ty).ok_or_else(|| syn::Error::new_spanned(&self_ty, "cannot name this type"))?
    } else {
        syn::parse2::<LitStr>(attr)?.value()
    };
    let mut methods = Vec::new();
    for it in &mut item.items {
        if let ImplItem::Fn(f) = it {
            let m = Method::parse(f)?;
            if m.role != Role::Skip {
                methods.push(m);
            }
        }
    }
    let wrappers: Vec<TokenStream> = methods.iter().map(|m| m.wrapper(&class)).collect();
    let native = quote!(::r8binding::frame::NativeFn);
    let ctor = match methods.iter().find(|m| m.role == Role::Constructor) {
        Some(m) => {
            let w = m.wrapper_name();
            let len = m.length;
            quote!(::core::option::Option::Some((Self::#w as #native, #len)))
        }
        None => quote!(::core::option::Option::None),
    };
    let mut members = Vec::new();
    let mut accessors: Vec<(String, Option<Ident>, Option<Ident>)> = Vec::new();
    for m in &methods {
        let w = m.wrapper_name();
        let (js, len) = (&m.js, m.length);
        match m.role {
            Role::Method => members.push(quote!(::r8binding::Member::Method { name: #js, f: Self::#w as #native, length: #len })),
            Role::Static => members.push(quote!(::r8binding::Member::Static { name: #js, f: Self::#w as #native, length: #len })),
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
        members.push(quote!(::r8binding::Member::Accessor { name: #name, get: #get, set: #set }));
    }
    let class_lit = LitStr::new(&class, Span::call_site());
    let (impl_generics, _, where_clause) = item.generics.split_for_impl();
    Ok(quote! {
        #item

        impl #impl_generics #self_ty #where_clause {
            #(#wrappers)*
        }

        impl #impl_generics ::r8binding::Class for #self_ty #where_clause {
            const NAME: &'static str = #class_lit;
            const CONSTRUCTOR: ::core::option::Option<(#native, u32)> = #ctor;
            const MEMBERS: &'static [::r8binding::Member] = &[#(#members),*];
        }
    })
}
