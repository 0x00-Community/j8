//! 把 Rust 类型导出成 JS 类：构造器、原型方法、访问器、静态方法；实例是放着 Rust 值的宿主对象。
use alloc::boxed::Box;
use core::any::Any;
use core::cell::{Ref, RefCell, RefMut};

use r8gc::{Gc, Static};
use r8frame::{Agent, Error, ErrorKind, HostData, JsObject, Native, NativeFn, ObjectData, ObjectKind, Property, Slot, Value};

/// 类的一个成员（宿主函数都是宏生成的转换外壳）。
#[derive(Clone, Copy)]
pub enum Member {
    /// 原型上的方法
    Method { name: &'static str, f: NativeFn, length: u32 },
    /// 原型上的访问器（`get` / `set` 可以只有一个）
    Accessor { name: &'static str, get: Option<NativeFn>, set: Option<NativeFn> },
    /// 构造器上的静态方法
    Static { name: &'static str, f: NativeFn, length: u32 },
}

/// 一个导出成 JS 类的 Rust 类型（一般由 `#[r8::class]` 生成）。
pub trait Class: Any + Sized {
    /// JS 里的类名
    const NAME: &'static str;
    /// 构造器的宿主函数与 `length`；没有就是不能 `new` 的类（只能由 Rust 建实例）
    const CONSTRUCTOR: Option<(NativeFn, u32)>;
    const MEMBERS: &'static [Member];

    /// 在 `target` 上装这个类（构造器、原型），给出两者。
    fn install<'gc>(a: &mut Agent<'gc, '_>, target: JsObject<'gc>) -> ClassHandle<'gc> {
        let (f, length) = Self::CONSTRUCTOR.unwrap_or((Glue::illegal_constructor, 0));
        let ctor = a.realm.named_native(a.mc, f, Self::NAME, length);
        if let ObjectKind::Native(n) = &mut ctor.borrow_mut(a.mc).kind {
            *n = Native::constructor(f);
        }
        let proto = a.object();
        ctor.borrow_mut(a.mc).define("prototype", Property { slot: Slot::Data(proto.into()), enumerable: false, writable: false, configurable: false });
        proto.borrow_mut(a.mc).define("constructor", Property::hidden(ctor.into()));
        for m in Self::MEMBERS {
            Glue::member(a, ctor, proto, m);
        }
        let tag = a.string(Self::NAME);
        proto.borrow_mut(a.mc).define_symbol(a.realm.symbols.to_string_tag, Property { slot: Slot::Data(tag.into()), enumerable: false, writable: false, configurable: true });
        target.borrow_mut(a.mc).define(Self::NAME, Property::hidden(ctor.into()));
        ClassHandle { ctor, proto }
    }
}

/// 一个导出成普通对象的 Rust 类型（`console` / `process` 这类，一般由 `#[r8::object]` 生成）。
pub trait Object {
    /// 装在 `target` 上的属性名
    const NAME: &'static str;
    const MEMBERS: &'static [Member];

    /// 在 `target` 上装这个对象，给出它。
    fn install<'gc>(a: &mut Agent<'gc, '_>, target: JsObject<'gc>) -> JsObject<'gc> {
        let o = a.object();
        for m in Self::MEMBERS {
            Glue::member(a, o, o, m);
        }
        target.borrow_mut(a.mc).define(Self::NAME, Property::hidden(o.into()));
        o
    }
}

/// 装好的类：构造器与原型（Rust 这边建实例要用原型）。
#[derive(Clone, Copy, Debug)]
pub struct ClassHandle<'gc> {
    pub ctor: JsObject<'gc>,
    pub proto: JsObject<'gc>,
}

impl<'gc> ClassHandle<'gc> {
    /// 用 Rust 值建一个实例。
    pub fn instance<T: Class>(&self, a: &Agent<'gc, '_>, value: T) -> JsObject<'gc> {
        Glue::wrap(a, value, self.proto)
    }
}

/// 宏生成代码用的胶水。
#[doc(hidden)]
pub struct Glue;

impl Glue {
    fn member<'gc>(a: &Agent<'gc, '_>, ctor: JsObject<'gc>, proto: JsObject<'gc>, m: &Member) {
        match *m {
            Member::Method { name, f, length } => a.realm.method(a.mc, proto, name, f, length),
            Member::Static { name, f, length } => a.realm.method(a.mc, ctor, name, f, length),
            Member::Accessor { name, get, set } => {
                let get = get.map(|f| a.realm.named_native(a.mc, f, &alloc::format!("get {name}"), 0));
                let set = set.map(|f| a.realm.named_native(a.mc, f, &alloc::format!("set {name}"), 1));
                proto.borrow_mut(a.mc).define(name, Property { slot: Slot::Accessor { get, set }, enumerable: false, writable: false, configurable: true });
            }
        }
    }

    /// `#[r8::function]`：在 `target` 上装一个 JS 函数。
    pub fn function<'gc>(a: &Agent<'gc, '_>, target: JsObject<'gc>, name: &str, f: NativeFn, length: u32) -> JsObject<'gc> {
        let func = a.realm.named_native(a.mc, f, name, length);
        target.borrow_mut(a.mc).define(name, Property::hidden(func.into()));
        func
    }

    /// 不能 `new` 的类的构造器。
    pub fn illegal_constructor<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Err(a.throw(ErrorKind::Type, "Illegal constructor"))
    }

    /// 取第 `i` 个实参，缺了就是 `undefined`。
    pub fn arg<'gc>(args: &[Value<'gc>], i: usize) -> Value<'gc> {
        args.get(i).copied().unwrap_or_default()
    }

    /// 宿主对象：原型 `proto`，里面放 `value`。
    pub fn wrap<'gc, T: Any>(a: &Agent<'gc, '_>, value: T, proto: JsObject<'gc>) -> JsObject<'gc> {
        let data = Gc::new(a.mc, Static(RefCell::new(Box::new(value) as Box<dyn Any>)));
        ObjectData { kind: ObjectKind::Host(data), proto: Some(proto), ..ObjectData::default() }.alloc(a.mc)
    }

    /// 构造调用：原型取 `new.target.prototype`；普通调用抛 TypeError。
    pub fn construct<'gc, T: Class>(a: &mut Agent<'gc, '_>, value: Result<T, Error>) -> Result<Value<'gc>, Error> {
        let value = value?;
        let proto = a.proto_from_ctor(a.new_target, a.realm.object_proto)?;
        Ok(Self::wrap(a, value, proto).into())
    }

    /// 构造器不许普通调用。
    pub fn require_new(a: &Agent<'_, '_>, name: &str) -> Result<(), Error> {
        if a.new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, &alloc::format!("Class constructor {name} cannot be invoked without 'new'")));
        }
        Ok(())
    }

    /// `this` 里的宿主数据（不是宿主对象抛 TypeError）。
    pub fn host<'gc, T: Class>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<HostData<'gc>, Error> {
        let data = match this {
            Value::Object(o) => match o.borrow().kind {
                ObjectKind::Host(h) => Some(h),
                _ => None,
            },
            _ => None,
        };
        data.ok_or_else(|| Self::illegal(a, T::NAME))
    }

    fn illegal(a: &Agent<'_, '_>, class: &str) -> Error {
        a.throw(ErrorKind::Type, &alloc::format!("Illegal invocation: receiver is not a {class}"))
    }

    /// 借出宿主数据（`&self` 方法）。
    pub fn borrow<'d>(a: &Agent<'_, '_>, data: &'d Static<RefCell<Box<dyn Any>>>, class: &str) -> Result<Ref<'d, Box<dyn Any>>, Error> {
        data.0.try_borrow().map_err(|_| Self::busy(a, class))
    }

    /// 可变借出宿主数据（`&mut self` 方法）。
    pub fn borrow_mut<'d>(a: &Agent<'_, '_>, data: &'d Static<RefCell<Box<dyn Any>>>, class: &str) -> Result<RefMut<'d, Box<dyn Any>>, Error> {
        data.0.try_borrow_mut().map_err(|_| Self::busy(a, class))
    }

    pub fn cast<'d, T: Class>(a: &Agent<'_, '_>, cell: &'d Ref<'_, Box<dyn Any>>) -> Result<&'d T, Error> {
        cell.downcast_ref::<T>().ok_or_else(|| Self::illegal(a, T::NAME))
    }

    pub fn cast_mut<'d, T: Class>(a: &Agent<'_, '_>, cell: &'d mut RefMut<'_, Box<dyn Any>>) -> Result<&'d mut T, Error> {
        cell.downcast_mut::<T>().ok_or_else(|| Self::illegal(a, T::NAME))
    }

    fn busy(a: &Agent<'_, '_>, class: &str) -> Error {
        a.throw(ErrorKind::Type, &alloc::format!("{class} is already in use (re-entrant call)"))
    }

}
