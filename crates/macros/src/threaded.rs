//! `threaded!(TABLE: Handler, Opcode, invalid, fn template(..) -> R { .. __arms!() .. }, match insn { arms })`: an
//! interpreter's instruction `match` turned into one handler function per opcode (threaded code).
//!
//! The arms are grouped by opcode as in `dispatch!` (opcodes whose arms are the same share a handler). Each handler is
//! a copy of `template` (named `template_<first variant>`) with `__arms!()` replaced by `match insn { the arms that can
//! match }` and `__ops!()` with the pattern of its opcodes (`Opcode::A | Opcode::B`). Inside the arms:
//! - `continue 'frame` ⇒ `break 'arm` (the template treats it as "frames changed")
//! - `continue` ⇒ `break 'next` (next instruction)
//! - `return e` ⇒ `break 'h Some(e)` (`e` runs to the next top-level `,` / `;` or the end of its group)
//! - `self` ⇒ `exe`
//!
//! The labels `'h` / `'next` / `'arm` and the binding `exe` come from the template. `?` is left alone: it returns from
//! the handler. Also generated: `const TABLE: [Handler; 256]`, indexed by the opcode byte, unused bytes ⇒ `invalid`.
use proc_macro2::{Delimiter, Group, Ident, Punct, Spacing, Span, TokenStream, TokenTree};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Arm, Expr, ExprMatch, ItemFn, Path, Result, Token, Type};

use crate::dispatch::variants;

pub struct Threaded {
    table: Ident,
    handler: Type,
    opcode: Path,
    invalid: Expr,
    template: ItemFn,
    body: ExprMatch,
}

impl Parse for Threaded {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let table = input.parse()?;
        input.parse::<Token![:]>()?;
        let handler = input.parse()?;
        input.parse::<Token![,]>()?;
        let opcode = input.parse()?;
        input.parse::<Token![,]>()?;
        let invalid = input.parse()?;
        input.parse::<Token![,]>()?;
        let template = input.parse()?;
        let body = input.parse()?;
        input.parse::<Option<Token![,]>>()?;
        Ok(Self { table, handler, opcode, invalid, template, body })
    }
}

/// Rewrites the control flow of an arm (see the module docs)
struct Rewrite;

impl Rewrite {
    fn label(name: &str) -> TokenStream {
        let mut t = TokenStream::new();
        t.extend([TokenTree::Punct(Punct::new('\'', Spacing::Joint)), TokenTree::Ident(Ident::new(name, Span::call_site()))]);
        t
    }

    fn is(tt: Option<&TokenTree>, word: &str) -> bool {
        matches!(tt, Some(TokenTree::Ident(i)) if i == word)
    }

    fn is_punct(tt: Option<&TokenTree>, c: char) -> bool {
        matches!(tt, Some(TokenTree::Punct(p)) if p.as_char() == c)
    }

    fn stream(ts: TokenStream) -> TokenStream {
        let tts: Vec<TokenTree> = ts.into_iter().collect();
        let mut out = TokenStream::new();
        let mut i = 0;
        while i < tts.len() {
            let tt = &tts[i];
            if Self::is(Some(tt), "continue") {
                // `continue 'frame`
                if Self::is_punct(tts.get(i + 1), '\'') && Self::is(tts.get(i + 2), "frame") {
                    out.extend(quote! { break });
                    out.extend(Self::label("arm"));
                    i += 3;
                } else {
                    out.extend(quote! { break });
                    out.extend(Self::label("next"));
                    i += 1;
                }
                continue;
            }
            if Self::is(Some(tt), "return") {
                // The returned expression: up to a top-level `,` / `;` (or the end of this group)
                let mut j = i + 1;
                while j < tts.len() && !Self::is_punct(tts.get(j), ',') && !Self::is_punct(tts.get(j), ';') {
                    j += 1;
                }
                let e: TokenStream = Self::stream(tts[i + 1..j].iter().cloned().collect());
                let e = if e.is_empty() { quote! { () } } else { e };
                out.extend(quote! { break });
                out.extend(Self::label("h"));
                out.extend(quote! { Some((#e)) });
                i = j;
                continue;
            }
            match tt {
                TokenTree::Ident(id) if id == "self" => out.extend([TokenTree::Ident(Ident::new("exe", id.span()))]),
                TokenTree::Group(g) => {
                    let mut ng = Group::new(g.delimiter(), Self::stream(g.stream()));
                    ng.set_span(g.span());
                    out.extend([TokenTree::Group(ng)]);
                }
                other => out.extend([other.clone()]),
            }
            i += 1;
        }
        out
    }
}

/// Replaces `__arms!()` (`__arms ! ( )`) in the template with `arms`, and `__ops!()` with `ops`
fn splice(ts: TokenStream, arms: &TokenStream, ops: &TokenStream) -> TokenStream {
    let tts: Vec<TokenTree> = ts.into_iter().collect();
    let mut out = TokenStream::new();
    let mut i = 0;
    while i < tts.len() {
        let call = Rewrite::is_punct(tts.get(i + 1), '!') && matches!(tts.get(i + 2), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis);
        if call && Rewrite::is(tts.get(i), "__arms") {
            out.extend(arms.clone());
            i += 3;
            continue;
        }
        if call && Rewrite::is(tts.get(i), "__ops") {
            out.extend(ops.clone());
            i += 3;
            continue;
        }
        match &tts[i] {
            TokenTree::Group(g) => {
                let mut ng = Group::new(g.delimiter(), splice(g.stream(), arms, ops));
                ng.set_span(g.span());
                out.extend([TokenTree::Group(ng)]);
            }
            other => out.extend([other.clone()]),
        }
        i += 1;
    }
    out
}

impl Threaded {
    pub fn expand(&self) -> Result<TokenStream> {
        let Self { table, handler, opcode, invalid, template, body } = self;
        let covers: Vec<Vec<Ident>> = body
            .arms
            .iter()
            .map(|a| variants(&a.pat).map(|v| v.into_iter().map(|(_, i)| i).collect()))
            .collect::<Option<_>>()
            .ok_or_else(|| syn::Error::new_spanned(&body.expr, "threaded!: every arm must name its variants (no `_` / bindings)"))?;
        // Every variant in order of first appearance, with the arms that can match it; then grouped by same arms
        let mut order: Vec<(Ident, Vec<usize>)> = Vec::new();
        for (i, c) in covers.iter().enumerate() {
            for v in c {
                match order.iter_mut().find(|(n, _)| n == v) {
                    Some((_, arms)) => arms.push(i),
                    None => order.push((v.clone(), vec![i])),
                }
            }
        }
        let mut groups: Vec<(Vec<Ident>, Vec<usize>)> = Vec::new();
        for (v, arms) in order {
            match groups.iter_mut().find(|(_, a)| *a == arms) {
                Some((vs, _)) => vs.push(v),
                None => groups.push((vec![v], arms)),
            }
        }
        let scrutinee = &body.expr;
        let name = &template.sig.ident;
        let mut fns = Vec::new();
        let mut entries = Vec::new();
        for (vs, arms) in &groups {
            let arms: Vec<TokenStream> = arms
                .iter()
                .map(|&i| {
                    let a: &Arm = &body.arms[i];
                    let pat = &a.pat;
                    let guard = a.guard.as_ref().map(|(_, g)| quote! { if #g });
                    let b = Rewrite::stream(quote::ToTokens::to_token_stream(&*a.body));
                    quote! { #pat #guard => { #b } }
                })
                .collect();
            let matched = quote! {
                match #scrutinee {
                    #(#arms,)*
                    #[allow(unreachable_patterns)]
                    _ => unreachable!("the opcode decides the instruction"),
                }
            };
            let hname = format_ident!("{}_{}", name, vs[0].to_string().to_lowercase());
            let mut f = template.clone();
            f.sig.ident = hname.clone();
            let stmts = &f.block.stmts;
            let ops = { let vs = vs.iter(); quote! { #(#opcode::#vs)|* } };
            let block = splice(quote! { #(#stmts)* }, &matched, &ops);
            let attrs = &f.attrs;
            let vis = &f.vis;
            let sig = &f.sig;
            fns.push(quote! {
                #(#attrs)*
                #[allow(unused_variables, unused_mut, unused_assignments, unused_labels, unused_macros, unreachable_code, clippy::needless_return)]
                #vis #sig { #block }
            });
            for v in vs {
                entries.push(quote! { t[#opcode::#v as usize] = Self::#hname; });
            }
        }
        Ok(quote! {
            #(#fns)*
            /// Handlers by opcode byte (unused bytes: the invalid handler)
            const #table: [#handler; 256] = {
                let mut t: [#handler; 256] = [#invalid; 256];
                #(#entries)*
                t
            };
        })
    }
}
