//! Inline caches for property reads/writes: one per `get.named` / `set.named` / `define.named` site.
//!
//! Keyed only by shape address (GC moves objects, so the cache must not hold object pointers); also holds a `Weak` so a
//! recognized address is not reused by another shape.
//! Dictionary shapes mutate in place (same address, different contents), so they are never cached.
//!
//! - Read: a data property owned by the receiver, or by a prototype 1..=[`DEPTH`] levels up (the shape of every level
//! on the way is checked)
//! - Write: a writable data property owned by the receiver
//! - Add property: old shape ⇒ new shape; assignments also check the shape of every level of the prototype chain (no
//! one on the chain has since added a setter / read-only property of the same name)
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use j8framework::{JsObject, Object, Shape, Slot, Value};
use j8framework::{Mutation};

/// Address in the cache of an empty object that has no shape yet (real shape addresses are 8-aligned, so never this)
const EMPTY: usize = 1;
/// Maximum number of prototype levels cached
const DEPTH: usize = 4;

/// Address of the object's current shape
fn addr(d: &Object<'_>) -> usize {
    d.props().shape_ptr().map_or(EMPTY, |s| s as usize)
}

/// All own properties are in the shape (and their names pass [`Ic::name_ok`]), and the shape does not mutate in place
fn plain(d: &Object<'_>) -> bool {
    d.is_plain() && !d.props().is_dict()
}

/// What it looked like before the change (compared against by the add-property cache).
#[derive(Clone)]
pub struct Before {
    shape: usize,
    /// Keeps the starting shape's allocation (so its address isn't reused while the cache holds it)
    from: Option<Weak<Shape>>,
    len: usize,
    plain: bool,
}

impl From<JsObject<'_>> for Before {
    fn from(o: JsObject<'_>) -> Self {
        let d = o.get();
        Self { shape: addr(d), from: d.props().shape().map(|s| Arc::downgrade(&s)), len: d.props().len(), plain: plain(d) }
    }
}

/// The cache for one property read/write site.
#[derive(Default)]
pub struct Ic {
    /// Receiver shape address (0: nothing cached)
    shape: Cell<usize>,
    /// Slot index
    slot: Cell<u32>,
    /// Read: how many prototype levels up holds it (0 is own); add property: how many levels the prototype chain has
    /// (checked to the end)
    depth: Cell<u8>,
    /// Shape addresses of each prototype level
    chain: [Cell<usize>; DEPTH],
    /// Add property: the shape transitioned to
    to: RefCell<Option<Arc<Shape>>>,
    /// `to` is set (a write adds the property rather than modifies it)
    adds: Cell<bool>,
    /// Keeps the recognized shape addresses alive
    keep: RefCell<Vec<Weak<Shape>>>,
}

impl Ic {
    /// Array `length`, indices and number-like names (typed arrays) take their own special paths and are not cached
    fn name_ok(name: &str) -> bool {
        name != "length" && name != "NaN" && name != "Infinity" && !name.starts_with(|c: char| c.is_ascii_digit() || c == '-')
    }

    fn reset(&self) {
        self.shape.set(0);
        self.keep.borrow_mut().clear();
        *self.to.borrow_mut() = None;
        self.adds.set(false);
    }

    fn keep(&self, d: &Object<'_>) -> usize {
        if let Some(s) = d.props().shape() {
            self.keep.borrow_mut().push(Arc::downgrade(&s));
        }
        addr(d)
    }

    /// Checks `depth` (≥ 1) shape levels along the prototype chain, returning the last level
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

    /// Every shape level of the whole prototype chain (ending after `depth` levels) is unchanged
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

    // ---- Read ----

    #[inline]
    pub fn get<'gc>(&self, o: JsObject<'gc>) -> Option<Value<'gc>> {
        // A cached shape is a fast-mode one: matching it rules out a dictionary
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
            Slot::Data(v) => Some(v),
            Slot::Accessor { .. } => None,
        }
    }

    /// After a slow-path read, records where it was found
    pub fn fill_get(&self, o: JsObject<'_>, name: &str) {
        self.reset();
        if !Self::name_ok(name) {
            return;
        }
        let d = o.get();
        if !plain(d) {
            return;
        }
        let recv = self.keep(d);
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
            c.set(self.keep(d));
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

    // ---- Write / add property ----

    /// On a hit, writes and returns `true`
    #[inline]
    pub fn set<'gc>(&self, mc: &Mutation<'gc>, o: JsObject<'gc>, v: Value<'gc>, assign: bool) -> bool {
        let want = self.shape.get();
        let d = o.get();
        if addr(d) != want || !d.is_plain() {
            return false;
        }
        // Modify an own writable data property (the cached shape is a fast-mode one)
        if !self.adds.get() {
            return d.props().write_data(mc, self.slot.get() as usize, v);
        }
        // Add property: extensible; assignments also require the whole prototype chain to be unchanged (an empty
        // object's address also matches a dictionary's)
        if d.props().is_dict() || d.non_extensible() || (assign && !self.chain_intact(d.proto(), usize::from(self.depth.get()))) {
            return false;
        }
        let to = self.to.borrow();
        let Some(to) = &*to else { return false };
        o.push_to(mc, to, Slot::Data(v));
        true
    }

    /// After a slow-path write, records it: an own property was modified, or a property was added (`assign`: an
    /// assignment, so the prototype chain must be checked)
    pub fn fill_set(&self, o: JsObject<'_>, name: &str, before: Before, assign: bool) {
        self.reset();
        if !Self::name_ok(name) || !before.plain {
            return;
        }
        let d = o.get();
        let Some(i) = d.props().position(name) else { return };
        if !plain(d) || !matches!(d.props().slot(i), Slot::Data(_)) {
            return;
        }
        let now = self.keep(d);
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
                c.set(self.keep(pd));
                proto = pd.proto();
                depth += 1;
            }
            self.depth.set(u8::try_from(depth).unwrap_or(u8::MAX));
        }
        // `to` is held strongly and the starting shape through a weak link in `keep`: neither address can be reused
        // while cached
        *self.to.borrow_mut() = Some(to);
        self.adds.set(true);
        self.keep.borrow_mut().extend(before.from);
        self.shape.set(before.shape);
    }
}

/// Address standing for a dictionary-mode object (never cached: real shape addresses are 8-aligned and `EMPTY` is 1)
const DICT: usize = 2;

/// The shape address of a fast-mode object (`EMPTY` with no keys yet), [`DICT`] in dictionary mode
fn fast(d: &Object<'_>) -> usize {
    if d.props().is_dict() { DICT } else { addr(d) }
}

/// Where a global name lives, for one `lda.global` / `sta.global` constant: an entry of the script scope or of the
/// global object, recognized by shape (the global object and the script scope stay in fast mode, see
/// `Object::widen`). A shape fixes the key at every index, so a hit reads the slot without comparing names; `keep`
/// stops the shapes' addresses from being reused while cached.
#[derive(Default)]
pub struct GlobalIc {
    /// Shape address of the script scope (0: nothing cached). Also checked for global object entries: a new top-level
    /// `let` changes it, and may shadow the name
    scope: Cell<usize>,
    /// Shape address of the global object; 0 when the name is a script scope entry
    global: Cell<usize>,
    slot: Cell<u32>,
    /// A writable data property (`sta.global` may write it in place)
    writable: Cell<bool>,
    keep: RefCell<Vec<Weak<Shape>>>,
}

impl GlobalIc {
    /// The object and slot holding the name, if the shapes are the cached ones
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

    /// The value, if cached and a data property
    #[inline]
    pub fn get<'gc>(&self, scope: JsObject<'gc>, global: JsObject<'gc>) -> Option<Value<'gc>> {
        let (o, at) = self.locate(scope, global)?;
        Ic::data(o.get().props().slot(at))
    }

    /// The holder and slot, if cached as a writable data property
    #[inline]
    pub fn writable<'gc>(&self, scope: JsObject<'gc>, global: JsObject<'gc>) -> Option<(JsObject<'gc>, usize)> {
        self.locate(scope, global).filter(|_| self.writable.get())
    }

    /// After a slow-path access, records where `name` is now (own data properties only)
    pub fn fill<'gc>(&self, scope: JsObject<'gc>, global: JsObject<'gc>, name: &str) {
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
        keep.extend(sd.props().shape().map(|s| Arc::downgrade(&s)));
        self.global.set(holder.map_or(0, |g| {
            keep.extend(g.props().shape().map(|s| Arc::downgrade(&s)));
            fast(g)
        }));
        self.slot.set(slot);
        self.writable.set(holder.unwrap_or(sd).props().attrs(at).writable());
        self.scope.set(fast(sd));
    }
}
