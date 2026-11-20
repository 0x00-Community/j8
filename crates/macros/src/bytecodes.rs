//! `bytecodes!("v8.json")`: V8's bytecode tables (written by tools/v8bytecodes.py from V8's interpreter headers) as
//! Rust: `Bytecode` (V8's numbering), `OperandType`, `OperandTypeInfo`, `ImplicitRegisterUse`, and per bytecode its
//! operand types and implicit accumulator use. The JSON is the single source of truth.
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::Deserialize;
use syn::{Error, LitStr, Result};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tables {
    source: String,
    implicit_register_use: Vec<Use>,
    operand_types: Vec<Operand>,
    bytecodes: Vec<Code>,
    runtime: Vec<RuntimeFn>,
    intrinsics: Vec<IntrinsicFn>,
    function_kinds: Vec<String>,
    feedback_slot_kinds: Vec<SlotKind>,
    scope_types: Vec<String>,
    variable_modes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SlotKind {
    name: String,
    size: u8,
}

/// `SCRIPT_SCOPE` ⇒ `ScriptScope`; CamelCase names stay
fn camel(name: &str) -> String {
    if !name.contains('_') && name.chars().any(char::is_lowercase) {
        return name.to_owned();
    }
    name.split('_').map(|w| {
        let mut c = w.chars();
        c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect())
    }).collect()
}

/// A field-less `repr(u8)` enum numbered in order, with `ALL` / `from_u8`
fn simple_enum(name: &str, doc: &str, variants: &[String]) -> TokenStream {
    let ident = format_ident!("{name}");
    let vs: Vec<_> = variants.iter().map(|v| format_ident!("{}", camel(v))).collect();
    let tags = (0..vs.len()).map(|i| u8::try_from(i).unwrap_or(u8::MAX));
    let n = vs.len();
    quote! {
        #[doc = #doc]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum #ident { #(#vs = #tags),* }
        impl #ident {
            pub const ALL: [#ident; #n] = [#(#ident::#vs),*];
            pub const fn from_u8(v: u8) -> Option<Self> {
                if (v as usize) < #n { Some(Self::ALL[v as usize]) } else { None }
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeFn {
    name: String,
    args: i8,
    results: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntrinsicFn {
    name: String,
    args: i8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Use {
    name: String,
    bits: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operand {
    name: String,
    info: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Code {
    name: String,
    #[serde(rename = "use")]
    implicit: String,
    operands: Vec<String>,
}

impl Tables {
    pub fn read(rel: &LitStr) -> Result<Self> {
        let dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|e| Error::new(rel.span(), e))?;
        let path = std::path::Path::new(&dir).join(rel.value());
        let text = std::fs::read_to_string(&path).map_err(|e| Error::new(rel.span(), format_args!("{}: {e}", path.display())))?;
        serde_json::from_str(&text).map_err(|e| Error::new(rel.span(), format_args!("{}: {e}", path.display())))
    }

    pub fn expand(&self, rel: &LitStr) -> Result<TokenStream> {
        let source = format!("V8's bytecodes, from {}.", self.source);
        let codes: Vec<_> = self.bytecodes.iter().map(|c| format_ident!("{}", c.name)).collect();
        let tags = (0..self.bytecodes.len()).map(|i| u8::try_from(i).map_err(|_| Error::new(rel.span(), "more than 256 bytecodes"))).collect::<Result<Vec<_>>>()?;
        let count = codes.len();
        let ops: Vec<_> = self.operand_types.iter().map(|o| format_ident!("{}", o.name)).collect();
        let infos: Vec<_> = self.operand_types.iter().map(|o| format_ident!("{}", o.info)).collect();
        let mut info_set: Vec<&str> = Vec::new();
        for o in &self.operand_types {
            if !info_set.contains(&o.info.as_str()) {
                info_set.push(&o.info);
            }
        }
        let info_set: Vec<_> = info_set.into_iter().map(|n| format_ident!("{n}")).collect();
        let uses: Vec<_> = self.implicit_register_use.iter().map(|u| format_ident!("{}", u.name)).collect();
        let bits: Vec<_> = self.implicit_register_use.iter().map(|u| u.bits).collect();
        let operand_lists = self
            .bytecodes
            .iter()
            .map(|c| {
                let o = c.operands.iter().map(|o| format_ident!("{o}"));
                quote! { &[#(OperandType::#o),*] }
            })
            .collect::<Vec<_>>();
        let implicit = self.bytecodes.iter().map(|c| format_ident!("{}", c.implicit)).collect::<Vec<_>>();
        let runtime = self.runtime_tables(rel)?;
        let kinds = simple_enum("FunctionKind", "V8's `FunctionKind`.", &self.function_kinds);
        let scopes = simple_enum("ScopeType", "V8's `ScopeType`.", &self.scope_types);
        let modes = simple_enum("VariableMode", "V8's `VariableMode`.", &self.variable_modes);
        let slot_names: Vec<String> = self.feedback_slot_kinds.iter().map(|k| k.name.clone()).collect();
        let slots = simple_enum("FeedbackSlotKind", "V8's `FeedbackSlotKind`.", &slot_names);
        let slot_vs: Vec<_> = slot_names.iter().map(|n| format_ident!("{n}")).collect();
        let slot_sizes: Vec<_> = self.feedback_slot_kinds.iter().map(|k| k.size).collect();
        let path = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default()).join(rel.value()).to_string_lossy().into_owned();
        Ok(quote! {
            const _: &[u8] = include_bytes!(#path);

            #[doc = #source]
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
            #[repr(u8)]
            pub enum Bytecode { #(#codes = #tags),* }

            /// A bytecode operand's type (V8's `OperandType`).
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            #[repr(u8)]
            pub enum OperandType { #(#ops),* }

            /// What an operand type is made of: scalable or fixed, signed or not, its unscaled size (V8's
            /// `OperandTypeInfo`).
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            pub enum OperandTypeInfo { #(#info_set),* }

            /// How a bytecode uses the accumulator implicitly (V8's `ImplicitRegisterUse`, bit flags).
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            pub struct ImplicitRegisterUse(pub u8);

            #[allow(non_upper_case_globals)]
            impl ImplicitRegisterUse {
                #(pub const #uses: Self = Self(#bits);)*
            }

            impl Bytecode {
                /// Number of bytecodes (the last is `Illegal`)
                pub const COUNT: usize = #count;
                /// Every bytecode, by its number
                pub const ALL: [Bytecode; #count] = [#(Bytecode::#codes),*];

                /// The bytecode with number `b`
                #[inline]
                pub const fn from_u8(b: u8) -> Option<Self> {
                    if (b as usize) < Self::COUNT { Some(Self::ALL[b as usize]) } else { None }
                }

                /// Its operand types, in order
                pub const fn operand_types(self) -> &'static [OperandType] {
                    match self { #(Bytecode::#codes => #operand_lists,)* }
                }

                /// Its implicit use of the accumulator
                pub const fn implicit_register_use(self) -> ImplicitRegisterUse {
                    match self { #(Bytecode::#codes => ImplicitRegisterUse::#implicit,)* }
                }
            }

            #runtime
            #kinds
            #scopes
            #modes
            #slots

            impl FeedbackSlotKind {
                /// Entries the slot takes in the feedback vector (V8's `FeedbackMetadata::GetSlotSize`)
                pub const fn slot_size(self) -> u8 {
                    match self { #(FeedbackSlotKind::#slot_vs => #slot_sizes,)* }
                }
            }

            impl OperandType {
                /// What it is made of
                pub const fn info(self) -> OperandTypeInfo {
                    match self { #(OperandType::#ops => OperandTypeInfo::#infos,)* }
                }
            }
        })
    }

    /// `Runtime` (`CallRuntime` ids) and `Intrinsic` (`InvokeIntrinsic` ids)
    fn runtime_tables(&self, rel: &LitStr) -> Result<TokenStream> {
        let rnames: Vec<_> = self.runtime.iter().map(|r| format_ident!("{}", r.name)).collect();
        let rids = (0..self.runtime.len()).map(|i| u16::try_from(i).map_err(|_| Error::new(rel.span(), "more than 65536 runtime functions"))).collect::<Result<Vec<_>>>()?;
        let rargs: Vec<_> = self.runtime.iter().map(|r| r.args).collect();
        let rresults: Vec<_> = self.runtime.iter().map(|r| r.results).collect();
        let rcount = rnames.len();
        let inames: Vec<_> = self.intrinsics.iter().map(|r| format_ident!("{}", r.name)).collect();
        let iids = (0..self.intrinsics.len()).map(|i| u8::try_from(i).map_err(|_| Error::new(rel.span(), "more than 256 intrinsics"))).collect::<Result<Vec<_>>>()?;
        let iargs: Vec<_> = self.intrinsics.iter().map(|r| r.args).collect();
        let icount = inames.len();
        Ok(quote! {
            /// A runtime function (`CallRuntime`'s `RuntimeId`, V8's `Runtime::FunctionId` numbering).
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            #[repr(u16)]
            #[allow(non_camel_case_types)]
            pub enum Runtime { #(#rnames = #rids),* }

            impl Runtime {
                pub const COUNT: usize = #rcount;
                pub const ALL: [Runtime; #rcount] = [#(Runtime::#rnames),*];

                pub const fn from_u16(v: u16) -> Option<Self> {
                    if (v as usize) < Self::COUNT { Some(Self::ALL[v as usize]) } else { None }
                }

                /// Argument count (-1: variable)
                pub const fn args(self) -> i8 {
                    match self { #(Runtime::#rnames => #rargs,)* }
                }

                /// Result count (1 or a pair)
                pub const fn results(self) -> u8 {
                    match self { #(Runtime::#rnames => #rresults,)* }
                }
            }

            /// An interpreter intrinsic (`InvokeIntrinsic`'s `IntrinsicId`, V8's `IntrinsicsHelper::IntrinsicId`).
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            #[repr(u8)]
            pub enum Intrinsic { #(#inames = #iids),* }

            impl Intrinsic {
                pub const COUNT: usize = #icount;
                pub const ALL: [Intrinsic; #icount] = [#(Intrinsic::#inames),*];

                pub const fn from_u8(v: u8) -> Option<Self> {
                    if (v as usize) < Self::COUNT { Some(Self::ALL[v as usize]) } else { None }
                }

                /// Argument count (-1: variable)
                pub const fn args(self) -> i8 {
                    match self { #(Intrinsic::#inames => #iargs,)* }
                }
            }

        })
    }
}
