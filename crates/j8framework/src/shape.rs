use alloc::vec::Vec;
use core::hash::BuildHasherDefault;

use j8lockfree::{AtomicRefCell, OnceBox};
use crate::{Handle, HandleSlot, Weak};

use crate::Collect;
use rustc_hash::FxHasher;

use crate::store::Store;
use crate::{Erased, ScopeLocal, GcCell, GcSlot, IsoCell, Mutation, Property, SmallStr, Slot, Value, ValueCell};

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

impl<'scope> From<(Slot<'scope>, Attrs)> for Property<'scope> {
    fn from((slot, a): (Slot<'scope>, Attrs)) -> Self {
        Self { slot, enumerable: a.enumerable(), writable: a.writable(), configurable: a.configurable() }
    }
}

pub type ShapeRef<'scope> = Handle<'scope, Shape<'scope>>;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ShapeKey(usize);

impl ShapeKey {
    pub fn id(self) -> usize {
        self.0
    }

    pub unsafe fn shape<'a>(self) -> &'a Shape<'a> {
        unsafe { Handle::<'a, Shape<'a>>::from_id(self.0) }.get().get()
    }

    pub unsafe fn handle<'scope>(self) -> ShapeRef<'scope> {
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

paragc::brand!(pub ShapeBrand = for<'heap, 'scope> Shape<'scope>);

type Transition<'scope> = (SmallStr, Attrs, Weak<'scope, ShapeBrand>);

pub struct ShapeSide<'scope> {
    transitions: AtomicRefCell<Vec<Transition<'scope>>>,
    index: OnceBox<hashbrown::HashMap<SmallStr, u32, BuildHasherDefault<FxHasher>>>,
}

unsafe impl crate::Trace for ShapeSide<'_> {
    const NO_EDGES: bool = true;

    fn trace(&self, _: &mut crate::Tracer<'_>) {}
}

#[repr(C)]
pub struct Shape<'scope> {
    this: IsoCell<usize>,
    side: GcSlot<'scope, ShapeSide<'scope>>,
    len: u32,
    bytes: u32,
    searches: IsoCell<u32>,
    digit_keys: u32,
}

const _: () = assert!(core::mem::size_of::<Shape<'static>>() % 8 == 0, "the key table starts right after the shape");

unsafe impl crate::Trace for Shape<'_> {
    fn trace(&self, t: &mut crate::Tracer<'_>) {
        self.side.trace(t);
    }

    fn relocate(&mut self, t: &mut crate::Tracer<'_>) {
        self.side.relocate(t);
    }
}

#[cfg(all(target_os = "linux", feature = "crossheap"))]
unsafe impl paragc::Shared for Shape<'_> {
    fn edges(map: &mut paragc::EdgeMap, at: u32) {
        <GcSlot<'_, ShapeSide<'_>> as paragc::Shared>::edges(map, at + paragc::EdgeMap::offset(core::mem::offset_of!(Shape<'static>, side)));
    }
}

const KEY_END: usize = core::mem::size_of::<u32>();

impl<'scope> Shape<'scope> {
    fn tail(&self) -> *const u8 {
        (&raw const *self).wrapping_add(1).cast()
    }

    fn ends(&self) -> &[u32] {
        #[expect(clippy::cast_ptr_alignment, reason = "the tail starts at an 8-byte boundary")]
        unsafe {
            core::slice::from_raw_parts(self.tail().cast::<u32>(), self.len as usize)
        }
    }

    fn attr_bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.tail().add(KEY_END * self.len as usize), self.len as usize) }
    }

    fn text(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.tail().add((KEY_END + 1) * self.len as usize), self.bytes as usize) }
    }

    fn build<'k>(mc: &Mutation<'scope>, keys: impl Iterator<Item = (&'k str, Attrs)> + Clone) -> ShapeRef<'scope> {
        let n = |x: usize| u32::try_from(x).expect("a shape is smaller than 4 GiB");
        let (len, bytes) = keys.clone().fold((0, 0), |(l, b), (k, _)| (l + 1, b + k.len()));
        let digit_keys = keys.clone().any(|(k, _)| k.as_bytes().first().is_some_and(u8::is_ascii_digit));
        let s = Self { this: IsoCell::new(0), side: GcSlot::default(), len: n(len), bytes: n(bytes), searches: IsoCell::new(0), digit_keys: u32::from(digit_keys) };
        let fill = |t: &mut [core::mem::MaybeUninit<u8>]| {
            let (ends, rest) = t.split_at_mut(KEY_END * len);
            let (attrs, text) = rest.split_at_mut(len);
            let mut end = 0;
            for (i, (k, a)) in keys.enumerate() {
                text[end..end + k.len()].iter_mut().zip(k.as_bytes()).for_each(|(d, &b)| _ = d.write(b));
                end += k.len();
                ends[KEY_END * i..KEY_END * (i + 1)].iter_mut().zip(n(end).to_ne_bytes()).for_each(|(d, b)| _ = d.write(b));
                attrs[i].write(a.0);
            }
        };
        let tail = (KEY_END + 1) * len + bytes;
        #[cfg(all(target_os = "linux", feature = "crossheap"))]
        let h = mc.gc_scope().alloc_shared_handle_tail(s, tail, fill);
        #[cfg(not(all(target_os = "linux", feature = "crossheap")))]
        let h = mc.gc_scope().alloc_handle_tail(s, tail, fill);
        h.this.set(h.id());
        if len > LINEAR {
            h.get().side_or_new(mc);
        }
        h
    }

    pub fn handle(&self, _: &Mutation<'scope>) -> ShapeRef<'scope> {
        unsafe { Handle::from_id(self.this.get()) }
    }

    pub fn id(&self) -> usize {
        self.this.get()
    }

    pub fn digit_keys(&self) -> bool {
        self.digit_keys != 0
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn keys(&self) -> impl Iterator<Item = (&str, Attrs)> + Clone + '_ {
        (0..self.len()).map(|i| (self.key(i), self.attrs(i)))
    }

    fn side(&self) -> Option<&ShapeSide<'scope>> {
        self.side.get().map(|s| ScopeLocal::get(s))
    }

    fn side_or_new(&self, mc: &Mutation<'scope>) -> &ShapeSide<'scope> {
        if let Some(s) = self.side() {
            return s;
        }
        let s = mc.alloc(ShapeSide { transitions: AtomicRefCell::new(Vec::new()), index: OnceBox::default() });
        self.side.set(mc.gc_scope(), Some(s));
        ScopeLocal::get(s)
    }

    pub fn find(&self, key: &str) -> Option<usize> {
        if let Some(m) = self.side().and_then(|s| s.index.get()) {
            return m.get(key).map(|&i| i as usize);
        }
        if self.len() > LINEAR
            && let Some(side) = self.side()
            && {
                let n = self.searches.get();
                self.searches.set(n.saturating_add(1));
                n >= INDEX_AFTER
            }
        {
            let m = side.index.get_or_init(|| self.keys().zip(0u32..).map(|((k, _), i)| (SmallStr::from(k), i)).collect());
            return m.get(key).map(|&i| i as usize);
        }
        self.keys().position(|(k, _)| k == key)
    }

    pub fn key(&self, i: usize) -> &str {
        let ends = self.ends();
        let start = if i == 0 { 0 } else { ends[i - 1] as usize };
        unsafe { core::str::from_utf8_unchecked(&self.text()[start..ends[i] as usize]) }
    }

    pub fn attrs(&self, i: usize) -> Attrs {
        Attrs(self.attr_bytes()[i])
    }

    pub fn rooted(mc: &Mutation<'scope>, keys: Vec<(SmallStr, Attrs)>) -> ShapeRef<'scope> {
        Self::build(mc, keys.iter().map(|(k, a)| (&**k, *a)))
    }

    pub fn empty(mc: &Mutation<'scope>) -> ShapeRef<'scope> {
        Self::build(mc, core::iter::empty())
    }

    pub fn child(&self, mc: &Mutation<'scope>, key: &str, attrs: Attrs) -> ShapeRef<'scope> {
        let mut ts = self.side_or_new(mc).transitions.borrow_mut();
        let mut found = None;
        ts.retain(|(k, a, w)| match mc.upgrade(w) {
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
        let child = Self::build(mc, self.keys().chain(core::iter::once((key, attrs))));
        ts.push((SmallStr::from(key), attrs, mc.weak::<ShapeBrand>(child.get())));
        child
    }
}

#[derive(Collect)]
pub struct Dict<'scope> {
    #[collect(require_static)]
    keys: Vec<(SmallStr, Attrs)>,
    #[collect(require_static)]
    index: Option<hashbrown::HashMap<SmallStr, u32, BuildHasherDefault<FxHasher>>>,
    slots: Vec<Slot<'scope>>,
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

pub struct Props<'scope> {
    shape: HandleSlot<'scope, Shape<'scope>>,
    store: GcSlot<'scope, Erased>,
    dict: GcSlot<'scope, GcCell<'scope, Dict<'scope>>>,
    inline: [ValueCell<'scope>; INLINE],
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

impl<'scope> Props<'scope> {
    pub const SHAPE_OFFSET: usize = core::mem::offset_of!(Props<'static>, shape);
    pub const STORE_OFFSET: usize = core::mem::offset_of!(Props<'static>, store);
    pub const DICT_OFFSET: usize = core::mem::offset_of!(Props<'static>, dict);
    pub const INLINE_OFFSET: usize = core::mem::offset_of!(Props<'static>, inline);
    pub const INLINE: usize = INLINE;

    pub fn with_capacity(mc: &Mutation<'scope>, n: usize) -> Self {
        let store = (n > INLINE).then(|| Store::reserve(mc, n - INLINE).gc());
        Self { store: GcSlot::from(store), ..Self::default() }
    }

    fn dict_cell(&self) -> Option<&'scope GcCell<'scope, Dict<'scope>>> {
        self.dict.get().map(ScopeLocal::get)
    }

    fn store(&self) -> Option<Store<'scope>> {
        self.store.get().map(|g| unsafe { Store::from_gc(g) })
    }

    pub fn store_path(&self, i: usize) -> Option<crate::store::StorePath> {
        self.store()?.path(i.checked_sub(INLINE)?)
    }

    #[inline]
    fn cell(&self, i: usize) -> Option<&ValueCell<'scope>> {
        match i.checked_sub(INLINE) {
            None => Some(&self.inline[i]),
            Some(k) => self.store()?.cell(k),
        }
    }

    #[inline]
    pub fn shape_ref(&self) -> Option<&'scope Shape<'scope>> {
        self.shape.get().map(|h| h.get().get())
    }

    #[inline]
    pub fn shape_id(&self) -> Option<usize> {
        self.shape.get().map(Handle::id)
    }

    #[inline]
    pub fn shape(&self) -> Option<ShapeRef<'scope>> {
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
            None => SmallStr::from(self.shape_ref().expect("a property's shape").key(i)),
        }
    }

    pub fn attrs(&self, i: usize) -> Attrs {
        match self.dict_cell() {
            Some(d) => d.borrow().keys[i].1,
            None => self.shape_ref().expect("a property's shape").attrs(i),
        }
    }

    #[inline]
    pub fn read_data(&self, i: usize) -> Option<Value<'scope>> {
        let v = self.cell(i)?.get();
        v.is_plain_slot().then_some(v)
    }

    #[inline]
    pub fn write_data(&self, mc: &Mutation<'scope>, i: usize, v: Value<'scope>) -> bool {
        let Some(c) = self.cell(i) else { return false };
        if c.get().as_accessor().is_some() {
            return false;
        }
        c.set(mc, v);
        true
    }

    #[inline]
    pub fn word(&self, i: usize) -> Option<Value<'scope>> {
        if self.dict.get().is_some() {
            return None;
        }
        Some(self.cell(i).map_or(Value::Undefined, ValueCell::get))
    }

    #[inline]
    pub fn same_words(&self, slots: &[(u16, Value<'scope>)]) -> bool {
        if self.dict.get().is_some() {
            return false;
        }
        let mut store = None;
        let mut loaded = false;
        for &(i, orig) in slots {
            let got = match usize::from(i).checked_sub(INLINE) {
                None => self.inline[usize::from(i)].get(),
                Some(k) => {
                    if !loaded {
                        store = self.store();
                        loaded = true;
                    }
                    let Some(cell) = store.and_then(|s| s.cell(k)) else { return false };
                    cell.get()
                }
            };
            if got.bits() != orig.bits() {
                return false;
            }
        }
        true
    }

    #[inline]
    pub fn slot(&self, i: usize) -> Slot<'scope> {
        match self.dict_cell() {
            Some(d) => d.borrow().slots[i],
            None => Slot::decode(self.cell(i).map_or(Value::Undefined, ValueCell::get)),
        }
    }

    pub fn get(&self, i: usize) -> Property<'scope> {
        if let Some(d) = self.dict_cell() {
            let d = d.borrow();
            return Property::from((d.slots[i], d.keys[i].1));
        }
        Property::from((self.slot(i), self.attrs(i)))
    }

    #[inline]
    pub fn get_if(&self, i: usize, key: &str) -> Option<Property<'scope>> {
        match self.dict_cell() {
            Some(d) => {
                let d = d.borrow();
                let (k, a) = d.keys.get(i)?;
                (**k == *key).then(|| Property::from((d.slots[i], *a)))
            }
            None => {
                let shape = self.shape_ref()?;
                (i < shape.len() && shape.key(i) == key).then(|| Property::from((self.slot(i), shape.attrs(i))))
            }
        }
    }

    pub fn lookup(&self, key: &str) -> Option<(usize, Property<'scope>)> {
        match self.dict_cell() {
            Some(d) => {
                let d = d.borrow();
                d.find(key).map(|i| (i, Property::from((d.slots[i], d.keys[i].1))))
            }
            None => self.shape_ref()?.find(key).map(|i| (i, self.get(i))),
        }
    }

    #[inline]
    pub fn set_slot(&self, mc: &Mutation<'scope>, i: usize, slot: Slot<'scope>) {
        match self.dict_cell() {
            Some(d) => d.borrow_mut(mc).slots[i] = slot,
            None => {
                if let Some(c) = self.cell(i) {
                    c.set(mc, slot.encode(mc));
                }
            }
        }
    }

    pub fn entries(&self) -> Vec<(SmallStr, Property<'scope>)> {
        (0..self.len()).map(|i| (self.key(i), self.get(i))).collect()
    }

    fn to_dict(&self) -> Dict<'scope> {
        let shape = self.shape_ref();
        let n = shape.map_or(0, Shape::len);
        let mut d = Dict {
            keys: (0..n).map(|i| (SmallStr::from(shape.expect("a fast shape").key(i)), shape.expect("a fast shape").attrs(i))).collect(),
            index: None,
            slots: (0..n).map(|i| Slot::decode(self.cell(i).map_or(Value::Undefined, ValueCell::get))).collect(),
        };
        d.reindex();
        d
    }

    fn with_dict(&self, mc: &Mutation<'scope>, f: impl FnOnce(&mut Dict<'scope>)) {
        if self.dict.get().is_none() {
            let d = mc.alloc(GcCell::new(mc, self.to_dict()));
            self.dict.set(mc.gc_scope(), Some(d));
            self.shape.set(None);
        }
        f(&mut self.dict_cell().expect("just converted").borrow_mut(mc));
    }

    fn advance(&self, mc: &Mutation<'scope>, from: Option<ShapeRef<'scope>>, to: ShapeRef<'scope>, slot: Slot<'scope>) -> bool {
        let n = to.len() - 1;
        let v = slot.encode(mc);
        match n.checked_sub(INLINE) {
            None => self.inline[n].set(mc, v),
            Some(k) => {
                let cur = self.store();
                let grown = Store::put(cur, mc, k, k, v);
                if cur.is_none_or(|s| !ScopeLocal::ptr_eq(s.gc(), grown.gc())) {
                    self.store.set(mc.gc_scope(), Some(grown.gc()));
                }
            }
        }
        if self.shape.get() != from {
            return false;
        }
        self.shape.set(Some(to));
        true
    }

    pub fn push(&self, mc: &Mutation<'scope>, key: &str, prop: Property<'scope>, wide: bool, root: impl Fn() -> Option<ShapeRef<'scope>>) {
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

    pub fn push_to(&self, mc: &Mutation<'scope>, to: ShapeRef<'scope>, slot: Slot<'scope>) {
        loop {
            let from = self.shape();
            debug_assert_eq!(to.len(), from.map_or(0, |f| f.len()) + 1);
            if self.advance(mc, from, to, slot) {
                return;
            }
        }
    }

    pub fn fill(&self, mc: &Mutation<'scope>, to: ShapeRef<'scope>, slots: impl IntoIterator<Item = Slot<'scope>>) {
        debug_assert!(self.shape_ref().is_none() && !self.is_dict());
        for (i, slot) in slots.into_iter().enumerate().take(to.len()) {
            let v = slot.encode(mc);
            match i.checked_sub(INLINE) {
                None => self.inline[i].set(mc, v),
                Some(k) => {
                    let cur = self.store();
                    let grown = Store::put(cur, mc, k, k, v);
                    if cur.is_none_or(|s| !ScopeLocal::ptr_eq(s.gc(), grown.gc())) {
                        self.store.set(mc.gc_scope(), Some(grown.gc()));
                    }
                }
            }
        }
        self.shape.set(Some(to));
    }

    pub fn replace(&self, mc: &Mutation<'scope>, i: usize, prop: Property<'scope>) {
        let attrs = Attrs::from(&prop);
        if self.attrs(i) == attrs {
            return self.set_slot(mc, i, prop.slot);
        }
        self.with_dict(mc, |d| {
            d.keys[i].1 = attrs;
            d.slots[i] = prop.slot;
        });
    }

    pub fn remove(&self, mc: &Mutation<'scope>, i: usize) {
        self.with_dict(mc, |d| {
            d.keys.remove(i);
            d.slots.remove(i);
            d.reindex();
        });
    }

    pub fn retain(&self, mc: &Mutation<'scope>, keep: impl Fn(&str) -> bool) {
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
