//! j8builtins: implementation of [`Intrinsics`]; installs the built-in objects into a new [`Realm`].
#![expect(clippy::unnecessary_wraps, reason = "host functions all share the NativeFn signature; Result can't be dropped just because one cannot fail")]
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

use j8framework::{Mutation};
use j8framework::{Intrinsics, JsObject, Realm, Value};

/// The default set of built-in objects.
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

/// Context for installing built-in objects.
struct Installer<'r, 'gc> {
    mc: &'r Mutation<'gc>,
    realm: &'r Realm<'gc>,
}

impl<'gc> Installer<'_, 'gc> {
    /// Built-in property: non-enumerable.
    fn set(&self, target: JsObject<'gc>, name: &str, v: Value<'gc>) {
        target.borrow_mut(self.mc).define(name, j8framework::Property::hidden(v));
    }

    /// Host method whose `length` is `n`.
    fn method(&self, target: JsObject<'gc>, name: &str, f: j8framework::NativeFn, n: u32) {
        self.realm.method(self.mc, target, name, f, n);
    }

    /// A constructor on the global object (`length` is `n`): a host function, with `prototype` ⇄ `constructor` pointing
    /// at each other.
    fn constructor(&self, name: &str, f: j8framework::NativeFn, proto: JsObject<'gc>, n: u32) -> JsObject<'gc> {
        let ctor = self.realm.named_native(self.mc, f, name, n);
        if let j8framework::ObjectKind::Native(native) = &mut ctor.borrow_mut(self.mc).kind {
            native.constructor = true;
        }
        // prototype of a built-in constructor: non-writable, non-enumerable, non-configurable
        ctor.borrow_mut(self.mc).define("prototype", j8framework::Property { slot: j8framework::Slot::Data(proto.into()), enumerable: false, writable: false, configurable: false });
        self.set(proto, "constructor", ctor.into());
        self.set(self.realm.global, name, ctor.into());
        ctor
    }

    /// `[Symbol.toStringTag]` of each prototype (non-writable, non-enumerable, configurable).
    fn tags(&self) {
        let r = self.realm;
        let global = |name: &str| r.global.borrow().own(self.mc, name).and_then(j8framework::Value::as_object);
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
            o.borrow_mut(self.mc).define_symbol(r.symbols.to_string_tag, j8framework::Property { slot: j8framework::Slot::Data(v), enumerable: false, writable: false, configurable: true });
        }
    }

    /// Symbol / BigInt: installed like constructors, but cannot be `new`ed.
    fn not_constructor(&self, f: JsObject<'gc>) {
        if let j8framework::ObjectKind::Native(native) = &mut f.borrow_mut(self.mc).kind {
            native.constructor = false;
        }
    }

    /// Host getter (non-enumerable).
    fn getter(&self, target: JsObject<'gc>, name: &str, f: j8framework::NativeFn) {
        let g = self.realm.named_native(self.mc, f, &alloc::format!("get {name}"), 0);
        target.borrow_mut(self.mc).define(name, j8framework::Property { slot: j8framework::Slot::Accessor { get: Some(g), set: None }, enumerable: false, writable: false, configurable: true });
    }

    /// A string value on the heap.
    fn text(&self, s: &str) -> Value<'gc> {
        Value::String(self.mc.alloc(j8framework::JsStr::from(s)))
    }

    /// A plain object (prototype `Object.prototype`).
    fn plain(&self) -> JsObject<'gc> {
        j8framework::ObjectData { proto: Some(self.realm.object_proto), ..j8framework::ObjectData::default() }.alloc(self.mc)
    }
}

/// Get an argument; `undefined` if missing.
trait Args<'gc> {
    fn at(&self, i: usize) -> Value<'gc>;
}

impl<'gc> Args<'gc> for [Value<'gc>] {
    fn at(&self, i: usize) -> Value<'gc> {
        self.get(i).copied().unwrap_or_default()
    }
}
