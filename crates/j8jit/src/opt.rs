use alloc::vec;
use alloc::vec::Vec;

use crate::ir::{Graph, Helper, Op, Slot, B, V};

pub fn simplify(g: &mut Graph) {
    trivial_phis(g);
    dead(g);
    hoist(g);
}

fn rpo(g: &Graph) -> (Vec<B>, Vec<Vec<B>>) {
    let n = g.blocks.len();
    let mut seen = vec![false; n];
    let mut post = Vec::new();
    let mut stack: Vec<(B, usize)> = vec![(0, 0)];
    seen[0] = true;
    let mut preds = vec![Vec::new(); n];
    while let Some((b, i)) = stack.pop() {
        let succ = g.succs(b);
        if i < succ.len() {
            stack.push((b, i + 1));
            let s = succ[i];
            preds[s as usize].push(b);
            if !seen[s as usize] {
                seen[s as usize] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(b);
        }
    }
    post.reverse();
    (post, preds)
}

fn dominators(order: &[B], preds: &[Vec<B>], n: usize) -> Vec<Option<B>> {
    let mut rank = vec![usize::MAX; n];
    for (i, &b) in order.iter().enumerate() {
        rank[b as usize] = i;
    }
    let mut idom: Vec<Option<B>> = vec![None; n];
    idom[order[0] as usize] = Some(order[0]);
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &order[1..] {
            let mut new: Option<B> = None;
            for &p in &preds[b as usize] {
                if idom[p as usize].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(mut x) => {
                        let mut y = p;
                        while x != y {
                            while rank[x as usize] > rank[y as usize] {
                                x = idom[x as usize].unwrap_or(x);
                            }
                            while rank[y as usize] > rank[x as usize] {
                                y = idom[y as usize].unwrap_or(y);
                            }
                        }
                        x
                    }
                });
            }
            if new != idom[b as usize] {
                idom[b as usize] = new;
                changed = true;
            }
        }
    }
    idom
}

fn dominates(idom: &[Option<B>], a: B, mut b: B) -> bool {
    loop {
        if a == b {
            return true;
        }
        match idom[b as usize] {
            Some(p) if p != b => b = p,
            _ => return false,
        }
    }
}

fn clobbers(op: &Op) -> bool {
    match op {
        Op::Call(h, ..) => !matches!(h, Helper::ModF | Helper::ToBoolean | Helper::Alloc(_) | Helper::CreateClosure(..) | Helper::CreateContext(..) | Helper::ObjectLiteral(..)),
        Op::CallJit(..) | Op::StoreCell(..) => true,
        _ => false,
    }
}

fn hoistable(op: &Op) -> bool {
    matches!(op, Op::Untag(_) | Op::BoxPtr(..) | Op::BoxInt(_) | Op::BoxBool(_) | Op::IntToFloat(_) | Op::CmpWord(..) | Op::CmpInt(..) | Op::LoadWord(..) | Op::LoadPtr(..) | Op::LoadRaw(..) | Op::ContextCell(..) | Op::StoreCellAddr(..) | Op::CtxField(_) | Op::I32(_) | Op::F64(_) | Op::Word(_) | Op::CheckObject(..) | Op::CheckShape(..) | Op::CheckFunc(..) | Op::CheckNotHole(..) | Op::CheckNotLazy(..) | Op::UnboxInt(..) | Op::UnboxFloat(..) | Op::Check(..))
}

fn hoist(g: &mut Graph) {
    let n = g.blocks.len();
    let (order, preds) = rpo(g);
    if order.is_empty() {
        return;
    }
    let idom = dominators(&order, &preds, n);
    let pre = g.preheaders.clone();
    for (h, ph, fs) in pre {
        if idom.get(h as usize).copied().flatten().is_none() {
            continue;
        }
        let latches: Vec<B> = preds[h as usize].iter().copied().filter(|&p| dominates(&idom, h, p)).collect();
        if latches.is_empty() || !preds[h as usize].contains(&ph) {
            continue;
        }
        let mut body = vec![false; n];
        body[h as usize] = true;
        let mut work = latches.clone();
        while let Some(b) = work.pop() {
            if body[b as usize] {
                continue;
            }
            body[b as usize] = true;
            work.extend(preds[b as usize].iter().copied());
        }
        let in_body: Vec<B> = order.iter().copied().filter(|b| body[*b as usize]).collect();
        let ctx_store = |op: &Op| matches!(op, Op::StoreCell(base, _, _) if matches!(g.insts[*base as usize].op, Op::ContextCell(..)));
        let mut ctx_writes = false;
        let mut other = false;
        for &b in &in_body {
            for &v in &g.blocks[b as usize].insts {
                let op = &g.insts[v as usize].op;
                if ctx_store(op) {
                    ctx_writes = true;
                } else if clobbers(op) {
                    other = true;
                }
            }
        }
        if other {
            continue;
        }
        let always: Vec<B> = in_body.iter().copied().filter(|&b| latches.iter().all(|&l| dominates(&idom, b, l))).collect();
        let mut inside = vec![false; g.insts.len()];
        for &b in &in_body {
            for &v in &g.blocks[b as usize].insts {
                inside[v as usize] = true;
            }
        }
        let mut moved = Vec::new();
        let mut ins = Vec::new();
        for &b in &always {
            for &v in &g.blocks[b as usize].insts.clone() {
                let op = &g.insts[v as usize].op;
                if !hoistable(op) {
                    continue;
                }
                op.inputs(&mut ins);
                if ins.iter().any(|&x| inside[x as usize]) {
                    continue;
                }
                if ctx_writes && matches!(op, Op::LoadWord(base, _) if matches!(g.insts[*base as usize].op, Op::ContextCell(..))) {
                    continue;
                }
                let mut op = op.clone();
                if let Some(old) = op.frame_state() {
                    set_frame_state(&mut op, fs);
                    g.hoisted.push((v, old));
                }
                g.insts[v as usize].op = op;
                g.blocks[b as usize].insts.retain(|&x| x != v);
                inside[v as usize] = false;
                moved.push(v);
            }
        }
        let blk = &mut g.blocks[ph as usize];
        let at = blk.insts.len().saturating_sub(1);
        for (k, v) in moved.into_iter().enumerate() {
            blk.insts.insert(at + k, v);
            g.insts[v as usize].block = ph;
        }
    }
}

fn set_frame_state(op: &mut Op, to: crate::ir::Fs) {
    match op {
        Op::UnboxInt(_, fs) | Op::UnboxFloat(_, fs) | Op::FloatToInt(_, fs) | Op::CheckObject(_, fs) | Op::CheckNotHole(_, fs) | Op::CheckNotLazy(_, fs) | Op::CheckFunc(_, _, fs) | Op::CheckShape(_, _, fs) | Op::Check(_, _, fs) => *fs = to,
        _ => {}
    }
}

fn replace_all(g: &mut Graph, from: V, to: V) {
    let f = |v: V| if v == from { to } else { v };
    for i in &mut g.insts {
        i.op.map_inputs(f);
    }
    let slot = |s: &mut Slot| {
        if let Slot::Value(v) = s
            && *v == from
        {
            *v = to;
        }
    };
    for st in &mut g.states {
        for fr in &mut st.frames {
            slot(&mut fr.callee);
            fr.params.iter_mut().for_each(slot);
            fr.regs.iter_mut().for_each(slot);
            slot(&mut fr.context);
            slot(&mut fr.acc);
            if let Some((a, b)) = &mut fr.construct {
                slot(a);
                slot(b);
            }
        }
        for o in &mut st.objects {
            o.proto = f(o.proto);
            o.fields.iter_mut().for_each(slot);
        }
        for (c, _, s) in &mut st.writes {
            *c = f(*c);
            slot(s);
        }
    }
}

fn trivial_phis(g: &mut Graph) {
    loop {
        let mut changed = false;
        for v in 0..g.insts.len() {
            let Op::Phi(ins) = &g.insts[v].op else { continue };
            let me = u32::try_from(v).unwrap_or(u32::MAX);
            let mut only = None;
            let mut trivial = true;
            for &x in ins {
                if x == me {
                    continue;
                }
                match only {
                    None => only = Some(x),
                    Some(y) if y == x => {}
                    Some(_) => {
                        trivial = false;
                        break;
                    }
                }
            }
            if !trivial {
                continue;
            }
            let Some(to) = only else { continue };
            if g.insts[to as usize].rep != g.insts[v].rep {
                continue;
            }
            let b = g.insts[v].block as usize;
            g.blocks[b].insts.retain(|&i| i != me);
            g.insts[v].op = Op::Word(0);
            replace_all(g, me, to);
            changed = true;
        }
        if !changed {
            break;
        }
    }
}

fn dead(g: &mut Graph) {
    let n = g.insts.len();
    let mut live = vec![false; n];
    let mut work: Vec<V> = Vec::new();
    let placed: Vec<bool> = {
        let mut p = vec![false; n];
        for b in &g.blocks {
            for &i in &b.insts {
                p[i as usize] = true;
            }
        }
        p
    };
    let mark = |v: V, live: &mut Vec<bool>, work: &mut Vec<V>| {
        if !live[v as usize] {
            live[v as usize] = true;
            work.push(v);
        }
    };
    for (i, inst) in g.insts.iter().enumerate() {
        if placed[i] && inst.op.effectful() {
            mark(u32::try_from(i).unwrap_or(0), &mut live, &mut work);
        }
    }
    let mut ins = Vec::new();
    while let Some(v) = work.pop() {
        let op = &g.insts[v as usize].op;
        op.inputs(&mut ins);
        for &x in &ins {
            mark(x, &mut live, &mut work);
        }
        if let Some(fs) = op.frame_state() {
            let st = &g.states[fs as usize];
            let mut vals = Vec::new();
            let slot = |s: &Slot, vals: &mut Vec<V>| {
                if let Slot::Value(v) = s {
                    vals.push(*v);
                }
            };
            for fr in &st.frames {
                slot(&fr.callee, &mut vals);
                fr.params.iter().for_each(|s| slot(s, &mut vals));
                fr.regs.iter().for_each(|s| slot(s, &mut vals));
                slot(&fr.context, &mut vals);
                slot(&fr.acc, &mut vals);
                if let Some((a, b)) = &fr.construct {
                    slot(a, &mut vals);
                    slot(b, &mut vals);
                }
            }
            for o in &st.objects {
                vals.push(o.proto);
                o.fields.iter().for_each(|s| slot(s, &mut vals));
            }
            for (c, _, s) in &st.writes {
                vals.push(*c);
                slot(s, &mut vals);
            }
            for x in vals {
                mark(x, &mut live, &mut work);
            }
        }
    }
    for b in &mut g.blocks {
        b.insts.retain(|&i| live[i as usize]);
    }
}
