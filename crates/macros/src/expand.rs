//! Generates code from the JSON definitions.
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Error, Ident, Path, Result, Type};

use crate::spec::{Accessor, ConstSpec, EnumSpec, FieldSpec, Spec, StructField, StructSpec, VariantSpec};

/// Generation errors all point at the macro call site
fn err(msg: impl core::fmt::Display) -> Error {
    Error::new(Span::call_site(), msg)
}

/// FNV-1a 64 offset basis / prime
const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

impl Spec {
    /// `json` is the file's text (fingerprinted into `SCHEMA` when `binary` is on)
    pub fn expand(&self, json: &str) -> Result<TokenStream> {
        let types: TokenStream = self.enums.iter().map(EnumSpec::expand).chain(self.structs.iter().map(StructSpec::expand)).collect::<Result<_>>()?;
        if !self.binary {
            return Ok(types);
        }
        let schema = json.bytes().fold(FNV_BASIS, |h, b| (h ^ u64::from(b)).wrapping_mul(FNV_PRIME));
        let binary: TokenStream = self.enums.iter().map(EnumSpec::binary_impl).chain(self.structs.iter().map(StructSpec::binary_impl)).collect::<Result<_>>()?;
        Ok(quote! {
            #types
            /// Fingerprint of the JSON definitions (FNV-1a 64 of the file): written into every binary, which only reads
            /// back under the same definitions
            pub const SCHEMA: u64 = #schema;
            #binary
        })
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
        let repr = match &self.repr {
            Some(r) => {
                let r = format_ident!("{r}");
                quote! { #[repr(#r)] }
            }
            None => TokenStream::new(),
        };
        let accessors = self.accessors.iter().map(|a| a.expand(self)).collect::<Result<Vec<_>>>()?;
        let accessors = if accessors.is_empty() {
            TokenStream::new()
        } else {
            quote! { impl #ident { #(#accessors)* } }
        };
        Ok(quote! {
            #[doc = #doc]
            #[derive(#(#derive),*)]
            #repr
            pub enum #ident { #(#variants,)* }
            #accessors
        })
    }

}


impl EnumSpec {
    /// `Binary`: a one-byte tag (the variant's position in the JSON), then the fields in order. Decoding dispatches on
    /// the tag as an integer
    fn binary_impl(&self) -> Result<TokenStream> {
        let ident = self.ident();
        let name = &self.name;
        let mut encode = Vec::new();
        let mut decode = Vec::new();
        for (tag, v) in self.variants.iter().enumerate() {
            let tag = u8::try_from(tag).map_err(|_| err(format_args!("{}: `binary` takes at most 256 variants", self.name)))?;
            let pattern = v.pattern(self)?;
            let b = v.bindings();
            encode.push(quote! { #pattern => { out.push(#tag); #(Binary::encode(#b, out);)* } });
            let vi = v.ident();
            let values = b.iter().map(|_| quote! { Binary::decode(r)? });
            decode.push(match v.named(self)? {
                None => quote! { #tag => Self::#vi, },
                Some(true) => quote! { #tag => Self::#vi { #(#b: #values),* }, },
                Some(false) => quote! { #tag => Self::#vi ( #(#values),* ), },
            });
        }
        Ok(quote! {
            impl Binary for #ident {
                fn encode(&self, out: &mut ::alloc::vec::Vec<u8>) {
                    match self { #(#encode)* }
                }
                fn decode(r: &mut Reader<'_>) -> ::core::result::Result<Self, BinError> {
                    Ok(match r.byte()? {
                        #(#decode)*
                        tag => return Err(BinError::Tag(#name, tag)),
                    })
                }
            }
        })
    }
}

impl VariantSpec {

    fn ident(&self) -> Ident {
        format_ident!("{}", self.name)
    }

    /// All fields named ⇒ `Some(true)`, none named ⇒ `Some(false)`, no fields ⇒ `None`
    fn named(&self, owner: &EnumSpec) -> Result<Option<bool>> {
        let mut named = self.fields.iter().map(|f| f.name.is_some());
        match named.next() {
            None => Ok(None),
            Some(first) if named.all(|n| n == first) => Ok(Some(first)),
            Some(_) => Err(err(format_args!("{}::{}: fields must be all named or all unnamed", owner.name, self.name))),
        }
    }

    /// Names that fields bind to in patterns: the field name if named, otherwise `_0`, `_1`, ...
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

}

impl FieldSpec {
    fn ty(&self, owner: &EnumSpec, v: &VariantSpec) -> Result<Type> {
        syn::parse_str(&self.ty).map_err(|e| err(format_args!("{}::{}: type `{}`: {e}", owner.name, v.name, self.ty)))
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

impl StructSpec {
    fn expand(&self) -> Result<TokenStream> {
        let ident = format_ident!("{}", self.name);
        let doc = &self.doc;
        let derive = self
            .derive
            .iter()
            .map(|d| syn::parse_str::<Path>(d).map_err(|e| err(format_args!("{}: derive `{d}`: {e}", self.name))))
            .collect::<Result<Vec<_>>>()?;
        let fields = self.fields.iter().map(|f| f.decl(self)).collect::<Result<Vec<_>>>()?;
        let body = match self.named()? {
            None => quote! { ; },
            Some(true) => quote! { { #(#fields),* } },
            Some(false) => quote! { ( #(#fields),* ); },
        };
        let consts = self.consts.iter().map(|c| c.expand(self)).collect::<Result<Vec<_>>>()?;
        let consts = (!consts.is_empty()).then(|| quote! { impl #ident { #(#consts)* } });
        Ok(quote! {
            #[doc = #doc]
            #[derive(#(#derive),*)]
            pub struct #ident #body
            #consts
        })
    }

    /// `Binary`: the fields in order
    fn binary_impl(&self) -> Result<TokenStream> {
        let ident = format_ident!("{}", self.name);
        let (encode, decode) = match self.named()? {
            None => (vec![quote! { let _ = out; }], quote! { { let _ = r; Self } }),
            Some(true) => {
                let names: Vec<Ident> = self.fields.iter().filter_map(|f| f.name.as_deref()).map(|n| format_ident!("{n}")).collect();
                (names.iter().map(|n| quote! { Binary::encode(&self.#n, out); }).collect(), quote! { Self { #(#names: Binary::decode(r)?),* } })
            }
            Some(false) => {
                let index: Vec<syn::Index> = (0..self.fields.len()).map(syn::Index::from).collect();
                let values = index.iter().map(|_| quote! { Binary::decode(r)? });
                (index.iter().map(|i| quote! { Binary::encode(&self.#i, out); }).collect(), quote! { Self ( #(#values),* ) })
            }
        };
        Ok(quote! {
            impl Binary for #ident {
                fn encode(&self, out: &mut ::alloc::vec::Vec<u8>) { #(#encode)* }
                fn decode(r: &mut Reader<'_>) -> ::core::result::Result<Self, BinError> { Ok(#decode) }
            }
        })
    }

    /// All fields named ⇒ `Some(true)`, none named ⇒ `Some(false)`, no fields ⇒ `None`
    fn named(&self) -> Result<Option<bool>> {
        let mut named = self.fields.iter().map(|f| f.name.is_some());
        match named.next() {
            None => Ok(None),
            Some(first) if named.all(|n| n == first) => Ok(Some(first)),
            Some(_) => Err(err(format_args!("{}: fields must be all named or all unnamed", self.name))),
        }
    }

}

impl StructField {
    fn decl(&self, owner: &StructSpec) -> Result<TokenStream> {
        let ty: Type = syn::parse_str(&self.ty).map_err(|e| err(format_args!("{}: type `{}`: {e}", owner.name, self.ty)))?;
        let doc = self.doc.as_ref().map(|d| quote! { #[doc = #d] });
        Ok(match &self.name {
            Some(n) => {
                let n = format_ident!("{n}");
                quote! { #doc pub #n: #ty }
            }
            None => quote! { #doc pub #ty },
        })
    }
}

impl ConstSpec {
    fn expand(&self, owner: &StructSpec) -> Result<TokenStream> {
        let name = format_ident!("{}", self.name);
        let ty: Type = syn::parse_str(&self.ty).map_err(|e| err(format_args!("{}::{}: type `{}`: {e}", owner.name, self.name, self.ty)))?;
        let value: syn::Expr = syn::parse_str(&self.value).map_err(|e| err(format_args!("{}::{}: value `{}`: {e}", owner.name, self.name, self.value)))?;
        let doc = self.doc.as_ref().map(|d| quote! { #[doc = #d] });
        Ok(quote! { #doc pub const #name: #ty = #value; })
    }
}
