//! r8builtins：[`Intrinsics`] 的实现，往新建的 [`Realm`] 里装内建对象（`no_std`）。
#![no_std]
#![expect(clippy::unnecessary_wraps, reason = "宿主函数的签名统一是 NativeFn，不能因为某个不会失败就去掉 Result")]
extern crate alloc;

mod array;
mod error;
mod global;
mod iter;
mod math;
mod object;
mod promise;
mod string;

use gc_arena::Mutation;
use r8frame::{Intrinsics, JsObject, Realm, Value};

/// 缺省的内建对象集合。
#[derive(Debug, Default, Clone, Copy)]
pub struct Builtins;

impl Intrinsics for Builtins {
    fn install<'gc>(&self, mc: &Mutation<'gc>, realm: &Realm<'gc>) {
        let mut b = Installer { mc, realm };
        b.global();
        b.object();
        b.function();
        b.array();
        b.string();
        b.number();
        b.math();
        b.errors();
        b.iterators();
        b.promise();
    }
}

/// 装内建对象时的上下文。
struct Installer<'r, 'gc> {
    mc: &'r Mutation<'gc>,
    realm: &'r Realm<'gc>,
}

impl<'gc> Installer<'_, 'gc> {
    /// 内建属性：不可枚举。
    fn set(&self, target: JsObject<'gc>, name: &str, v: Value<'gc>) {
        target.borrow_mut(self.mc).define(self.mc, name, r8frame::Property::hidden(v));
    }

    fn method(&self, target: JsObject<'gc>, name: &str, f: r8frame::NativeFn) {
        self.realm.method(self.mc, target, name, f);
    }

    /// 全局对象上的一个构造器：宿主函数，`prototype` ⇄ `constructor` 互指。
    fn constructor(&self, name: &str, f: r8frame::NativeFn, proto: JsObject<'gc>) -> JsObject<'gc> {
        let ctor = self.realm.native(self.mc, f);
        self.set(ctor, "prototype", proto.into());
        self.set(proto, "constructor", ctor.into());
        self.set(self.realm.global, name, ctor.into());
        ctor
    }

    /// 宿主 getter（不可枚举）。
    fn getter(&self, target: JsObject<'gc>, name: &str, f: r8frame::NativeFn) {
        let g = self.realm.native(self.mc, f);
        target.borrow_mut(self.mc).define(self.mc, name, r8frame::Property { slot: r8frame::Slot::Accessor { get: Some(g), set: None }, enumerable: false, writable: false, configurable: true });
    }

    /// 堆上的字符串值。
    fn text(&self, s: &str) -> Value<'gc> {
        Value::String(gc_arena::Gc::new(self.mc, alloc::boxed::Box::from(s)))
    }

    /// 一个普通对象（原型 `Object.prototype`）。
    fn plain(&self) -> JsObject<'gc> {
        r8frame::ObjectData { proto: Some(self.realm.object_proto), ..r8frame::ObjectData::default() }.alloc(self.mc)
    }
}

/// 取实参，缺了就是 `undefined`。
trait Args<'gc> {
    fn at(&self, i: usize) -> Value<'gc>;
}

impl<'gc> Args<'gc> for [Value<'gc>] {
    fn at(&self, i: usize) -> Value<'gc> {
        self.get(i).copied().unwrap_or_default()
    }
}
