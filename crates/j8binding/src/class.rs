use core::any::TypeId;
use core::ptr::NonNull;

use j8framework::{Agent, CellMut, CellRef, Error, ErrorKind, GcCell, Host, HostData, HostObject, JsObject, Native, NativeFn, ObjectData, ObjectKind, Property, Slot, Trace, Tracer, Value, ValueKind};

#[derive(Clone, Copy)]
pub enum Member {
    Method { name: &'static str, f: NativeFn, length: u32 },
    Accessor { name: &'static str, get: Option<NativeFn>, set: Option<NativeFn> },
    Static { name: &'static str, f: NativeFn, length: u32 },
}

pub unsafe trait Class<'scope>: Trace + Send + Sync + Sized + 'scope {
    fn brand() -> TypeId;
    const NAME: &'static str;
    const CONSTRUCTOR: Option<(NativeFn, u32)>;
    const MEMBERS: &'static [Member];

    fn parent(a: &mut Agent<'scope, '_>, target: JsObject<'scope>) -> Option<ClassHandle<'scope>> {
        let _ = (a, target);
        None
    }

    fn find(&self, brand: TypeId) -> Option<NonNull<()>> {
        (brand == Self::brand()).then(|| NonNull::from(self).cast())
    }

    fn find_mut(&mut self, brand: TypeId) -> Option<NonNull<()>> {
        (brand == Self::brand()).then(|| NonNull::from(self).cast())
    }

    fn install(a: &mut Agent<'scope, '_>, target: JsObject<'scope>) -> ClassHandle<'scope> {
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
        if let Some(base) = Self::parent(a, target) {
            proto.set_proto(a.mc, Some(base.proto));
            ctor.set_proto(a.mc, Some(base.ctor));
        }
        let tag = a.string(Self::NAME);
        proto.borrow_mut(a.mc).define_symbol(a.realm.symbols.to_string_tag, Property { slot: Slot::Data(tag.into()), enumerable: false, writable: false, configurable: true });
        target.borrow_mut(a.mc).define(Self::NAME, Property::hidden(ctor.into()));
        ClassHandle { ctor, proto }
    }
}

pub trait Object {
    const NAME: &'static str;
    const MEMBERS: &'static [Member];

    fn install<'scope>(a: &mut Agent<'scope, '_>, target: JsObject<'scope>) -> JsObject<'scope> {
        let o = a.object();
        for m in Self::MEMBERS {
            Glue::member(a, o, o, m);
        }
        target.borrow_mut(a.mc).define(Self::NAME, Property::hidden(o.into()));
        o
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ClassHandle<'scope> {
    pub ctor: JsObject<'scope>,
    pub proto: JsObject<'scope>,
}

impl<'scope> ClassHandle<'scope> {
    pub fn instance<T: Class<'scope>>(&self, a: &Agent<'scope, '_>, value: T) -> JsObject<'scope> {
        Glue::wrap(a, value, self.proto)
    }
}

struct Hosted<T>(T);

unsafe impl<'scope, T: Class<'scope>> HostObject<'scope> for Hosted<T> {
    fn trace(&self, t: &mut Tracer<'_>) {
        self.0.trace(t);
    }

    fn relocate(&mut self, t: &mut Tracer<'_>) {
        self.0.relocate(t);
    }

    fn find(&self, brand: TypeId) -> Option<NonNull<()>> {
        self.0.find(brand)
    }

    fn find_mut(&mut self, brand: TypeId) -> Option<NonNull<()>> {
        self.0.find_mut(brand)
    }
}

pub struct Glue;

impl Glue {
    fn member<'scope>(a: &Agent<'scope, '_>, ctor: JsObject<'scope>, proto: JsObject<'scope>, m: &Member) {
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

    #[doc(hidden)]
    pub fn function<'scope>(a: &Agent<'scope, '_>, target: JsObject<'scope>, name: &str, f: NativeFn, length: u32) -> JsObject<'scope> {
        let func = a.realm.named_native(a.mc, f, name, length);
        target.borrow_mut(a.mc).define(name, Property::hidden(func.into()));
        func
    }

    #[doc(hidden)]
    pub fn illegal_constructor<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Err(a.throw(ErrorKind::Type, "Illegal constructor"))
    }

    #[doc(hidden)]
    pub fn arg<'scope>(args: &[Value<'scope>], i: usize) -> Value<'scope> {
        args.get(i).copied().unwrap_or_default()
    }

    #[doc(hidden)]
    pub fn wrap<'scope, T: Class<'scope>>(a: &Agent<'scope, '_>, value: T, proto: JsObject<'scope>) -> JsObject<'scope> {
        let data = a.mc.alloc(GcCell::new(a.mc, Host::new(Hosted(value))));
        ObjectData { kind: ObjectKind::Host(data), proto: Some(proto), ..ObjectData::default() }.alloc(a.mc)
    }

    #[doc(hidden)]
    pub fn construct<'scope, T: Class<'scope>>(a: &mut Agent<'scope, '_>, value: Result<T, Error>) -> Result<Value<'scope>, Error> {
        let value = value?;
        let proto = a.proto_from_ctor(a.new_target, a.realm.object_proto)?;
        Ok(Self::wrap(a, value, proto).into())
    }

    #[doc(hidden)]
    pub fn require_new(a: &Agent<'_, '_>, name: &str) -> Result<(), Error> {
        if a.new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, &alloc::format!("Class constructor {name} cannot be invoked without 'new'")));
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn host<'scope, T: Class<'scope>>(a: &Agent<'scope, '_>, this: Value<'scope>) -> Result<HostData<'scope>, Error> {
        let data = match this.kind() {
            ValueKind::Object(o) => match o.borrow().kind {
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

    #[doc(hidden)]
    pub fn borrow<'d, 'scope>(a: &Agent<'scope, '_>, data: &'d GcCell<'scope, Host<'scope>>, class: &str) -> Result<CellRef<'d, Host<'scope>>, Error> {
        data.try_borrow().ok_or_else(|| Self::busy(a, class))
    }

    #[doc(hidden)]
    pub fn borrow_mut<'d, 'scope>(a: &Agent<'scope, '_>, data: &'d GcCell<'scope, Host<'scope>>, class: &str) -> Result<CellMut<'d, Host<'scope>>, Error> {
        data.try_borrow_mut(a.mc).ok_or_else(|| Self::busy(a, class))
    }

    #[doc(hidden)]
    pub fn cast<'d, 'scope, T: Class<'scope>>(a: &Agent<'scope, '_>, cell: &'d CellRef<'_, Host<'scope>>) -> Result<&'d T, Error> {
        cell.find(T::brand()).map(|p| unsafe { p.cast::<T>().as_ref() }).ok_or_else(|| Self::illegal(a, T::NAME))
    }

    #[doc(hidden)]
    pub fn cast_mut<'d, 'scope, T: Class<'scope>>(a: &Agent<'scope, '_>, cell: &'d mut CellMut<'_, Host<'scope>>) -> Result<&'d mut T, Error> {
        cell.find_mut(T::brand()).map(|p| unsafe { p.cast::<T>().as_mut() }).ok_or_else(|| Self::illegal(a, T::NAME))
    }

    #[doc(hidden)]
    pub fn parent<'scope, P: Class<'scope>>(a: &mut Agent<'scope, '_>, target: JsObject<'scope>) -> ClassHandle<'scope> {
        let ctor = target.borrow().own(a.mc, P::NAME).and_then(Value::as_object);
        let proto = ctor.and_then(|c| c.borrow().own(a.mc, "prototype")).and_then(Value::as_object);
        match (ctor, proto) {
            (Some(ctor), Some(proto)) => ClassHandle { ctor, proto },
            _ => P::install(a, target),
        }
    }

    fn busy(a: &Agent<'_, '_>, class: &str) -> Error {
        a.throw(ErrorKind::Type, &alloc::format!("{class} is already in use (re-entrant call)"))
    }
}
