use alloc::sync::Arc;
use alloc::vec::Vec;
use core::hash::BuildHasherDefault;

use j8lockfree::{Chain, OnceBox, Prefix};

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

pub struct Shape {
    keys: Arc<Prefix<(SmallStr, Attrs)>>,
    len: usize,
    index: OnceBox<hashbrown::HashMap<SmallStr, u32, BuildHasherDefault<FxHasher>>>,
    searches: IsoCell<u32>,
    transitions: Chain<(SmallStr, Attrs, Arc<Shape>)>,
    tree: IsoCell<*const Shape>,
    digit_keys: bool,
}

impl Shape {
    fn new(keys: Arc<Prefix<(SmallStr, Attrs)>>, len: usize) -> Self {
        let digit_keys = keys.prefix(len).iter().any(|(k, _)| k.as_bytes().first().is_some_and(u8::is_ascii_digit));
        Self { keys, len, index: OnceBox::default(), searches: IsoCell::new(0), transitions: Chain::default(), tree: IsoCell::new(core::ptr::null()), digit_keys }
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

    pub fn in_tree(&self, root: &Shape) -> bool {
        core::ptr::eq(self.tree.get(), root)
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

    fn has_child(&self, to: &Shape) -> bool {
        to.len() == self.len() + 1 && to.tree.get() == self.tree.get()
    }

    fn rooted(keys: Vec<(SmallStr, Attrs)>) -> Arc<Self> {
        let len = keys.len();
        let s = Arc::new(Self::new(Prefix::new(keys), len));
        s.tree.set(Arc::as_ptr(&s));
        s
    }

    fn child_ref(&self, key: &str, attrs: Attrs) -> &Shape {
        if let Some((_, _, c)) = self.transitions.iter().find(|(k, a, _)| **k == *key && *a == attrs) {
            return c;
        }
        let keys = self.keys.extend(self.len, (SmallStr::from(key), attrs));
        let child = Arc::new(Self { tree: IsoCell::new(self.tree.get()), ..Self::new(keys, self.len + 1) });
        &self.transitions.push((SmallStr::from(key), attrs, child)).2
    }

    fn child(&self, key: &str, attrs: Attrs) -> Arc<Self> {
        let c = self.child_ref(key, attrs);
        unsafe {
            Arc::increment_strong_count(c);
            Arc::from_raw(c)
        }
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

#[derive(Collect)]
#[collect(require_static)]
pub struct Anchor(pub Arc<Shape>);

pub struct Props<'gc> {
    shape: IsoCell<*const Shape>,
    store: GcSlot<'gc, Erased>,
    dict: GcSlot<'gc, GcCell<'gc, Dict<'gc>>>,
    anchor: GcSlot<'gc, Anchor>,
    inline: [ValueCell<'gc>; INLINE],
}

impl Default for Props<'_> {
    fn default() -> Self {
        Self {
            shape: IsoCell::new(core::ptr::null()),
            store: GcSlot::default(),
            dict: GcSlot::default(),
            anchor: GcSlot::default(),
            inline: core::array::from_fn(|_| ValueCell::new(Value::Undefined)),
        }
    }
}

unsafe impl crate::Trace for Props<'_> {
    fn trace(&self, t: &mut crate::Tracer<'_>) {
        self.store.trace(t);
        self.dict.trace(t);
        self.anchor.trace(t);
        self.inline.iter().for_each(|c| c.trace(t));
    }

    fn relocate(&mut self, t: &mut crate::Tracer<'_>) {
        self.store.relocate(t);
        self.dict.relocate(t);
        self.anchor.relocate(t);
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
        unsafe { self.shape.get().as_ref() }
    }

    #[inline]
    pub fn shape_ptr(&self) -> Option<*const Shape> {
        self.shape_ref().map(core::ptr::from_ref)
    }

    pub fn shape(&self) -> Option<Arc<Shape>> {
        let p = self.shape_ptr()?;
        unsafe {
            Arc::increment_strong_count(p);
            Some(Arc::from_raw(p))
        }
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

    pub fn empty() -> Arc<Shape> {
        Shape::rooted(Vec::new())
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
            self.shape.set(core::ptr::null());
        }
        f(&mut self.dict_cell().expect("just converted").borrow_mut(mc));
    }

    fn advance(&self, mc: &Mutation<'gc>, from: Option<&Shape>, to: &Shape, slot: Slot<'gc>) -> bool {
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
        let from = from.map_or(core::ptr::null(), core::ptr::from_ref);
        if self.shape.get() != from {
            return false;
        }
        self.shape.set(core::ptr::from_ref(to));
        true
    }

    fn anchor(&self, mc: &Mutation<'gc>, tree: Arc<Shape>) {
        self.anchor.set(mc.mutator(), Some(mc.alloc(Anchor(tree))));
    }

    pub fn push<'r>(&self, mc: &Mutation<'gc>, key: &str, prop: Property<'gc>, wide: bool, root: impl Fn() -> Option<&'r Shape>)
    where
        'gc: 'r,
    {
        let attrs = Attrs::from(&prop);
        while !self.is_dict() {
            let from = self.shape_ref();
            let anchored;
            let to: &Shape = match from {
                Some(f) if !wide && f.len() >= MAX_FAST => break,
                Some(f) => f.child_ref(key, attrs),
                None => match root() {
                    Some(r) => r.child_ref(key, attrs),
                    None => {
                        anchored = Shape::rooted(alloc::vec![(SmallStr::from(key), attrs)]);
                        self.anchor(mc, Arc::clone(&anchored));
                        &anchored
                    }
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

    pub fn push_to<'r>(&self, mc: &Mutation<'gc>, to: &Arc<Shape>, slot: Slot<'gc>, root: impl Fn() -> Option<&'r Shape>)
    where
        'gc: 'r,
    {
        loop {
            let from = self.shape_ref();
            debug_assert_eq!(to.len(), from.map_or(0, Shape::len) + 1);
            if from.is_none() && !root().is_some_and(|r| r.has_child(to)) {
                self.anchor(mc, Arc::clone(to));
            }
            if self.advance(mc, from, to, slot) {
                return;
            }
        }
    }

    pub fn fill<'r>(&self, mc: &Mutation<'gc>, to: &Arc<Shape>, slots: impl IntoIterator<Item = Slot<'gc>>, root: impl Fn() -> Option<&'r Shape>)
    where
        'gc: 'r,
    {
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
        if !root().is_some_and(|r| to.in_tree(r)) {
            self.anchor(mc, Arc::clone(to));
        }
        self.shape.set(Arc::as_ptr(to));
    }

    pub fn reanchor(&self, mc: &Mutation<'gc>) {
        if self.anchor.get().is_none()
            && let Some(s) = self.shape()
        {
            self.anchor(mc, s);
        }
    }

    pub fn transition(&self, key: &str, prop: &Property<'gc>) -> Option<Arc<Shape>> {
        let attrs = Attrs::from(prop);
        let s = self.shape()?;
        (s.len() < MAX_FAST).then(|| s.child(key, attrs))
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
