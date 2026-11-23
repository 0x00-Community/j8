use alloc::sync::Arc;
use alloc::vec::Vec;
use core::hash::BuildHasherDefault;

use j8lockfree::{AtomicRefCell, OnceBox, Prefix};
use paragc::{Handle, HandleSlot};

use crate::Collect;
use rustc_hash::FxHasher;

use crate::store::Store;
use crate::{Erased, Gc, GcCell, GcSlot, IsoCell, Mutation, Property, SmallStr, Slot, Value, ValueCell};

const LINEAR: usize = 8;

const INDEX_AFTER: u32 = 4;

const MAX_FAST: usize = 64;

const INLINE: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attrs(u8);

impl Attrs {
    const ENUMERABLE: u8 = 1;
    const WRITABLE: u8 = 1 << 1;
    const CONFIGURABLE: u8 = 1 << 2;

    pub fn enumerable(self) -> bool {
        self.0 & Self::ENUMERABLE != 0
    }

    pub fn writable(self) -> bool {
        self.0 & Self::WRITABLE != 0
    }

    pub fn configurable(self) -> bool {
        self.0 & Self::CONFIGURABLE != 0
    }
}

impl From<&Property<'_>> for Attrs {
    fn from(p: &Property<'_>) -> Self {
        Self((u8::from(p.enumerable) * Self::ENUMERABLE) | (u8::from(p.writable) * Self::WRITABLE) | (u8::from(p.configurable) * Self::CONFIGURABLE))
    }
}

impl<'gc> From<(Slot<'gc>, Attrs)> for Property<'gc> {
    fn from((slot, a): (Slot<'gc>, Attrs)) -> Self {
        Self { slot, enumerable: a.enumerable(), writable: a.writable(), configurable: a.configurable() }
    }
}

pub type ShapeRef<'gc> = Handle<'gc, Shape>;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ShapeKey(usize);

impl ShapeKey {
    pub fn id(self) -> usize {
        self.0
    }

    pub unsafe fn shape<'a>(self) -> &'a Shape {
        unsafe { Handle::<'a, Shape>::from_id(self.0) }.get().get()
    }

    pub unsafe fn handle<'gc>(self) -> ShapeRef<'gc> {
        unsafe { Handle::from_id(self.0) }
    }
}

unsafe impl crate::Trace for ShapeKey {
    fn trace(&self, t: &mut crate::Tracer<'_>) {
        unsafe { self.handle() }.trace(t);
    }

    fn relocate(&mut self, t: &mut crate::Tracer<'_>) {
        self.trace(t);
    }
}

impl From<ShapeRef<'_>> for ShapeKey {
    fn from(h: ShapeRef<'_>) -> Self {
        Self(h.id())
    }
}

paragc::brand!(pub ShapeBrand = for<'gc> Shape);

type Transition = (SmallStr, Attrs, paragc::Weak<ShapeBrand>);

pub struct Shape {
    keys: Arc<Prefix<(SmallStr, Attrs)>>,
    len: usize,
    index: OnceBox<hashbrown::HashMap<SmallStr, u32, BuildHasherDefault<FxHasher>>>,
    searches: IsoCell<u32>,
    transitions: AtomicRefCell<Vec<Transition>>,
    this: IsoCell<usize>,
    digit_keys: bool,
}

unsafe impl crate::Trace for Shape {
    const NO_EDGES: bool = true;

    fn trace(&self, _: &mut crate::Tracer<'_>) {}
}

impl Shape {
    fn new(keys: Arc<Prefix<(SmallStr, Attrs)>>, len: usize) -> Self {
        let digit_keys = keys.prefix(len).iter().any(|(k, _)| k.as_bytes().first().is_some_and(u8::is_ascii_digit));
        Self { keys, len, index: OnceBox::default(), searches: IsoCell::new(0), transitions: AtomicRefCell::new(Vec::new()), this: IsoCell::new(0), digit_keys }
    }

    fn alloc<'gc>(mc: &Mutation<'gc>, keys: Arc<Prefix<(SmallStr, Attrs)>>, len: usize) -> ShapeRef<'gc> {
        let h = mc.mutator().alloc_handle(Self::new(keys, len));
        h.this.set(h.id());
        h
    }

    pub fn handle<'gc>(&self, _: &Mutation<'gc>) -> ShapeRef<'gc> {
        unsafe { Handle::from_id(self.this.get()) }
    }

    pub fn id(&self) -> usize {
        self.this.get()
    }

    pub fn digit_keys(&self) -> bool {
        self.digit_keys
    }

    fn keys(&self) -> &[(SmallStr, Attrs)] {
        self.keys.prefix(self.len)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn find(&self, key: &str) -> Option<usize> {
        if let Some(m) = self.index.get() {
            return m.get(key).map(|&i| i as usize);
        }
        if self.len > LINEAR && {
            let n = self.searches.get();
            self.searches.set(n.saturating_add(1));
            n >= INDEX_AFTER
        } {
            let m = self.index.get_or_init(|| self.keys().iter().zip(0u32..).map(|((k, _), i)| (k.clone(), i)).collect());
            return m.get(key).map(|&i| i as usize);
        }
        self.keys().iter().position(|(k, _)| **k == *key)
    }

    pub fn key(&self, i: usize) -> &SmallStr {
        &self.keys()[i].0
    }

    pub fn attrs(&self, i: usize) -> Attrs {
        self.keys()[i].1
    }

    pub fn rooted<'gc>(mc: &Mutation<'gc>, keys: Vec<(SmallStr, Attrs)>) -> ShapeRef<'gc> {
        let len = keys.len();
        Self::alloc(mc, Prefix::new(keys), len)
    }

    pub fn empty<'gc>(mc: &Mutation<'gc>) -> ShapeRef<'gc> {
        Self::rooted(mc, Vec::new())
    }

    pub fn child<'gc>(&self, mc: &Mutation<'gc>, key: &str, attrs: Attrs) -> ShapeRef<'gc> {
        let m = mc.mutator();
        let mut ts = self.transitions.borrow_mut();
        let mut found = None;
        ts.retain(|(k, a, w)| match m.upgrade(w) {
            Some(c) => {
                if found.is_none() && **k == *key && *a == attrs {
                    found = Some(c);
                }
                true
            }
            None => false,
        });
        if let Some(c) = found {
            return c.get().handle(mc);
        }
        let child = Self::alloc(mc, self.keys.extend(self.len, (SmallStr::from(key), attrs)), self.len + 1);
        ts.push((SmallStr::from(key), attrs, m.weak::<ShapeBrand>(child.get())));
        child
    }
}

#[derive(Collect)]
pub struct Dict<'gc> {
    #[collect(require_static)]
    keys: Vec<(SmallStr, Attrs)>,
    #[collect(require_static)]
    index: Option<hashbrown::HashMap<SmallStr, u32, BuildHasherDefault<FxHasher>>>,
    slots: Vec<Slot<'gc>>,
}

impl Dict<'_> {
    fn reindex(&mut self) {
        self.index = (self.keys.len() > LINEAR).then(|| self.keys.iter().zip(0u32..).map(|((k, _), i)| (k.clone(), i)).collect());
    }

    fn find(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(m) => m.get(key).map(|&i| i as usize),
            None => self.keys.iter().position(|(k, _)| **k == *key),
        }
    }
}

pub struct Props<'gc> {
    shape: HandleSlot<'gc, Shape>,
    store: GcSlot<'gc, Erased>,
    dict: GcSlot<'gc, GcCell<'gc, Dict<'gc>>>,
    inline: [ValueCell<'gc>; INLINE],
}

impl Default for Props<'_> {
    fn default() -> Self {
        Self {
            shape: HandleSlot::default(),
            store: GcSlot::default(),
            dict: GcSlot::default(),
            inline: core::array::from_fn(|_| ValueCell::new(Value::Undefined)),
        }
    }
}

unsafe impl crate::Trace for Props<'_> {
    fn trace(&self, t: &mut crate::Tracer<'_>) {
        self.shape.trace(t);
        self.store.trace(t);
        self.dict.trace(t);
        self.inline.iter().for_each(|c| c.trace(t));
    }

    fn relocate(&mut self, t: &mut crate::Tracer<'_>) {
        self.shape.relocate(t);
        self.store.relocate(t);
        self.dict.relocate(t);
        self.inline.iter_mut().for_each(|c| c.relocate(t));
    }
}

impl<'gc> Props<'gc> {
    pub const SHAPE_OFFSET: usize = core::mem::offset_of!(Props<'static>, shape);
    pub const STORE_OFFSET: usize = core::mem::offset_of!(Props<'static>, store);
    pub const DICT_OFFSET: usize = core::mem::offset_of!(Props<'static>, dict);
    pub const INLINE_OFFSET: usize = core::mem::offset_of!(Props<'static>, inline);
    pub const INLINE: usize = INLINE;

    pub fn with_capacity(mc: &Mutation<'gc>, n: usize) -> Self {
        let store = (n > INLINE).then(|| Store::reserve(mc, n - INLINE).gc());
        Self { store: GcSlot::from(store), ..Self::default() }
    }

    fn dict_cell(&self) -> Option<&'gc GcCell<'gc, Dict<'gc>>> {
        self.dict.get().map(Gc::get)
    }

    fn store(&self) -> Option<Store<'gc>> {
        self.store.get().map(|g| unsafe { Store::from_gc(g) })
    }

    pub fn store_path(&self, i: usize) -> Option<crate::store::StorePath> {
        self.store()?.path(i.checked_sub(INLINE)?)
    }

    #[inline]
    fn cell(&self, i: usize) -> Option<&ValueCell<'gc>> {
        match i.checked_sub(INLINE) {
            None => Some(&self.inline[i]),
            Some(k) => self.store()?.cell(k),
        }
    }

    #[inline]
    pub fn shape_ref(&self) -> Option<&'gc Shape> {
        self.shape.get().map(|h| h.get().get())
    }

    #[inline]
    pub fn shape_id(&self) -> Option<usize> {
        self.shape.get().map(Handle::id)
    }

    #[inline]
    pub fn shape(&self) -> Option<ShapeRef<'gc>> {
        self.shape.get()
    }

    #[inline]
    pub fn is_dict(&self) -> bool {
        self.dict.get().is_some()
    }

    pub fn len(&self) -> usize {
        match self.dict_cell() {
            Some(d) => d.borrow().keys.len(),
            None => self.shape_ref().map_or(0, Shape::len),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn position(&self, key: &str) -> Option<usize> {
        match self.dict_cell() {
            Some(d) => d.borrow().find(key),
            None => self.shape_ref()?.find(key),
        }
    }

    pub fn key(&self, i: usize) -> SmallStr {
        match self.dict_cell() {
            Some(d) => d.borrow().keys[i].0.clone(),
            None => self.shape_ref().expect("a property's shape").key(i).clone(),
        }
    }

    pub fn attrs(&self, i: usize) -> Attrs {
        match self.dict_cell() {
            Some(d) => d.borrow().keys[i].1,
            None => self.shape_ref().expect("a property's shape").attrs(i),
        }
    }

    #[inline]
    pub fn read_data(&self, i: usize) -> Option<Value<'gc>> {
        let v = self.cell(i)?.get();
        v.is_plain_slot().then_some(v)
    }

    #[inline]
    pub fn write_data(&self, mc: &Mutation<'gc>, i: usize, v: Value<'gc>) -> bool {
        let Some(c) = self.cell(i) else { return false };
        if c.get().as_accessor().is_some() {
            return false;
        }
        c.set(mc, v);
        true
    }

    #[inline]
    pub fn word(&self, i: usize) -> Option<Value<'gc>> {
        if self.dict.get().is_some() {
            return None;
        }
        Some(self.cell(i).map_or(Value::Undefined, ValueCell::get))
    }

    #[inline]
    pub fn slot(&self, i: usize) -> Slot<'gc> {
        match self.dict_cell() {
            Some(d) => d.borrow().slots[i],
            None => Slot::decode(self.cell(i).map_or(Value::Undefined, ValueCell::get)),
        }
    }

    pub fn get(&self, i: usize) -> Property<'gc> {
        if let Some(d) = self.dict_cell() {
            let d = d.borrow();
            return Property::from((d.slots[i], d.keys[i].1));
        }
        Property::from((self.slot(i), self.attrs(i)))
    }

    #[inline]
    pub fn get_if(&self, i: usize, key: &str) -> Option<Property<'gc>> {
        match self.dict_cell() {
            Some(d) => {
                let d = d.borrow();
                let (k, a) = d.keys.get(i)?;
                (**k == *key).then(|| Property::from((d.slots[i], *a)))
            }
            None => {
                let (k, a) = self.shape_ref()?.keys().get(i)?;
                (**k == *key).then(|| Property::from((self.slot(i), *a)))
            }
        }
    }

    pub fn lookup(&self, key: &str) -> Option<(usize, Property<'gc>)> {
        match self.dict_cell() {
            Some(d) => {
                let d = d.borrow();
                d.find(key).map(|i| (i, Property::from((d.slots[i], d.keys[i].1))))
            }
            None => self.shape_ref()?.find(key).map(|i| (i, self.get(i))),
        }
    }

    #[inline]
    pub fn set_slot(&self, mc: &Mutation<'gc>, i: usize, slot: Slot<'gc>) {
        match self.dict_cell() {
            Some(d) => d.borrow_mut(mc).slots[i] = slot,
            None => {
                if let Some(c) = self.cell(i) {
                    c.set(mc, slot.encode(mc));
                }
            }
        }
    }

    pub fn entries(&self) -> Vec<(SmallStr, Property<'gc>)> {
        (0..self.len()).map(|i| (self.key(i), self.get(i))).collect()
    }

    fn to_dict(&self) -> Dict<'gc> {
        let shape = self.shape_ref();
        let n = shape.map_or(0, Shape::len);
        let mut d = Dict {
            keys: (0..n).map(|i| (shape.expect("a fast shape").key(i).clone(), shape.expect("a fast shape").attrs(i))).collect(),
            index: None,
            slots: (0..n).map(|i| Slot::decode(self.cell(i).map_or(Value::Undefined, ValueCell::get))).collect(),
        };
        d.reindex();
        d
    }

    fn with_dict(&self, mc: &Mutation<'gc>, f: impl FnOnce(&mut Dict<'gc>)) {
        if self.dict.get().is_none() {
            let d = mc.alloc(GcCell::new(mc, self.to_dict()));
            self.dict.set(mc.mutator(), Some(d));
            self.shape.set(None);
        }
        f(&mut self.dict_cell().expect("just converted").borrow_mut(mc));
    }

    fn advance(&self, mc: &Mutation<'gc>, from: Option<ShapeRef<'gc>>, to: ShapeRef<'gc>, slot: Slot<'gc>) -> bool {
        let n = to.len() - 1;
        let v = slot.encode(mc);
        match n.checked_sub(INLINE) {
            None => self.inline[n].set(mc, v),
            Some(k) => {
                let cur = self.store();
                let grown = Store::put(cur, mc, k, k, v);
                if cur.is_none_or(|s| !Gc::ptr_eq(s.gc(), grown.gc())) {
                    self.store.set(mc.mutator(), Some(grown.gc()));
                }
            }
        }
        if self.shape.get() != from {
            return false;
        }
        self.shape.set(Some(to));
        true
    }

    pub fn push(&self, mc: &Mutation<'gc>, key: &str, prop: Property<'gc>, wide: bool, root: impl Fn() -> Option<ShapeRef<'gc>>) {
        let attrs = Attrs::from(&prop);
        while !self.is_dict() {
            let from = self.shape();
            let to = match from {
                Some(f) if !wide && f.len() >= MAX_FAST => break,
                Some(f) => f.child(mc, key, attrs),
                None => match root() {
                    Some(r) => r.child(mc, key, attrs),
                    None => Shape::rooted(mc, alloc::vec![(SmallStr::from(key), attrs)]),
                },
            };
            if self.advance(mc, from, to, prop.slot) {
                return;
            }
        }
        self.with_dict(mc, |d| {
            d.keys.push((SmallStr::from(key), attrs));
            d.slots.push(prop.slot);
            match &mut d.index {
                Some(m) => {
                    m.insert(SmallStr::from(key), u32::try_from(d.keys.len() - 1).expect("fewer than 2^32 properties"));
                }
                None => d.reindex(),
            }
        });
    }

    pub fn push_to(&self, mc: &Mutation<'gc>, to: ShapeRef<'gc>, slot: Slot<'gc>) {
        loop {
            let from = self.shape();
            debug_assert_eq!(to.len(), from.map_or(0, |f| f.len()) + 1);
            if self.advance(mc, from, to, slot) {
                return;
            }
        }
    }

    pub fn fill(&self, mc: &Mutation<'gc>, to: ShapeRef<'gc>, slots: impl IntoIterator<Item = Slot<'gc>>) {
        debug_assert!(self.shape_ref().is_none() && !self.is_dict());
        for (i, slot) in slots.into_iter().enumerate().take(to.len()) {
            let v = slot.encode(mc);
            match i.checked_sub(INLINE) {
                None => self.inline[i].set(mc, v),
                Some(k) => {
                    let cur = self.store();
                    let grown = Store::put(cur, mc, k, k, v);
                    if cur.is_none_or(|s| !Gc::ptr_eq(s.gc(), grown.gc())) {
                        self.store.set(mc.mutator(), Some(grown.gc()));
                    }
                }
            }
        }
        self.shape.set(Some(to));
    }

    pub fn replace(&self, mc: &Mutation<'gc>, i: usize, prop: Property<'gc>) {
        let attrs = Attrs::from(&prop);
        if self.attrs(i) == attrs {
            return self.set_slot(mc, i, prop.slot);
        }
        self.with_dict(mc, |d| {
            d.keys[i].1 = attrs;
            d.slots[i] = prop.slot;
        });
    }

    pub fn remove(&self, mc: &Mutation<'gc>, i: usize) {
        self.with_dict(mc, |d| {
            d.keys.remove(i);
            d.slots.remove(i);
            d.reindex();
        });
    }

    pub fn retain(&self, mc: &Mutation<'gc>, keep: impl Fn(&str) -> bool) {
        let gone: Vec<usize> = (0..self.len()).filter(|&i| !keep(&self.key(i))).collect();
        if gone.is_empty() {
            return;
        }
        self.with_dict(mc, |d| {
            for &i in gone.iter().rev() {
                d.keys.remove(i);
                d.slots.remove(i);
            }
            d.reindex();
        });
    }
}
