use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::TypeId;
use core::ptr::NonNull;

use j8bytecode::{WasmExternKind, WasmFuncType, WasmGlobalType, WasmHeapType, WasmImportKind, WasmLimits, WasmMemoryType, WasmModule, WasmRefType, WasmTableType, WasmValType};
use j8compiler::{CompileWasm, WasmError};
use j8framework::{Backing, BufferData, Context, Error, ErrorKind, External, Host, HostObject, Intrinsics, JsObject, Mutation, NativeClosure, ObjectData, ObjectKind, Property, Scope, ScopeLocal, Slot, Tracer, Trace, Value, ValueCell, ValueKind, WordSlot};
use num_bigint::BigInt;
use num_traits::ToPrimitive;

use super::entity::Ptr;
use super::gc::{WasmObject, Word};
use super::types::Hierarchy;
use super::{WasmTier, WasmCompiled, WasmExtern, WasmFunc, WasmGlobal, WasmHost, WasmInstance, WasmInstantiateError, WasmMemory, WasmStore, WasmTable, WasmTag, WasmThrow, WasmTrap, WasmTypes};

type Tier = Arc<dyn WasmTier + Send + Sync>;

pub struct WebAssembly<I> {
    intrinsics: I,
    tier: Option<Tier>,
}

impl<I> From<I> for WebAssembly<I> {
    fn from(intrinsics: I) -> Self {
        Self { intrinsics, tier: None }
    }
}

impl<I> WebAssembly<I> {
    #[must_use]
    pub fn with_tier(mut self, tier: impl Into<Option<Tier>>) -> Self {
        self.tier = tier.into();
        self
    }
}

impl<I: Intrinsics> Intrinsics for WebAssembly<I> {
    fn install<'scope>(&self, mc: &Mutation<'scope>, context: &Context<'scope>) {
        self.intrinsics.install(mc, context);
        Api { mc, cx: context }.install(self.tier.clone());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Realm,
    Module,
    Instance,
    Memory,
    Table,
    Global,
    Tag,
    Exception,
    Func,
    Object,
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Self::Realm => "WebAssembly",
            Self::Module => "Module",
            Self::Instance => "Instance",
            Self::Memory => "Memory",
            Self::Table => "Table",
            Self::Global => "Global",
            Self::Tag => "Tag",
            Self::Exception => "Exception",
            Self::Func => "Function",
            Self::Object => "Object",
        }
    }
}

struct Marker;

struct Wrapped<'scope> {
    kind: Kind,
    word: WordSlot<'scope>,
    module: Option<Compiled>,
    values: Box<[ValueCell<'scope>]>,
    tier: Option<Tier>,
}

unsafe impl<'scope> HostObject<'scope> for Wrapped<'scope> {
    fn trace(&self, t: &mut Tracer<'_>) {
        self.word.trace(t);
        self.values.trace(t);
    }

    fn relocate(&mut self, t: &mut Tracer<'_>) {
        self.word.relocate(t);
        self.values.relocate(t);
    }

    fn find(&self, brand: TypeId) -> Option<NonNull<()>> {
        (brand == TypeId::of::<Marker>()).then(|| NonNull::from(self).cast())
    }

    fn find_mut(&mut self, brand: TypeId) -> Option<NonNull<()>> {
        (brand == TypeId::of::<Marker>()).then(|| NonNull::from(self).cast())
    }
}

#[derive(Clone, Default)]
struct Options {
    js_string: bool,
    constants: Option<String>,
}

#[derive(Clone)]
struct Compiled {
    code: Arc<WasmCompiled>,
    options: Options,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Builtin {
    Test,
    Cast,
    FromCharCodeArray,
    IntoCharCodeArray,
    FromCharCode,
    FromCodePoint,
    CharCodeAt,
    CodePointAt,
    Length,
    Concat,
    Substring,
    Equals,
    Compare,
}

const JS_STRING: &str = "wasm:js-string";
const JS_STRING_BUILTINS: &str = "js-string";
const BUILTINS: [(Builtin, &str); 13] = [
    (Builtin::Test, "test"),
    (Builtin::Cast, "cast"),
    (Builtin::FromCharCodeArray, "fromCharCodeArray"),
    (Builtin::IntoCharCodeArray, "intoCharCodeArray"),
    (Builtin::FromCharCode, "fromCharCode"),
    (Builtin::FromCodePoint, "fromCodePoint"),
    (Builtin::CharCodeAt, "charCodeAt"),
    (Builtin::CodePointAt, "codePointAt"),
    (Builtin::Length, "length"),
    (Builtin::Concat, "concat"),
    (Builtin::Substring, "substring"),
    (Builtin::Equals, "equals"),
    (Builtin::Compare, "compare"),
];
const MAX_CODE_POINT: u32 = 0x10_FFFF;
const UNIT_MASK: u32 = 0xFFFF;

impl Builtin {
    fn named(name: &str) -> Option<Self> {
        BUILTINS.iter().find(|(_, n)| *n == name).map(|&(b, _)| b)
    }

    fn id(self) -> u32 {
        u32::try_from(BUILTINS.iter().position(|&(b, _)| b == self).unwrap_or(0)).unwrap_or(0)
    }

    fn of(id: u32) -> Option<Self> {
        BUILTINS.get(id as usize).map(|&(b, _)| b)
    }

    fn signature(self, array: u32) -> WasmFuncType {
        let ext = WasmValType::Ref(WasmRefType { nullable: true, heap: WasmHeapType::Extern });
        let string = WasmValType::Ref(WasmRefType { nullable: false, heap: WasmHeapType::Extern });
        let arr = WasmValType::Ref(WasmRefType { nullable: true, heap: WasmHeapType::Concrete(array) });
        let i32t = WasmValType::I32;
        let (params, results): (&[WasmValType], &[WasmValType]) = match self {
            Self::Test | Self::Length => (&[ext], &[i32t]),
            Self::Cast => (&[ext], &[string]),
            Self::FromCharCodeArray => (&[arr, i32t, i32t], &[string]),
            Self::IntoCharCodeArray => (&[ext, arr, i32t], &[i32t]),
            Self::FromCharCode | Self::FromCodePoint => (&[i32t], &[string]),
            Self::CharCodeAt | Self::CodePointAt => (&[ext, i32t], &[i32t]),
            Self::Concat => (&[ext, ext], &[string]),
            Self::Substring => (&[ext, i32t, i32t], &[string]),
            Self::Equals | Self::Compare => (&[ext, ext], &[i32t]),
        };
        WasmFuncType { params: params.into(), results: results.into() }
    }
}

impl Options {
    fn builtin(&self, imp: &j8bytecode::WasmImport) -> bool {
        self.js_string && &*imp.module == JS_STRING
    }

    fn constant(&self, imp: &j8bytecode::WasmImport) -> bool {
        self.constants.as_deref().is_some_and(|c| *imp.module == *c)
    }

    fn hidden(&self, imp: &j8bytecode::WasmImport) -> bool {
        self.builtin(imp) || self.constant(imp)
    }
}

#[derive(Clone)]
struct Seen<'scope> {
    kind: Kind,
    word: u64,
    module: Option<Compiled>,
    values: Vec<Value<'scope>>,
}

const REALM_JSTAG: usize = 0;
const REALM_PROTOS: usize = 1;
const PROTO_KINDS: [Kind; 7] = [Kind::Module, Kind::Instance, Kind::Memory, Kind::Table, Kind::Global, Kind::Tag, Kind::Exception];
const ERRORS: [&str; 3] = ["CompileError", "LinkError", "RuntimeError"];
const ERROR_COMPILE: usize = 0;
const ERROR_LINK: usize = 1;
const ERROR_RUNTIME: usize = 2;
const REALM_ERRORS: usize = REALM_PROTOS + PROTO_KINDS.len();
const REALM_LEN: usize = REALM_ERRORS + ERRORS.len();
const SLOT_REALM: usize = 0;
const SLOT_TARGET: usize = 1;
const INSTANCE_EXPORTS: usize = 0;
const PAGE_LOG2: u32 = 16;
const MAX_PAGES32: u64 = 65_536;
const MAX_PAGES64: u64 = 1 << 37;
const TABLE_MAX: u64 = 10_000_000;
const I31_MIN: f64 = -1_073_741_824.0;
const I31_MAX: f64 = 1_073_741_823.0;
const U32_LIMIT: f64 = 4_294_967_295.0;

impl Seen<'_> {
    fn of(o: JsObject<'_>) -> Option<Seen<'_>> {
        let ObjectKind::Host(h) = o.borrow().kind else { return None };
        let cell = h.get().try_borrow()?;
        let p = cell.find(TypeId::of::<Marker>())?;
        let w = unsafe { p.cast::<Wrapped<'_>>().as_ref() };
        Some(Seen { kind: w.kind, word: w.word.get(), module: w.module.clone(), values: w.values.iter().map(ValueCell::get).collect() })
    }
}

#[derive(Clone, Copy)]
struct Realm<'scope> {
    object: JsObject<'scope>,
    types: ScopeLocal<'scope, WasmTypes>,
    tier: Option<&'scope (dyn WasmTier + Send + Sync)>,
}

impl<'scope> Realm<'scope> {
    fn of(o: JsObject<'scope>) -> Option<Self> {
        let s = Seen::of(o)?;
        let ObjectKind::Host(h) = o.borrow().kind else { return None };
        let tier = h.get().try_borrow().and_then(|cell| {
            let p = cell.find(TypeId::of::<Marker>())?;
            let w = unsafe { p.cast::<Wrapped<'scope>>().as_ref() };
            w.tier.as_deref().map(|t| unsafe { &*core::ptr::from_ref(t) })
        });
        (s.kind == Kind::Realm).then(|| Self { object: o, types: Ptr::local(s.word).unwrap_or_else(|| unreachable!("a realm always holds its type registry")), tier })
    }

    fn value(self, i: usize) -> Value<'scope> {
        Seen::of(self.object).and_then(|s| s.values.get(i).copied()).unwrap_or_default()
    }

    fn proto(self, k: Kind) -> JsObject<'scope> {
        let i = PROTO_KINDS.iter().position(|&x| x == k).unwrap_or(0);
        self.value(REALM_PROTOS + i).as_object().unwrap_or_else(|| unreachable!("every interface has a prototype"))
    }

    fn error_proto(self, i: usize) -> JsObject<'scope> {
        self.value(REALM_ERRORS + i).as_object().unwrap_or_else(|| unreachable!("every error class has a prototype"))
    }

    fn jstag(self) -> ScopeLocal<'scope, WasmTag<'scope>> {
        let tag = self.value(REALM_JSTAG).as_object().and_then(Seen::of).map_or(0, |s| s.word);
        Ptr::local(tag).unwrap_or_else(|| unreachable!("the realm creates JSTag at install"))
    }
}

struct Api<'r, 'scope> {
    mc: &'r Mutation<'scope>,
    cx: &'r Context<'scope>,
}

type Native = for<'scope, 'x> fn(&mut Scope<'scope, 'x>, Realm<'scope>, JsObject<'scope>, Value<'scope>, &[Value<'scope>]) -> Result<Value<'scope>, Error>;

macro_rules! natives {
    ($($name:ident => $target:path;)*) => {
        struct Natives;

        impl Natives {
            $(
                fn $name<'scope>(a: &mut Scope<'scope, '_>, me: JsObject<'scope>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
                    let realm = Js::realm_of(me);
                    let f: Native = $target;
                    f(a, realm, me, this, args)
                }
            )*
        }
    };
}

natives! {
    validate => Js::validate;
    compile => Js::compile;
    instantiate => Js::instantiate;
    instantiate_later => Js::instantiate_later;
    module => Js::new_module;
    module_exports => Js::module_exports;
    module_imports => Js::module_imports;
    module_custom => Js::module_custom;
    instance => Js::new_instance;
    instance_exports => Js::instance_exports;
    memory => Js::new_memory;
    memory_buffer => Js::memory_buffer;
    memory_grow => Js::memory_grow;
    table => Js::new_table;
    table_length => Js::table_length;
    table_grow => Js::table_grow;
    table_get => Js::table_get;
    table_set => Js::table_set;
    global => Js::new_global;
    global_value => Js::global_value;
    global_set => Js::global_set;
    tag => Js::new_tag;
    exception => Js::new_exception;
    exception_get_arg => Js::exception_get_arg;
    exception_is => Js::exception_is;
    compile_error => Js::compile_error;
    link_error => Js::link_error;
    runtime_error => Js::runtime_error;
    exported => Js::call_exported;
}

impl<'scope> Api<'_, 'scope> {
    fn closure(&self, f: j8framework::ClosureFn, realm: Value<'scope>, name: &str, length: u32, constructor: bool) -> JsObject<'scope> {
        let o = ObjectData { kind: ObjectKind::NativeClosure(NativeClosure { f, slots: alloc::vec![realm], constructor }), proto: Some(self.cx.function_proto), ..ObjectData::default() }.alloc(self.mc);
        self.cx.name_function(self.mc, o, name, Some(length));
        o
    }

    fn text(&self, s: &str) -> Value<'scope> {
        Value::String(j8framework::JsStr::alloc(self.mc, s))
    }

    fn tag(&self, o: JsObject<'scope>, tag: &str) {
        let v = self.text(tag);
        o.borrow_mut(self.mc).define_symbol(self.cx.symbols.to_string_tag, Property { slot: Slot::Data(v), enumerable: false, writable: false, configurable: true });
    }

    fn plain(&self) -> JsObject<'scope> {
        ObjectData { proto: Some(self.cx.object_proto), ..ObjectData::default() }.alloc(self.mc)
    }

    fn method(&self, target: JsObject<'scope>, realm: Value<'scope>, name: &str, f: j8framework::ClosureFn, length: u32) {
        let m = self.closure(f, realm, name, length, false);
        target.borrow_mut(self.mc).define(name, Property::data(m.into()));
    }

    fn getter(&self, target: JsObject<'scope>, realm: Value<'scope>, name: &str, get: j8framework::ClosureFn, set: Option<j8framework::ClosureFn>) {
        let g = self.closure(get, realm, &format!("get {name}"), 0, false);
        let s = set.map(|f| self.closure(f, realm, &format!("set {name}"), 1, false));
        target.borrow_mut(self.mc).define(name, Property { slot: Slot::Accessor { get: Some(g), set: s }, enumerable: true, writable: false, configurable: true });
    }

    fn interface(&self, ns: JsObject<'scope>, realm: Value<'scope>, kind: Kind, f: j8framework::ClosureFn, length: u32) -> (JsObject<'scope>, JsObject<'scope>) {
        let proto = self.plain();
        let ctor = self.closure(f, realm, kind.name(), length, true);
        ctor.borrow_mut(self.mc).define("prototype", Property { slot: Slot::Data(proto.into()), enumerable: false, writable: false, configurable: false });
        proto.borrow_mut(self.mc).define("constructor", Property::hidden(ctor.into()));
        ns.borrow_mut(self.mc).define(kind.name(), Property::hidden(ctor.into()));
        self.tag(proto, &format!("WebAssembly.{}", kind.name()));
        (ctor, proto)
    }

    fn install(&self, tier: Option<Tier>) {
        let mc = self.mc;
        let types = WasmTypes::alloc(mc);
        let realm_obj = Js::wrap(mc, Kind::Realm, Ptr::of(types), None, &[Value::Undefined; REALM_LEN], None);
        if let ObjectKind::Host(h) = realm_obj.borrow().kind
            && let Some(mut cell) = h.get().try_borrow_mut(mc)
            && let Some(p) = cell.find_mut(TypeId::of::<Marker>())
        {
            unsafe { p.cast::<Wrapped<'scope>>().as_mut() }.tier = tier;
        }
        let realm: Value<'scope> = realm_obj.into();
        let ns = self.plain();
        self.tag(ns, "WebAssembly");
        self.cx.global.borrow_mut(mc).define("WebAssembly", Property::hidden(ns.into()));
        self.method(ns, realm, "validate", Natives::validate, 1);
        self.method(ns, realm, "compile", Natives::compile, 1);
        self.method(ns, realm, "instantiate", Natives::instantiate, 1);
        let (module, mp) = self.interface(ns, realm, Kind::Module, Natives::module, 1);
        self.method(module, realm, "exports", Natives::module_exports, 1);
        self.method(module, realm, "imports", Natives::module_imports, 1);
        self.method(module, realm, "customSections", Natives::module_custom, 2);
        let (_, ip) = self.interface(ns, realm, Kind::Instance, Natives::instance, 1);
        self.getter(ip, realm, "exports", Natives::instance_exports, None);
        let (_, memp) = self.interface(ns, realm, Kind::Memory, Natives::memory, 1);
        self.method(memp, realm, "grow", Natives::memory_grow, 1);
        self.getter(memp, realm, "buffer", Natives::memory_buffer, None);
        let (_, tp) = self.interface(ns, realm, Kind::Table, Natives::table, 1);
        self.getter(tp, realm, "length", Natives::table_length, None);
        self.method(tp, realm, "grow", Natives::table_grow, 1);
        self.method(tp, realm, "get", Natives::table_get, 1);
        self.method(tp, realm, "set", Natives::table_set, 1);
        let (_, gp) = self.interface(ns, realm, Kind::Global, Natives::global, 1);
        self.method(gp, realm, "valueOf", Natives::global_value, 0);
        self.getter(gp, realm, "value", Natives::global_value, Some(Natives::global_set));
        let (_, tagp) = self.interface(ns, realm, Kind::Tag, Natives::tag, 1);
        let (_, ep) = self.interface(ns, realm, Kind::Exception, Natives::exception, 2);
        self.method(ep, realm, "getArg", Natives::exception_get_arg, 2);
        self.method(ep, realm, "is", Natives::exception_is, 1);
        let mut values: Vec<Value<'scope>> = alloc::vec![Value::Undefined; REALM_LEN];
        for (i, p) in [mp, ip, memp, tp, gp, tagp, ep].into_iter().enumerate() {
            values[REALM_PROTOS + i] = p.into();
        }
        let error_ctor = self.cx.global.borrow().own(mc, "Error").and_then(Value::as_object);
        for (i, (name, f)) in ERRORS.into_iter().zip([Natives::compile_error as j8framework::ClosureFn, Natives::link_error, Natives::runtime_error]).enumerate() {
            let proto = ObjectData { proto: Some(self.cx.error_proto(ErrorKind::Error)), ..ObjectData::default() }.alloc(mc);
            let ctor = self.closure(f, realm, name, 1, true);
            if let Some(e) = error_ctor {
                ctor.set_proto(mc, Some(e));
            }
            ctor.borrow_mut(mc).define("prototype", Property { slot: Slot::Data(proto.into()), enumerable: false, writable: false, configurable: false });
            proto.borrow_mut(mc).define("constructor", Property::hidden(ctor.into()));
            proto.borrow_mut(mc).define("name", Property::hidden(self.text(name)));
            proto.borrow_mut(mc).define("message", Property::hidden(self.text("")));
            ns.borrow_mut(mc).define(name, Property::hidden(ctor.into()));
            values[REALM_ERRORS + i] = proto.into();
        }
        let ext = WasmValType::Ref(WasmRefType { nullable: true, heap: WasmHeapType::Extern });
        let canon = types.get().0.borrow_mut().func(&WasmFuncType { params: Box::new([ext]), results: Box::new([]) });
        let jstag = mc.alloc(WasmTag { canon, js: WordSlot::new(0) });
        let jstag_obj = Js::wrap(mc, Kind::Tag, Ptr::of(jstag), None, &[], Some(tagp));
        jstag.get().js.set(mc.gc_scope(), Value::from(jstag_obj).bits());
        values[REALM_JSTAG] = jstag_obj.into();
        ns.borrow_mut(mc).define("JSTag", Property { slot: Slot::Data(jstag_obj.into()), enumerable: true, writable: false, configurable: true });
        if let ObjectKind::Host(h) = realm_obj.borrow().kind
            && let Some(cell) = h.get().try_borrow()
            && let Some(p) = cell.find(TypeId::of::<Marker>())
        {
            let w = unsafe { p.cast::<Wrapped<'scope>>().as_ref() };
            for (c, v) in w.values.iter().zip(values) {
                c.set(mc, v);
            }
        }
    }
}

struct JsHost<'a, 'scope, 'x> {
    a: &'a mut Scope<'scope, 'x>,
    realm: Realm<'scope>,
}

impl<'scope> WasmHost<'scope> for JsHost<'_, 'scope, '_> {
    fn call(&mut self, _: &Mutation<'scope>, func: &'scope WasmFunc<'scope>, ty: &WasmFuncType, args: &[(u64, u128)]) -> Result<Vec<(u64, u128)>, WasmThrow> {
        let a = &mut *self.a;
        let realm = self.realm;
        if let Word::Host(id) = Word::decode(func.host.get(), Hierarchy::Any)
            && let Some(b) = Builtin::of(id)
        {
            return Js::builtin(a, realm, b, args).map_err(|e| match e {
                Fault::Trap => WasmThrow::Trap(WasmTrap::Unreachable),
                Fault::Js(e) => Js::catch(a, realm, &e),
            });
        }
        let callee = unsafe { Value::from_bits(func.host.get()) };
        let r = (|| {
            if ty.params.iter().chain(&ty.results).any(|&t| Js::opaque(realm, t)) {
                return Err(a.throw(ErrorKind::Type, "type incompatible with JavaScript"));
            }
            let mut jsargs = Vec::with_capacity(args.len());
            for (&t, &w) in ty.params.iter().zip(args) {
                jsargs.push(Js::to_js(a, realm, t, w));
            }
            let ret = a.call(callee, Value::Undefined, &jsargs)?;
            match ty.results.len() {
                0 => Ok(Vec::new()),
                1 => Ok(alloc::vec![Js::to_wasm(a, realm, ty.results[0], ret)?]),
                n => {
                    let items = Js::iterate(a, ret)?;
                    if items.len() != n {
                        return Err(a.throw(ErrorKind::Type, "wrong number of results"));
                    }
                    ty.results.iter().zip(items).map(|(&t, v)| Js::to_wasm(a, realm, t, v)).collect()
                }
            }
        })();
        r.map_err(|e| Js::catch(a, realm, &e))
    }
}

enum Fault {
    Trap,
    Js(Error),
}

pub struct Js;

impl Js {
    fn wrap<'scope>(mc: &Mutation<'scope>, kind: Kind, word: u64, module: Option<Compiled>, values: &[Value<'scope>], proto: Option<JsObject<'scope>>) -> JsObject<'scope> {
        let w = Wrapped { kind, word: WordSlot::new(word), module, values: values.iter().map(|&v| ValueCell::new(v)).collect(), tier: None };
        let data = mc.alloc(j8framework::GcCell::new(mc, Host::new(w)));
        ObjectData { kind: ObjectKind::Host(data), proto, ..ObjectData::default() }.alloc(mc)
    }

    fn realm_of(me: JsObject<'_>) -> Realm<'_> {
        let slot = match &me.borrow().kind {
            ObjectKind::NativeClosure(c) => c.slots.get(SLOT_REALM).copied(),
            _ => None,
        };
        slot.and_then(Value::as_object).and_then(Realm::of).unwrap_or_else(|| unreachable!("WebAssembly natives carry their realm"))
    }

    fn arg<'scope>(args: &[Value<'scope>], i: usize) -> Value<'scope> {
        args.get(i).copied().unwrap_or_default()
    }

    fn present<'scope>(args: &[Value<'scope>], i: usize) -> Option<Value<'scope>> {
        args.get(i).copied().filter(|&v| v != Value::Undefined)
    }

    fn seen(v: Value<'_>, kind: Kind) -> Option<Seen<'_>> {
        v.as_object().and_then(Seen::of).filter(|s| s.kind == kind)
    }

    fn this<'scope>(a: &Scope<'scope, '_>, v: Value<'scope>, kind: Kind) -> Result<Seen<'scope>, Error> {
        Self::seen(v, kind).ok_or_else(|| a.throw(ErrorKind::Type, &format!("receiver is not a WebAssembly.{}", kind.name())))
    }

    fn require_new(a: &Scope<'_, '_>, kind: Kind) -> Result<(), Error> {
        if a.new_target == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, &format!("WebAssembly.{} must be called with 'new'", kind.name())));
        }
        Ok(())
    }

    fn create<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, kind: Kind, word: u64, module: Option<Compiled>, values: &[Value<'scope>]) -> Result<JsObject<'scope>, Error> {
        let proto = a.proto_from_ctor(a.new_target, realm.proto(kind))?;
        Ok(Self::wrap(a.mc, kind, word, module, values, Some(proto)))
    }

    fn error<'scope>(a: &Scope<'scope, '_>, realm: Realm<'scope>, which: usize, message: &str) -> Error {
        let e = a.error(ErrorKind::Error, message);
        e.borrow_mut(a.mc).set_proto(Some(realm.error_proto(which)));
        a.throw_value(e.into())
    }

    fn construct_error<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, which: usize, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let proto = a.proto_from_ctor(a.new_target, realm.error_proto(which))?;
        let e = a.error(ErrorKind::Error, "");
        e.borrow_mut(a.mc).set_proto(Some(proto));
        e.borrow_mut(a.mc).remove("message");
        if Self::arg(args, 0) != Value::Undefined {
            let m = a.to_string(Self::arg(args, 0))?;
            e.borrow_mut(a.mc).define("message", Property::hidden(m.into()));
        }
        let options = Self::arg(args, 1);
        if let Some(opts) = options.as_object()
            && a.has_key(opts, j8framework::Key::Str("cause"))?
        {
            let cause = a.get_named(options, "cause")?;
            e.borrow_mut(a.mc).define("cause", Property::hidden(cause));
        }
        Ok(e.into())
    }

    fn compile_error<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::construct_error(a, realm, ERROR_COMPILE, args)
    }

    fn link_error<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::construct_error(a, realm, ERROR_LINK, args)
    }

    fn runtime_error<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::construct_error(a, realm, ERROR_RUNTIME, args)
    }

    fn bytes<'scope>(a: &Scope<'scope, '_>, v: Value<'scope>) -> Result<Vec<u8>, Error> {
        let bad = || a.throw(ErrorKind::Type, "argument must be a BufferSource");
        let o = v.as_object().ok_or_else(bad)?;
        let d = o.borrow();
        match &d.kind {
            ObjectKind::ArrayBuffer(b) => Ok(b.bytes.as_deref().map(<[u8]>::to_vec).unwrap_or_default()),
            ObjectKind::TypedArray(k, view) => Self::view(view, view.length.map(|n| n * k.size())).ok_or_else(bad),
            ObjectKind::DataView(view) => Self::view(view, view.length).ok_or_else(bad),
            _ => Err(bad()),
        }
    }

    fn view(view: &j8framework::ViewData<'_>, len: Option<usize>) -> Option<Vec<u8>> {
        let b = view.buffer.borrow();
        let ObjectKind::ArrayBuffer(data) = &b.kind else { return None };
        let bytes = data.bytes.as_deref().unwrap_or(&[]);
        let len = len.unwrap_or_else(|| bytes.len().saturating_sub(view.offset));
        Some(bytes.get(view.offset..view.offset + len).map(<[u8]>::to_vec).unwrap_or_default())
    }

    fn options<'scope>(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Options, Error> {
        if v == Value::Undefined {
            return Ok(Options::default());
        }
        if v.as_object().is_none() {
            return Err(a.throw(ErrorKind::Type, "options must be an object"));
        }
        let mut o = Options::default();
        let b = a.get_named(v, "builtins")?;
        if b != Value::Undefined {
            for x in Self::iterate(a, b)? {
                let name = a.to_string(x)?;
                o.js_string |= *String::from(&**name) == *JS_STRING_BUILTINS;
            }
        }
        let c = a.get_named(v, "importedStringConstants")?;
        if c != Value::Undefined {
            let name = a.to_string(c)?;
            o.constants = Some(String::from(&**name));
        }
        Ok(o)
    }

    fn check(realm: Realm<'_>, module: &WasmModule, options: &Options) -> Result<(), String> {
        let mut types = realm.types.get().0.borrow_mut();
        let ids = types.canonicalize(module);
        let array = types.char_array();
        let externref = WasmValType::Ref(WasmRefType { nullable: true, heap: WasmHeapType::Extern });
        let string = WasmValType::Ref(WasmRefType { nullable: false, heap: WasmHeapType::Extern });
        for imp in &module.imports {
            if options.builtin(imp) {
                let WasmImportKind::Func(t) = imp.kind else { return Err(format!("{JS_STRING} import {} is not a function", &*imp.name)) };
                let b = Builtin::named(&imp.name).ok_or_else(|| format!("unknown builtin {}", &*imp.name))?;
                if ids[t as usize] != types.func(&b.signature(array)) {
                    return Err(format!("builtin {} has the wrong type", &*imp.name));
                }
            } else if options.constant(imp) {
                let ok = matches!(imp.kind, WasmImportKind::Global(g) if !g.mutable && (g.content == externref || g.content == string));
                if !ok {
                    return Err(format!("string constant {} must be an immutable externref global", &*imp.name));
                }
            }
        }
        Ok(())
    }

    fn compile_bytes<'scope>(a: &Scope<'scope, '_>, realm: Realm<'scope>, bytes: &[u8], options: Options) -> Result<Compiled, Error> {
        let module = WasmModule::compile_wasm(bytes).map_err(|e: WasmError| Self::error(a, realm, ERROR_COMPILE, &e.message))?;
        Self::check(realm, &module, &options).map_err(|e| Self::error(a, realm, ERROR_COMPILE, &e))?;
        Ok(Compiled { code: Arc::new(WasmCompiled::from(module)), options })
    }

    fn validate<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let bytes = Self::bytes(a, Self::arg(args, 0))?;
        let options = Self::options(a, Self::arg(args, 1))?;
        Ok(WasmModule::compile_wasm(&bytes).is_ok_and(|m| Self::check(realm, &m, &options).is_ok()).into())
    }

    fn settle<'scope>(a: &mut Scope<'scope, '_>, r: Result<Value<'scope>, Error>) -> Result<Value<'scope>, Error> {
        let p = a.new_promise();
        match r {
            Ok(v) => a.resolve_promise(p, v)?,
            Err(Error::Exception) => {
                let e = a.take_exception();
                a.reject_promise(p, e);
            }
            Err(e) => return Err(e),
        }
        Ok(p.into())
    }

    fn module_object<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, m: Compiled) -> JsObject<'scope> {
        Self::wrap(a.mc, Kind::Module, 0, Some(m), &[], Some(realm.proto(Kind::Module)))
    }

    fn compile<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let r = Self::bytes(a, Self::arg(args, 0)).and_then(|b| {
            let options = Self::options(a, Self::arg(args, 1))?;
            Self::compile_bytes(a, realm, &b, options)
        });
        let r = r.map(|m| Self::module_object(a, realm, m).into());
        Self::settle(a, r)
    }

    fn instantiate<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let (source, imports) = (Self::arg(args, 0), Self::arg(args, 1));
        let r = (|| {
            if let Some(s) = Self::seen(source, Kind::Module) {
                let m = s.module.unwrap_or_else(|| unreachable!("a Module wraps its compiled code"));
                return Self::instance_object(a, realm, &m, imports, None).map(Value::from);
            }
            let bytes = Self::bytes(a, source)?;
            let options = Self::options(a, Self::arg(args, 2))?;
            let m = Self::compile_bytes(a, realm, &bytes, options)?;
            Ok(Self::module_object(a, realm, m).into())
        })();
        let compiled = Self::settle(a, r)?;
        let later = ObjectData { kind: ObjectKind::NativeClosure(NativeClosure { f: Natives::instantiate_later, slots: alloc::vec![realm.object.into(), imports], constructor: false }), proto: Some(a.context.function_proto), ..ObjectData::default() }.alloc(a.mc);
        if Self::seen(source, Kind::Module).is_some() {
            return Ok(compiled);
        }
        let then = a.get_named(compiled, "then")?;
        a.call(then, compiled, &[later.into()])
    }

    fn instantiate_later<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, me: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let module = Self::arg(args, 0);
        let imports = match &me.borrow().kind {
            ObjectKind::NativeClosure(c) => c.slots.get(SLOT_TARGET).copied().unwrap_or_default(),
            _ => Value::Undefined,
        };
        let m = Self::module_arg(a, module)?;
        let instance = Self::instance_object(a, realm, &m, imports, None)?;
        let o = ObjectData { proto: Some(a.context.object_proto), ..ObjectData::default() }.alloc(a.mc);
        o.borrow_mut(a.mc).define("module", Property::data(module));
        o.borrow_mut(a.mc).define("instance", Property::data(instance.into()));
        Ok(o.into())
    }

    fn new_module<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::require_new(a, Kind::Module)?;
        let bytes = Self::bytes(a, Self::arg(args, 0))?;
        let options = Self::options(a, Self::arg(args, 1))?;
        let m = Self::compile_bytes(a, realm, &bytes, options)?;
        Ok(Self::create(a, realm, Kind::Module, 0, Some(m), &[])?.into())
    }

    fn module_arg<'scope>(a: &Scope<'scope, '_>, v: Value<'scope>) -> Result<Compiled, Error> {
        Self::seen(v, Kind::Module).and_then(|s| s.module).ok_or_else(|| a.throw(ErrorKind::Type, "argument is not a WebAssembly.Module"))
    }

    fn kind_name(k: WasmExternKind) -> &'static str {
        match k {
            WasmExternKind::Func => "function",
            WasmExternKind::Table => "table",
            WasmExternKind::Memory => "memory",
            WasmExternKind::Global => "global",
            WasmExternKind::Tag => "tag",
        }
    }

    fn descriptors<'scope>(a: &mut Scope<'scope, '_>, rows: &[(Option<&str>, &str, WasmExternKind)]) -> Value<'scope> {
        let items: Vec<Value<'scope>> = rows
            .iter()
            .map(|&(module, name, kind)| {
                let o = ObjectData { proto: Some(a.context.object_proto), ..ObjectData::default() }.alloc(a.mc);
                if let Some(m) = module {
                    o.borrow_mut(a.mc).define("module", Property::data(a.string(m).into()));
                }
                o.borrow_mut(a.mc).define("name", Property::data(a.string(name).into()));
                o.borrow_mut(a.mc).define("kind", Property::data(a.string(Self::kind_name(kind)).into()));
                o.into()
            })
            .collect();
        a.array_from_slice(&items).into()
    }

    fn module_exports<'scope>(a: &mut Scope<'scope, '_>, _: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let m = Self::module_arg(a, Self::arg(args, 0))?;
        let rows: Vec<(Option<&str>, &str, WasmExternKind)> = m.code.module.exports.iter().map(|e| (None, &*e.name, e.kind)).collect();
        Ok(Self::descriptors(a, &rows))
    }

    fn module_imports<'scope>(a: &mut Scope<'scope, '_>, _: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let m = Self::module_arg(a, Self::arg(args, 0))?;
        let rows: Vec<(Option<&str>, &str, WasmExternKind)> = m.code.module.imports.iter().filter(|i| !m.options.hidden(i)).map(|i| (Some(&*i.module), &*i.name, WasmExternKind::from(&i.kind))).collect();
        Ok(Self::descriptors(a, &rows))
    }

    fn buffer<'scope>(a: &Scope<'scope, '_>, bytes: Backing) -> JsObject<'scope> {
        ObjectData { kind: ObjectKind::ArrayBuffer(Box::new(BufferData { bytes: Some(bytes), max: None, shared: false })), proto: Some(a.context.array_buffer_proto), ..ObjectData::default() }.alloc(a.mc)
    }

    fn module_custom<'scope>(a: &mut Scope<'scope, '_>, _: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let m = Self::module_arg(a, Self::arg(args, 0))?;
        if args.len() < 2 {
            return Err(a.throw(ErrorKind::Type, "customSections requires a section name"));
        }
        let name = a.to_string(Self::arg(args, 1))?.units().to_vec();
        let items: Vec<Value<'scope>> = m.code.module.customs.iter().filter(|c| c.name.encode_utf16().eq(name.iter().copied())).map(|c| Self::buffer(a, Backing::from(c.bytes.to_vec())).into()).collect();
        Ok(a.array_from_slice(&items).into())
    }

    fn store<'a, 'scope, 'x>(a: &'a mut Scope<'scope, 'x>, realm: Realm<'scope>, host: &'a mut Option<JsHost<'a, 'scope, 'x>>) -> WasmStore<'a, 'scope> {
        let mc: &'x Mutation<'scope> = a.mc;
        let h = host.insert(JsHost { a, realm });
        let store = WasmStore::new(mc, realm.types, h);
        match realm.tier {
            Some(t) => store.with_tier(t),
            None => store,
        }
    }

    fn read_imports<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, m: &Compiled, imports: Value<'scope>) -> Result<Vec<WasmExtern<'scope>>, Error> {
        let module = &m.code.module;
        if module.imports.iter().any(|i| !m.options.hidden(i)) && imports.as_object().is_none() {
            return Err(a.throw(ErrorKind::Type, "imports argument must be an object"));
        }
        if imports != Value::Undefined && imports.as_object().is_none() {
            return Err(a.throw(ErrorKind::Type, "imports argument must be an object"));
        }
        let ids = realm.types.get().0.borrow_mut().canonicalize(module);
        let canon = |i: u32| ids[i as usize];
        let mut out = Vec::with_capacity(module.imports.len());
        for imp in &module.imports {
            if m.options.builtin(imp) {
                let b = Builtin::named(&imp.name).unwrap_or(Builtin::Test);
                let WasmImportKind::Func(t) = imp.kind else { continue };
                let ty = module.func_type(t).map(&canon);
                let mut host = None;
                let store = Self::store(a, realm, &mut host);
                out.push(WasmExtern::Func(store.host_func(&ty, u64::from(Word::Host(b.id())))));
                continue;
            }
            if m.options.constant(imp) {
                let WasmImportKind::Global(g) = imp.kind else { continue };
                let s = a.string(&imp.name);
                out.push(WasmExtern::Global(a.mc.alloc(WasmGlobal::new(WasmGlobalType { content: g.content.map(&canon), mutable: false }, (Value::from(s).bits(), 0)))));
                continue;
            }
            let o = a.get_named(imports, &imp.module)?;
            if o.as_object().is_none() {
                return Err(a.throw(ErrorKind::Type, &format!("import module {} is not an object", &*imp.module)));
            }
            let v = a.get_named(o, &imp.name)?;
            let link = |a: &Scope<'scope, '_>| Self::error(a, realm, ERROR_LINK, &format!("import {}.{} is incompatible", &*imp.module, &*imp.name));
            let ext = match &imp.kind {
                WasmImportKind::Func(t) => {
                    if !a.is_callable(v) {
                        return Err(link(a));
                    }
                    match Self::exported_func(v) {
                        Some(f) => WasmExtern::Func(f),
                        None => {
                            let ty = module.func_type(*t).map(&canon);
                            let mut host = None;
                            let store = Self::store(a, realm, &mut host);
                            WasmExtern::Func(store.host_func(&ty, v.bits()))
                        }
                    }
                }
                WasmImportKind::Global(g) => {
                    let ty = WasmGlobalType { content: g.content.map(&canon), mutable: g.mutable };
                    if let Some(s) = Self::seen(v, Kind::Global) {
                        WasmExtern::Global(Ptr::local(s.word).ok_or_else(|| link(a))?)
                    } else {
                        let numeric = matches!(ty.content, WasmValType::I32 | WasmValType::F32 | WasmValType::F64) && v.is_number();
                        let big = ty.content == WasmValType::I64 && matches!(v.kind(), ValueKind::BigInt(_));
                        let reference = matches!(ty.content, WasmValType::Ref(_));
                        if ty.mutable || !(numeric || big || reference) {
                            return Err(link(a));
                        }
                        let w = Self::to_wasm(a, realm, ty.content, v)?;
                        WasmExtern::Global(a.mc.alloc(WasmGlobal::new(ty, w)))
                    }
                }
                WasmImportKind::Memory(_) => WasmExtern::Memory(Self::seen(v, Kind::Memory).and_then(|s| Ptr::local(s.word)).ok_or_else(|| link(a))?),
                WasmImportKind::Table(_) => WasmExtern::Table(Self::seen(v, Kind::Table).and_then(|s| Ptr::local(s.word)).ok_or_else(|| link(a))?),
                WasmImportKind::Tag(_) => WasmExtern::Tag(Self::seen(v, Kind::Tag).and_then(|s| Ptr::local(s.word)).ok_or_else(|| link(a))?),
            };
            out.push(ext);
        }
        Ok(out)
    }

    fn instance_object<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, m: &Compiled, imports: Value<'scope>, proto: Option<JsObject<'scope>>) -> Result<JsObject<'scope>, Error> {
        let externs = Self::read_imports(a, realm, m, imports)?;
        let r = {
            let mut host = None;
            let mut store = Self::store(a, realm, &mut host);
            store.instantiate(&m.code, &externs)
        };
        let inst = match r {
            Ok(i) => i,
            Err(WasmInstantiateError::Link(_)) => return Err(Self::error(a, realm, ERROR_LINK, "incompatible import")),
            Err(WasmInstantiateError::Trap(t)) => return Err(Self::raise(a, realm, t)),
        };
        let exports = Self::exports_object(a, realm, inst.get());
        let proto = proto.unwrap_or_else(|| realm.proto(Kind::Instance));
        Ok(Self::wrap(a.mc, Kind::Instance, Ptr::of(inst), None, &[exports.into()], Some(proto)))
    }

    fn exports_object<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, inst: &'scope WasmInstance<'scope>) -> JsObject<'scope> {
        let o = ObjectData { proto: None, ..ObjectData::default() }.alloc(a.mc);
        for e in &inst.module.module.exports {
            let Some(ext) = WasmStore::export(inst, &e.name) else { continue };
            let v = Self::extern_value(a, realm, ext);
            o.borrow_mut(a.mc).define(&e.name, Property { slot: Slot::Data(v), enumerable: true, writable: false, configurable: false });
        }
        o.borrow_mut(a.mc).set_non_extensible(true);
        o
    }

    fn cached<'scope>(a: &Scope<'scope, '_>, slot: &WordSlot<'scope>, make: impl FnOnce() -> JsObject<'scope>) -> Value<'scope> {
        match slot.get() {
            0 => {
                let o = make();
                slot.set(a.mc.gc_scope(), Value::from(o).bits());
                o.into()
            }
            bits => unsafe { Value::from_bits(bits) },
        }
    }

    fn extern_value<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, e: WasmExtern<'scope>) -> Value<'scope> {
        let mc = a.mc;
        match e {
            WasmExtern::Func(f) => Self::function(a, realm, f),
            WasmExtern::Table(t) => Self::cached(a, &t.get().js, || Self::wrap(mc, Kind::Table, Ptr::of(t), None, &[], Some(realm.proto(Kind::Table)))),
            WasmExtern::Memory(m) => Self::cached(a, &m.get().js, || Self::wrap(mc, Kind::Memory, Ptr::of(m), None, &[], Some(realm.proto(Kind::Memory)))),
            WasmExtern::Global(g) => Self::cached(a, &g.get().js, || Self::wrap(mc, Kind::Global, Ptr::of(g), None, &[], Some(realm.proto(Kind::Global)))),
            WasmExtern::Tag(t) => Self::cached(a, &t.get().js, || Self::wrap(mc, Kind::Tag, Ptr::of(t), None, &[], Some(realm.proto(Kind::Tag)))),
        }
    }

    fn function<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, f: ScopeLocal<'scope, WasmFunc<'scope>>) -> Value<'scope> {
        let mc = a.mc;
        let cx = a.context;
        let params = realm.types.get().0.borrow().get(f.get().canon).sub.func().map_or(0, |t| t.params.len());
        let name = format!("{}", f.get().index);
        Self::cached(a, &f.get().js, || {
            let target = Self::wrap(mc, Kind::Func, Ptr::of(f), None, &[], None);
            let o = ObjectData { kind: ObjectKind::NativeClosure(NativeClosure { f: Natives::exported, slots: alloc::vec![realm.object.into(), target.into()], constructor: false }), proto: Some(cx.function_proto), ..ObjectData::default() }.alloc(mc);
            cx.name_function(mc, o, &name, Some(u32::try_from(params).unwrap_or(0)));
            o
        })
    }

    fn exported_func(v: Value<'_>) -> Option<ScopeLocal<'_, WasmFunc<'_>>> {
        let o = v.as_object()?;
        let target = match &o.borrow().kind {
            ObjectKind::NativeClosure(c) => c.slots.get(SLOT_TARGET).copied(),
            _ => None,
        }?;
        Self::seen(target, Kind::Func).and_then(|s| Ptr::local(s.word))
    }

    fn string(v: Value<'_>) -> Result<j8framework::JsString<'_>, Fault> {
        v.as_string().ok_or(Fault::Trap)
    }

    fn units(s: j8framework::JsString<'_>) -> Vec<u16> {
        s.units().to_vec()
    }

    fn char_array<'scope>(w: u64) -> Result<&'scope WasmObject<'scope>, Fault> {
        WasmObject::get(w).ok_or(Fault::Trap)
    }

    fn builtin<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, b: Builtin, args: &[(u64, u128)]) -> Result<Vec<(u64, u128)>, Fault> {
        let word = |i: usize| args.get(i).map_or(0, |x| x.0);
        let int = |i: usize| super::Slot(word(i)).u32();
        let js = |a: &mut Scope<'scope, '_>, i: usize| Self::any_to_js(a, realm, word(i));
        let out = |v: u64| Ok(alloc::vec![(v, 0)]);
        let make = |a: &mut Scope<'scope, '_>, units: &[u16]| Value::from(j8framework::JsStr::alloc_units(a.mc, units)).bits();
        let layout = |o: &WasmObject<'_>| realm.types.get().0.borrow().get(o.head()).layout.clone();
        match b {
            Builtin::Test => out(u64::from(js(a, 0).as_string().is_some())),
            Builtin::Cast => {
                Self::string(js(a, 0))?;
                out(word(0))
            }
            Builtin::Length => out(u64::try_from(Self::string(js(a, 0))?.units().len()).unwrap_or(0)),
            Builtin::FromCharCode => {
                let u = u16::try_from(int(0) & UNIT_MASK).unwrap_or(0);
                out(make(a, &[u]))
            }
            Builtin::FromCodePoint => {
                let cp = int(0);
                if cp > MAX_CODE_POINT {
                    return Err(Fault::Js(a.throw(ErrorKind::Range, "Invalid code point")));
                }
                let mut buf = [0u16; 2];
                let units: &[u16] = match char::from_u32(cp) {
                    Some(c) => c.encode_utf16(&mut buf),
                    None => {
                        buf[0] = u16::try_from(cp).unwrap_or(0);
                        &buf[..1]
                    }
                };
                out(make(a, units))
            }
            Builtin::CharCodeAt | Builtin::CodePointAt => {
                let u = Self::units(Self::string(js(a, 0))?);
                let i = int(1) as usize;
                let c = *u.get(i).ok_or(Fault::Trap)?;
                if b == Builtin::CodePointAt
                    && let Some(&lo) = u.get(i + 1)
                    && let Some(Ok(ch)) = char::decode_utf16([c, lo]).next()
                {
                    return out(u64::from(u32::from(ch)));
                }
                out(u64::from(c))
            }
            Builtin::Concat => {
                let (x, y) = (Self::string(js(a, 0))?, Self::string(js(a, 1))?);
                out(Value::from(j8framework::JsStr::concat(a.mc, x, y)).bits())
            }
            Builtin::Substring => {
                let s = Self::string(js(a, 0))?;
                let u = Self::units(s);
                let (start, end) = (int(1) as usize, int(2) as usize);
                if end < start {
                    return out(make(a, &[]));
                }
                let (start, end) = (start.min(u.len()), end.min(u.len()));
                out(make(a, &u[start..end]))
            }
            Builtin::Equals => {
                let (x, y) = (js(a, 0), js(a, 1));
                let sx = if x == Value::Null { None } else { Some(Self::units(Self::string(x)?)) };
                let sy = if y == Value::Null { None } else { Some(Self::units(Self::string(y)?)) };
                out(u64::from(sx == sy))
            }
            Builtin::Compare => {
                let (x, y) = (Self::units(Self::string(js(a, 0))?), Self::units(Self::string(js(a, 1))?));
                out(u64::from(match x.cmp(&y) {
                    core::cmp::Ordering::Less => u32::MAX,
                    core::cmp::Ordering::Equal => 0,
                    core::cmp::Ordering::Greater => 1,
                }))
            }
            Builtin::FromCharCodeArray => {
                let arr = Self::char_array(word(0))?;
                let (start, count) = (int(1), int(2));
                let end = start.checked_add(count).filter(|&e| e <= arr.length()).ok_or(Fault::Trap)?;
                let l = layout(arr);
                let units: Vec<u16> = (start..end).map(|i| u16::try_from(arr.read(l.fields[0], i, &l).0 & u64::from(UNIT_MASK)).unwrap_or(0)).collect();
                out(make(a, &units))
            }
            Builtin::IntoCharCodeArray => {
                let u = Self::units(Self::string(js(a, 0))?);
                let arr = Self::char_array(word(1))?;
                let start = int(2);
                let n = u32::try_from(u.len()).unwrap_or(u32::MAX);
                start.checked_add(n).filter(|&e| e <= arr.length()).ok_or(Fault::Trap)?;
                let l = layout(arr);
                for (i, &c) in (start..).zip(&u) {
                    arr.write(a.mc, l.fields[0], i, &l, (u64::from(c), 0));
                }
                out(u64::from(n))
            }
        }
    }

    fn opaque(realm: Realm<'_>, t: WasmValType) -> bool {
        match t {
            WasmValType::V128 => true,
            WasmValType::Ref(r) => realm.types.get().0.borrow().hierarchy(r.heap) == Hierarchy::Exn,
            _ => false,
        }
    }

    fn call_exported<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, me: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let f = Self::exported_func(me.into()).ok_or_else(|| a.throw(ErrorKind::Type, "not a WebAssembly function"))?;
        let ty = realm.types.get().0.borrow().get(f.get().canon).sub.func().cloned().unwrap_or_default();
        if ty.params.iter().chain(&ty.results).any(|&t| Self::opaque(realm, t)) {
            return Err(a.throw(ErrorKind::Type, "type incompatible with JavaScript"));
        }
        let mut words = Vec::with_capacity(ty.params.len());
        for (i, &t) in ty.params.iter().enumerate() {
            words.push(Self::to_wasm(a, realm, t, Self::arg(args, i))?);
        }
        let r = {
            let mut host = None;
            let mut store = Self::store(a, realm, &mut host);
            store.invoke(f, &words)
        };
        let results = r.map_err(|t| Self::raise(a, realm, t))?;
        match results.len() {
            0 => Ok(Value::Undefined),
            1 => Ok(Self::to_js(a, realm, ty.results[0], results[0])),
            _ => {
                let items: Vec<Value<'scope>> = ty.results.iter().zip(results).map(|(&t, w)| Self::to_js(a, realm, t, w)).collect();
                Ok(a.array_from_slice(&items).into())
            }
        }
    }

    fn raise<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, t: WasmThrow) -> Error {
        match t {
            WasmThrow::Trap(WasmTrap::CallStackExhausted) => a.throw(ErrorKind::Range, "Maximum call stack size exceeded"),
            WasmThrow::Trap(t) => Self::error(a, realm, ERROR_RUNTIME, t.message()),
            WasmThrow::Exception(w) => {
                let jstag = realm.jstag();
                if WasmStore::exception_tag(w).is_some_and(|t| Ptr::of(t) == Ptr::of(jstag)) {
                    let mut host = None;
                    let store = Self::store(a, realm, &mut host);
                    let payload = store.exception_payload(w);
                    let v = payload.first().map_or(0, |p| p.0);
                    let js = Self::any_to_js(a, realm, v);
                    return a.throw_value(js);
                }
                let v = Self::exception_value(a, realm, w);
                a.throw_value(v)
            }
        }
    }

    fn exception_value<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, w: u64) -> Value<'scope> {
        let Some(obj) = WasmObject::get(w) else { return Value::Undefined };
        let mc = a.mc;
        Self::cached(a, obj.js(), || Self::wrap(mc, Kind::Exception, w, None, &[], Some(realm.proto(Kind::Exception))))
    }

    fn catch<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, e: &Error) -> WasmThrow {
        match e {
            Error::Exception => {
                let v = a.take_exception();
                if let Some(s) = Self::seen(v, Kind::Exception) {
                    return WasmThrow::Exception(s.word);
                }
                let payload = Self::js_to_any(a, v);
                let mut host = None;
                let store = Self::store(a, realm, &mut host);
                WasmThrow::Exception(store.exception(realm.jstag(), &[(payload, 0)]))
            }
            _ => WasmThrow::Trap(WasmTrap::Unsupported),
        }
    }

    fn iterate<'scope>(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> Result<Vec<Value<'scope>>, Error> {
        if v.as_object().is_none() {
            return Err(a.throw(ErrorKind::Type, "multiple results must be iterable"));
        }
        let it = a.get_iterator(v)?;
        let mut out = Vec::new();
        while let Some(x) = a.iterator_step(it)? {
            out.push(x);
        }
        Ok(out)
    }

    fn to_js<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, t: WasmValType, (s, _): (u64, u128)) -> Value<'scope> {
        match t {
            WasmValType::I32 => f64::from(super::Slot(s).i32()).into(),
            WasmValType::I64 => a.bigint(BigInt::from(s.cast_signed())),
            WasmValType::F32 => f64::from(super::Slot(s).f32()).into(),
            WasmValType::F64 => f64::from_bits(s).into(),
            WasmValType::V128 => Value::Undefined,
            WasmValType::Ref(r) => match realm.types.get().0.borrow().hierarchy(r.heap) {
                Hierarchy::Func => match Ptr::local::<WasmFunc<'scope>>(s) {
                    Some(f) => Self::function(a, realm, f),
                    None => Value::Null,
                },
                _ => Self::any_to_js(a, realm, s),
            },
        }
    }

    fn any_to_js<'scope>(a: &mut Scope<'scope, '_>, _: Realm<'scope>, w: u64) -> Value<'scope> {
        match Word::decode(w, Hierarchy::Any) {
            Word::Null => Value::Null,
            Word::I31(v) => f64::from(v).into(),
            Word::Js(bits) => unsafe { Value::from_bits(bits) },
            Word::Host(_) | Word::Func(_) => Value::Undefined,
            Word::Object(w) => match WasmObject::get(w) {
                Some(o) if o.is_box() => unsafe { Value::from_bits(o.boxed()) },
                Some(o) => {
                    let mc = a.mc;
                    Self::cached(a, o.js(), || {
                        let x = Self::wrap(mc, Kind::Object, w, None, &[], None);
                        x.borrow_mut(mc).set_non_extensible(true);
                        x
                    })
                }
                None => Value::Undefined,
            },
        }
    }

    fn js_to_any<'scope>(a: &mut Scope<'scope, '_>, v: Value<'scope>) -> u64 {
        if v == Value::Null {
            return 0;
        }
        if let Some(n) = v.as_number()
            && n.fract() == 0.0
            && (I31_MIN..=I31_MAX).contains(&n)
            && !(n == 0.0 && n.is_sign_negative())
        {
            #[expect(clippy::cast_possible_truncation, reason = "checked to be in the i31 range")]
            return u64::from(Word::I31(n as i32));
        }
        if let Some(s) = Self::seen(v, Kind::Object) {
            return s.word;
        }
        if v.is_pointer() {
            return v.bits();
        }
        WasmObject::boxed_value(a.mc, v.bits())
    }

    fn to_wasm<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, t: WasmValType, v: Value<'scope>) -> Result<(u64, u128), Error> {
        Ok(match t {
            WasmValType::I32 => (u64::from(a.to_int32(v)?.cast_unsigned()), 0),
            WasmValType::I64 => {
                let b = a.to_bigint(v)?;
                let w = Scope::wrap_bigint(&b, u64::from(u64::BITS), true);
                (w.to_i64().unwrap_or(0).cast_unsigned(), 0)
            }
            WasmValType::F32 => {
                #[expect(clippy::cast_possible_truncation, reason = "ToWebAssemblyValue rounds to the nearest f32")]
                let x = a.to_number(v)? as f32;
                (u64::from(x.to_bits()), 0)
            }
            WasmValType::F64 => (a.to_number(v)?.to_bits(), 0),
            WasmValType::V128 => return Err(a.throw(ErrorKind::Type, "v128 cannot be converted from JavaScript")),
            WasmValType::Ref(r) => {
                let h = realm.types.get().0.borrow().hierarchy(r.heap);
                if v == Value::Null {
                    if r.nullable {
                        return Ok((0, 0));
                    }
                    return Err(a.throw(ErrorKind::Type, "null is not allowed for a non-nullable reference"));
                }
                let w = match h {
                    Hierarchy::Func => Self::exported_func(v).map(Ptr::of).ok_or_else(|| a.throw(ErrorKind::Type, "value is not a WebAssembly function"))?,
                    Hierarchy::Exn => return Err(a.throw(ErrorKind::Type, "exnref cannot be converted from JavaScript")),
                    Hierarchy::Extern | Hierarchy::Any => Self::js_to_any(a, v),
                };
                let mut host = None;
                let store = Self::store(a, realm, &mut host);
                let ok = store.matches(w, r.heap, r.nullable);
                if !ok {
                    return Err(a.throw(ErrorKind::Type, "value does not match the reference type"));
                }
                (w, 0)
            }
        })
    }

    fn val_type(a: &Scope<'_, '_>, s: &str) -> Result<WasmValType, Error> {
        let r = |heap| WasmValType::Ref(WasmRefType { nullable: true, heap });
        Ok(match s {
            "i32" => WasmValType::I32,
            "i64" => WasmValType::I64,
            "f32" => WasmValType::F32,
            "f64" => WasmValType::F64,
            "v128" => WasmValType::V128,
            "externref" => r(WasmHeapType::Extern),
            "anyfunc" | "funcref" => r(WasmHeapType::Func),
            "anyref" => r(WasmHeapType::Any),
            "eqref" => r(WasmHeapType::Eq),
            "i31ref" => r(WasmHeapType::I31),
            "structref" => r(WasmHeapType::Struct),
            "arrayref" => r(WasmHeapType::Array),
            "exnref" => r(WasmHeapType::Exn),
            _ => return Err(a.throw(ErrorKind::Type, &format!("invalid value type {s}"))),
        })
    }

    fn field<'scope>(a: &mut Scope<'scope, '_>, desc: Value<'scope>, name: &str) -> Result<Value<'scope>, Error> {
        a.get_named(desc, name)
    }

    fn enforce_u32<'scope>(a: &mut Scope<'scope, '_>, v: Value<'scope>, what: &str) -> Result<u64, Error> {
        let n = a.to_number(v)?;
        if !n.is_finite() {
            return Err(a.throw(ErrorKind::Type, &format!("{what} must be a finite number")));
        }
        let n = n.trunc();
        if !(0.0..=U32_LIMIT).contains(&n) {
            return Err(a.throw(ErrorKind::Type, &format!("{what} is out of range")));
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked to be in [0, 2^32 - 1]")]
        Ok(n as u64)
    }

    fn enforce_u64<'scope>(a: &mut Scope<'scope, '_>, v: Value<'scope>, what: &str) -> Result<u64, Error> {
        let b = a.to_bigint(v)?;
        b.to_u64().ok_or_else(|| a.throw(ErrorKind::Type, &format!("{what} is out of range")))
    }

    fn address<'scope>(a: &mut Scope<'scope, '_>, wide: bool, v: Value<'scope>, what: &str) -> Result<u64, Error> {
        if wide { Self::enforce_u64(a, v, what) } else { Self::enforce_u32(a, v, what) }
    }

    fn address_value<'scope>(a: &Scope<'scope, '_>, wide: bool, n: u64) -> Value<'scope> {
        if wide {
            a.bigint(BigInt::from(n))
        } else {
            #[expect(clippy::cast_precision_loss, reason = "a 32-bit address is exact in f64")]
            (n as f64).into()
        }
    }

    fn address_kind<'scope>(a: &mut Scope<'scope, '_>, desc: Value<'scope>) -> Result<bool, Error> {
        let v = Self::field(a, desc, "address")?;
        if v == Value::Undefined {
            return Ok(false);
        }
        let s = a.to_string(v)?;
        match &*String::from(&**s) {
            "i32" => Ok(false),
            "i64" => Ok(true),
            other => Err(a.throw(ErrorKind::Type, &format!("invalid address type {other}"))),
        }
    }

    fn limits<'scope>(a: &mut Scope<'scope, '_>, desc: Value<'scope>, wide: bool, cap: u64) -> Result<WasmLimits, Error> {
        let iv = Self::field(a, desc, "initial")?;
        if iv == Value::Undefined {
            return Err(a.throw(ErrorKind::Type, "descriptor.initial is required"));
        }
        let min = Self::address(a, wide, iv, "initial")?;
        let mv = Self::field(a, desc, "maximum")?;
        let max = if mv == Value::Undefined { None } else { Some(Self::address(a, wide, mv, "maximum")?) };
        if max.is_some_and(|m| m < min) {
            return Err(a.throw(ErrorKind::Range, "initial exceeds maximum"));
        }
        if min > cap || max.is_some_and(|m| m > cap) {
            return Err(a.throw(ErrorKind::Range, "size exceeds the limit"));
        }
        Ok(WasmLimits { min, max })
    }

    fn descriptor<'scope>(a: &Scope<'scope, '_>, v: Value<'scope>) -> Result<Value<'scope>, Error> {
        if v.as_object().is_none() {
            return Err(a.throw(ErrorKind::Type, "descriptor must be an object"));
        }
        Ok(v)
    }

    fn new_memory<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::require_new(a, Kind::Memory)?;
        let desc = Self::descriptor(a, Self::arg(args, 0))?;
        let wide = Self::address_kind(a, desc)?;
        let limits = Self::limits(a, desc, wide, if wide { MAX_PAGES64 } else { MAX_PAGES32 })?;
        let shared = Self::field(a, desc, "shared")?;
        let shared = a.to_boolean(shared);
        if shared && limits.max.is_none() {
            return Err(a.throw(ErrorKind::Type, "shared memory needs a maximum"));
        }
        let ty = WasmMemoryType { limits, memory64: wide, shared: false, page_size_log2: u8::try_from(PAGE_LOG2).unwrap_or(0) };
        let mem = a.mc.alloc(WasmMemory::new(ty));
        let o = Self::create(a, realm, Kind::Memory, Ptr::of(mem), None, &[])?;
        mem.get().js.set(a.mc.gc_scope(), Value::from(o).bits());
        Ok(o.into())
    }

    fn memory_of<'scope>(a: &Scope<'scope, '_>, this: Value<'scope>) -> Result<&'scope WasmMemory<'scope>, Error> {
        let s = Self::this(a, this, Kind::Memory)?;
        Ptr::get(s.word).ok_or_else(|| a.throw(ErrorKind::Type, "detached memory"))
    }

    fn memory_buffer<'scope>(a: &mut Scope<'scope, '_>, _: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let m = Self::memory_of(a, this)?;
        Ok(Self::cached(a, &m.buffer, || {
            let data = m.data();
            let owner: Box<dyn Send> = Box::new(m.bytes.0.clone());
            let ext = unsafe { External::from_raw(data.as_mut_ptr(), data.len(), owner) };
            Self::buffer(a, Backing::from(ext))
        }))
    }

    pub fn detach<'scope>(mc: &Mutation<'scope>, m: &WasmMemory<'scope>) {
        let bits = m.buffer.get();
        if bits == 0 {
            return;
        }
        if let Some(o) = unsafe { Value::from_bits(bits) }.as_object()
            && let ObjectKind::ArrayBuffer(b) = &mut o.borrow_mut(mc).kind
        {
            b.bytes = None;
        }
        m.buffer.set(mc.gc_scope(), 0);
    }

    fn memory_grow<'scope>(a: &mut Scope<'scope, '_>, _: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let m = Self::memory_of(a, this)?;
        let wide = m.ty.memory64;
        let delta = Self::address(a, wide, Self::arg(args, 0), "delta")?;
        let old = WasmStore::grow_memory(a.mc, m, delta);
        if old == u64::MAX {
            return Err(a.throw(ErrorKind::Range, "failed to grow memory"));
        }
        Ok(Self::address_value(a, wide, old))
    }

    fn element(a: &Scope<'_, '_>, s: &str) -> Result<WasmRefType, Error> {
        match Self::val_type(a, s)? {
            WasmValType::Ref(r) => Ok(r),
            _ => Err(a.throw(ErrorKind::Type, &format!("invalid element type {s}"))),
        }
    }

    fn default_value<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, t: WasmValType, v: Option<Value<'scope>>) -> Result<(u64, u128), Error> {
        match (t, v) {
            (WasmValType::Ref(r), None) if realm.types.get().0.borrow().hierarchy(r.heap) == Hierarchy::Extern => Self::to_wasm(a, realm, t, Value::Undefined),
            (WasmValType::Ref(r), None) if !r.nullable => Err(a.throw(ErrorKind::Type, "a non-nullable reference needs a value")),
            (_, None) => Ok((0, 0)),
            (_, Some(v)) => Self::to_wasm(a, realm, t, v),
        }
    }

    fn new_table<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::require_new(a, Kind::Table)?;
        let desc = Self::descriptor(a, Self::arg(args, 0))?;
        let ev = Self::field(a, desc, "element")?;
        let es = a.to_string(ev)?;
        let element = Self::element(a, &String::from(&**es))?;
        let wide = Self::address_kind(a, desc)?;
        let limits = Self::limits(a, desc, wide, TABLE_MAX)?;
        let init = Self::present(args, 1);
        let init = Self::default_value(a, realm, WasmValType::Ref(element), init)?;
        let ty = WasmTableType { element, limits, table64: wide };
        let t = {
            let mut host = None;
            let store = Self::store(a, realm, &mut host);
            store.host_table(ty, init.0)
        };
        let o = Self::create(a, realm, Kind::Table, Ptr::of(t), None, &[])?;
        t.get().js.set(a.mc.gc_scope(), Value::from(o).bits());
        Ok(o.into())
    }

    fn table_of<'scope>(a: &Scope<'scope, '_>, this: Value<'scope>) -> Result<&'scope WasmTable<'scope>, Error> {
        let s = Self::this(a, this, Kind::Table)?;
        Ptr::get(s.word).ok_or_else(|| a.throw(ErrorKind::Type, "invalid table"))
    }

    fn table_length<'scope>(a: &mut Scope<'scope, '_>, _: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let t = Self::table_of(a, this)?;
        Ok(Self::address_value(a, t.ty.table64, u64::try_from(t.cells().len()).unwrap_or(0)))
    }

    fn table_grow<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let t = Self::table_of(a, this)?;
        let delta = Self::address(a, t.ty.table64, Self::arg(args, 0), "delta")?;
        let init = Self::present(args, 1);
        let init = Self::default_value(a, realm, WasmValType::Ref(t.ty.element), init)?;
        let old = WasmStore::grow_table(a.mc, t, init.0, delta);
        if old == u64::MAX {
            return Err(a.throw(ErrorKind::Range, "failed to grow table"));
        }
        Ok(Self::address_value(a, t.ty.table64, old))
    }

    fn table_index<'scope>(a: &mut Scope<'scope, '_>, t: &WasmTable<'scope>, v: Value<'scope>) -> Result<usize, Error> {
        let i = Self::address(a, t.ty.table64, v, "index")?;
        let i = usize::try_from(i).unwrap_or(usize::MAX);
        if i >= t.cells().len() {
            return Err(a.throw(ErrorKind::Range, "table index out of bounds"));
        }
        Ok(i)
    }

    fn table_get<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let t = Self::table_of(a, this)?;
        let i = Self::table_index(a, t, Self::arg(args, 0))?;
        let w = t.cells()[i].get();
        Ok(Self::to_js(a, realm, WasmValType::Ref(t.ty.element), (w, 0)))
    }

    fn table_set<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let t = Self::table_of(a, this)?;
        let i = Self::table_index(a, t, Self::arg(args, 0))?;
        let v = Self::present(args, 1);
        let w = Self::default_value(a, realm, WasmValType::Ref(t.ty.element), v)?;
        t.cells()[i].set(a.mc.gc_scope(), w.0);
        Ok(Value::Undefined)
    }

    fn new_global<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::require_new(a, Kind::Global)?;
        let desc = Self::descriptor(a, Self::arg(args, 0))?;
        let mv = Self::field(a, desc, "mutable")?;
        let mutable = a.to_boolean(mv);
        let tv = Self::field(a, desc, "value")?;
        let ts = a.to_string(tv)?;
        let content = Self::val_type(a, &String::from(&**ts))?;
        if content == WasmValType::V128 {
            return Err(a.throw(ErrorKind::Type, "v128 globals cannot be created from JavaScript"));
        }
        let v = Self::present(args, 1);
        let w = Self::default_value(a, realm, content, v)?;
        let g = a.mc.alloc(WasmGlobal::new(WasmGlobalType { content, mutable }, w));
        let o = Self::create(a, realm, Kind::Global, Ptr::of(g), None, &[])?;
        g.get().js.set(a.mc.gc_scope(), Value::from(o).bits());
        Ok(o.into())
    }

    fn global_of<'scope>(a: &Scope<'scope, '_>, this: Value<'scope>) -> Result<&'scope WasmGlobal<'scope>, Error> {
        let s = Self::this(a, this, Kind::Global)?;
        Ptr::get(s.word).ok_or_else(|| a.throw(ErrorKind::Type, "invalid global"))
    }

    fn global_value<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let g = Self::global_of(a, this)?;
        if Self::opaque(realm, g.ty.content) {
            return Err(a.throw(ErrorKind::Type, "type incompatible with JavaScript"));
        }
        Ok(Self::to_js(a, realm, g.ty.content, g.get()))
    }

    fn global_set<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let g = Self::global_of(a, this)?;
        if !g.ty.mutable {
            return Err(a.throw(ErrorKind::Type, "global is immutable"));
        }
        if Self::opaque(realm, g.ty.content) {
            return Err(a.throw(ErrorKind::Type, "type incompatible with JavaScript"));
        }
        let w = Self::to_wasm(a, realm, g.ty.content, Self::arg(args, 0))?;
        g.set(a.mc, w);
        Ok(Value::Undefined)
    }

    fn new_tag<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::require_new(a, Kind::Tag)?;
        let desc = Self::descriptor(a, Self::arg(args, 0))?;
        let pv = Self::field(a, desc, "parameters")?;
        let list = Self::iterate(a, pv)?;
        let mut params = Vec::with_capacity(list.len());
        for v in list {
            let s = a.to_string(v)?;
            params.push(Self::val_type(a, &String::from(&**s))?);
        }
        let tag = {
            let mut host = None;
            let store = Self::store(a, realm, &mut host);
            store.host_tag(&WasmFuncType { params: params.into(), results: Box::new([]) })
        };
        let o = Self::create(a, realm, Kind::Tag, Ptr::of(tag), None, &[])?;
        tag.get().js.set(a.mc.gc_scope(), Value::from(o).bits());
        Ok(o.into())
    }

    fn tag_arg<'scope>(a: &Scope<'scope, '_>, v: Value<'scope>) -> Result<ScopeLocal<'scope, WasmTag<'scope>>, Error> {
        Self::seen(v, Kind::Tag).and_then(|s| Ptr::local(s.word)).ok_or_else(|| a.throw(ErrorKind::Type, "argument is not a WebAssembly.Tag"))
    }

    fn tag_params(realm: Realm<'_>, tag: &WasmTag<'_>) -> Vec<WasmValType> {
        realm.types.get().0.borrow().get(tag.canon).sub.func().map(|f| f.params.to_vec()).unwrap_or_default()
    }

    fn new_exception<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::require_new(a, Kind::Exception)?;
        let tag = Self::tag_arg(a, Self::arg(args, 0))?;
        if Ptr::of(tag) == Ptr::of(realm.jstag()) {
            return Err(a.throw(ErrorKind::Type, "cannot construct an exception with JSTag"));
        }
        let params = Self::tag_params(realm, tag.get());
        let list = Self::iterate(a, Self::arg(args, 1))?;
        if list.len() != params.len() {
            return Err(a.throw(ErrorKind::Type, "payload does not match the tag"));
        }
        let mut payload = Vec::with_capacity(list.len());
        for (&t, v) in params.iter().zip(list) {
            payload.push(Self::to_wasm(a, realm, t, v)?);
        }
        let w = {
            let mut host = None;
            let store = Self::store(a, realm, &mut host);
            store.exception(tag, &payload)
        };
        let o = Self::create(a, realm, Kind::Exception, w, None, &[])?;
        if let Some(obj) = WasmObject::get(w) {
            obj.js().set(a.mc.gc_scope(), Value::from(o).bits());
        }
        let options = Self::arg(args, 2);
        if options.as_object().is_some() {
            let trace = a.get_named(options, "traceStack")?;
            if a.to_boolean(trace) {
                let s = a.string("");
                o.borrow_mut(a.mc).define("stack", Property::hidden(s.into()));
            }
        }
        Ok(o.into())
    }

    fn exception_of<'scope>(a: &Scope<'scope, '_>, this: Value<'scope>) -> Result<u64, Error> {
        Ok(Self::this(a, this, Kind::Exception)?.word)
    }

    fn exception_get_arg<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let w = Self::exception_of(a, this)?;
        let tag = Self::tag_arg(a, Self::arg(args, 0))?;
        if WasmStore::exception_tag(w).is_none_or(|t| Ptr::of(t) != Ptr::of(tag)) {
            return Err(a.throw(ErrorKind::Type, "exception does not have this tag"));
        }
        let i = Self::enforce_u32(a, Self::arg(args, 1), "index")?;
        let params = Self::tag_params(realm, tag.get());
        let payload = {
            let mut host = None;
            let store = Self::store(a, realm, &mut host);
            store.exception_payload(w)
        };
        let i = usize::try_from(i).unwrap_or(usize::MAX);
        let (Some(&t), Some(&v)) = (params.get(i), payload.get(i)) else { return Err(a.throw(ErrorKind::Range, "index out of range")) };
        if Self::opaque(realm, t) {
            return Err(a.throw(ErrorKind::Type, "type incompatible with JavaScript"));
        }
        Ok(Self::to_js(a, realm, t, v))
    }

    fn exception_is<'scope>(a: &mut Scope<'scope, '_>, _: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let w = Self::exception_of(a, this)?;
        let tag = Self::tag_arg(a, Self::arg(args, 0))?;
        Ok(WasmStore::exception_tag(w).is_some_and(|t| Ptr::of(t) == Ptr::of(tag)).into())
    }

    fn new_instance<'scope>(a: &mut Scope<'scope, '_>, realm: Realm<'scope>, _: JsObject<'scope>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Self::require_new(a, Kind::Instance)?;
        let m = Self::module_arg(a, Self::arg(args, 0))?;
        let proto = a.proto_from_ctor(a.new_target, realm.proto(Kind::Instance))?;
        Ok(Self::instance_object(a, realm, &m, Self::arg(args, 1), Some(proto))?.into())
    }

    fn instance_exports<'scope>(a: &mut Scope<'scope, '_>, _: Realm<'scope>, _: JsObject<'scope>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let s = Self::this(a, this, Kind::Instance)?;
        Ok(s.values.get(INSTANCE_EXPORTS).copied().unwrap_or_default())
    }
}
