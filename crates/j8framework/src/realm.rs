use core::fmt;
use core::ops::Deref;

use core::sync::atomic::AtomicU64;

use crate::{Collect, Cow, Gc, GcQueue, Mutation};
use paragc::Slot;

use crate::{ErrorKind, Job, JsObject, JsString, JsSymbol, Native, NativeFn, ObjectData, ObjectKind, Property, Scope, SymbolData, Value};

/// The spec's built-in Symbols (`Symbol.iterator` etc.).
#[derive(Clone, Copy, Collect)]
#[collect(no_drop)]
pub struct WellKnown<'gc> {
    pub iterator: JsSymbol<'gc>,
    pub async_iterator: JsSymbol<'gc>,
    pub has_instance: JsSymbol<'gc>,
    pub to_primitive: JsSymbol<'gc>,
    pub to_string_tag: JsSymbol<'gc>,
    pub species: JsSymbol<'gc>,
    pub unscopables: JsSymbol<'gc>,
    pub is_concat_spreadable: JsSymbol<'gc>,
    pub match_: JsSymbol<'gc>,
    pub match_all: JsSymbol<'gc>,
    pub replace: JsSymbol<'gc>,
    pub search: JsSymbol<'gc>,
    pub split: JsSymbol<'gc>,
}

impl<'gc> WellKnown<'gc> {
    /// Name ⇒ field; used by j8builtins to attach them to `Symbol`.
    pub fn all(&self) -> [(&'static str, JsSymbol<'gc>); 13] {
        [
            ("iterator", self.iterator),
            ("asyncIterator", self.async_iterator),
            ("hasInstance", self.has_instance),
            ("toPrimitive", self.to_primitive),
            ("toStringTag", self.to_string_tag),
            ("species", self.species),
            ("unscopables", self.unscopables),
            ("isConcatSpreadable", self.is_concat_spreadable),
            ("match", self.match_),
            ("matchAll", self.match_all),
            ("replace", self.replace),
            ("search", self.search),
            ("split", self.split),
        ]
    }
}

/// All built-in objects of an execution context: the global object and the prototypes. j8framework only creates empty
/// shells and links the prototype chains; the methods are installed by the [`Intrinsics`] implementation (j8builtins).
#[derive(Collect)]
#[collect(no_drop)]
pub struct Realm<'gc> {
    pub global: JsObject<'gc>,
    pub object_proto: JsObject<'gc>,
    pub function_proto: JsObject<'gc>,
    pub array_proto: JsObject<'gc>,
    pub string_proto: JsObject<'gc>,
    /// Single-character ASCII strings (so `s[i]` / `charAt` don't allocate each time)
    pub chars: alloc::vec::Vec<crate::JsString<'gc>>,
    pub number_proto: JsObject<'gc>,
    pub boolean_proto: JsObject<'gc>,
    /// Error prototypes ordered by [`ErrorKind`] (`Error.prototype` first)
    pub error_protos: [JsObject<'gc>; ErrorKind::ALL.len()],
    pub symbol_proto: JsObject<'gc>,
    /// %IteratorPrototype%: `[Symbol.iterator]() { return this }`
    pub iterator_proto: JsObject<'gc>,
    pub array_iterator_proto: JsObject<'gc>,
    pub string_iterator_proto: JsObject<'gc>,
    /// %GeneratorPrototype%: `next` / `return` / `throw`
    pub generator_proto: JsObject<'gc>,
    /// %GeneratorFunction.prototype%: the prototype of generator function objects
    pub generator_function_proto: JsObject<'gc>,
    /// %AsyncFunction.prototype%
    pub async_function_proto: JsObject<'gc>,
    /// %AsyncGeneratorFunction.prototype%
    pub async_generator_function_proto: JsObject<'gc>,
    /// %AsyncIteratorPrototype%: `[Symbol.asyncIterator]() { return this }`
    pub async_iterator_proto: JsObject<'gc>,
    /// %AsyncGeneratorPrototype%: `next` / `return` / `throw` all return Promises
    pub async_generator_proto: JsObject<'gc>,
    pub promise_proto: JsObject<'gc>,
    /// %Promise% (j8builtins installs it as a constructor)
    pub promise_ctor: JsObject<'gc>,
    pub regexp_proto: JsObject<'gc>,
    /// The constructor used by regex literals: j8builtins swaps in the real implementation (user code overwriting the
    /// global `RegExp` doesn't affect literals)
    pub regexp_ctor: JsObject<'gc>,
    /// %eval%: a direct call to it in strict code is treated as a direct eval
    pub eval: JsObject<'gc>,
    pub bigint_proto: JsObject<'gc>,
    pub map_proto: JsObject<'gc>,
    pub set_proto: JsObject<'gc>,
    pub weakmap_proto: JsObject<'gc>,
    pub weakset_proto: JsObject<'gc>,
    /// The prototype of Map / Set iterators (`next`)
    pub collection_iterator_proto: JsObject<'gc>,
    /// %SetIteratorPrototype% (`collection_iterator_proto` is Map's)
    pub set_iterator_proto: JsObject<'gc>,
    /// %IteratorHelperPrototype%
    pub iterator_helper_proto: JsObject<'gc>,
    /// %WrapForValidIteratorPrototype%
    pub wrap_iterator_proto: JsObject<'gc>,
    pub date_proto: JsObject<'gc>,
    pub weak_ref_proto: JsObject<'gc>,
    pub finalization_registry_proto: JsObject<'gc>,
    /// %RegExpStringIteratorPrototype%
    pub regexp_string_iterator_proto: JsObject<'gc>,
    pub array_buffer_proto: JsObject<'gc>,
    pub shared_array_buffer_proto: JsObject<'gc>,
    pub data_view_proto: JsObject<'gc>,
    /// %TypedArray% and %TypedArray.prototype%
    pub typed_array_ctor: JsObject<'gc>,
    pub typed_array_proto: JsObject<'gc>,
    /// Constructors and prototypes of each concrete TypedArray (ordered by
    /// [`ElementKind::ALL`](crate::ElementKind::ALL))
    pub typed_ctors: [JsObject<'gc>; 12],
    pub typed_protos: [JsObject<'gc>; 12],
    /// Script scope: top-level `let` / `const` / `class` of each script (shared across scripts, not properties of the
    /// global object)
    pub script_scope: JsObject<'gc>,
    pub symbols: WellKnown<'gc>,
    /// Loaded modules (the index is the module id)
    pub modules: Cow<'gc, alloc::vec::Vec<crate::ModuleRecord<'gc>>>,
    /// The global registry of `Symbol.for`
    pub symbol_registry: Cow<'gc, alloc::vec::Vec<(JsString<'gc>, JsSymbol<'gc>)>>,
    /// The state of `Math.random` (xorshift; 0 means not yet seeded)
    pub random_state: AtomicU64,
    /// The microtask queue (lock-free)
    pub jobs: GcQueue<'gc, Job<'gc>>,
    /// Template objects of tagged templates: cached by (function index, call site), so the same site always gets the
    /// same one
    pub templates: Cow<'gc, alloc::collections::BTreeMap<(u32, u32), JsObject<'gc>>>,
    /// Compiled regexes shared by (source, flags) (compiled ones are immutable): a direct-mapped cache, one atomic slot
    /// per bucket ([`RegExpCompiled::bucket`]); a miss overwrites the bucket
    pub regexp_cache: [Slot<'gc, RegExpCompiled<'gc>>; RegExpCompiled::BUCKETS],
    /// What `RegExp.prototype` looked like when installed (built-in regex methods use it to decide whether they can
    /// skip the protocol and match directly)
    pub regexp_pristine: Cow<'gc, Option<RegExpPristine<'gc>>>,
    /// The built-in array iterator / string iterator `next` (when an iterator record's `next` is one of them, no result
    /// object is created; it steps directly)
    pub builtin_nexts: Cow<'gc, [Option<JsObject<'gc>>; 2]>,
}

/// The shape of `RegExp.prototype` when installed, plus the slots holding `exec` / `flags` / each flag getter and their
/// original contents.
#[derive(Clone, Collect)]
#[collect(no_drop)]
pub struct RegExpPristine<'gc> {
    #[collect(require_static)]
    pub shape: alloc::sync::Arc<crate::Shape>,
    pub slots: alloc::vec::Vec<(usize, crate::Slot<'gc>)>,
}

/// One compiled regex: source, flags, the execution layer's compiled result.
#[derive(Clone, Collect)]
#[collect(no_drop)]
pub struct RegExpCompiled<'gc> {
    pub source: crate::SmallStr,
    pub flags: crate::SmallStr,
    pub compiled: Gc<'gc, crate::Static<alloc::boxed::Box<dyn core::any::Any + Send + Sync>>>,
}

impl RegExpCompiled<'_> {
    /// Buckets of the realm's regex cache
    pub const BUCKETS: usize = 64;
    /// FNV-1a offset basis and prime (64-bit)
    const FNV: (u64, u64) = (0xcbf2_9ce4_8422_2325, 0x0100_0000_01b3);

    /// The cache bucket of (source, flags)
    pub fn bucket(source: &str, flags: &str) -> usize {
        let h = source.bytes().chain([0]).chain(flags.bytes()).fold(Self::FNV.0, |h, b| (h ^ u64::from(b)).wrapping_mul(Self::FNV.1));
        // The modulus is below BUCKETS, so it fits
        usize::try_from(h % Self::BUCKETS as u64).unwrap_or(0)
    }
}

impl fmt::Debug for Realm<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Realm")
    }
}

impl<'gc> Realm<'gc> {
    fn new(mc: &Mutation<'gc>) -> Self {
        let object_proto = ObjectData::default().alloc(mc);
        let derived = |proto: JsObject<'gc>| ObjectData { proto: Some(proto), ..ObjectData::default() }.alloc(mc);
        let error = derived(object_proto);
        let error_protos = ErrorKind::ALL.map(|k| if k == ErrorKind::Error { error } else { derived(error) });
        let symbol = |d: &str| mc.alloc(SymbolData { description: Some(alloc::format!("Symbol.{d}").into()) });
        let iterator_proto = derived(object_proto);
        let async_iterator_proto = derived(object_proto);
        let function_proto = derived(object_proto);
        let placeholder = |proto: JsObject<'gc>| ObjectData { kind: ObjectKind::Native(Native::function(Self::no_regexp)), proto: Some(proto), ..ObjectData::default() }.alloc(mc);
        let typed_array_ctor = placeholder(function_proto);
        let typed_array_proto = derived(object_proto);
        Self {
            date_proto: derived(object_proto),
            weak_ref_proto: derived(object_proto),
            finalization_registry_proto: derived(object_proto),
            regexp_string_iterator_proto: derived(iterator_proto),
            array_buffer_proto: derived(object_proto),
            shared_array_buffer_proto: derived(object_proto),
            data_view_proto: derived(object_proto),
            typed_array_ctor,
            typed_array_proto,
            typed_ctors: [(); 12].map(|()| placeholder(typed_array_ctor)),
            typed_protos: [(); 12].map(|()| derived(typed_array_proto)),
            symbol_proto: derived(object_proto),
            iterator_proto,
            array_iterator_proto: derived(iterator_proto),
            string_iterator_proto: derived(iterator_proto),
            generator_proto: derived(iterator_proto),
            async_iterator_proto,
            generator_function_proto: derived(function_proto),
            async_function_proto: derived(function_proto),
            async_generator_function_proto: derived(function_proto),
            async_generator_proto: derived(async_iterator_proto),
            promise_proto: derived(object_proto),
            promise_ctor: placeholder(function_proto),
            regexp_proto: derived(object_proto),
            regexp_ctor: ObjectData { kind: ObjectKind::Native(Native::function(Self::no_regexp)), ..ObjectData::default() }.alloc(mc),
            eval: ObjectData { kind: ObjectKind::Native(Native::function(Self::no_eval)), proto: Some(function_proto), ..ObjectData::default() }.alloc(mc),
            bigint_proto: derived(object_proto),
            map_proto: derived(object_proto),
            set_proto: derived(object_proto),
            weakmap_proto: derived(object_proto),
            weakset_proto: derived(object_proto),
            collection_iterator_proto: derived(iterator_proto),
            set_iterator_proto: derived(iterator_proto),
            iterator_helper_proto: derived(iterator_proto),
            wrap_iterator_proto: derived(iterator_proto),
            script_scope: ObjectData::default().alloc(mc),
            symbols: WellKnown {
                iterator: symbol("iterator"),
                async_iterator: symbol("asyncIterator"),
                has_instance: symbol("hasInstance"),
                to_primitive: symbol("toPrimitive"),
                to_string_tag: symbol("toStringTag"),
                species: symbol("species"),
                unscopables: symbol("unscopables"),
                is_concat_spreadable: symbol("isConcatSpreadable"),
                match_: symbol("match"),
                match_all: symbol("matchAll"),
                replace: symbol("replace"),
                search: symbol("search"),
                split: symbol("split"),
            },
            jobs: GcQueue::new(mc),
            regexp_cache: core::array::from_fn(|_| Slot::default()),
            regexp_pristine: Cow::new(mc, None),
            builtin_nexts: Cow::new(mc, [None, None]),
            symbol_registry: Cow::new(mc, alloc::vec::Vec::new()),
            modules: Cow::new(mc, alloc::vec::Vec::new()),
            random_state: AtomicU64::new(0),
            templates: Cow::new(mc, alloc::collections::BTreeMap::new()),
            global: derived(object_proto),
            object_proto,
            function_proto,
            array_proto: derived(object_proto),
            string_proto: derived(object_proto),
            chars: (0u8..128).map(|c| mc.alloc(crate::JsStr::from(alloc::string::String::from(char::from(c)).as_str()))).collect(),
            number_proto: derived(object_proto),
            boolean_proto: derived(object_proto),
            error_protos,
        }
    }

    /// Without a regex engine, regex literals throw a SyntaxError.
    fn no_regexp<'g>(a: &mut crate::Agent<'g, '_>, _: Value<'g>, _: &[Value<'g>]) -> Result<Value<'g>, crate::Error> {
        Err(a.throw(ErrorKind::Syntax, "regular expressions are not available in this realm"))
    }

    fn no_eval<'g>(a: &mut crate::Agent<'g, '_>, _: Value<'g>, _: &[Value<'g>]) -> Result<Value<'g>, crate::Error> {
        Err(a.throw(ErrorKind::Type, "eval is not available in this realm"))
    }

    pub fn error_proto(&self, kind: ErrorKind) -> JsObject<'gc> {
        self.error_protos[kind as usize]
    }

    /// A host function object (its prototype is `Function.prototype`).
    pub fn native(&self, mc: &Mutation<'gc>, f: NativeFn) -> JsObject<'gc> {
        ObjectData { kind: ObjectKind::Native(Native::function(f)), proto: Some(self.function_proto), ..ObjectData::default() }.alloc(mc)
    }

    /// Define `name` (and optionally `length`) on a function object: non-enumerable, non-writable, configurable,
    /// `length` first.
    pub fn name_function(&self, mc: &Mutation<'gc>, f: JsObject<'gc>, name: &str, length: Option<u32>) {
        let prop = |v: Value<'gc>| Property { slot: crate::Slot::Data(v), enumerable: false, writable: false, configurable: true };
        let d = f.borrow_mut(mc);
        if let Some(n) = length {
            d.define("length", prop(f64::from(n).into()));
        }
        d.define("name", prop(Value::String(mc.alloc(crate::JsStr::from(name)))));
    }

    /// A host function object with a `name`.
    pub fn named_native(&self, mc: &Mutation<'gc>, f: NativeFn, name: &str, length: u32) -> JsObject<'gc> {
        let func = self.native(mc, f);
        self.name_function(mc, func, name, Some(length));
        func
    }

    /// Attach a Symbol-keyed host method on `target` (its name is `[description]`).
    pub fn symbol_method(&self, mc: &Mutation<'gc>, target: JsObject<'gc>, key: JsSymbol<'gc>, f: NativeFn, length: u32) {
        let func = self.named_native(mc, f, &alloc::format!("[{}]", key.description.as_deref().unwrap_or("")), length);
        target.borrow_mut(mc).define_symbol(key, Property::hidden(Value::Object(func)));
    }

    /// Attach a host method on `target`.
    pub fn method(&self, mc: &Mutation<'gc>, target: JsObject<'gc>, name: &str, f: NativeFn, length: u32) {
        let func = self.named_native(mc, f, name, length);
        target.borrow_mut(mc).define(name, Property::hidden(Value::Object(func)));
    }
}

/// An execution context: points to a [`Realm`]. To keep it across `enter`, store `global` as a
/// [`Global`](crate::Global).
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Context<'gc>(Gc<'gc, Realm<'gc>>);

impl<'gc> Context<'gc> {
    /// The realm on the heap
    pub fn realm(self) -> Gc<'gc, Realm<'gc>> {
        self.0
    }
}

impl<'gc> From<Gc<'gc, Realm<'gc>>> for Context<'gc> {
    fn from(r: Gc<'gc, Realm<'gc>>) -> Self {
        Self(r)
    }
}

impl<'gc> Deref for Context<'gc> {
    type Target = Realm<'gc>;

    fn deref(&self) -> &Realm<'gc> {
        &self.0
    }
}

/// Installs built-in objects into a newly created [`Realm`] (implemented by j8builtins).
pub trait Intrinsics {
    fn install<'gc>(&self, mc: &Mutation<'gc>, realm: &Realm<'gc>);
}

impl<'gc> Scope<'gc, '_> {
    /// Create a new execution context and install its built-in objects.
    pub fn context(&self, intrinsics: &dyn Intrinsics) -> Context<'gc> {
        // Into the heap first: installing writes the realm's slots, and the write barrier records their addresses
        let realm = self.mc.alloc(Realm::new(self.mc));
        intrinsics.install(self.mc, realm.get());
        Context(realm)
    }

}
