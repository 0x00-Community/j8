//! r8macros：r8 的过程宏。
//!
//! `enums!("x.json")`：从调用方 crate 根目录下的 JSON 文件生成枚举（定义、`Display`、按类型取字段）。
//! JSON 是唯一来源，⛔ 手写这些枚举。格式见 `USAGE.md`。
//!
//! `#[class]`：r8binding 的类绑定（`impl` 块上），见 `USAGE.md`。
mod class;
mod expand;
mod spec;

use proc_macro::TokenStream;
use syn::{LitStr, parse_macro_input};

/// `#[r8binding::class]` / `#[r8binding::class("名字")]`：写在 `impl` 块上，把方法导出成 JS 类的成员。
/// 方法上用 `#[js(constructor)]` / `#[js(getter)]` / `#[js(setter)]` / `#[js(static)]` / `#[js(skip)]` /
/// `#[js(name = "…")]` 标；没有 `self` 的缺省是静态方法。用法见 `USAGE.md`。
#[proc_macro_attribute]
pub fn class(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemImpl);
    class::expand(attr.into(), item).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// 读 `$CARGO_MANIFEST_DIR/<路径>` 的 JSON，生成其中的全部枚举。
/// 生成代码里 `include_bytes!` 了这个文件，JSON 一改调用方就重编。
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
    let body = spec.expand()?;
    let path = path.to_string_lossy();
    Ok(quote::quote! {
        const _: &[u8] = include_bytes!(#path);
        #body
    })
}
