//! Shapes (hidden classes): the layout of an object's own string-keyed properties — keys and attributes, in insertion
//! order; the values are stored separately in the object's slots.
//!
//! Objects built the same way share shapes: adding a property follows the transition table to (or creates) a child
//! shape, and the execution layer keeps an inline cache keyed by "shape ⇒ slot index". Shapes contain no `Gc` pointers
//! (collection moves objects) and are managed with `Arc`; the transition table stores `Weak`, so shapes no object uses
//! are freed naturally. Deleting a property, changing attributes, or having too many keys copies out a dictionary
//! shape, owned exclusively by one object and modified in place; inline caches don't accept it.
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::hash::BuildHasherDefault;

use j8lockfree::SegVec;

use crate::Collect;
use rustc_hash::FxHasher;

use crate::{Property, SmallStr, Slot};

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
    /// Child shapes reached by adding a key (with attributes)
    transitions: SegVec<(SmallStr, Attrs, Weak<Shape>)>,
    /// The shape this one came from by adding a key: a child shape keeps the whole chain alive (objects only hold the
    /// final shape, and the intermediate ones must not be freed)
    _parent: Option<Arc<Shape>>,
    /// Dictionary shape: owned exclusively by one object, modified in place
    dict: bool,
}

impl Shape {
    fn new(keys: Vec<(SmallStr, Attrs)>, dict: bool) -> Self {
        let mut s = Self { keys, index: None, transitions: SegVec::default(), _parent: None, dict };
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

    /// Dictionary shapes don't go into inline caches (they are modified in place)
    pub fn is_dict(&self) -> bool {
        self.dict
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

    /// The shape after adding a key (shared: look up the transition table; create and register one if absent)
    fn child(self: &Arc<Self>, key: &str, attrs: Attrs) -> Arc<Self> {
        let t = &self.transitions;
        if let Some(c) = t.iter().find(|(k, a, _)| **k == *key && *a == attrs).and_then(|(_, _, w)| w.upgrade()) {
            return c;
        }
        let mut keys = Vec::with_capacity(self.keys.len() + 1);
        keys.extend(self.keys.iter().cloned());
        keys.push((SmallStr::from(key), attrs));
        let child = Arc::new(Self { _parent: Some(Arc::clone(self)), ..Self::new(keys, false) });
        // Append-only (other threads may be reading it); entries whose shape died stay as dead weak links
        t.push((SmallStr::from(key), attrs, Arc::downgrade(&child)));
        child
    }

    /// An exclusively owned, modifiable dictionary shape (used in place if already a dictionary and exclusively owned,
    /// otherwise copied)
    fn dict_mut(this: &mut Arc<Self>) -> &mut Self {
        if !this.dict || Arc::get_mut(this).is_none() {
            *this = Arc::new(Self::new(this.keys.clone(), true));
        }
        Arc::get_mut(this).expect("a fresh dictionary shape is unique")
    }
}

/// An object's string-keyed properties: shape (`None` when empty) plus slots in shape order.
#[derive(Default, Collect)]
#[collect(no_drop)]
pub struct Props<'gc> {
    #[collect(require_static)]
    shape: Option<Arc<Shape>>,
    slots: Vec<Slot<'gc>>,
}

impl<'gc> Props<'gc> {
    /// Empty, reserving slots for `n` properties
    pub fn with_capacity(n: usize) -> Self {
        Self { shape: None, slots: Vec::with_capacity(n) }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The current shape (inline caches identify it by address)
    pub fn shape(&self) -> Option<&Arc<Shape>> {
        self.shape.as_ref()
    }

    pub fn position(&self, key: &str) -> Option<usize> {
        self.shape.as_ref()?.find(key)
    }

    pub fn key(&self, i: usize) -> &SmallStr {
        self.shape.as_ref().expect("a property's shape").key(i)
    }

    pub fn attrs(&self, i: usize) -> Attrs {
        self.shape.as_ref().expect("a property's shape").attrs(i)
    }

    pub fn get(&self, i: usize) -> Property<'gc> {
        let a = self.attrs(i);
        Property { slot: self.slots[i], enumerable: a.enumerable(), writable: a.writable(), configurable: a.configurable() }
    }

    pub fn slot(&self, i: usize) -> Slot<'gc> {
        self.slots[i]
    }

    pub fn slot_mut(&mut self, i: usize) -> &mut Slot<'gc> {
        &mut self.slots[i]
    }

    /// (key, property) pairs in insertion order
    pub fn iter(&self) -> impl Iterator<Item = (&SmallStr, Property<'gc>)> {
        (0..self.len()).map(|i| (self.key(i), self.get(i)))
    }

    /// An empty shared shape (all objects with a given object as prototype start from it; see `ObjectData::shape_root`)
    pub fn empty() -> Arc<Shape> {
        Arc::new(Shape::new(Vec::new(), false))
    }

    /// Append a new key (the caller guarantees it isn't present); an empty object starts from `root` (if absent, it
    /// builds an unshared chain of its own)
    pub fn push(&mut self, key: &str, prop: Property<'gc>, root: impl FnOnce() -> Option<Arc<Shape>>) {
        let attrs = Attrs::from(&prop);
        if self.shape.is_none() {
            self.shape = root();
        }
        match &mut self.shape {
            None => self.shape = Some(Arc::new(Shape::new(alloc::vec![(SmallStr::from(key), attrs)], false))),
            Some(s) if s.dict || s.len() >= MAX_FAST => {
                let d = Shape::dict_mut(s);
                d.keys.push((SmallStr::from(key), attrs));
                if let Some(m) = &mut d.index {
                    m.insert(SmallStr::from(key), u32::try_from(d.keys.len() - 1).expect("fewer than 2^32 properties"));
                } else {
                    d.reindex();
                }
            }
            Some(s) => *s = s.child(key, attrs),
        }
        self.slots.push(prop.slot);
    }

    /// Append a new key, switching the shape directly to `to` (cached by the execution layer: adding this key to the
    /// current shape gives `to`)
    pub fn push_to(&mut self, to: Arc<Shape>, slot: Slot<'gc>) {
        debug_assert_eq!(to.len(), self.len() + 1);
        self.shape = Some(to);
        self.slots.push(slot);
    }

    /// The shared shape that adding a key would reach (the execution layer uses it to fill inline caches; `None` for
    /// dictionary shapes)
    pub fn transition(&self, key: &str, prop: &Property<'gc>) -> Option<Arc<Shape>> {
        let attrs = Attrs::from(prop);
        match &self.shape {
            None => None,
            Some(s) if s.dict || s.len() >= MAX_FAST => None,
            Some(s) => Some(s.child(key, attrs)),
        }
    }

    /// Replace entry `i` as a whole (converts to a dictionary shape if the attributes change)
    pub fn replace(&mut self, i: usize, prop: Property<'gc>) {
        let attrs = Attrs::from(&prop);
        if let Some(s) = &mut self.shape
            && s.attrs(i) != attrs
        {
            Shape::dict_mut(s).keys[i].1 = attrs;
        }
        self.slots[i] = prop.slot;
    }

    /// Delete entry `i` (converts to a dictionary shape)
    pub fn remove(&mut self, i: usize) {
        if let Some(s) = &mut self.shape {
            let d = Shape::dict_mut(s);
            d.keys.remove(i);
            d.reindex();
        }
        self.slots.remove(i);
    }

    /// Keep only those whose key satisfies `keep` (converts to a dictionary shape if anything is deleted)
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        let Some(s) = &self.shape else { return };
        let gone: Vec<usize> = (0..s.len()).filter(|&i| !keep(s.key(i))).collect();
        for &i in gone.iter().rev() {
            self.remove(i);
        }
    }
}
