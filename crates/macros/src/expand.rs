//! 由 JSON 定义生成代码。
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Error, Ident, Path, Result, Type};

use crate::spec::{Accessor, EnumSpec, FieldSpec, Show, Spec, VariantSpec};

/// 生成时出错都指向宏调用处
fn err(msg: impl core::fmt::Display) -> Error {
    Error::new(Span::call_site(), msg)
}

impl Spec {
    pub fn expand(&self) -> Result<TokenStream> {
        self.enums.iter().map(EnumSpec::expand).collect()
    }
}

impl EnumSpec {
    fn ident(&self) -> Ident {
        format_ident!("{}", self.name)
    }

    fn expand(&self) -> Result<TokenStream> {
        let ident = self.ident();
        let doc = &self.doc;
        let derive = self
            .derive
            .iter()
            .map(|d| syn::parse_str::<Path>(d).map_err(|e| err(format_args!("{}: derive `{d}`: {e}", self.name))))
            .collect::<Result<Vec<_>>>()?;
        let variants = self.variants.iter().map(|v| v.decl(self)).collect::<Result<Vec<_>>>()?;
        let display = if self.display { self.display_impl()? } else { TokenStream::new() };
        let accessors = self.accessors.iter().map(|a| a.expand(self)).collect::<Result<Vec<_>>>()?;
        let accessors = if accessors.is_empty() {
            TokenStream::new()
        } else {
            quote! { impl #ident { #(#accessors)* } }
        };
        Ok(quote! {
            #[doc = #doc]
            #[derive(#(#derive),*)]
            pub enum #ident { #(#variants,)* }
            #display
            #accessors
        })
    }

    fn display_impl(&self) -> Result<TokenStream> {
        let ident = self.ident();
        let arms = self.variants.iter().map(|v| v.display_arm(self)).collect::<Result<Vec<_>>>()?;
        Ok(quote! {
            impl ::core::fmt::Display for #ident {
                fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    match self { #(#arms)* }
                    Ok(())
                }
            }
        })
    }
}

impl VariantSpec {
    fn ident(&self) -> Ident {
        format_ident!("{}", self.name)
    }

    /// 字段全有名字 ⇒ `Some(true)`，全没有 ⇒ `Some(false)`，没有字段 ⇒ `None`
    fn named(&self, owner: &EnumSpec) -> Result<Option<bool>> {
        let mut named = self.fields.iter().map(|f| f.name.is_some());
        match named.next() {
            None => Ok(None),
            Some(first) if named.all(|n| n == first) => Ok(Some(first)),
            Some(_) => Err(err(format_args!("{}::{}: fields must be all named or all unnamed", owner.name, self.name))),
        }
    }

    /// 字段在模式里绑定的名字：有名字用名字，没名字用 `_0`、`_1`……
    fn bindings(&self) -> Vec<Ident> {
        self.fields
            .iter()
            .enumerate()
            .map(|(i, f)| f.name.as_ref().map_or_else(|| format_ident!("_{i}"), |n| format_ident!("{n}")))
            .collect()
    }

    fn pattern(&self, owner: &EnumSpec) -> Result<TokenStream> {
        let ident = self.ident();
        let b = self.bindings();
        Ok(match self.named(owner)? {
            None => quote! { Self::#ident },
            Some(true) => quote! { Self::#ident { #(#b),* } },
            Some(false) => quote! { Self::#ident ( #(#b),* ) },
        })
    }

    fn decl(&self, owner: &EnumSpec) -> Result<TokenStream> {
        let ident = self.ident();
        let doc = self.doc.as_ref().map(|d| quote! { #[doc = #d] });
        let default = self.default.then(|| quote! { #[default] });
        let tys = self.fields.iter().map(|f| f.ty(owner, self)).collect::<Result<Vec<_>>>()?;
        let body = match self.named(owner)? {
            None => TokenStream::new(),
            Some(true) => {
                let names = self.bindings();
                quote! { { #(#names: #tys),* } }
            }
            Some(false) => quote! { ( #(#tys),* ) },
        };
        Ok(quote! { #doc #default #ident #body })
    }

    fn display_arm(&self, owner: &EnumSpec) -> Result<TokenStream> {
        let pattern = self.pattern(owner)?;
        let head = self.fields.iter().zip(self.bindings()).find(|(f, _)| matches!(f.show, Show::Name));
        let head = match (head, &self.mnemonic) {
            (Some((_, b)), None) => quote! { f.write_str(#b.name())?; },
            (None, Some(m)) => quote! { f.write_str(#m)?; },
            _ => return Err(err(format_args!("{}::{}: needs exactly one of `mnemonic` or a `show: name` field", owner.name, self.name))),
        };
        let operands = self
            .fields
            .iter()
            .zip(self.bindings())
            .filter(|(f, _)| !matches!(f.show, Show::Name))
            .map(|(f, b)| f.display(owner, self, &b))
            .collect::<Result<Vec<_>>>()?;
        let sep = (!operands.is_empty()).then(|| quote! { let mut sep = " "; });
        Ok(quote! { #pattern => { #head #sep #(#operands)* } })
    }
}

impl FieldSpec {
    fn ty(&self, owner: &EnumSpec, v: &VariantSpec) -> Result<Type> {
        syn::parse_str(&self.ty).map_err(|e| err(format_args!("{}::{}: type `{}`: {e}", owner.name, v.name, self.ty)))
    }

    /// `Option<T>` 的 `T`
    fn option_inner(ty: &Type) -> Option<&Type> {
        let Type::Path(p) = ty else { return None };
        let last = p.path.segments.last()?;
        if last.ident != "Option" {
            return None;
        }
        let syn::PathArguments::AngleBracketed(a) = &last.arguments else { return None };
        match a.args.first()? {
            syn::GenericArgument::Type(t) => Some(t),
            _ => None,
        }
    }

    fn display(&self, owner: &EnumSpec, v: &VariantSpec, b: &Ident) -> Result<TokenStream> {
        let prefix = &self.prefix;
        let fmt = match self.show {
            Show::Debug => "{}{}{:?}",
            Show::Display | Show::Name => "{}{}{}",
        };
        let write = quote! { write!(f, #fmt, sep, #prefix, #b)?; sep = ", "; };
        Ok(match Self::option_inner(&self.ty(owner, v)?) {
            Some(_) => quote! { if let Some(#b) = #b { #write } },
            None => write,
        })
    }
}

impl Accessor {
    fn expand(&self, owner: &EnumSpec) -> Result<TokenStream> {
        let name = format_ident!("{}", self.name);
        let doc = &self.doc;
        let ty: Type = syn::parse_str(&self.ty).map_err(|e| err(format_args!("{}: accessor type `{}`: {e}", owner.name, self.ty)))?;
        let mut arms = Vec::new();
        for v in &owner.variants {
            let Some((i, _)) = v.fields.iter().enumerate().find_map(|(i, f)| f.ty(owner, v).ok().filter(|t| *t == ty).map(|t| (i, t))) else {
                continue;
            };
            let b = &v.bindings()[i];
            let vi = v.ident();
            arms.push(match v.named(owner)? {
                Some(true) => quote! { Self::#vi { #b, .. } => Some(#b), },
                _ => {
                    let wild = (0..v.fields.len()).map(|j| if j == i { quote! { #b } } else { quote! { _ } });
                    quote! { Self::#vi ( #(#wild),* ) => Some(#b), }
                }
            });
        }
        Ok(quote! {
            #[doc = #doc]
            pub fn #name(&mut self) -> Option<&mut #ty> {
                match self { #(#arms)* _ => None }
            }
        })
    }
}
