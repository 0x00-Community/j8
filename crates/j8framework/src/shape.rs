//! Shapes (hidden classes): the layout of an object's own string-keyed properties — keys and attributes, in insertion
//! order; the values are stored separately in the object's slots.
//!
//! Objects built the same way share shapes: adding a property follows the transition table to (or creates) a child
//! shape, and the execution layer keeps an inline cache keyed by "shape ⇒ slot index". Shapes contain no `Gc` pointers
//! (collection moves objects) and are managed with `Arc`; the transition table stores `Weak`, so shapes no object uses
//! are freed naturally. Deleting a property, changing attributes, or having too many keys turns the object's properties
//! into a dictionary ([`Dict`], modified in place); inline caches don't accept it.
use alloc::sync::Arc;
use core::sync::atomic::{AtomicPtr, Ordering};
use alloc::vec::Vec;
use core::hash::BuildHasherDefault;

use j8lockfree::SegVec;

use crate::Collect;
use rustc_hash::FxHasher;

use crate::store::Store;
use crate::{Erased, Gc, GcCell, GcSlot, Mutation, Property, SmallStr, Slot, Value, ValueCell};

/// Up to this many keys, search linearly; beyond that, build an index
const LINEAR: usize = 8;
/// A shared shape has at most this many keys; adding more converts to a dictionary shape (overly long transition chains
/// aren't worth it)
const MAX_FAST: usize = 64;

/// Property attributes (whether it is data or an accessor depends on what the slot holds).
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

/// A shape.
pub struct Shape {
    keys: Vec<(SmallStr, Attrs)>,
    index: Option<hashbrown::HashMap<SmallStr, u32, BuildHasherDefault<FxHasher>>>,
    /// Child shapes reached by adding a key (with attributes), held strongly: a whole tree lives as long as its root
    /// (the root a prototype object hands out, or the anchor of an object that has none), so objects can point at
    /// their shape without counting references
    transitions: SegVec<(SmallStr, Attrs, Arc<Shape>)>,
}

impl Shape {
    fn new(keys: Vec<(SmallStr, Attrs)>) -> Self {
        let mut s = Self { keys, index: None, transitions: SegVec::default() };
        s.reindex();
        s
    }

    fn reindex(&mut self) {
        self.index = (self.keys.len() > LINEAR).then(|| self.keys.iter().zip(0u32..).map(|((k, _), i)| (k.clone(), i)).collect());
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn find(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(m) => m.get(key).map(|&i| i as usize),
            None => self.keys.iter().position(|(k, _)| **k == *key),
        }
    }

    pub fn key(&self, i: usize) -> &SmallStr {
        &self.keys[i].0
    }

    pub fn attrs(&self, i: usize) -> Attrs {
        self.keys[i].1
    }

    /// Whether `to` is this shape's child by a transition (so this shape's tree holds it)
    fn has_child(&self, to: &Arc<Shape>) -> bool {
        self.transitions.iter().any(|(_, _, c)| Arc::ptr_eq(c, to))
    }

    /// The shape after adding a key (shared: look up the transition table; create and register one if absent)
    fn child(self: &Arc<Self>, key: &str, attrs: Attrs) -> Arc<Self> {
        let t = &self.transitions;
        if let Some((_, _, c)) = t.iter().find(|(k, a, _)| **k == *key && *a == attrs) {
            return Arc::clone(c);
        }
        let mut keys = Vec::with_capacity(self.keys.len() + 1);
        keys.extend(self.keys.iter().cloned());
        keys.push((SmallStr::from(key), attrs));
        let child = Arc::new(Self::new(keys));
        // Append-only (other threads may be reading it; a racing thread may add a twin, which is harmless)
        t.push((SmallStr::from(key), attrs, Arc::clone(&child)));
        child
    }
}

/// The properties of a dictionary-mode object: keys, attributes and slots, modified in place (only through the object's
/// own [`GcCell`]; dictionary objects are rare, so their accesses may pay for the borrow).
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

/// Keeps a shape tree alive for an object whose tree no prototype root holds (see [`Props::shape`]).
#[derive(Collect)]
#[collect(require_static)]
struct Anchor(#[expect(dead_code, reason = "held only to keep the tree alive")] Arc<Shape>);

/// An object's string-keyed properties, read without borrowing anything. Fast mode: a shared shape plus a value store
/// (values change in place through their [`ValueCell`]s; adding a key grows the store first, then swaps in the child
/// shape). Dictionary mode: a [`Dict`] behind its own cell. Every change takes `&Mutation` (write barrier).
pub struct Props<'gc> {
    /// The current shape; null when empty or a dictionary. It owns no count: it lives in a tree held by `anchor`, or by
    /// the root shape of the prototype the object had when it got its first key (an object keeps its prototype alive;
    /// changing the prototype anchors the tree here)
    shape: AtomicPtr<Shape>,
    store: GcSlot<'gc, Erased>,
    dict: GcSlot<'gc, GcCell<'gc, Dict<'gc>>>,
    anchor: GcSlot<'gc, Anchor>,
}

impl Default for Props<'_> {
    fn default() -> Self {
        Self { shape: AtomicPtr::new(core::ptr::null_mut()), store: GcSlot::default(), dict: GcSlot::default(), anchor: GcSlot::default() }
    }
}

// SAFETY: the three slots are the edges (the shape is not on the heap)
unsafe impl crate::Trace for Props<'_> {
    fn trace(&self, t: &mut crate::Tracer<'_>) {
        self.store.trace(t);
        self.dict.trace(t);
        self.anchor.trace(t);
    }

    fn relocate(&mut self, t: &mut crate::Tracer<'_>) {
        self.store.relocate(t);
        self.dict.relocate(t);
        self.anchor.relocate(t);
    }
}

impl<'gc> Props<'gc> {
    /// Empty, with room for `n` values already (an object literal knows how many it gets)
    pub fn with_capacity(mc: &Mutation<'gc>, n: usize) -> Self {
        let store = (n > 0).then(|| Store::reserve(mc, n).gc());
        Self { store: GcSlot::from(store), ..Self::default() }
    }

    fn dict_cell(&self) -> Option<&'gc GcCell<'gc, Dict<'gc>>> {
        self.dict.get().map(Gc::get)
    }

    fn store(&self) -> Option<Store<'gc>> {
        // SAFETY: only ever set from `Store::gc`
        self.store.get().map(|g| unsafe { Store::from_gc(g) })
    }

    /// The current shared shape; `None` when empty or a dictionary
    #[inline]
    pub fn shape_ref(&self) -> Option<&'gc Shape> {
        // SAFETY: the shape's tree outlives this object's use of it (see `Props::shape`); `'gc` is bounded by this
        // mutate, during which the object (and so its prototype or anchor) is alive
        unsafe { self.shape.load(Ordering::Acquire).as_ref() }
    }

    /// The current shape's address (inline caches identify shapes by it); `None` when empty or a dictionary
    #[inline]
    pub fn shape_ptr(&self) -> Option<*const Shape> {
        self.shape_ref().map(core::ptr::from_ref)
    }

    /// The current shared shape, counted (to keep it beyond this mutate)
    pub fn shape(&self) -> Option<Arc<Shape>> {
        let p = self.shape_ptr()?;
        // SAFETY: tree shapes are `Arc` allocations, alive while this object uses them
        unsafe {
            Arc::increment_strong_count(p);
            Some(Arc::from_raw(p))
        }
    }

    /// Dictionary mode (inline caches don't accept it)
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
    pub fn slot(&self, i: usize) -> Slot<'gc> {
        match self.dict_cell() {
            Some(d) => d.borrow().slots[i],
            None => Slot::decode(self.store().and_then(|s| s.cell(i)).map_or(Value::Undefined, ValueCell::get)),
        }
    }

    pub fn get(&self, i: usize) -> Property<'gc> {
        let a = self.attrs(i);
        Property { slot: self.slot(i), enumerable: a.enumerable(), writable: a.writable(), configurable: a.configurable() }
    }

    /// Store the contents of entry `i` (attributes unchanged)
    #[inline]
    pub fn set_slot(&self, mc: &Mutation<'gc>, i: usize, slot: Slot<'gc>) {
        match self.dict_cell() {
            Some(d) => d.borrow_mut(mc).slots[i] = slot,
            None => {
                if let Some(c) = self.store().and_then(|s| s.cell(i)) {
                    c.set(mc, slot.encode(mc));
                }
            }
        }
    }

    /// (key, property) pairs in insertion order (a snapshot)
    pub fn entries(&self) -> Vec<(SmallStr, Property<'gc>)> {
        (0..self.len()).map(|i| (self.key(i), self.get(i))).collect()
    }

    /// An empty shared shape (all objects with a given object as prototype start from it; see `Object::root_shape`)
    pub fn empty() -> Arc<Shape> {
        Arc::new(Shape::new(Vec::new()))
    }

    /// A dictionary holding everything in fast mode now
    fn to_dict(&self) -> Dict<'gc> {
        let (shape, store) = (self.shape_ref(), self.store());
        let n = shape.map_or(0, Shape::len);
        let mut d = Dict {
            keys: (0..n).map(|i| (shape.expect("a fast shape").key(i).clone(), shape.expect("a fast shape").attrs(i))).collect(),
            index: None,
            slots: (0..n).map(|i| Slot::decode(store.and_then(|s| s.cell(i)).map_or(Value::Undefined, ValueCell::get))).collect(),
        };
        d.reindex();
        d
    }

    /// Change the dictionary with `f`, converting to one first if needed
    fn with_dict(&self, mc: &Mutation<'gc>, f: impl FnOnce(&mut Dict<'gc>)) {
        if self.dict.get().is_none() {
            let d = mc.alloc(GcCell::new(mc, self.to_dict()));
            // A racing converter may win; then its dictionary is the one
            let _ = self.dict.compare_exchange(mc.mutator(), None, Some(d));
            self.shape.store(core::ptr::null_mut(), Ordering::Release);
        }
        f(&mut self.dict_cell().expect("just converted").borrow_mut(mc));
    }

    /// Moves from shape `from` (`None`: empty) to its child `to`, storing `slot` in the new last cell; false when the
    /// shape changed meanwhile (the caller retries)
    fn advance(&self, mc: &Mutation<'gc>, from: Option<&Shape>, to: &Arc<Shape>, slot: Slot<'gc>) -> bool {
        let n = to.len() - 1;
        let cur = self.store();
        let grown = Store::put(cur, mc, n, n, slot.encode(mc));
        if cur.is_none_or(|s| !Gc::ptr_eq(s.gc(), grown.gc())) {
            self.store.set(mc.mutator(), Some(grown.gc()));
        }
        let from = from.map_or(core::ptr::null_mut(), |f| core::ptr::from_ref(f).cast_mut());
        self.shape.compare_exchange(from, Arc::as_ptr(to).cast_mut(), Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    /// Keep `tree` alive from here (no prototype root holds it)
    fn anchor(&self, mc: &Mutation<'gc>, tree: Arc<Shape>) {
        self.anchor.set(mc.mutator(), Some(mc.alloc(Anchor(tree))));
    }

    /// Append a new key (the caller guarantees it isn't present); an empty object starts from `root` (the prototype's
    /// root shape; without one it anchors a tree of its own)
    pub fn push(&self, mc: &Mutation<'gc>, key: &str, prop: Property<'gc>, root: impl Fn() -> Option<Arc<Shape>>) {
        let attrs = Attrs::from(&prop);
        while !self.is_dict() {
            let from = self.shape_ref();
            let to = match from {
                Some(f) if f.len() >= MAX_FAST => break,
                // `f` is in a tree held elsewhere, so a temporary count is enough to walk to the child
                Some(_) => self.shape().expect("a fast shape").child(key, attrs),
                None => match root() {
                    Some(r) => r.child(key, attrs),
                    None => {
                        let s = Arc::new(Shape::new(alloc::vec![(SmallStr::from(key), attrs)]));
                        self.anchor(mc, Arc::clone(&s));
                        s
                    }
                },
            };
            if self.advance(mc, from, &to, prop.slot) {
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

    /// Append a new key, switching the shape directly to `to` (cached by the execution layer: adding this key to the
    /// current shape gives `to`, a child in the current shape's tree)
    pub fn push_to(&self, mc: &Mutation<'gc>, to: &Arc<Shape>, slot: Slot<'gc>, root: impl Fn() -> Option<Arc<Shape>>) {
        loop {
            let from = self.shape_ref();
            debug_assert_eq!(to.len(), from.map_or(0, Shape::len) + 1);
            // An empty object: `to` is a first-key shape; held by the prototype's root, or anchored here
            if from.is_none() && !root().is_some_and(|r| r.has_child(to)) {
                self.anchor(mc, Arc::clone(to));
            }
            if self.advance(mc, from, to, slot) {
                return;
            }
        }
    }

    /// The prototype changed: anchor the current shape's tree here (the new prototype's root may not hold it)
    pub fn reanchor(&self, mc: &Mutation<'gc>) {
        if self.anchor.get().is_none()
            && let Some(s) = self.shape()
        {
            self.anchor(mc, s);
        }
    }

    /// The shared shape that adding a key would reach (the execution layer uses it to fill inline caches; `None` for
    /// dictionaries)
    pub fn transition(&self, key: &str, prop: &Property<'gc>) -> Option<Arc<Shape>> {
        let attrs = Attrs::from(prop);
        let s = self.shape()?;
        (s.len() < MAX_FAST).then(|| s.child(key, attrs))
    }

    /// Replace entry `i` as a whole (converts to a dictionary if the attributes change)
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

    /// Delete entry `i` (converts to a dictionary)
    pub fn remove(&self, mc: &Mutation<'gc>, i: usize) {
        self.with_dict(mc, |d| {
            d.keys.remove(i);
            d.slots.remove(i);
            d.reindex();
        });
    }

    /// Keep only those whose key satisfies `keep` (converts to a dictionary if anything is deleted)
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
