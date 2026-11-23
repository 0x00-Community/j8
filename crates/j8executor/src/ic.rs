use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use j8framework::{JsObject, Mutation, Object, ShapeKey, ShapeRef, Slot, Trace, Tracer, Value};

type Kept = ShapeKey;

fn kept_from<'gc>(_: &Mutation<'gc>, h: ShapeRef<'gc>) -> Kept {
    ShapeKey::from(h)
}

const EMPTY: usize = 1;

const DEPTH: usize = 4;

fn addr(d: &Object<'_>) -> usize {
    d.props().shape_id().unwrap_or(EMPTY)
}

fn plain(d: &Object<'_>) -> bool {
    d.is_plain() && !d.props().is_dict()
}

#[derive(Clone, Copy)]
pub struct Before {
    shape: usize,
    from: Option<ShapeKey>,
    len: usize,
    plain: bool,
}

impl From<JsObject<'_>> for Before {
    fn from(o: JsObject<'_>) -> Self {
        let d = o.get();
        Self { shape: addr(d), from: d.props().shape().map(ShapeKey::from), len: d.props().len(), plain: plain(d) }
    }
}

#[derive(Default)]
pub struct Ic {
    shape: Cell<usize>,
    slot: Cell<u32>,
    depth: Cell<u8>,
    chain: [Cell<usize>; DEPTH],
    to: RefCell<Option<Kept>>,
    adds: Cell<bool>,
    string_kind: Cell<u8>,
    keep: RefCell<Vec<Kept>>,
}

const STRING_LENGTH: u8 = 1;

const STRING_PROTO: u8 = 2;

impl Ic {
    fn name_ok(name: &str) -> bool {
        name != "length" && name != "NaN" && name != "Infinity" && !name.starts_with(|c: char| c.is_ascii_digit() || c == '-')
    }

    fn reset(&self) {
        self.shape.set(0);
        self.string_kind.set(0);
        self.keep.borrow_mut().clear();
        *self.to.borrow_mut() = None;
        self.adds.set(false);
    }

    #[inline]
    pub fn names_length(&self) -> bool {
        self.string_kind.get() == STRING_LENGTH
    }

    #[inline]
    pub fn note_length(&self) {
        if self.string_kind.get() == STRING_LENGTH {
            return;
        }
        self.reset();
        self.string_kind.set(STRING_LENGTH);
    }

    pub fn fill_primitive<'gc>(&self, mc: &Mutation<'gc>, proto: JsObject<'gc>, name: &str) {
        self.fill_get(mc, proto, name);
        if self.shape.get() != 0 {
            self.string_kind.set(STRING_PROTO);
        }
    }

    pub fn string_value<'gc>(&self, proto: JsObject<'gc>) -> Option<Value<'gc>> {
        (self.string_kind.get() == STRING_PROTO).then(|| self.probe(proto)).flatten()
    }

    #[inline(never)]
    fn probe<'gc>(&self, o: JsObject<'gc>) -> Option<Value<'gc>> {
        self.get(o)
    }

    fn keep<'gc>(&self, mc: &Mutation<'gc>, d: &Object<'gc>) -> usize {
        if let Some(s) = d.props().shape() {
            self.keep.borrow_mut().push(kept_from(mc, s));
        }
        addr(d)
    }

    fn holder<'gc>(&self, mut proto: Option<JsObject<'gc>>, depth: usize) -> Option<JsObject<'gc>> {
        let mut last = None;
        for c in &self.chain[..depth] {
            let p = proto?;
            let d = p.get();
            if addr(d) != c.get() || !plain(d) {
                return None;
            }
            proto = d.proto();
            last = Some(p);
        }
        last
    }

    fn chain_intact(&self, mut proto: Option<JsObject<'_>>, depth: usize) -> bool {
        for c in &self.chain[..depth] {
            let Some(p) = proto else { return false };
            let d = p.get();
            if addr(d) != c.get() || !plain(d) {
                return false;
            }
            proto = d.proto();
        }
        proto.is_none()
    }

    #[expect(clippy::inline_always, reason = "the property-load handler keeps its cache hit inline; with the string caller added the compiler stopped inlining it")]
    #[inline(always)]
    pub fn get<'gc>(&self, o: JsObject<'gc>) -> Option<Value<'gc>> {
        let want = self.shape.get();
        let d = o.get();
        if addr(d) != want || !d.is_plain() {
            return None;
        }
        let slot = self.slot.get() as usize;
        let holder = match self.depth.get() {
            0 => return d.props().read_data(slot),
            n => self.holder(d.proto(), usize::from(n))?,
        };
        holder.props().read_data(slot)
    }

    fn data(s: Slot<'_>) -> Option<Value<'_>> {
        match s {
            Slot::Data(v) if v.as_lazy().is_none() => Some(v),
            _ => None,
        }
    }

    pub fn fill_get<'gc>(&self, mc: &Mutation<'gc>, o: JsObject<'gc>, name: &str) {
        self.reset();
        if !Self::name_ok(name) {
            return;
        }
        let d = o.get();
        if !plain(d) {
            return;
        }
        let recv = self.keep(mc, d);
        if let Some(i) = d.props().position(name) {
            if matches!(d.props().slot(i), Slot::Data(_)) {
                self.set_slot(recv, i, 0);
            }
            return;
        }
        let mut proto = d.proto();
        for (level, c) in self.chain.iter().enumerate() {
            let Some(p) = proto else { return };
            let d = p.get();
            if !plain(d) {
                return;
            }
            c.set(self.keep(mc, d));
            if let Some(i) = d.props().position(name) {
                if matches!(d.props().slot(i), Slot::Data(_)) {
                    self.set_slot(recv, i, level + 1);
                }
                return;
            }
            proto = d.proto();
        }
    }

    fn set_slot(&self, shape: usize, slot: usize, depth: usize) {
        if let (Ok(slot), Ok(depth)) = (u32::try_from(slot), u8::try_from(depth)) {
            self.slot.set(slot);
            self.depth.set(depth);
            self.shape.set(shape);
        }
    }

    #[inline]
    pub fn set<'gc>(&self, mc: &Mutation<'gc>, o: JsObject<'gc>, v: Value<'gc>, assign: bool) -> bool {
        let want = self.shape.get();
        let d = o.get();
        if addr(d) != want || !d.is_plain() {
            return false;
        }
        if !self.adds.get() {
            return d.props().write_data(mc, self.slot.get() as usize, v);
        }
        if d.props().is_dict() || d.non_extensible() || (assign && !self.chain_intact(d.proto(), usize::from(self.depth.get()))) {
            return false;
        }
        let to = self.to.borrow();
        let Some(to) = *to else { return false };
        o.push_to(mc, unsafe { to.handle() }, Slot::Data(v));
        true
    }

    pub fn fill_set<'gc>(&self, mc: &Mutation<'gc>, o: JsObject<'gc>, name: &str, before: Before, assign: bool) {
        self.reset();
        if !Self::name_ok(name) || !before.plain {
            return;
        }
        let d = o.get();
        let Some(i) = d.props().position(name) else { return };
        if !plain(d) || !matches!(d.props().slot(i), Slot::Data(_)) {
            return;
        }
        let now = self.keep(mc, d);
        if now == before.shape {
            if d.props().attrs(i).writable() {
                self.set_slot(now, i, 0);
            }
            return;
        }
        if d.props().len() != before.len + 1 || i != before.len {
            return;
        }
        let Some(to) = d.props().shape() else { return };
        if assign {
            let mut proto = d.proto();
            let mut depth = 0;
            while let Some(p) = proto {
                let Some(c) = self.chain.get(depth) else { return };
                let pd = p.get();
                if !plain(pd) || pd.props().position(name).is_some() {
                    return;
                }
                c.set(self.keep(mc, pd));
                proto = pd.proto();
                depth += 1;
            }
            self.depth.set(u8::try_from(depth).unwrap_or(u8::MAX));
        }
        *self.to.borrow_mut() = Some(kept_from(mc, to));
        self.adds.set(true);
        self.keep.borrow_mut().extend(before.from.map(|k| kept_from(mc, unsafe { k.handle() })));
        self.shape.set(before.shape);
    }
}

const DICT: usize = 2;

fn fast(d: &Object<'_>) -> usize {
    if d.props().is_dict() { DICT } else { addr(d) }
}

#[derive(Default)]
pub struct GlobalIc {
    scope: Cell<usize>,
    global: Cell<usize>,
    slot: Cell<u32>,
    writable: Cell<bool>,
    keep: RefCell<Vec<Kept>>,
}

impl GlobalIc {
    #[inline]
    pub fn locate<'gc>(&self, scope: JsObject<'gc>, global: JsObject<'gc>) -> Option<(JsObject<'gc>, usize)> {
        let want = self.scope.get();
        if want == 0 || fast(scope.get()) != want {
            return None;
        }
        let holder = match self.global.get() {
            0 => scope,
            g if fast(global.get()) == g => global,
            _ => return None,
        };
        Some((holder, self.slot.get() as usize))
    }

    #[inline]
    pub fn get<'gc>(&self, scope: JsObject<'gc>, global: JsObject<'gc>) -> Option<Value<'gc>> {
        let (o, at) = self.locate(scope, global)?;
        Ic::data(o.get().props().slot(at))
    }

    #[inline]
    pub fn writable<'gc>(&self, scope: JsObject<'gc>, global: JsObject<'gc>) -> Option<(JsObject<'gc>, usize)> {
        self.locate(scope, global).filter(|_| self.writable.get())
    }

    pub fn fill<'gc>(&self, mc: &Mutation<'gc>, scope: JsObject<'gc>, global: JsObject<'gc>, name: &str) {
        self.scope.set(0);
        let mut keep = self.keep.borrow_mut();
        keep.clear();
        let own = |d: &Object<'_>| d.position(name).filter(|&i| matches!(d.props().slot(i), Slot::Data(_)));
        let (sd, gd) = (scope.get(), global.get());
        if sd.props().is_dict() {
            return;
        }
        let (holder, at) = match own(sd) {
            Some(at) => (None, at),
            None if sd.position(name).is_some() || gd.props().is_dict() => return,
            None => match own(gd) {
                Some(at) => (Some(gd), at),
                None => return,
            },
        };
        let Ok(slot) = u32::try_from(at) else { return };
        keep.extend(sd.props().shape().map(|s| kept_from(mc, s)));
        self.global.set(holder.map_or(0, |g| {
            keep.extend(g.props().shape().map(|s| kept_from(mc, s)));
            fast(g)
        }));
        self.slot.set(slot);
        self.writable.set(holder.unwrap_or(sd).props().attrs(at).writable());
        self.scope.set(fast(sd));
    }
}

#[derive(Clone)]
pub struct PropertyFeedback {
    pub shape: Option<ShapeKey>,
    pub slot: usize,
    pub depth: usize,
    pub chain: Vec<ShapeKey>,
    pub to: Option<ShapeKey>,
}

impl Ic {
    fn kept(&self, addr: usize) -> Option<ShapeKey> {
        let to = self.to.borrow();
        self.keep.borrow().iter().chain(to.as_ref()).copied().find(|k| k.id() == addr)
    }

    pub fn feedback(&self) -> Option<PropertyFeedback> {
        if self.string_kind.get() != 0 {
            return None;
        }
        self.recorded()
    }

    pub fn string_feedback(&self) -> Option<StringFeedback> {
        match self.string_kind.get() {
            STRING_LENGTH => Some(StringFeedback::Length),
            STRING_PROTO => self.recorded().map(StringFeedback::Proto),
            _ => None,
        }
    }

    fn recorded(&self) -> Option<PropertyFeedback> {
        let shape = match self.shape.get() {
            0 => return None,
            EMPTY => None,
            a => Some(self.kept(a)?),
        };
        let depth = usize::from(self.depth.get());
        let adds = self.adds.get();
        let levels = depth.min(DEPTH);
        let chain = self.chain[..levels].iter().map(|c| self.kept(c.get())).collect::<Option<Vec<_>>>()?;
        let to = if adds { Some((*self.to.borrow())?) } else { None };
        let slot = to.map_or(self.slot.get() as usize, |t| unsafe { t.shape() }.len() - 1);
        Some(PropertyFeedback { shape, slot, depth, chain, to })
    }

}

#[derive(Clone)]
pub enum StringFeedback {
    Length,
    Proto(PropertyFeedback),
}

#[derive(Clone)]
pub struct GlobalFeedback {
    pub scope: Option<ShapeKey>,
    pub global: Option<ShapeKey>,
    pub slot: usize,
    pub writable: bool,
}

impl GlobalIc {
    fn kept(&self, addr: usize) -> Option<ShapeKey> {
        self.keep.borrow().iter().copied().find(|k| k.id() == addr)
    }

    pub fn feedback(&self) -> Option<GlobalFeedback> {
        let scope = match self.scope.get() {
            0 | DICT => return None,
            EMPTY => None,
            a => Some(self.kept(a)?),
        };
        let global = match self.global.get() {
            0 => None,
            EMPTY | DICT => return None,
            g => Some(self.kept(g)?),
        };
        Some(GlobalFeedback { scope, global, slot: self.slot.get() as usize, writable: self.writable.get() })
    }
}

impl Ic {
    pub fn trace_keys(&self, t: &mut Tracer<'_>) {
        self.keep.borrow().iter().chain(self.to.borrow().as_ref()).for_each(|k| k.trace(t));
    }
}

impl GlobalIc {
    pub fn trace_keys(&self, t: &mut Tracer<'_>) {
        self.keep.borrow().iter().for_each(|k| k.trace(t));
    }
}
