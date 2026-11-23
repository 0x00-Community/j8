use std::collections::{BTreeMap, HashMap};
use std::string::String;
use std::vec::Vec;

use std::format;

use mlirpass::mlirformat::llvm::{FCmpPredicate, ICmpPredicate};
use mlirpass::mlirformat::{Error, FloatKind, Type};
use mlirpass::{Attrs, Body, BlockId, OpId, ValueId};

#[derive(Clone, Copy, Debug)]
pub struct Interval {
    pub value: ValueId,
    pub lo: u32,
    pub hi: u32,
    pub float: bool,
    pub across_call: bool,
}

impl Interval {
    const PSEUDO: [&str; 3] = [Template::HOLE, Template::SLOT, Template::JUMP];

    fn clobbers(body: &Body<'_>, op: OpId) -> bool {
        body[op].name == "llvm.call" && !body[op].get("callee").and_then(|a| Attrs::as_symbol(a).ok()).is_some_and(|c| Self::PSEUDO.iter().any(|p| c.starts_with(p)))
    }

    pub fn of(body: &Body<'_>, blocks: &[BlockId]) -> Result<Vec<Interval>, Error> {
        let mut index: HashMap<ValueId, u32> = HashMap::new();
        let mut values: Vec<ValueId> = Vec::new();
        let mut def: Vec<u32> = Vec::new();
        let mut used: Vec<u32> = Vec::new();
        let block_at: HashMap<BlockId, usize> = blocks.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let (mut start, mut end) = (std::vec![0u32; blocks.len()], std::vec![0u32; blocks.len()]);
        let mut calls = Vec::new();
        let mut uses: Vec<Vec<u32>> = std::vec![Vec::new(); blocks.len()];
        let mut defs: Vec<Vec<u32>> = std::vec![Vec::new(); blocks.len()];
        let mut succs: Vec<Vec<usize>> = std::vec![Vec::new(); blocks.len()];
        let mut pos = 0u32;
        let define = |v: ValueId, at: u32, index: &mut HashMap<ValueId, u32>, values: &mut Vec<ValueId>, def: &mut Vec<u32>, used: &mut Vec<u32>| -> u32 {
            let k = u32::try_from(values.len()).unwrap_or(u32::MAX);
            index.insert(v, k);
            values.push(v);
            def.push(at);
            used.push(at);
            k
        };
        for (bi, &b) in blocks.iter().enumerate() {
            start[bi] = pos;
            for &a in body[b].args.iter() {
                let k = define(a, pos, &mut index, &mut values, &mut def, &mut used);
                defs[bi].push(k);
            }
            for op in body.block_ops(b) {
                pos += 1;
                for &r in body[op].results.iter() {
                    let k = define(r, pos, &mut index, &mut values, &mut def, &mut used);
                    defs[bi].push(k);
                }
                if Self::clobbers(body, op) {
                    calls.push(pos);
                }
                succs[bi] = body[op].successors.iter().filter_map(|s| block_at.get(s).copied()).collect();
            }
            end[bi] = pos;
            pos += 1;
        }
        let mut seen = std::vec![u32::MAX; values.len()];
        for (bi, &b) in blocks.iter().enumerate() {
            let mut at = start[bi];
            for &k in &defs[bi] {
                seen[k as usize] = u32::try_from(bi).unwrap_or(u32::MAX);
            }
            for op in body.block_ops(b) {
                at += 1;
                for &v in body[op].operands.iter() {
                    let k = *index.get(&v).ok_or_else(|| Error("a value defined outside the function".into()))?;
                    used[k as usize] = used[k as usize].max(at);
                    let local = seen[k as usize] == u32::try_from(bi).unwrap_or(u32::MAX) && def[k as usize] < at;
                    if !local {
                        uses[bi].push(k);
                    }
                }
            }
        }
        let n = values.len();
        let words = n.div_ceil(64);
        let bits = |list: &[u32]| {
            let mut w = std::vec![0u64; words];
            for &k in list {
                w[k as usize / 64] |= 1 << (k % 64);
            }
            w
        };
        let gen_: Vec<Vec<u64>> = uses.iter().map(|u| bits(u)).collect();
        let kill: Vec<Vec<u64>> = defs.iter().map(|d| bits(d)).collect();
        let mut live_in = std::vec![std::vec![0u64; words]; blocks.len()];
        let mut live_out = std::vec![std::vec![0u64; words]; blocks.len()];
        loop {
            let mut changed = false;
            for i in (0..blocks.len()).rev() {
                let mut out = std::vec![0u64; words];
                for &s in &succs[i] {
                    for (o, x) in out.iter_mut().zip(&live_in[s]) {
                        *o |= *x;
                    }
                }
                let inn: Vec<u64> = gen_[i].iter().zip(&out).zip(&kill[i]).map(|((g, o), k)| g | (o & !k)).collect();
                if out != live_out[i] || inn != live_in[i] {
                    changed = true;
                    live_out[i] = out;
                    live_in[i] = inn;
                }
            }
            if !changed {
                break;
            }
        }
        let (mut lo, mut hi): (Vec<u32>, Vec<u32>) = (def.clone(), def.iter().zip(&used).map(|(d, u)| (*d).max(*u)).collect());
        for i in 0..blocks.len() {
            for (wi, (&li, &lo_)) in live_in[i].iter().zip(&live_out[i]).enumerate() {
                let mut w = li;
                while w != 0 {
                    let k = wi * 64 + w.trailing_zeros() as usize;
                    w &= w - 1;
                    lo[k] = lo[k].min(start[i]);
                    hi[k] = hi[k].max(start[i]);
                }
                let mut w = lo_;
                while w != 0 {
                    let k = wi * 64 + w.trailing_zeros() as usize;
                    w &= w - 1;
                    hi[k] = hi[k].max(end[i]);
                }
            }
        }
        let mut out = Vec::with_capacity(n);
        for (k, &value) in values.iter().enumerate() {
            let float = matches!(body[value].ty, Type::Float(FloatKind::F64));
            let first = calls.partition_point(|&c| c <= lo[k]);
            let across_call = calls.get(first).is_some_and(|&c| c < hi[k]);
            out.push(Interval { value, lo: lo[k], hi: hi[k], float, across_call });
        }
        out.sort_by_key(|iv| (iv.lo, iv.hi));
        Ok(out)
    }
}

pub struct Template;

impl Template {
    pub const SPILL_TOP: i32 = crate::asm::SAVED_END;
    pub const SPILL_WORDS: i32 = 16;
    pub const ATTR: &str = "stencil";
    pub const HOLE: &str = "hole.";
    pub const SLOT: &str = "slot.";
    pub const JUMP: &str = "jump.";

    pub fn callee<'s>(body: &'s Body<'_>, op: OpId) -> Option<&'s str> {
        (body[op].name == "llvm.call").then(|| body[op].get("callee").and_then(|a| Attrs::as_symbol(a).ok())).flatten()
    }

    pub fn slots(body: &Body<'_>, f: OpId) -> Option<BTreeMap<ValueId, String>> {
        if !body[f].has_flag(Self::ATTR) {
            return None;
        }
        let region = body[f].regions[0];
        let frame = body[region].blocks.first().and_then(|b| body[*b].args.get(1).copied());
        let mut slots: BTreeMap<ValueId, String> = BTreeMap::new();
        for &b in body[region].blocks.iter() {
            for op in body.block_ops(b) {
                let virt = match Self::callee(body, op) {
                    Some(c) if c.starts_with(Self::SLOT) => Some(String::from(c)),
                    _ => match (&*body[op].name, body[op].operands.first(), body[op].operands.get(1)) {
                        ("llvm.add", Some(&a), Some(o)) if Some(a) == frame => slots.get(o).cloned(),
                        ("llvm.add", Some(o), Some(&a)) if Some(a) == frame => slots.get(o).cloned(),
                        ("llvm.inttoptr", Some(a), _) => slots.get(a).cloned(),
                        _ => None,
                    },
                };
                if let (Some(name), Some(&r)) = (virt, body[op].results.first()) {
                    slots.insert(r, name);
                }
            }
        }
        Some(slots)
    }

    pub fn stencils(body: &Body<'_>, root: OpId) -> Vec<(String, OpId)> {
        let mut out = Vec::new();
        for &region in body[root].regions.iter() {
            for &block in body[region].blocks.iter() {
                for f in body.block_ops(block) {
                    if body[f].name == "llvm.func" && body[f].has_flag(Self::ATTR)
                        && let Some(name) = body[f].get_str("sym_name")
                    {
                        out.push((String::from(name), f));
                    }
                }
            }
        }
        out
    }
}

pub trait Predicate: Sized + Copy + 'static {
    const ALL: &'static [Self];

    fn of(i: i64) -> Result<Self, Error> {
        usize::try_from(i).ok().and_then(|i| Self::ALL.get(i)).copied().ok_or_else(|| Error(format!("predicate {i} out of range")))
    }
}

impl Predicate for ICmpPredicate {
    const ALL: &'static [Self] = &[ICmpPredicate::Eq, ICmpPredicate::Ne, ICmpPredicate::Slt, ICmpPredicate::Sle, ICmpPredicate::Sgt, ICmpPredicate::Sge, ICmpPredicate::Ult, ICmpPredicate::Ule, ICmpPredicate::Ugt, ICmpPredicate::Uge];
}

impl Predicate for FCmpPredicate {
    const ALL: &'static [Self] = &[FCmpPredicate::False, FCmpPredicate::Oeq, FCmpPredicate::Ogt, FCmpPredicate::Oge, FCmpPredicate::Olt, FCmpPredicate::Ole, FCmpPredicate::One, FCmpPredicate::Ord, FCmpPredicate::Ueq, FCmpPredicate::Ugt, FCmpPredicate::Uge, FCmpPredicate::Ult, FCmpPredicate::Ule, FCmpPredicate::Une, FCmpPredicate::Uno, FCmpPredicate::True];
}
