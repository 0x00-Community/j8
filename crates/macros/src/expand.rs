//! Generates code from the JSON definitions.
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Error, Ident, Path, Result, Type};

use crate::spec::{Accessor, ConstSpec, EnumSpec, FieldSpec, Show, Spec, StructField, StructSpec, VariantSpec};

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
        let texts = self.texts()?;
        let display = match (self.display, &texts) {
            (false, _) => TokenStream::new(),
            (true, Some(_)) => self.text_display(),
            (true, None) => self.display_impl()?,
        };
        let asm = match (self.asm, &texts, self.display) {
            (false, ..) => TokenStream::new(),
            (true, Some(_), _) => self.text_operand(),
            (true, None, true) => self.parse_impl()?,
            (true, None, false) => self.operand_impl()?,
        };
        let names = texts.as_deref().map(|t| self.names_impl(t)).unwrap_or_default();
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
            #display
            #asm
            #accessors
            #names
        })
    }

    /// `Some(texts)` when every variant has `text` (a text enum, unit variants only); `None` when none has
    fn texts(&self) -> Result<Option<Vec<&str>>> {
        let texts: Vec<&str> = self.variants.iter().filter_map(|v| v.text.as_deref()).collect();
        match texts.len() {
            0 => Ok(None),
            n if n == self.variants.len() && self.variants.iter().all(|v| v.fields.is_empty()) => Ok(Some(texts)),
            _ => Err(err(format_args!("{}: `text` must be on every variant, and the variants must be unit", self.name))),
        }
    }

    /// `ALL` (every variant in order) and `name()` (the variant's text)
    fn names_impl(&self, texts: &[&str]) -> TokenStream {
        let ident = self.ident();
        let vs: Vec<Ident> = self.variants.iter().map(VariantSpec::ident).collect();
        let n = vs.len();
        quote! {
            impl #ident {
                pub const ALL: [#ident; #n] = [#(#ident::#vs),*];

                pub fn name(self) -> &'static str {
                    match self { #(Self::#vs => #texts,)* }
                }
            }
        }
    }

    fn text_display(&self) -> TokenStream {
        let ident = self.ident();
        quote! {
            impl ::core::fmt::Display for #ident {
                fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    f.write_str(self.name())
                }
            }
        }
    }

    /// `Operand`: reverse lookup of the text in `ALL`
    fn text_operand(&self) -> TokenStream {
        let ident = self.ident();
        quote! {
            impl Operand for #ident {
                fn operand(text: &str) -> ::core::result::Result<Self, AsmError> {
                    Self::ALL.into_iter().find(|v| v.name() == text).ok_or_else(|| AsmError::Operand(text.into()))
                }
            }
        }
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

impl EnumSpec {
    /// `FromStr`: the inverse of `Display`. Matches the mnemonic first; if that fails, tries the type of the
    /// `show: name` field. Operands are handed to the caller's `Operands`
    fn parse_impl(&self) -> Result<TokenStream> {
        let ident = self.ident();
        let mut arms = Vec::new();
        let mut named = Vec::new();
        for v in &self.variants {
            let head = v.fields.iter().zip(v.bindings()).find(|(f, _)| matches!(f.show, Show::Name));
            match (head, &v.mnemonic) {
                (None, Some(m)) => {
                    let build = v.assemble(self, None)?;
                    arms.push(quote! { #m => #build, });
                }
                (Some((f, b)), None) => {
                    let ty = f.ty(self, v)?;
                    let build = v.assemble(self, Some(&b))?;
                    named.push(match &v.suffix {
                        Some(s) => {
                            let s = format!(".{s}");
                            quote! { if let Some(Ok(#b)) = head.strip_suffix(#s).map(<#ty as Operand>::operand) { #build } else }
                        }
                        None => quote! { if let Ok(#b) = <#ty as Operand>::operand(head) { #build } else },
                    });
                }
                _ => return Err(err(format_args!("{}::{}: needs exactly one of `mnemonic` or a `show: name` field", self.name, v.name))),
            }
        }
        Ok(quote! {
            impl ::core::str::FromStr for #ident {
                type Err = AsmError;
                fn from_str(text: &str) -> ::core::result::Result<Self, AsmError> {
                    let (head, mut ops) = Operands::split(text);
                    let value = match head {
                        #(#arms)*
                        _ => #(#named)* { return Err(AsmError::Mnemonic(head.into())) }
                    };
                    ops.finish()?;
                    Ok(value)
                }
            }
        })
    }

    /// `Operand`: matched by variant name (i.e. `Debug`); unit variants only
    fn operand_impl(&self) -> Result<TokenStream> {
        let ident = self.ident();
        let arms = self
            .variants
            .iter()
            .map(|v| {
                if !v.fields.is_empty() {
                    return Err(err(format_args!("{}::{}: `asm` without `display` needs unit variants", self.name, v.name)));
                }
                let (name, vi) = (&v.name, v.ident());
                Ok(quote! { #name => Ok(Self::#vi), })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(quote! {
            impl Operand for #ident {
                fn operand(text: &str) -> ::core::result::Result<Self, AsmError> {
                    match text { #(#arms)* _ => Err(AsmError::Operand(text.into())) }
                }
            }
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
    /// Builds the variant by taking operands from `ops` in field order; `head` is the `show: name` field already
    /// recognized from the mnemonic position
    fn assemble(&self, owner: &EnumSpec, head: Option<&Ident>) -> Result<TokenStream> {
        let ident = self.ident();
        let values = self
            .fields
            .iter()
            .map(|f| {
                if matches!(f.show, Show::Name) {
                    return Ok(head.map_or_else(TokenStream::new, |h| quote! { #h }));
                }
                let prefix = &f.prefix;
                Ok(match FieldSpec::option_inner(&f.ty(owner, self)?) {
                    Some(_) if prefix.is_empty() => {
                        return Err(err(format_args!("{}::{}: an `Option` field needs a `prefix` to be assembled", owner.name, self.name)));
                    }
                    Some(_) => quote! { ops.optional(#prefix)? },
                    None => quote! { ops.next(#prefix)? },
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(match self.named(owner)? {
            None => quote! { Self::#ident },
            Some(true) => {
                let names = self.bindings();
                quote! { Self::#ident { #(#names: #values),* } }
            }
            Some(false) => quote! { Self::#ident ( #(#values),* ) },
        })
    }


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

    fn display_arm(&self, owner: &EnumSpec) -> Result<TokenStream> {
        let pattern = self.pattern(owner)?;
        let head = self.fields.iter().zip(self.bindings()).find(|(f, _)| matches!(f.show, Show::Name));
        let head = match (head, &self.mnemonic) {
            (Some((_, b)), None) => match &self.suffix {
                Some(s) => {
                    let s = format!(".{s}");
                    quote! { f.write_str(#b.name())?; f.write_str(#s)?; }
                }
                None => quote! { f.write_str(#b.name())?; },
            },
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

    /// The `T` of `Option<T>`
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
        let prefixed = self.prefix.as_deref().map(|p| self.prefixed(&ident, p)).transpose()?;
        Ok(quote! {
            #[doc = #doc]
            #[derive(#(#derive),*)]
            pub struct #ident #body
            #consts
            #prefixed
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

    /// `Display` = prefix + field, `Operand` = strip the prefix, parse the field
    fn prefixed(&self, ident: &Ident, prefix: &str) -> Result<TokenStream> {
        if self.fields.len() != 1 || self.named()? != Some(false) {
            return Err(err(format_args!("{}: `prefix` needs a single-field tuple struct", self.name)));
        }
        let fmt = format!("{prefix}{{}}");
        Ok(quote! {
            impl ::core::fmt::Display for #ident {
                fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    write!(f, #fmt, self.0)
                }
            }
            impl Operand for #ident {
                fn operand(text: &str) -> ::core::result::Result<Self, AsmError> {
                    text.strip_prefix(#prefix).and_then(|n| n.parse().ok()).map(#ident).ok_or_else(|| AsmError::Operand(text.into()))
                }
            }
        })
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
