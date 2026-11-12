//! 形状（隐藏类）：对象自有字符串键属性的布局——键与特性，按加入顺序；值另存在对象的槽里。
//!
//! 同样方式建出来的对象共享形状：加属性沿转移表走到（或建出）子形状，执行层按「形状 ⇒ 槽号」做内联缓存。
//! 形状里没有 `Gc` 指针（回收会搬对象），用 `Rc` 管理；转移表存 `Weak`，没对象用的形状自然释放。
//! 删属性、改特性、键太多时复制出一份字典形状，归一个对象独占、原地改，内联缓存不收它。
use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::hash::BuildHasherDefault;

use r8gc::Collect;
use rustc_hash::FxHasher;

use crate::{Property, SmallStr, Slot};

/// 键不超过这么多就线性找，多了建索引
const LINEAR: usize = 8;
/// 共享形状最多这么多个键，再加就转成字典形状（转移链太长不划算）
const MAX_FAST: usize = 64;

/// 属性特性（数据还是访问器看槽里存的是什么）。
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

/// 一个形状。
pub struct Shape {
    keys: Vec<(SmallStr, Attrs)>,
    index: Option<hashbrown::HashMap<SmallStr, u32, BuildHasherDefault<FxHasher>>>,
    /// 加一个键（及特性）走到的子形状
    transitions: RefCell<Vec<(SmallStr, Attrs, Weak<Shape>)>>,
    /// 从哪个形状加一个键来的：子形状保住整条链（对象只持有最终形状，中间的不能被释放）
    _parent: Option<Rc<Shape>>,
    /// 字典形状：一个对象独占，原地改
    dict: bool,
}

impl Shape {
    fn new(keys: Vec<(SmallStr, Attrs)>, dict: bool) -> Self {
        let mut s = Self { keys, index: None, transitions: RefCell::default(), _parent: None, dict };
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

    /// 字典形状不进内联缓存（会原地改）
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

    /// 加一个键后的形状（共享：查转移表，没有就建并登记）
    fn child(self: &Rc<Self>, key: &str, attrs: Attrs) -> Rc<Self> {
        let mut t = self.transitions.borrow_mut();
        if let Some(c) = t.iter().find(|(k, a, _)| **k == *key && *a == attrs).and_then(|(_, _, w)| w.upgrade()) {
            return c;
        }
        let mut keys = Vec::with_capacity(self.keys.len() + 1);
        keys.extend(self.keys.iter().cloned());
        keys.push((SmallStr::from(key), attrs));
        let child = Rc::new(Self { _parent: Some(Rc::clone(self)), ..Self::new(keys, false) });
        t.retain(|(_, _, w)| w.strong_count() > 0);
        t.push((SmallStr::from(key), attrs, Rc::downgrade(&child)));
        child
    }

    /// 独占可改的字典形状（已经是字典且独占就原地用，否则复制一份）
    fn dict_mut(this: &mut Rc<Self>) -> &mut Self {
        if !this.dict || Rc::get_mut(this).is_none() {
            *this = Rc::new(Self::new(this.keys.clone(), true));
        }
        Rc::get_mut(this).expect("a fresh dictionary shape is unique")
    }
}

/// 一个对象的字符串键属性：形状（`None` 是空）加按形状顺序的槽。
#[derive(Default, Collect)]
#[collect(no_drop)]
pub struct Props<'gc> {
    #[collect(require_static)]
    shape: Option<Rc<Shape>>,
    slots: Vec<Slot<'gc>>,
}

impl<'gc> Props<'gc> {
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// 当前形状（内联缓存按它的地址认）
    pub fn shape(&self) -> Option<&Rc<Shape>> {
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

    /// 按加入顺序的（键, 属性）
    pub fn iter(&self) -> impl Iterator<Item = (&SmallStr, Property<'gc>)> {
        (0..self.len()).map(|i| (self.key(i), self.get(i)))
    }

    /// 一个空的共享形状（以某个对象为原型的对象都从它起步，见 `ObjectData::shape_root`）
    pub fn empty() -> Rc<Shape> {
        Rc::new(Shape::new(Vec::new(), false))
    }

    /// 追加一个新键（调用方保证原来没有）；空对象从 `root` 起步（没有就自己建一条不共享的）
    pub fn push(&mut self, key: &str, prop: Property<'gc>, root: impl FnOnce() -> Option<Rc<Shape>>) {
        let attrs = Attrs::from(&prop);
        if self.shape.is_none() {
            self.shape = root();
        }
        match &mut self.shape {
            None => self.shape = Some(Rc::new(Shape::new(alloc::vec![(SmallStr::from(key), attrs)], false))),
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

    /// 追加一个新键，形状直接换成 `to`（执行层缓存过：从当前形状加这个键就是 `to`）
    pub fn push_to(&mut self, to: Rc<Shape>, slot: Slot<'gc>) {
        debug_assert_eq!(to.len(), self.len() + 1);
        self.shape = Some(to);
        self.slots.push(slot);
    }

    /// 加一个键会走到的共享形状（执行层用它填内联缓存；字典形状给 `None`）
    pub fn transition(&self, key: &str, prop: &Property<'gc>) -> Option<Rc<Shape>> {
        let attrs = Attrs::from(prop);
        match &self.shape {
            None => None,
            Some(s) if s.dict || s.len() >= MAX_FAST => None,
            Some(s) => Some(s.child(key, attrs)),
        }
    }

    /// 整个替换第 `i` 项（特性变了就转成字典形状）
    pub fn replace(&mut self, i: usize, prop: Property<'gc>) {
        let attrs = Attrs::from(&prop);
        if let Some(s) = &mut self.shape
            && s.attrs(i) != attrs
        {
            Shape::dict_mut(s).keys[i].1 = attrs;
        }
        self.slots[i] = prop.slot;
    }

    /// 删第 `i` 项（转成字典形状）
    pub fn remove(&mut self, i: usize) {
        if let Some(s) = &mut self.shape {
            let d = Shape::dict_mut(s);
            d.keys.remove(i);
            d.reindex();
        }
        self.slots.remove(i);
    }

    /// 只留下键满足 `keep` 的（删了就转成字典形状）
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        let Some(s) = &self.shape else { return };
        let gone: Vec<usize> = (0..s.len()).filter(|&i| !keep(s.key(i))).collect();
        for &i in gone.iter().rev() {
            self.remove(i);
        }
    }
}
