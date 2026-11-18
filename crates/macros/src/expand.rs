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
        let fixed = self.fixed();
        let fixed_impls: TokenStream = self.enums.iter().filter(|e| fixed.contains(&e.name)).map(EnumSpec::fixed_impl).chain(self.structs.iter().filter(|s| fixed.contains(&s.name)).map(StructSpec::fixed_impl)).collect::<Result<_>>()?;
        let decode_at: TokenStream = self.enums.iter().filter(|e| e.opcodes.is_some()).map(|e| e.decode_at(&fixed)).collect::<Result<_>>()?;
        let opcodes: TokenStream = self.enums.iter().filter_map(|e| e.opcodes.as_deref().map(|o| e.opcodes_impl(o))).collect::<Result<_>>()?;
        Ok(quote! {
            #types
            #fixed_impls
            #decode_at
            #opcodes
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


/// Types whose encoding has a fixed size (besides the ones in the JSON)
const FIXED_PRIMITIVES: [&str; 7] = ["bool", "u8", "u16", "u32", "i32", "u64", "f64"];

impl Spec {
    /// Names of the types with a fixed-size encoding: primitives, all-unit enums, structs of fixed fields
    fn fixed(&self) -> Vec<String> {
        let mut fixed: Vec<String> = FIXED_PRIMITIVES.iter().map(|p| String::from(*p)).collect();
        fixed.extend(self.enums.iter().filter(|e| e.variants.iter().all(|v| v.fields.is_empty())).map(|e| e.name.clone()));
        loop {
            let more: Vec<String> = self.structs.iter().filter(|s| !fixed.contains(&s.name) && s.fields.iter().all(|f| fixed.contains(&f.ty))).map(|s| s.name.clone()).collect();
            if more.is_empty() {
                return fixed;
            }
            fixed.extend(more);
        }
    }
}

/// `field: Ty::read(&b[offset..])` pieces for fixed-size fields laid out after `start` bytes; gives the reads and the
/// total size expression
fn fixed_reads<'t>(tys: impl Iterator<Item = &'t str>, start: TokenStream) -> Result<(Vec<TokenStream>, TokenStream)> {
    let mut at = start;
    let mut reads = Vec::new();
    for ty in tys {
        let ty: Type = syn::parse_str(ty).map_err(|e| err(format_args!("type `{ty}`: {e}")))?;
        reads.push(quote! { <#ty as Fixed>::read(&b[#at..]) });
        at = quote! { #at + <#ty as Fixed>::SIZE };
    }
    Ok((reads, at))
}

impl EnumSpec {
    /// The byte values no variant uses, as one or-pattern: listed one by one, so with the variants' arms a `match`
    /// on the byte covers all 256 values and compiles to a jump table without a range check
    fn unused_tags(&self) -> TokenStream {
        let unused = (self.variants.len()..=usize::from(u8::MAX)).filter_map(|t| u8::try_from(t).ok());
        quote! { #(#unused)|* }
    }

    /// `Fixed` for an all-unit enum: one byte, the variant's position
    fn fixed_impl(&self) -> Result<TokenStream> {
        let ident = self.ident();
        let arms = self.variants.iter().enumerate().map(|(tag, v)| {
            let tag = u8::try_from(tag).map_err(|_| err(format_args!("{}: `binary` takes at most 256 variants", self.name)))?;
            let vi = v.ident();
            Ok(quote! { #tag => Self::#vi, })
        });
        let arms = arms.collect::<Result<Vec<_>>>()?;
        let name = &self.name;
        Ok(quote! {
            impl Fixed for #ident {
                const SIZE: usize = 1;
                #[inline]
                fn read(b: &[u8]) -> Self {
                    match b[0] { #(#arms)* tag => panic!("checked code has no {} tag {}", #name, tag) }
                }
            }
        })
    }

    /// The opcode enum `name`: one field-less variant per variant, discriminant = tag; `at(code, pc)` reads it
    fn opcodes_impl(&self, name: &str) -> Result<TokenStream> {
        let ident = format_ident!("{name}");
        let of = &self.name;
        let doc = format!("The opcode of a `{of}`: its tag, one variant per `{of}` variant (`j8macros::dispatch!` matches on it).");
        let mut decls = Vec::new();
        let mut arms = Vec::new();
        for (tag, v) in self.variants.iter().enumerate() {
            let tag = u8::try_from(tag).map_err(|_| err(format_args!("{}: `binary` takes at most 256 variants", self.name)))?;
            let vi = v.ident();
            decls.push(quote! { #vi = #tag });
            arms.push(quote! { #tag => Self::#vi, });
        }
        let owner = self.ident();
        let unused = self.unused_tags();
        Ok(quote! {
            #[doc = #doc]
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            #[repr(u8)]
            pub enum #ident { #(#decls),* }
            impl #ident {
                /// The opcode at byte `pc` of padded, checked code (the same window check as `decode_at`, so the two
                /// share it)
                #[inline(always)]
                pub fn at(code: &[u8], pc: usize) -> Self {
                    let b = #owner::window(code, pc);
                    match b[0] { #(#arms)* #unused => panic!("checked code has no {} tag {}", #of, b[0]) }
                }

                /// [`Self::at`] without any check (one byte load): for the interpreter's dispatch.
                ///
                /// # Safety
                /// `code` is padded, checked code and `pc` the start of an instruction in it.
                #[inline(always)]
                pub unsafe fn at_unchecked(code: &[u8], pc: usize) -> Self {
                    // SAFETY: checked code starts every instruction with a valid tag (= discriminant), and `pc` is in it
                    unsafe { core::mem::transmute::<u8, Self>(*code.get_unchecked(pc)) }
                }
            }
        })
    }

    /// `decode_at(code, pc)`: the value encoded at byte `pc` and the offset after it. A variant whose fields all have a
    /// fixed size takes one fixed-length chunk (a single bounds check) and reads every field at a constant offset;
    /// others go through `Binary`. For code already checked (`Binary::decode` succeeded on it): panics otherwise
    fn decode_at(&self, fixed: &[String]) -> Result<TokenStream> {
        let ident = self.ident();
        let name = &self.name;
        let mut arms = Vec::new();
        let mut sizes = Vec::new();
        for (tag, v) in self.variants.iter().enumerate() {
            let tag = u8::try_from(tag).map_err(|_| err(format_args!("{}: `binary` takes at most 256 variants", self.name)))?;
            let vi = v.ident();
            let b = v.bindings();
            if v.fields.iter().all(|f| fixed.contains(&f.ty)) {
                let (reads, size) = fixed_reads(v.fields.iter().map(|f| f.ty.as_str()), quote! { 1 })?;
                let value = match v.named(self)? {
                    None => quote! { Self::#vi },
                    Some(true) => quote! { Self::#vi { #(#b: #reads),* } },
                    Some(false) => quote! { Self::#vi ( #(#reads),* ) },
                };
                sizes.push(size.clone());
                arms.push(quote! { #tag => (#value, pc + (#size)), });
            } else {
                let values = b.iter().map(|_| quote! { Binary::decode(&mut r).expect("checked code") });
                let value = match v.named(self)? {
                    None => quote! { Self::#vi },
                    Some(true) => quote! { Self::#vi { #(#b: #values),* } },
                    Some(false) => quote! { Self::#vi ( #(#values),* ) },
                };
                arms.push(quote! {
                    #tag => {
                        let mut r = Reader::from(&code[pc + 1..]);
                        let v = #value;
                        (v, code.len() - r.left())
                    }
                });
            }
        }
        let unused = self.unused_tags();
        Ok(quote! {
            impl #ident {
                /// Bytes read at once when decoding: the largest fixed-size encoding. Code carries `PAD` bytes after its
                /// last instruction, so the window always fits and a whole instruction costs one bounds check
                pub const WINDOW: usize = {
                    let mut m = 1;
                    #(if #sizes > m { m = #sizes; })*
                    m
                };
                /// Padding after the last instruction of code
                pub const PAD: usize = Self::WINDOW - 1;

                /// The `WINDOW` bytes from `pc`. Code is under 4 GiB, so `pc` is taken as a `u32`: then `pc + WINDOW`
                /// cannot overflow and the slice costs a single bounds check
                #[inline(always)]
                pub fn window(code: &[u8], pc: usize) -> &[u8; Self::WINDOW] {
                    let pc = usize::try_from(u32::try_from(pc).unwrap_or(u32::MAX)).unwrap_or(usize::MAX);
                    code[pc..pc + Self::WINDOW].first_chunk().expect("a window is WINDOW bytes")
                }

                /// The value encoded at byte `pc` of padded, checked code, and the offset after it (always inlined: the
                /// caller's `match` on the result then merges with the one on the tag). Fixed-size variants read their
                /// fields at constant offsets in the window; the others go through `Binary`
                #[inline(always)]
                pub fn decode_at(code: &[u8], pc: usize) -> (Self, usize) {
                    let b = Self::window(code, pc);
                    match b[0] {
                        #(#arms)*
                        #unused => panic!("checked code has no {} tag {}", #name, b[0]),
                    }
                }

                /// The `WINDOW` bytes from `pc`, without a bounds check.
                ///
                /// # Safety
                /// `code` is padded, checked code and `pc` the start of an instruction in it (so `pc + WINDOW` ≤ its
                /// length).
                #[inline(always)]
                pub unsafe fn window_unchecked(code: &[u8], pc: usize) -> &[u8; Self::WINDOW] {
                    // SAFETY: caller: the padding keeps the whole window inside `code`
                    unsafe { &*code.as_ptr().add(pc).cast::<[u8; Self::WINDOW]>() }
                }

                /// [`Self::decode_at`] without checks for fixed-size instructions (variable-size ones still slice).
                ///
                /// # Safety
                /// As [`Self::window_unchecked`].
                #[inline(always)]
                pub unsafe fn decode_unchecked(code: &[u8], pc: usize) -> (Self, usize) {
                    // SAFETY: caller
                    let b = unsafe { Self::window_unchecked(code, pc) };
                    match b[0] {
                        #(#arms)*
                        // SAFETY: checked code only has tags of its variants
                        _ => unsafe { core::hint::unreachable_unchecked() },
                    }
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

    /// `Fixed`: the fields at constant offsets
    fn fixed_impl(&self) -> Result<TokenStream> {
        let ident = format_ident!("{}", self.name);
        let (reads, size) = fixed_reads(self.fields.iter().map(|f| f.ty.as_str()), quote! { 0 })?;
        let value = match self.named()? {
            None => quote! { Self },
            Some(true) => {
                let names: Vec<Ident> = self.fields.iter().filter_map(|f| f.name.as_deref()).map(|n| format_ident!("{n}")).collect();
                quote! { Self { #(#names: #reads),* } }
            }
            Some(false) => quote! { Self ( #(#reads),* ) },
        };
        Ok(quote! {
            impl Fixed for #ident {
                const SIZE: usize = #size;
                #[inline]
                fn read(b: &[u8]) -> Self { let _ = b; #value }
            }
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
