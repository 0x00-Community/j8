//! `dispatch!(opcode, { prelude }, match insn { arms })`: an interpreter's instruction `match`, dispatched once on the
//! opcode.
//!
//! The arms are written against the decoded instruction (`Insn::Ldar(x) => …`) as usual. The expansion matches on
//! `opcode` (an `Opcode` value, variants named like the instruction's), and in each case runs `prelude` (which decodes
//! the instruction) and then only the arms that can match that opcode, in their original order (a guarded arm still
//! falls through to the later ones). With the opcode known in each case, the inlined decoder folds down to reading
//! that instruction's fields: one indirect jump per instruction instead of one for decoding plus one for the `match`.
//! Opcodes whose arms are the same share a case.
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Arm, Block, Expr, ExprMatch, Ident, Pat, Path, Result, Token};

pub struct Dispatch {
    opcode: Expr,
    prelude: Block,
    body: ExprMatch,
}

impl Parse for Dispatch {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let opcode = input.parse()?;
        input.parse::<Token![,]>()?;
        let prelude = input.parse()?;
        input.parse::<Token![,]>()?;
        let body = input.parse()?;
        input.parse::<Option<Token![,]>>()?;
        Ok(Self { opcode, prelude, body })
    }
}

/// Which variants an arm's pattern can match: `None` for any (a wildcard / binding)
fn variants(pat: &Pat) -> Option<Vec<(Path, Ident)>> {
    let path = match pat {
        Pat::Or(or) => return or.cases.iter().map(variants).try_fold(Vec::new(), |mut all, v| v.map(|v| {
            all.extend(v);
            all
        })),
        Pat::Path(p) => &p.path,
        Pat::TupleStruct(p) => &p.path,
        Pat::Struct(p) => &p.path,
        Pat::Paren(p) => return variants(&p.pat),
        _ => return None,
    };
    let mut ty = path.clone();
    let variant = ty.segments.pop()?.into_value().ident;
    ty.segments.pop_punct();
    Some(vec![(ty, variant)])
}

impl Dispatch {
    pub fn expand(&self) -> Result<TokenStream> {
        let Self { opcode, prelude, body } = self;
        let stmts = &prelude.stmts;
        let scrutinee = &body.expr;
        // Per arm, the variants it covers (None = all)
        let covers: Vec<Option<Vec<Ident>>> = body.arms.iter().map(|a| variants(&a.pat).map(|v| v.into_iter().map(|(_, i)| i).collect())).collect();
        if covers.iter().any(Option::is_none) {
            return Err(syn::Error::new_spanned(&body.expr, "dispatch!: every arm must name its variants (no `_` / bindings)"));
        }
        let opcode_ty = match opcode {
            Expr::Call(c) => match &*c.func {
                Expr::Path(p) => {
                    let mut ty = p.path.clone();
                    ty.segments.pop();
                    ty.segments.pop_punct();
                    ty
                }
                _ => return Err(syn::Error::new_spanned(opcode, "dispatch!: the opcode is `Opcode::at(..)`")),
            },
            _ => return Err(syn::Error::new_spanned(opcode, "dispatch!: the opcode is `Opcode::at(..)`")),
        };
        // Every variant in order of first appearance, with the arms that can match it
        let mut order: Vec<(Ident, Vec<usize>)> = Vec::new();
        for (i, c) in covers.iter().enumerate() {
            for v in c.iter().flatten() {
                match order.iter_mut().find(|(n, _)| n == v) {
                    Some((_, arms)) => arms.push(i),
                    None => order.push((v.clone(), vec![i])),
                }
            }
        }
        // Group variants with the same arms
        let mut groups: Vec<(Vec<Ident>, Vec<usize>)> = Vec::new();
        for (v, arms) in order {
            match groups.iter_mut().find(|(_, a)| *a == arms) {
                Some((vs, _)) => vs.push(v),
                None => groups.push((vec![v], arms)),
            }
        }
        let cases = groups.iter().map(|(vs, arms)| {
            // Every arm gets its comma: a later arm (or the catch-all) follows
            let arms: Vec<Arm> = arms.iter().map(|&i| Arm { comma: Some(Token![,](proc_macro2::Span::call_site())), ..body.arms[i].clone() }).collect();
            let vs = vs.iter().map(|v| format_ident!("{v}"));
            quote! {
                #(#opcode_ty::#vs)|* => {
                    #(#stmts)*
                    match #scrutinee {
                        #(#arms)*
                        #[allow(unreachable_patterns)]
                        _ => unreachable!("the opcode decides the instruction"),
                    }
                }
            }
        });
        Ok(quote! {
            match #opcode { #(#cases)* }
        })
    }
}
