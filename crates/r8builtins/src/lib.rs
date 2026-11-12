//! r8builtins：[`Intrinsics`] 的实现，往新建的 [`Realm`] 里装内建对象（`no_std`）。
#![no_std]
#![expect(clippy::unnecessary_wraps, reason = "宿主函数的签名统一是 NativeFn，不能因为某个不会失败就去掉 Result")]
extern crate alloc;

mod array;
mod asyncgen;
mod bigint;
mod buffer;
mod collections;
mod date;
mod error;
mod function;
mod global;
mod helpers;
mod iter;
mod json;
mod math;
mod object;
mod promise;
mod reflect;
mod regexp;
mod string;
mod text;
mod typed;
mod weak;

use r8gc::Mutation;
use r8framework::{Intrinsics, JsObject, Realm, Value};

/// 缺省的内建对象集合。
#[derive(Debug, Default, Clone, Copy)]
pub struct Builtins;

impl Intrinsics for Builtins {
    fn install<'gc>(&self, mc: &Mutation<'gc>, realm: &Realm<'gc>) {
        let mut b = Installer { mc, realm };
        b.global();
        b.object();
        b.functions();
        b.function();
        b.array();
        b.string();
        b.number();
        b.math();
        b.errors();
        b.iterators();
        b.helpers();
        b.promise();
        b.bigint();
        b.regexp();
        b.collections();
        b.async_generators();
        b.json();
        b.buffers();
        b.date();
        b.texts();
        b.weaks();
        b.typed_arrays();
        b.reflect();
        b.tags();
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
        target.borrow_mut(self.mc).define(name, r8framework::Property::hidden(v));
    }

    /// 宿主方法，`length` 是 `n`。
    fn method(&self, target: JsObject<'gc>, name: &str, f: r8framework::NativeFn, n: u32) {
        self.realm.method(self.mc, target, name, f, n);
    }

    /// 全局对象上的一个构造器（`length` 是 `n`）：宿主函数，`prototype` ⇄ `constructor` 互指。
    fn constructor(&self, name: &str, f: r8framework::NativeFn, proto: JsObject<'gc>, n: u32) -> JsObject<'gc> {
        let ctor = self.realm.named_native(self.mc, f, name, n);
        if let r8framework::ObjectKind::Native(native) = &mut ctor.borrow_mut(self.mc).kind {
            native.constructor = true;
        }
        // 内建构造器的 prototype：不可写、不可枚举、不可配置
        ctor.borrow_mut(self.mc).define("prototype", r8framework::Property { slot: r8framework::Slot::Data(proto.into()), enumerable: false, writable: false, configurable: false });
        self.set(proto, "constructor", ctor.into());
        self.set(self.realm.global, name, ctor.into());
        ctor
    }

    /// 各原型的 `[Symbol.toStringTag]`（不可写、不可枚举、可配置）。
    fn tags(&self) {
        let r = self.realm;
        let global = |name: &str| r.global.borrow().own(name).and_then(|v| if let Value::Object(o) = v { Some(o) } else { None });
        let mut tagged = alloc::vec![
            (r.symbol_proto, "Symbol"),
            (r.bigint_proto, "BigInt"),
            (r.map_proto, "Map"),
            (r.set_proto, "Set"),
            (r.weakmap_proto, "WeakMap"),
            (r.weakset_proto, "WeakSet"),
            (r.promise_proto, "Promise"),
            (r.generator_proto, "Generator"),
            (r.async_generator_proto, "AsyncGenerator"),
            (r.generator_function_proto, "GeneratorFunction"),
            (r.async_function_proto, "AsyncFunction"),
            (r.async_generator_function_proto, "AsyncGeneratorFunction"),
            (r.array_iterator_proto, "Array Iterator"),
            (r.collection_iterator_proto, "Map Iterator"),
            (r.set_iterator_proto, "Set Iterator"),
            (r.iterator_helper_proto, "Iterator Helper"),
            (r.regexp_string_iterator_proto, "RegExp String Iterator"),
            (r.string_iterator_proto, "String Iterator"),
        ];
        for name in ["Math", "JSON"] {
            if let Some(o) = global(name) {
                tagged.push((o, name));
            }
        }
        for (o, tag) in tagged {
            let v = self.text(tag);
            o.borrow_mut(self.mc).define_symbol(r.symbols.to_string_tag, r8framework::Property { slot: r8framework::Slot::Data(v), enumerable: false, writable: false, configurable: true });
        }
    }

    /// Symbol / BigInt：挂成构造器的样子，但不能 `new`。
    fn not_constructor(&self, f: JsObject<'gc>) {
        if let r8framework::ObjectKind::Native(native) = &mut f.borrow_mut(self.mc).kind {
            native.constructor = false;
        }
    }

    /// 宿主 getter（不可枚举）。
    fn getter(&self, target: JsObject<'gc>, name: &str, f: r8framework::NativeFn) {
        let g = self.realm.named_native(self.mc, f, &alloc::format!("get {name}"), 0);
        target.borrow_mut(self.mc).define(name, r8framework::Property { slot: r8framework::Slot::Accessor { get: Some(g), set: None }, enumerable: false, writable: false, configurable: true });
    }

    /// 堆上的字符串值。
    fn text(&self, s: &str) -> Value<'gc> {
        Value::String(r8gc::Gc::new(self.mc, r8framework::SmallStr::from(s)))
    }

    /// 一个普通对象（原型 `Object.prototype`）。
    fn plain(&self) -> JsObject<'gc> {
        r8framework::ObjectData { proto: Some(self.realm.object_proto), ..r8framework::ObjectData::default() }.alloc(self.mc)
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
