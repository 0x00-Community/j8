//! j8macros: procedural macros for j8.
//!
//! `enums!("x.json")`: generates enums (definitions, `Display`, field access by type) and structs from a JSON file
//! under the calling crate's root directory. The JSON is the single source of truth; ⛔ do not hand-write these types.
//! See `USAGE.md` for the format.
//!
//! `#[class]` / `#[object]` / `#[function]`: Rust ⇄ JS bindings (used through the facade crate `j8`:
//! `#[j8::class]` etc.); see `USAGE.md`.
mod class;
mod expand;
mod function;
mod heap;
mod method;
mod object;
mod spec;
mod trace;

use proc_macro::TokenStream;
use syn::{DeriveInput, LitStr, parse_macro_input};

/// `#[j8::class]` / `#[j8::class("name")]`: placed on an `impl` block; exports the methods as members of a JS
/// class. Mark methods with `#[js(constructor)]` / `#[js(getter)]` / `#[js(setter)]` / `#[js(static)]` /
/// `#[js(skip)]` / `#[js(name = "…")]`; methods without `self` default to static methods. See `USAGE.md`.
#[proc_macro_attribute]
pub fn class(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemImpl);
    class::expand(attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// `#[j8::object]` / `#[j8::object("name")]`: exports an `impl` block containing only associated functions as a
/// plain object.
#[proc_macro_attribute]
pub fn object(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemImpl);
    object::expand(attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// `#[j8::module("node:fs", "fs")]`: like `#[j8::object]`, exported as a native module under these specifiers (the
/// first is canonical); a runtime built with `j8::runtime!` serves it to `import` and `require`.
#[proc_macro_attribute]
pub fn module(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemImpl);
    object::expand_module(attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// `#[j8::function]` / `#[j8::function("name")]`: exports a function as a JS function (`name::install`).
#[proc_macro_attribute]
pub fn function(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemFn);
    function::expand(attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// `#[derive(Collect)]`: j8 堆类型的追踪 / 重定位（paragc 的 `Trace`）；`#[collect(require_static)]` 放在字段上不追踪它、
/// 放在类型上整个没有边。
#[proc_macro_derive(Collect, attributes(collect))]
pub fn derive_trace(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    trace::expand(&input).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// `#[j8::heap]` / `#[j8::heap(new)]`: a heap struct whose `#[mutable]` fields change in place through cells chosen by
/// `j8framework::Mutable` (accessors `x()` / `set_x(mc, v)`; `#[mutable(set = name)]` renames the setter). Adds
/// `#[derive(Collect)]` when missing; `new` generates a constructor taking every field.
#[proc_macro_attribute]
pub fn heap(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemStruct);
    heap::expand(&attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Reads the JSON at `$CARGO_MANIFEST_DIR/<path>` and generates all the enums in it.
/// The generated code `include_bytes!`s the file, so the caller is rebuilt whenever the JSON changes.
#[proc_macro]
pub fn enums(input: TokenStream) -> TokenStream {
    let rel = parse_macro_input!(input as LitStr);
    expand(&rel).unwrap_or_else(syn::Error::into_compile_error).into()
}

fn expand(rel: &LitStr) -> syn::Result<proc_macro2::TokenStream> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|e| syn::Error::new(rel.span(), e))?;
    let path = std::path::Path::new(&dir).join(rel.value());
    let text = std::fs::read_to_string(&path).map_err(|e| syn::Error::new(rel.span(), format_args!("{}: {e}", path.display())))?;
    let spec: spec::Spec = serde_json::from_str(&text).map_err(|e| syn::Error::new(rel.span(), format_args!("{}: {e}", path.display())))?;
    let body = spec.expand(&text)?;
    let path = path.to_string_lossy();
    Ok(quote::quote! {
        const _: &[u8] = include_bytes!(#path);
        #body
    })
}
