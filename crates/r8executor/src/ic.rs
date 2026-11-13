//! 属性读写的内联缓存：每处 `get.named` / `set.named` / `define.named` 一个。
//!
//! 只认形状地址（回收会搬对象，缓存里不能存对象指针），另持有 `Weak` 让认过的地址不被别的形状复用。
//! 字典形状会原地改（地址不变内容变），一律不缓存。
//!
//! - 读：接收者自有的数据属性，或往上第 1..=[`DEPTH`] 层原型上的（沿途各层形状都核对）
//! - 写：接收者自有、可写的数据属性
//! - 加属性：原形状 ⇒ 新形状；赋值的还要核对整条原型链各层的形状（链上没人中途加同名的 setter / 只读属性）
use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use r8framework::{JsObject, ObjectData, ObjectKind, Shape, Slot, Value};
use r8gc::Mutation;

/// 还没有形状的空对象在缓存里的地址（真的形状地址按 8 对齐，不会是它）
const EMPTY: usize = 1;
/// 最多缓存几层原型
const DEPTH: usize = 4;

/// 对象当前形状的地址
fn addr(d: &ObjectData<'_>) -> usize {
    d.props.shape().map_or(EMPTY, |s| Rc::as_ptr(s) as usize)
}

/// 自有属性全在形状里（名字也过了 [`Ic::name_ok`]）、形状不会原地改
fn plain(d: &ObjectData<'_>) -> bool {
    !matches!(d.kind, ObjectKind::Proxy(_) | ObjectKind::TypedArray(..) | ObjectKind::ModuleNamespace(_)) && d.props.shape().is_none_or(|s| !s.is_dict())
}

/// 改之前的样子（给加属性的缓存比对）。
#[derive(Clone, Copy)]
pub(crate) struct Before {
    shape: usize,
    len: usize,
    plain: bool,
}

impl From<JsObject<'_>> for Before {
    fn from(o: JsObject<'_>) -> Self {
        let d = o.borrow();
        Self { shape: addr(&d), len: d.props.len(), plain: plain(&d) }
    }
}

/// 一处属性读写的缓存。
#[derive(Default)]
pub(crate) struct Ic {
    /// 接收者的形状地址（0：没缓存）
    shape: Cell<usize>,
    /// 槽号
    slot: Cell<u32>,
    /// 读：往上第几层原型持有（0 是自有）；加属性：原型链有几层（核对到底）
    depth: Cell<u8>,
    /// 各层原型的形状地址
    chain: [Cell<usize>; DEPTH],
    /// 加属性：转移到的形状
    to: RefCell<Option<Rc<Shape>>>,
    /// 保住认过的形状地址
    keep: RefCell<Vec<Weak<Shape>>>,
}

impl Ic {
    /// 数组的 `length`、下标与像数的名字（类型化数组）走各自的特殊逻辑，不缓存
    fn name_ok(name: &str) -> bool {
        name != "length" && name != "NaN" && name != "Infinity" && !name.starts_with(|c: char| c.is_ascii_digit() || c == '-')
    }

    fn reset(&self) {
        self.shape.set(0);
        self.keep.borrow_mut().clear();
        *self.to.borrow_mut() = None;
    }

    fn keep(&self, d: &ObjectData<'_>) -> usize {
        if let Some(s) = d.props.shape() {
            self.keep.borrow_mut().push(Rc::downgrade(s));
        }
        addr(d)
    }

    /// 沿原型链核对 `depth`（≥ 1）层形状，给出最后一层
    fn holder<'gc>(&self, mut proto: Option<JsObject<'gc>>, depth: usize) -> Option<JsObject<'gc>> {
        let mut last = None;
        for c in &self.chain[..depth] {
            let p = proto?;
            let d = p.borrow();
            if addr(&d) != c.get() || !plain(&d) {
                return None;
            }
            proto = d.proto;
            last = Some(p);
        }
        last
    }

    /// 整条原型链（`depth` 层后到头）各层形状都没变
    fn chain_intact(&self, mut proto: Option<JsObject<'_>>, depth: usize) -> bool {
        for c in &self.chain[..depth] {
            let Some(p) = proto else { return false };
            let d = p.borrow();
            if addr(&d) != c.get() || !plain(&d) {
                return false;
            }
            proto = d.proto;
        }
        proto.is_none()
    }

    // ---- 读 ----

    #[inline]
    pub(crate) fn get<'gc>(&self, o: JsObject<'gc>) -> Option<Value<'gc>> {
        let want = self.shape.get();
        let d = o.borrow();
        if addr(&d) != want || !plain(&d) {
            return None;
        }
        let slot = self.slot.get() as usize;
        let holder = match self.depth.get() {
            0 => return Self::data(d.props.slot(slot)),
            n => self.holder(d.proto, usize::from(n))?,
        };
        drop(d);
        Self::data(holder.borrow().props.slot(slot))
    }

    fn data(s: Slot<'_>) -> Option<Value<'_>> {
        match s {
            Slot::Data(v) => Some(v),
            Slot::Accessor { .. } => None,
        }
    }

    /// 慢路读完后记下在哪找到的
    pub(crate) fn fill_get(&self, o: JsObject<'_>, name: &str) {
        self.reset();
        if !Self::name_ok(name) {
            return;
        }
        let d = o.borrow();
        if !plain(&d) {
            return;
        }
        let recv = self.keep(&d);
        if let Some(i) = d.props.position(name) {
            if matches!(d.props.slot(i), Slot::Data(_)) {
                self.set_slot(recv, i, 0);
            }
            return;
        }
        let mut proto = d.proto;
        drop(d);
        for (level, c) in self.chain.iter().enumerate() {
            let Some(p) = proto else { return };
            let d = p.borrow();
            if !plain(&d) {
                return;
            }
            c.set(self.keep(&d));
            if let Some(i) = d.props.position(name) {
                if matches!(d.props.slot(i), Slot::Data(_)) {
                    self.set_slot(recv, i, level + 1);
                }
                return;
            }
            proto = d.proto;
        }
    }

    fn set_slot(&self, shape: usize, slot: usize, depth: usize) {
        if let (Ok(slot), Ok(depth)) = (u32::try_from(slot), u8::try_from(depth)) {
            self.slot.set(slot);
            self.depth.set(depth);
            self.shape.set(shape);
        }
    }

    // ---- 写 / 加属性 ----

    /// 命中就写好返回 `true`
    #[inline]
    pub(crate) fn set<'gc>(&self, mc: &Mutation<'gc>, o: JsObject<'gc>, v: Value<'gc>, assign: bool) -> bool {
        let want = self.shape.get();
        let d = o.borrow();
        if addr(&d) != want || !plain(&d) {
            return false;
        }
        let to = self.to.borrow();
        match &*to {
            // 改自有的可写数据属性
            None => {
                let slot = self.slot.get() as usize;
                if !matches!(d.props.slot(slot), Slot::Data(_)) {
                    return false;
                }
                drop(d);
                o.borrow_mut(mc).set_at(slot, v);
                true
            }
            // 加属性：可扩展；赋值的还要整条原型链没变
            Some(to) => {
                if d.non_extensible || (assign && !self.chain_intact(d.proto, usize::from(self.depth.get()))) {
                    return false;
                }
                drop(d);
                o.borrow_mut(mc).push_to(Rc::clone(to), Slot::Data(v));
                true
            }
        }
    }

    /// 慢路写完后记下：改了自有属性，或加了一个属性（`assign`：赋值，要核对原型链）
    pub(crate) fn fill_set(&self, o: JsObject<'_>, name: &str, before: Before, assign: bool) {
        self.reset();
        if !Self::name_ok(name) || !before.plain {
            return;
        }
        let d = o.borrow();
        let Some(i) = d.props.position(name) else { return };
        if !plain(&d) || !matches!(d.props.slot(i), Slot::Data(_)) {
            return;
        }
        let now = self.keep(&d);
        if now == before.shape {
            if d.props.attrs(i).writable() {
                self.set_slot(now, i, 0);
            }
            return;
        }
        if d.props.len() != before.len + 1 || i != before.len {
            return;
        }
        let Some(to) = d.props.shape().cloned() else { return };
        if assign {
            let mut proto = d.proto;
            let mut depth = 0;
            while let Some(p) = proto {
                let Some(c) = self.chain.get(depth) else { return };
                let pd = p.borrow();
                if !plain(&pd) || pd.props.position(name).is_some() {
                    return;
                }
                c.set(self.keep(&pd));
                proto = pd.proto;
                depth += 1;
            }
            self.depth.set(u8::try_from(depth).unwrap_or(u8::MAX));
        }
        // 起始形状是 `to` 的父形状（子形状强引用父形状），`to` 活着它的地址就不会被复用
        *self.to.borrow_mut() = Some(to);
        self.shape.set(before.shape);
    }
}
