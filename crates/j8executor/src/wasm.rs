use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

mod entity;
mod gc;
mod simd;
mod types;

pub use entity::{CellsBrand, FuncBrand, GlobalBrand, InstanceBrand, MemoryBrand, TableBrand, TagBrand, TypesBrand, WasmCells, WasmCompiled, WasmFunc, WasmGlobal, WasmInstance, WasmMemory, WasmTable, WasmTag, WasmTypes};
use entity::{Code, Ptr};
use gc::{Objects, WasmObject, Word};
use j8bytecode::{Decoded, OperandValue, WasmBytecode, WasmCatchKind, WasmCompositeType, WasmExternKind, WasmFuncType, WasmGlobalType, WasmImportKind, WasmMemoryType, WasmSegmentMode, WasmStorageType, WasmTableType, WasmValType};
use j8framework::{GcSlot, IsoCell, Mutation, ScopeLocal, Static, WordSlot};
use j8lockfree::AtomicRefCell;
use types::{Hierarchy, Layout, Types};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmTrap {
    Unreachable,
    IntegerDivideByZero,
    IntegerOverflow,
    InvalidConversion,
    OutOfBoundsMemory,
    OutOfBoundsTable,
    UndefinedElement,
    UninitializedElement,
    IndirectCallTypeMismatch,
    NullReference,
    CallStackExhausted,
    NullStructure,
    NullArray,
    NullI31,
    CastFailure,
    OutOfBoundsArray,
    Exception,
    Unsupported,
}

impl WasmTrap {
    pub const fn message(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::IntegerDivideByZero => "integer divide by zero",
            Self::IntegerOverflow => "integer overflow",
            Self::InvalidConversion => "invalid conversion to integer",
            Self::OutOfBoundsMemory => "out of bounds memory access",
            Self::OutOfBoundsTable => "out of bounds table access",
            Self::UndefinedElement => "undefined element",
            Self::UninitializedElement => "uninitialized element",
            Self::IndirectCallTypeMismatch => "indirect call type mismatch",
            Self::NullReference => "null reference",
            Self::CallStackExhausted => "call stack exhausted",
            Self::NullStructure => "null structure reference",
            Self::NullArray => "null array reference",
            Self::NullI31 => "null i31 reference",
            Self::CastFailure => "cast failure",
            Self::OutOfBoundsArray => "out of bounds array access",
            Self::Exception => "uncaught exception",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmThrow {
    Trap(WasmTrap),
    Exception(u64),
}

impl From<WasmTrap> for WasmThrow {
    fn from(t: WasmTrap) -> Self {
        Self::Trap(t)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmRef {
    Null,
    Func,
    Host(u32),
    I31(i32),
    Struct,
    Array,
    Exn,
    Js(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    V128(u128),
    Ref(WasmRef),
}

impl WasmValue {
    pub fn vector(self) -> u128 {
        if let Self::V128(v) = self { v } else { 0 }
    }

    pub fn slot(self) -> u64 {
        match self {
            Self::I32(v) => u64::from(v.cast_unsigned()),
            Self::I64(v) => v.cast_unsigned(),
            Self::F32(v) => u64::from(v),
            Self::F64(v) => v,
            Self::V128(v) => u64::try_from(v & u128::from(u64::MAX)).unwrap_or(0),
            Self::Ref(r) => u64::from(match r {
                WasmRef::Host(n) => Word::Host(n),
                WasmRef::I31(v) => Word::I31(v),
                WasmRef::Js(w) => Word::Js(w),
                _ => Word::Null,
            }),
        }
    }

    pub fn words(self) -> (u64, u128) {
        (self.slot(), self.vector())
    }
}

#[derive(Clone, Copy)]
struct Slot(u64);

impl Slot {
    fn u32(self) -> u32 {
        u32::try_from(self.0 & u64::from(u32::MAX)).unwrap_or(0)
    }

    fn i32(self) -> i32 {
        self.u32().cast_signed()
    }

    fn f32(self) -> f32 {
        f32::from_bits(self.u32())
    }

    fn f64(self) -> f64 {
        f64::from_bits(self.0)
    }
}

#[derive(Clone, Copy)]
pub enum WasmExtern<'scope> {
    Func(ScopeLocal<'scope, WasmFunc<'scope>>),
    Table(ScopeLocal<'scope, WasmTable<'scope>>),
    Memory(ScopeLocal<'scope, WasmMemory<'scope>>),
    Global(ScopeLocal<'scope, WasmGlobal<'scope>>),
    Tag(ScopeLocal<'scope, WasmTag<'scope>>),
}

impl From<WasmExtern<'_>> for WasmExternKind {
    fn from(e: WasmExtern<'_>) -> Self {
        match e {
            WasmExtern::Func(_) => Self::Func,
            WasmExtern::Table(_) => Self::Table,
            WasmExtern::Memory(_) => Self::Memory,
            WasmExtern::Global(_) => Self::Global,
            WasmExtern::Tag(_) => Self::Tag,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmLinkError {
    IncompatibleImport,
    MissingImport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmInstantiateError {
    Link(WasmLinkError),
    Trap(WasmThrow),
}

impl From<WasmThrow> for WasmInstantiateError {
    fn from(t: WasmThrow) -> Self {
        Self::Trap(t)
    }
}

impl From<WasmTrap> for WasmInstantiateError {
    fn from(t: WasmTrap) -> Self {
        Self::Trap(WasmThrow::Trap(t))
    }
}

pub trait WasmHost<'scope> {
    fn call(&mut self, mc: &Mutation<'scope>, func: &'scope WasmFunc<'scope>, ty: &WasmFuncType, args: &[(u64, u128)]) -> Result<Vec<(u64, u128)>, WasmThrow>;
}

struct Activation<'scope> {
    instance: &'scope WasmInstance<'scope>,
    code: Arc<Code>,
    fp: usize,
    pc: usize,
}

const MAX_DEPTH: usize = 50_000;
const MAX_PAGES: u64 = 65_536;
const MAX_TABLE: u64 = 10_000_000;
const FLAG_TO_NULLABLE: OperandValue = 0b10;

impl WasmTypes {
    pub fn alloc<'scope>(mc: &Mutation<'scope>) -> ScopeLocal<'scope, Self> {
        mc.alloc(Self(Static(AtomicRefCell::new(Types::default()))))
    }
}

pub struct WasmStore<'a, 'scope> {
    mc: &'a Mutation<'scope>,
    types: &'scope WasmTypes,
    host: &'a mut dyn WasmHost<'scope>,
    stack: Vec<u64>,
    vstack: Vec<u128>,
    thrown: u64,
}

impl<'a, 'scope> WasmStore<'a, 'scope> {
    pub fn new(mc: &'a Mutation<'scope>, types: ScopeLocal<'scope, WasmTypes>, host: &'a mut dyn WasmHost<'scope>) -> Self {
        Self { mc, types: types.get(), host, stack: Vec::new(), vstack: Vec::new(), thrown: 0 }
    }

    fn registry(&self) -> j8lockfree::AtomicRef<'_, Types> {
        self.types.0.borrow()
    }

    pub fn func_type_id(&self, ty: &WasmFuncType) -> u32 {
        self.types.0.borrow_mut().func(ty)
    }

    pub fn func_type(&self, f: &WasmFunc<'_>) -> WasmFuncType {
        self.registry().get(f.canon).sub.func().cloned().unwrap_or_default()
    }

    pub fn host_func(&self, ty: &WasmFuncType, host: u64) -> ScopeLocal<'scope, WasmFunc<'scope>> {
        let canon = self.func_type_id(ty);
        self.mc.alloc(WasmFunc { canon, index: 0, instance: GcSlot::default(), host: WordSlot::new(host), js: WordSlot::new(0) })
    }

    pub fn host_table(&self, ty: WasmTableType, init: u64) -> ScopeLocal<'scope, WasmTable<'scope>> {
        let n = usize::try_from(ty.limits.min).unwrap_or(0);
        let cells = WasmCells::alloc(self.mc, core::iter::repeat_n(init, n));
        self.mc.alloc(WasmTable { ty: Static(ty), cells: GcSlot::from(cells), js: WordSlot::new(0) })
    }

    pub fn host_memory(&self, ty: WasmMemoryType) -> ScopeLocal<'scope, WasmMemory<'scope>> {
        self.mc.alloc(WasmMemory::new(ty))
    }

    pub fn host_global(&self, ty: WasmGlobalType, v: (u64, u128)) -> ScopeLocal<'scope, WasmGlobal<'scope>> {
        self.mc.alloc(WasmGlobal::new(ty, v))
    }

    pub fn host_tag(&self, ty: &WasmFuncType) -> ScopeLocal<'scope, WasmTag<'scope>> {
        let canon = self.func_type_id(ty);
        self.mc.alloc(WasmTag { canon, js: WordSlot::new(0) })
    }

    pub fn export(instance: &WasmInstance<'scope>, name: &str) -> Option<WasmExtern<'scope>> {
        let e = instance.module.module.export(name)?;
        let i = e.index as usize;
        Some(match e.kind {
            WasmExternKind::Func => WasmExtern::Func(instance.funcs[i].get()?),
            WasmExternKind::Table => WasmExtern::Table(instance.tables[i].get()?),
            WasmExternKind::Memory => WasmExtern::Memory(instance.memories[i].get()?),
            WasmExternKind::Global => WasmExtern::Global(instance.globals[i].get()?),
            WasmExternKind::Tag => WasmExtern::Tag(instance.tags[i].get()?),
        })
    }

    pub fn value(&self, ty: WasmValType, (s, v): (u64, u128)) -> WasmValue {
        match ty {
            WasmValType::I32 => WasmValue::I32(Slot(s).i32()),
            WasmValType::I64 => WasmValue::I64(s.cast_signed()),
            WasmValType::F32 => WasmValue::F32(Slot(s).u32()),
            WasmValType::F64 => WasmValue::F64(s),
            WasmValType::V128 => WasmValue::V128(v),
            WasmValType::Ref(t) => {
                let h = self.registry().hierarchy(t.heap);
                WasmValue::Ref(match Word::decode(s, h) {
                    Word::Null => WasmRef::Null,
                    Word::Func(_) => WasmRef::Func,
                    Word::Host(n) => WasmRef::Host(n),
                    Word::I31(v) => WasmRef::I31(v),
                    Word::Js(w) => WasmRef::Js(w),
                    Word::Object(_) if h == Hierarchy::Exn => WasmRef::Exn,
                    Word::Object(w) => match self.composite(w) {
                        Some(true) => WasmRef::Array,
                        _ => WasmRef::Struct,
                    },
                })
            }
        }
    }

    fn composite(&self, w: u64) -> Option<bool> {
        let o = WasmObject::get(w)?;
        Some(matches!(self.registry().get(o.head()).sub.composite, WasmCompositeType::Array(_)))
    }

    pub fn hierarchy(&self, h: j8bytecode::WasmHeapType) -> Hierarchy {
        self.registry().hierarchy(h)
    }

    fn limits_match(have: (u64, Option<u64>), want: (u64, Option<u64>)) -> bool {
        have.0 >= want.0 && match (have.1, want.1) {
            (_, None) => true,
            (Some(h), Some(w)) => h <= w,
            (None, Some(_)) => false,
        }
    }

    fn link(&self, kind: &WasmImportKind, ext: WasmExtern<'scope>, ids: &[u32]) -> Result<(), WasmLinkError> {
        let canon = |i: u32| ids[i as usize];
        let types = self.registry();
        let ok = match (kind, ext) {
            (WasmImportKind::Func(t), WasmExtern::Func(f)) => types.sub(f.get().canon, canon(*t)),
            (WasmImportKind::Table(t), WasmExtern::Table(x)) => {
                let have = x.get();
                let size = u64::try_from(have.cells().len()).unwrap_or(0);
                WasmValType::Ref(have.ty.element) == WasmValType::Ref(t.element).map(&canon) && have.ty.table64 == t.table64 && Self::limits_match((size, have.ty.limits.max), (t.limits.min, t.limits.max))
            }
            (WasmImportKind::Memory(t), WasmExtern::Memory(x)) => {
                let have = x.get();
                have.ty.memory64 == t.memory64 && have.ty.page_size_log2 == t.page_size_log2 && Self::limits_match((have.pages(), have.ty.limits.max), (t.limits.min, t.limits.max))
            }
            (WasmImportKind::Global(t), WasmExtern::Global(g)) => {
                let have = *g.get().ty;
                let want = t.content.map(&canon);
                have.mutable == t.mutable && if t.mutable { have.content == want } else { types.val_sub(have.content, want) }
            }
            (WasmImportKind::Tag(t), WasmExtern::Tag(x)) => x.get().canon == canon(*t),
            _ => false,
        };
        if ok { Ok(()) } else { Err(WasmLinkError::IncompatibleImport) }
    }

    pub fn instantiate(&mut self, compiled: &Arc<WasmCompiled>, imports: &[WasmExtern<'scope>]) -> Result<ScopeLocal<'scope, WasmInstance<'scope>>, WasmInstantiateError> {
        let module = &compiled.module;
        if imports.len() != module.imports.len() {
            return Err(WasmInstantiateError::Link(WasmLinkError::MissingImport));
        }
        let ids = self.types.0.borrow_mut().canonicalize(module);
        for (imp, &ext) in module.imports.iter().zip(imports) {
            self.link(&imp.kind, ext, &ids).map_err(WasmInstantiateError::Link)?;
        }
        let canon = |i: u32| ids[i as usize];
        let count = |k: WasmExternKind, defined: usize| module.imported(k).count() + defined;
        let (mut funcs, mut tables, mut memories, mut globals, mut tags) = (Self::slots(module.functions.len()), Self::slots(count(WasmExternKind::Table, module.tables.len())), Self::slots(count(WasmExternKind::Memory, module.memories.len())), Self::slots(count(WasmExternKind::Global, module.globals.len())), Self::slots(count(WasmExternKind::Tag, module.tags.len())));
        let mut at = [0usize; 5];
        for &ext in imports {
            match ext {
                WasmExtern::Func(f) => (funcs[at[0]], at[0]) = (GcSlot::from(f), at[0] + 1),
                WasmExtern::Table(t) => (tables[at[1]], at[1]) = (GcSlot::from(t), at[1] + 1),
                WasmExtern::Memory(m) => (memories[at[2]], at[2]) = (GcSlot::from(m), at[2] + 1),
                WasmExtern::Global(g) => (globals[at[3]], at[3]) = (GcSlot::from(g), at[3] + 1),
                WasmExtern::Tag(t) => (tags[at[4]], at[4]) = (GcSlot::from(t), at[4] + 1),
            }
        }
        for (i, m) in module.memories.iter().enumerate() {
            memories[at[2] + i] = GcSlot::from(self.host_memory(*m));
        }
        for (i, &t) in module.tags.iter().enumerate() {
            tags[at[4] + i] = GcSlot::from(self.mc.alloc(WasmTag { canon: canon(t), js: WordSlot::new(0) }));
        }
        let mc = self.mc;
        let inst = mc.alloc(WasmInstance {
            module: Static(compiled.clone()),
            types: Static(ids.clone().into()),
            funcs: funcs.into(),
            tables: tables.into(),
            memories: memories.into(),
            globals: globals.into(),
            tags: tags.into(),
            elems: Self::slots(module.elements.len()).into(),
            data: module.data.iter().map(|_| IsoCell::new(false)).collect(),
            js: WordSlot::new(0),
        });
        let instance = inst.get();
        for (i, f) in module.functions.iter().enumerate().skip(at[0]) {
            let func = mc.alloc(WasmFunc { canon: canon(f.ty), index: u32::try_from(i).unwrap_or(0), instance: GcSlot::from(inst), host: WordSlot::new(0), js: WordSlot::new(0) });
            instance.funcs[i].set(mc.gc_scope(), Some(func));
        }
        for (i, g) in module.globals.iter().enumerate() {
            let v = self.eval(instance, g.init)?;
            let ty = WasmGlobalType { content: g.ty.content.map(&canon), mutable: g.ty.mutable };
            instance.globals[at[3] + i].set(mc.gc_scope(), Some(self.host_global(ty, v)));
        }
        for (i, t) in module.tables.iter().enumerate() {
            let init = match t.init {
                Some(x) => self.eval(instance, x)?.0,
                None => 0,
            };
            let element = match WasmValType::Ref(t.ty.element).map(&canon) {
                WasmValType::Ref(r) => r,
                _ => t.ty.element,
            };
            instance.tables[at[1] + i].set(mc.gc_scope(), Some(self.host_table(WasmTableType { element, ..t.ty }, init)));
        }
        for (i, el) in module.elements.iter().enumerate() {
            let mut items = Vec::with_capacity(el.items.len());
            for &x in &el.items {
                items.push(self.eval(instance, x)?.0);
            }
            instance.elems[i].set(mc.gc_scope(), Some(WasmCells::alloc(mc, items)));
        }
        for (i, el) in module.elements.iter().enumerate() {
            match el.mode {
                WasmSegmentMode::Active { index, offset } => {
                    let at = self.eval(instance, offset)?.0;
                    let n = u64::try_from(el.items.len()).unwrap_or(0);
                    Self::table_init(mc, instance, index, u32::try_from(i).unwrap_or(0), at, 0, n)?;
                    instance.elems[i].set(mc.gc_scope(), None);
                }
                WasmSegmentMode::Declared => instance.elems[i].set(mc.gc_scope(), None),
                WasmSegmentMode::Passive => {}
            }
        }
        for (i, d) in module.data.iter().enumerate() {
            if let WasmSegmentMode::Active { index, offset } = d.mode {
                let at = self.eval(instance, offset)?.0;
                let n = u64::try_from(d.bytes.len()).unwrap_or(0);
                Self::memory_init(instance, index, u32::try_from(i).unwrap_or(0), at, 0, n)?;
                instance.data[i].set(true);
            }
        }
        if let Some(s) = module.start {
            let f = instance.funcs[s as usize].get().ok_or(WasmTrap::Unsupported)?;
            self.invoke(f, &[])?;
        }
        Ok(inst)
    }

    fn slots<T>(n: usize) -> Vec<GcSlot<'scope, T>> {
        (0..n).map(|_| GcSlot::default()).collect()
    }

    fn eval(&mut self, instance: &'scope WasmInstance<'scope>, init: u32) -> Result<(u64, u128), WasmThrow> {
        let fp = self.stack.len();
        let code = instance.module.init(init);
        let frame = self.frame(instance, code, fp);
        self.call(frame)?;
        let v = (self.stack[fp], self.vstack[fp]);
        self.truncate(fp);
        Ok(v)
    }

    fn truncate(&mut self, n: usize) {
        self.stack.truncate(n);
        self.vstack.truncate(n);
    }

    fn reserve(&mut self, n: usize) {
        if self.stack.len() < n {
            self.stack.resize(n, 0);
            self.vstack.resize(n, 0);
        }
    }

    fn copy(&mut self, from: usize, to: usize, n: usize) {
        self.stack.copy_within(from..from + n, to);
        self.vstack.copy_within(from..from + n, to);
    }

    pub fn invoke(&mut self, func: ScopeLocal<'scope, WasmFunc<'scope>>, args: &[(u64, u128)]) -> Result<Vec<(u64, u128)>, WasmThrow> {
        let n = self.func_type(func.get()).results.len();
        let fp = self.stack.len();
        self.stack.extend(args.iter().map(|a| a.0));
        self.vstack.extend(args.iter().map(|a| a.1));
        self.reserve(fp + n);
        let r = self.enter(func.get(), fp);
        let out = r.map(|()| (0..n).map(|i| (self.stack[fp + i], self.vstack[fp + i])).collect());
        self.truncate(fp);
        let thrown = core::mem::take(&mut self.thrown);
        out.map_err(|t| if t == WasmTrap::Exception { WasmThrow::Exception(thrown) } else { WasmThrow::Trap(t) })
    }

    fn enter(&mut self, func: &'scope WasmFunc<'scope>, fp: usize) -> Result<(), WasmTrap> {
        match func.instance.get() {
            Some(inst) => {
                let instance = inst.get();
                let frame = self.frame(instance, instance.module.code(func.index), fp);
                self.call(frame)
            }
            None => self.host(func, fp),
        }
    }

    fn host(&mut self, func: &'scope WasmFunc<'scope>, fp: usize) -> Result<(), WasmTrap> {
        let ty = self.func_type(func);
        let args: Vec<(u64, u128)> = (0..ty.params.len()).map(|i| (self.stack.get(fp + i).copied().unwrap_or(0), self.vstack.get(fp + i).copied().unwrap_or(0))).collect();
        let results = match self.host.call(self.mc, func, &ty, &args) {
            Ok(r) => r,
            Err(WasmThrow::Trap(t)) => return Err(t),
            Err(WasmThrow::Exception(w)) => {
                self.thrown = w;
                return Err(WasmTrap::Exception);
            }
        };
        self.reserve(fp + results.len());
        for (i, (s, v)) in results.into_iter().enumerate() {
            self.stack[fp + i] = s;
            self.vstack[fp + i] = v;
        }
        Ok(())
    }

    fn frame(&mut self, instance: &'scope WasmInstance<'scope>, code: Arc<Code>, fp: usize) -> Activation<'scope> {
        let end = fp + code.registers as usize;
        self.reserve(end);
        self.stack[fp + code.params as usize..end].fill(0);
        self.vstack[fp + code.params as usize..end].fill(0);
        Activation { instance, code, fp, pc: 0 }
    }

    fn table_init(mc: &Mutation<'scope>, instance: &'scope WasmInstance<'scope>, table: u32, elem: u32, d: u64, s: u64, n: u64) -> Result<(), WasmTrap> {
        let items = instance.elem(elem as usize);
        let cells = instance.table(table as usize).cells();
        let (d, s, n) = (usize::try_from(d).unwrap_or(usize::MAX), usize::try_from(s).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
        if s.checked_add(n).is_none_or(|e| e > items.len()) || d.checked_add(n).is_none_or(|e| e > cells.len()) {
            return Err(WasmTrap::OutOfBoundsTable);
        }
        for (c, i) in cells[d..d + n].iter().zip(&items[s..s + n]) {
            c.set(mc.gc_scope(), i.get());
        }
        Ok(())
    }

    fn memory_init(instance: &WasmInstance<'_>, memory: u32, data: u32, d: u64, s: u64, n: u64) -> Result<(), WasmTrap> {
        let m = instance.memory(memory as usize);
        let bytes = instance.data(data as usize);
        let (s, n) = (usize::try_from(s).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
        let d = usize::try_from(m.index(d)).unwrap_or(usize::MAX);
        let mem = m.data();
        if s.checked_add(n).is_none_or(|e| e > bytes.len()) || d.checked_add(n).is_none_or(|e| e > mem.len()) {
            return Err(WasmTrap::OutOfBoundsMemory);
        }
        mem[d..d + n].copy_from_slice(&bytes[s..s + n]);
        Ok(())
    }

    pub fn grow_memory(memory: &WasmMemory<'_>, delta: u64) -> u64 {
        let page = 1u64 << memory.ty.page_size_log2;
        let old = memory.pages();
        let limit = memory.ty.limits.max.unwrap_or(u64::MAX).min(if memory.ty.page_size_log2 == 16 { MAX_PAGES } else { u64::from(u32::MAX) });
        match old.checked_add(delta) {
            Some(n) if n <= limit => {
                memory.bytes.resize(usize::try_from(n * page).unwrap_or(0));
                old
            }
            _ => u64::MAX,
        }
    }

    pub fn grow_table(mc: &Mutation<'scope>, table: &WasmTable<'scope>, init: u64, delta: u64) -> u64 {
        let words: Vec<u64> = table.cells().iter().map(WordSlot::get).collect();
        let old = u64::try_from(words.len()).unwrap_or(0);
        let limit = table.ty.limits.max.unwrap_or(u64::from(u32::MAX)).min(MAX_TABLE);
        match old.checked_add(delta) {
            Some(n) if n <= limit => {
                let extra = usize::try_from(delta).unwrap_or(0);
                table.cells.set(mc.gc_scope(), Some(WasmCells::alloc(mc, words.into_iter().chain(core::iter::repeat_n(init, extra)))));
                old
            }
            _ => u64::MAX,
        }
    }

    fn callee(&self, instance: &WasmInstance<'scope>, type_index: OperandValue, r: u64) -> Result<&'scope WasmFunc<'scope>, WasmTrap> {
        let f = Ptr::get::<WasmFunc<'scope>>(r).ok_or(WasmTrap::UninitializedElement)?;
        if self.registry().sub(f.canon, instance.ty(type_index)) { Ok(f) } else { Err(WasmTrap::IndirectCallTypeMismatch) }
    }

    fn throw(&mut self, instance: &WasmInstance<'scope>, tag: OperandValue, base: usize) -> WasmTrap {
        let tag = instance.tags[usize::try_from(tag).unwrap_or(0)].get();
        let Some(tag) = tag else { return WasmTrap::Unsupported };
        let layout = self.tag_layout(tag.get());
        let w = WasmObject::exception(self.mc, tag, &layout);
        if let Some(obj) = WasmObject::get(w) {
            for (i, &f) in layout.fields.iter().enumerate() {
                obj.write(self.mc, f, 0, &layout, (self.stack[base + i], self.vstack[base + i]));
            }
        }
        self.thrown = w;
        WasmTrap::Exception
    }

    pub fn exception(&self, tag: ScopeLocal<'scope, WasmTag<'scope>>, payload: &[(u64, u128)]) -> u64 {
        let layout = self.tag_layout(tag.get());
        let w = WasmObject::exception(self.mc, tag, &layout);
        if let Some(obj) = WasmObject::get(w) {
            for (&f, &v) in layout.fields.iter().zip(payload) {
                obj.write(self.mc, f, 0, &layout, v);
            }
        }
        w
    }

    pub fn exception_tag(w: u64) -> Option<ScopeLocal<'scope, WasmTag<'scope>>> {
        WasmObject::get(w).and_then(|o| Ptr::local(o.aux()))
    }

    pub fn exception_payload(&self, w: u64) -> Vec<(u64, u128)> {
        let Some(o) = WasmObject::get(w) else { return Vec::new() };
        let Some(tag) = Ptr::get::<WasmTag<'_>>(o.aux()) else { return Vec::new() };
        let layout = self.tag_layout(tag);
        layout.fields.iter().map(|&f| o.read(f, 0, &layout)).collect()
    }

    fn tag_layout(&self, tag: &WasmTag<'_>) -> Layout {
        match &self.registry().get(tag.canon).sub.composite {
            WasmCompositeType::Func(f) => {
                let storages: Vec<WasmStorageType> = f.params.iter().map(|&p| WasmStorageType::Val(p)).collect();
                storages.iter().collect()
            }
            _ => Layout::default(),
        }
    }

    fn catch(&mut self, frames: &mut Vec<Activation<'scope>>) -> bool {
        let Some(exn) = WasmObject::get(self.thrown) else { return false };
        let tag = exn.aux();
        while let Some(f) = frames.last_mut() {
            let at = f.pc.checked_sub(1).and_then(|i| f.code.ops.get(i)).map_or(u32::MAX, |d| u32::try_from(d.offset).unwrap_or(u32::MAX));
            let inst = f.instance;
            let found = f.code.handlers.iter().filter(|h| h.start <= at && at < h.end).flat_map(|h| h.catches.iter()).find(|c| match c.kind {
                WasmCatchKind::Catch | WasmCatchKind::CatchRef => inst.tags[c.tag as usize].get().is_some_and(|t| Ptr::of(t) == tag),
                WasmCatchKind::CatchAll | WasmCatchKind::CatchAllRef => true,
            });
            if let Some(&c) = found {
                let (fp, pc) = (f.fp, f.code.at[c.target as usize] as usize);
                f.pc = pc;
                let mut land = fp + c.land as usize;
                if matches!(c.kind, WasmCatchKind::Catch | WasmCatchKind::CatchRef) {
                    for (s, v) in self.exception_payload(self.thrown) {
                        self.stack[land] = s;
                        self.vstack[land] = v;
                        land += 1;
                    }
                }
                if matches!(c.kind, WasmCatchKind::CatchRef | WasmCatchKind::CatchAllRef) {
                    self.stack[land] = self.thrown;
                    self.vstack[land] = 0;
                }
                return true;
            }
            frames.pop();
        }
        false
    }

    fn call(&mut self, frame: Activation<'scope>) -> Result<(), WasmTrap> {
        let mut frames: Vec<Activation<'scope>> = vec![frame];
        loop {
            match self.run(&mut frames) {
                Err(WasmTrap::Exception) if self.catch(&mut frames) => {}
                r => return r,
            }
        }
    }

    pub fn test(&self, instance: &WasmInstance<'_>, w: u64, ht: OperandValue, nullable: bool) -> bool {
        let heap = match j8bytecode::WasmHeapType::from(ht) {
            j8bytecode::WasmHeapType::Concrete(i) => j8bytecode::WasmHeapType::Concrete(instance.ty(i as usize)),
            h => h,
        };
        self.matches(w, heap, nullable)
    }

    pub fn matches(&self, w: u64, heap: j8bytecode::WasmHeapType, nullable: bool) -> bool {
        use j8bytecode::WasmHeapType as H;
        let types = self.registry();
        let h = types.hierarchy(heap);
        let actual = match Word::decode(w, h) {
            Word::Null => return nullable,
            Word::I31(_) => H::I31,
            Word::Host(_) | Word::Js(_) => return matches!(heap, H::Any | H::Extern),
            Word::Func(w) => Ptr::get::<WasmFunc<'_>>(w).map_or(H::None, |f| H::Concrete(f.canon)),
            Word::Object(w) => match h {
                Hierarchy::Exn => H::Exn,
                Hierarchy::Extern => H::Extern,
                _ => WasmObject::get(w).map_or(H::None, |o| H::Concrete(o.head())),
            },
        };
        match h {
            Hierarchy::Extern => heap == H::Extern,
            Hierarchy::Exn => heap == H::Exn,
            _ => types.heap_sub(actual, heap),
        }
    }

    #[expect(clippy::too_many_lines, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::float_cmp, reason = "the interpreter loop: one arm per bytecode, with WebAssembly's wrapping and bit-level numeric semantics")]
    fn run(&mut self, frames: &mut Vec<Activation<'scope>>) -> Result<(), WasmTrap> {
        let mc = self.mc;
        macro_rules! r {
            ($f:expr, $o:expr) => {
                self.stack[$f.fp + $o as usize]
            };
        }
        macro_rules! un {
            ($f:expr, $o:expr, |$a:ident: $get:ident| $e:expr) => {{
                let $a = Slot(r!($f, $o[1])).$get();
                r!($f, $o[0]) = $e;
            }};
        }
        macro_rules! bin {
            ($f:expr, $o:expr, |$a:ident, $b:ident: $get:ident| $e:expr) => {{
                let $a = Slot(r!($f, $o[1])).$get();
                let $b = Slot(r!($f, $o[2])).$get();
                r!($f, $o[0]) = $e;
            }};
        }
        macro_rules! load {
            ($f:expr, $o:expr, $n:expr, |$b:ident| $e:expr) => {{
                let m = $f.instance.memory($o[2]);
                let at = m.address(r!($f, $o[1]), $o[3], $n)?;
                let mut $b = [0u8; $n];
                $b.copy_from_slice(&m.data()[at..at + $n]);
                r!($f, $o[0]) = $e;
            }};
        }
        macro_rules! store {
            ($f:expr, $o:expr, $n:expr) => {{
                let m = $f.instance.memory($o[2]);
                let at = m.address(r!($f, $o[0]), $o[3], $n)?;
                let v = r!($f, $o[1]).to_le_bytes();
                m.data()[at..at + $n].copy_from_slice(&v[..$n]);
            }};
        }
        macro_rules! trunc {
            ($f:expr, $o:expr, $get:ident, $lo:expr, $hi:expr, $t:ty, $out:expr) => {{
                let x = Slot(r!($f, $o[1])).$get();
                if x.is_nan() {
                    return Err(WasmTrap::InvalidConversion);
                }
                let t = x.trunc();
                if !(t > $lo && t < $hi) {
                    return Err(WasmTrap::IntegerOverflow);
                }
                r!($f, $o[0]) = $out(t as $t);
            }};
        }
        let i32s = |v: i32| u64::from(v.cast_unsigned());
        let u32s = |v: u32| u64::from(v);
        let f32s = |v: f32| u64::from(v.to_bits());
        let f64s = |v: f64| v.to_bits();
        let b = |v: bool| u64::from(v);
        while let Some(f) = frames.last_mut() {
            let Some(d) = f.code.ops.get(f.pc) else { return Err(WasmTrap::Unsupported) };
            let d: &Decoded<WasmBytecode> = d;
            let o = d.operands;
            f.pc += 1;
            match d.bytecode {
                WasmBytecode::Jump => f.pc = f.code.target(d, o[0]),
                WasmBytecode::JumpIfZero => {
                    if Slot(r!(f, o[0])).u32() == 0 {
                        f.pc = f.code.target(d, o[1]);
                    }
                }
                WasmBytecode::JumpIfNonZero => {
                    if Slot(r!(f, o[0])).u32() != 0 {
                        f.pc = f.code.target(d, o[1]);
                    }
                }
                WasmBytecode::BrOnNull => {
                    if r!(f, o[0]) == 0 {
                        f.pc = f.code.target(d, o[1]);
                    }
                }
                WasmBytecode::BrOnNonNull => {
                    if r!(f, o[0]) != 0 {
                        f.pc = f.code.target(d, o[1]);
                    }
                }
                WasmBytecode::BrTable => {
                    let idx = Slot(r!(f, o[0])).u32() as usize;
                    let table = &f.code.tables[usize::try_from(o[1]).unwrap_or(0)];
                    let at = table[idx.min(table.len() - 1)] as usize;
                    f.pc = f.code.at[at] as usize;
                }
                WasmBytecode::Mov => self.copy(f.fp + o[1] as usize, f.fp + o[0] as usize, 1),
                WasmBytecode::Unreachable => return Err(WasmTrap::Unreachable),
                WasmBytecode::Return => {
                    let (fp, first, n) = (f.fp, f.fp + o[0] as usize, o[1] as usize);
                    self.copy(first, fp, n);
                    frames.pop();
                }
                WasmBytecode::CallFunction | WasmBytecode::ReturnCall | WasmBytecode::CallIndirect | WasmBytecode::ReturnCallIndirect | WasmBytecode::CallRef | WasmBytecode::ReturnCallRef => {
                    let inst = f.instance;
                    let (target, base) = match d.bytecode {
                        WasmBytecode::CallFunction | WasmBytecode::ReturnCall => (inst.func(o[0]), o[1]),
                        WasmBytecode::CallIndirect | WasmBytecode::ReturnCallIndirect => {
                            let t = inst.table(o[1]);
                            let i = t.index(r!(f, o[2]));
                            let e = t.cells().get(usize::try_from(i).unwrap_or(usize::MAX)).ok_or(WasmTrap::UndefinedElement)?.get();
                            (self.callee(inst, o[0], e)?, o[3])
                        }
                        _ => (Ptr::get::<WasmFunc<'scope>>(r!(f, o[1])).ok_or(WasmTrap::NullReference)?, o[2]),
                    };
                    let tail = matches!(d.bytecode, WasmBytecode::ReturnCall | WasmBytecode::ReturnCallIndirect | WasmBytecode::ReturnCallRef);
                    let mut fp = f.fp + base as usize;
                    if tail {
                        let n = self.func_type(target).params.len();
                        self.copy(fp, f.fp, n);
                        fp = f.fp;
                        frames.pop();
                    }
                    match target.instance.get() {
                        None => self.host(target, fp)?,
                        Some(i) => {
                            if frames.len() >= MAX_DEPTH {
                                return Err(WasmTrap::CallStackExhausted);
                            }
                            let i = i.get();
                            let a = self.frame(i, i.module.code(target.index), fp);
                            frames.push(a);
                        }
                    }
                }
                WasmBytecode::Select => {
                    let from = if Slot(r!(f, o[3])).u32() != 0 { o[1] } else { o[2] };
                    self.copy(f.fp + from as usize, f.fp + o[0] as usize, 1);
                }
                WasmBytecode::GlobalGet => {
                    let (s, v) = f.instance.global(o[1]).get();
                    r!(f, o[0]) = s;
                    self.vstack[f.fp + o[0] as usize] = v;
                }
                WasmBytecode::GlobalSet => {
                    let at = f.fp + o[1] as usize;
                    f.instance.global(o[0]).set(mc, (self.stack[at], self.vstack[at]));
                }
                WasmBytecode::TableGet => {
                    let t = f.instance.table(o[2]);
                    let i = t.index(r!(f, o[1]));
                    r!(f, o[0]) = t.cells().get(usize::try_from(i).unwrap_or(usize::MAX)).ok_or(WasmTrap::OutOfBoundsTable)?.get();
                }
                WasmBytecode::TableSet => {
                    let t = f.instance.table(o[2]);
                    let i = t.index(r!(f, o[0]));
                    let v = r!(f, o[1]);
                    t.cells().get(usize::try_from(i).unwrap_or(usize::MAX)).ok_or(WasmTrap::OutOfBoundsTable)?.set(mc.gc_scope(), v);
                }
                WasmBytecode::TableSize => r!(f, o[0]) = f.instance.table(o[1]).cells().len() as u64,
                WasmBytecode::TableGrow => {
                    let t = f.instance.table(o[3]);
                    let (init, n) = (r!(f, o[1]), t.index(r!(f, o[2])));
                    let old = Self::grow_table(mc, t, init, n);
                    r!(f, o[0]) = t.index(old);
                }
                WasmBytecode::TableFill => {
                    let t = f.instance.table(o[1]);
                    let base = f.fp + o[0] as usize;
                    let (i, v, n) = (t.index(self.stack[base]), self.stack[base + 1], t.index(self.stack[base + 2]));
                    let (i, n) = (usize::try_from(i).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
                    let cells = t.cells();
                    if i.checked_add(n).is_none_or(|e| e > cells.len()) {
                        return Err(WasmTrap::OutOfBoundsTable);
                    }
                    for c in &cells[i..i + n] {
                        c.set(mc.gc_scope(), v);
                    }
                }
                WasmBytecode::TableCopy => {
                    let (dt, st) = (f.instance.table(o[3]), f.instance.table(o[4]));
                    let (dd, s, n) = (dt.index(r!(f, o[0])), st.index(r!(f, o[1])), r!(f, o[2]));
                    let n = if dt.ty.table64 && st.ty.table64 { n } else { n & u64::from(u32::MAX) };
                    let (dd, s, n) = (usize::try_from(dd).unwrap_or(usize::MAX), usize::try_from(s).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
                    let (dst, src) = (dt.cells(), st.cells());
                    if s.checked_add(n).is_none_or(|e| e > src.len()) || dd.checked_add(n).is_none_or(|e| e > dst.len()) {
                        return Err(WasmTrap::OutOfBoundsTable);
                    }
                    let words: Vec<u64> = src[s..s + n].iter().map(WordSlot::get).collect();
                    for (c, w) in dst[dd..dd + n].iter().zip(words) {
                        c.set(mc.gc_scope(), w);
                    }
                }
                WasmBytecode::TableInit => {
                    let t = f.instance.table(o[3]);
                    let (dd, s, n) = (t.index(r!(f, o[0])), Slot(r!(f, o[1])).u32(), Slot(r!(f, o[2])).u32());
                    Self::table_init(mc, f.instance, u32::try_from(o[3]).unwrap_or(0), u32::try_from(o[4]).unwrap_or(0), dd, u64::from(s), u64::from(n))?;
                }
                WasmBytecode::ElemDrop => f.instance.elems[o[0] as usize].set(mc.gc_scope(), None),
                WasmBytecode::DataDrop => f.instance.data[o[0] as usize].set(true),
                WasmBytecode::Throw => {
                    let (inst, base) = (f.instance, f.fp + o[1] as usize);
                    return Err(self.throw(inst, o[0], base));
                }
                WasmBytecode::ThrowRef => {
                    let w = r!(f, o[0]);
                    if w == 0 {
                        return Err(WasmTrap::NullReference);
                    }
                    self.thrown = w;
                    return Err(WasmTrap::Exception);
                }
                WasmBytecode::BrOnCast | WasmBytecode::BrOnCastFail => {
                    let ok = self.test(f.instance, r!(f, o[0]), o[3], o[1] & FLAG_TO_NULLABLE != 0);
                    if ok == (d.bytecode == WasmBytecode::BrOnCast) {
                        f.pc = f.code.target(d, o[4]);
                    }
                }
                WasmBytecode::MemoryInit => {
                    let base = f.fp + o[0] as usize;
                    let (dd, s, n) = (self.stack[base], Slot(self.stack[base + 1]).u32(), Slot(self.stack[base + 2]).u32());
                    Self::memory_init(f.instance, u32::try_from(o[1]).unwrap_or(0), u32::try_from(o[2]).unwrap_or(0), dd, u64::from(s), u64::from(n))?;
                }
                WasmBytecode::MemoryCopy => {
                    let (dm, sm) = (f.instance.memory(o[1]), f.instance.memory(o[2]));
                    let base = f.fp + o[0] as usize;
                    let (dd, s, n) = (dm.index(self.stack[base]), sm.index(self.stack[base + 1]), self.stack[base + 2]);
                    let n = if dm.ty.memory64 && sm.ty.memory64 { n } else { n & u64::from(u32::MAX) };
                    let (dd, s, n) = (usize::try_from(dd).unwrap_or(usize::MAX), usize::try_from(s).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
                    if s.checked_add(n).is_none_or(|e| e > sm.data().len()) || dd.checked_add(n).is_none_or(|e| e > dm.data().len()) {
                        return Err(WasmTrap::OutOfBoundsMemory);
                    }
                    if core::ptr::eq(dm, sm) {
                        dm.data().copy_within(s..s + n, dd);
                    } else {
                        let src: Vec<u8> = sm.data()[s..s + n].to_vec();
                        dm.data()[dd..dd + n].copy_from_slice(&src);
                    }
                }
                WasmBytecode::MemoryFill => {
                    let m = f.instance.memory(o[1]);
                    let base = f.fp + o[0] as usize;
                    let (dd, v, n) = (m.index(self.stack[base]), self.stack[base + 1] as u8, m.index(self.stack[base + 2]));
                    let (dd, n) = (usize::try_from(dd).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
                    let mem = m.data();
                    if dd.checked_add(n).is_none_or(|e| e > mem.len()) {
                        return Err(WasmTrap::OutOfBoundsMemory);
                    }
                    mem[dd..dd + n].fill(v);
                }
                WasmBytecode::MemorySize => r!(f, o[0]) = f.instance.memory(o[1]).pages(),
                WasmBytecode::MemoryGrow => {
                    let m = f.instance.memory(o[2]);
                    let n = m.index(r!(f, o[1]));
                    let old = Self::grow_memory(m, n);
                    r!(f, o[0]) = m.index(old);
                }
                WasmBytecode::I32Const | WasmBytecode::F32Const => r!(f, o[0]) = u32s(o[1] as u32),
                WasmBytecode::I64Const => r!(f, o[0]) = o[1] as u64,
                WasmBytecode::I64ConstPool | WasmBytecode::F64Const => {
                    r!(f, o[0]) = f.code.bits[o[1] as usize];
                }
                WasmBytecode::RefNull => r!(f, o[0]) = 0,
                WasmBytecode::RefIsNull => r!(f, o[0]) = b(r!(f, o[1]) == 0),
                WasmBytecode::RefFunc => r!(f, o[0]) = f.instance.funcs[o[1] as usize].get().map_or(0, Ptr::of),
                WasmBytecode::RefAsNonNull => {
                    let v = r!(f, o[1]);
                    if v == 0 {
                        return Err(WasmTrap::NullReference);
                    }
                    r!(f, o[0]) = v;
                }
                WasmBytecode::I32Add => bin!(f, o, |x, y: i32| i32s(x.wrapping_add(y))),
                WasmBytecode::I32Sub => bin!(f, o, |x, y: i32| i32s(x.wrapping_sub(y))),
                WasmBytecode::I32Mul => bin!(f, o, |x, y: i32| i32s(x.wrapping_mul(y))),
                WasmBytecode::I32DivS => {
                    let (x, y) = (Slot(r!(f, o[1])).i32(), Slot(r!(f, o[2])).i32());
                    if y == 0 {
                        return Err(WasmTrap::IntegerDivideByZero);
                    }
                    r!(f, o[0]) = i32s(x.checked_div(y).ok_or(WasmTrap::IntegerOverflow)?);
                }
                WasmBytecode::I32DivU | WasmBytecode::I32RemU => {
                    let (x, y) = (Slot(r!(f, o[1])).u32(), Slot(r!(f, o[2])).u32());
                    if y == 0 {
                        return Err(WasmTrap::IntegerDivideByZero);
                    }
                    r!(f, o[0]) = u32s(if d.bytecode == WasmBytecode::I32DivU { x / y } else { x % y });
                }
                WasmBytecode::I32RemS => {
                    let (x, y) = (Slot(r!(f, o[1])).i32(), Slot(r!(f, o[2])).i32());
                    if y == 0 {
                        return Err(WasmTrap::IntegerDivideByZero);
                    }
                    r!(f, o[0]) = i32s(x.wrapping_rem(y));
                }
                WasmBytecode::I32And => bin!(f, o, |x, y: u32| u32s(x & y)),
                WasmBytecode::I32Ior => bin!(f, o, |x, y: u32| u32s(x | y)),
                WasmBytecode::I32Xor => bin!(f, o, |x, y: u32| u32s(x ^ y)),
                WasmBytecode::I32Shl => bin!(f, o, |x, y: u32| u32s(x.wrapping_shl(y))),
                WasmBytecode::I32ShrS => bin!(f, o, |x, y: i32| i32s(x.wrapping_shr(y.cast_unsigned()))),
                WasmBytecode::I32ShrU => bin!(f, o, |x, y: u32| u32s(x.wrapping_shr(y))),
                WasmBytecode::I32Rol => bin!(f, o, |x, y: u32| u32s(x.rotate_left(y % 32))),
                WasmBytecode::I32Ror => bin!(f, o, |x, y: u32| u32s(x.rotate_right(y % 32))),
                WasmBytecode::I32Clz => un!(f, o, |x: u32| u32s(x.leading_zeros())),
                WasmBytecode::I32Ctz => un!(f, o, |x: u32| u32s(x.trailing_zeros())),
                WasmBytecode::I32Popcnt => un!(f, o, |x: u32| u32s(x.count_ones())),
                WasmBytecode::I32Eqz => un!(f, o, |x: u32| b(x == 0)),
                WasmBytecode::I32Eq => bin!(f, o, |x, y: u32| b(x == y)),
                WasmBytecode::I32Ne => bin!(f, o, |x, y: u32| b(x != y)),
                WasmBytecode::I32LtS => bin!(f, o, |x, y: i32| b(x < y)),
                WasmBytecode::I32LtU => bin!(f, o, |x, y: u32| b(x < y)),
                WasmBytecode::I32GtS => bin!(f, o, |x, y: i32| b(x > y)),
                WasmBytecode::I32GtU => bin!(f, o, |x, y: u32| b(x > y)),
                WasmBytecode::I32LeS => bin!(f, o, |x, y: i32| b(x <= y)),
                WasmBytecode::I32LeU => bin!(f, o, |x, y: u32| b(x <= y)),
                WasmBytecode::I32GeS => bin!(f, o, |x, y: i32| b(x >= y)),
                WasmBytecode::I32GeU => bin!(f, o, |x, y: u32| b(x >= y)),
                WasmBytecode::I64Add => bin!(f, o, |x, y: i64| x.wrapping_add(y) as u64),
                WasmBytecode::I64Sub => bin!(f, o, |x, y: i64| x.wrapping_sub(y) as u64),
                WasmBytecode::I64Mul => bin!(f, o, |x, y: i64| x.wrapping_mul(y) as u64),
                WasmBytecode::I64DivS => {
                    let (x, y) = (Slot(r!(f, o[1])).i64(), Slot(r!(f, o[2])).i64());
                    if y == 0 {
                        return Err(WasmTrap::IntegerDivideByZero);
                    }
                    r!(f, o[0]) = x.checked_div(y).ok_or(WasmTrap::IntegerOverflow)? as u64;
                }
                WasmBytecode::I64DivU | WasmBytecode::I64RemU => {
                    let (x, y) = (r!(f, o[1]), r!(f, o[2]));
                    if y == 0 {
                        return Err(WasmTrap::IntegerDivideByZero);
                    }
                    r!(f, o[0]) = if d.bytecode == WasmBytecode::I64DivU { x / y } else { x % y };
                }
                WasmBytecode::I64RemS => {
                    let (x, y) = (Slot(r!(f, o[1])).i64(), Slot(r!(f, o[2])).i64());
                    if y == 0 {
                        return Err(WasmTrap::IntegerDivideByZero);
                    }
                    r!(f, o[0]) = x.wrapping_rem(y) as u64;
                }
                WasmBytecode::I64And => bin!(f, o, |x, y: u64| x & y),
                WasmBytecode::I64Ior => bin!(f, o, |x, y: u64| x | y),
                WasmBytecode::I64Xor => bin!(f, o, |x, y: u64| x ^ y),
                WasmBytecode::I64Shl => bin!(f, o, |x, y: u64| x.wrapping_shl(y as u32)),
                WasmBytecode::I64ShrS => bin!(f, o, |x, y: i64| x.wrapping_shr(y as u32) as u64),
                WasmBytecode::I64ShrU => bin!(f, o, |x, y: u64| x.wrapping_shr(y as u32)),
                WasmBytecode::I64Rol => bin!(f, o, |x, y: u64| x.rotate_left((y % 64) as u32)),
                WasmBytecode::I64Ror => bin!(f, o, |x, y: u64| x.rotate_right((y % 64) as u32)),
                WasmBytecode::I64Clz => un!(f, o, |x: u64| u64::from(x.leading_zeros())),
                WasmBytecode::I64Ctz => un!(f, o, |x: u64| u64::from(x.trailing_zeros())),
                WasmBytecode::I64Popcnt => un!(f, o, |x: u64| u64::from(x.count_ones())),
                WasmBytecode::I64Eqz => un!(f, o, |x: u64| b(x == 0)),
                WasmBytecode::I64Eq | WasmBytecode::RefEq => bin!(f, o, |x, y: u64| b(x == y)),
                WasmBytecode::I64Ne => bin!(f, o, |x, y: u64| b(x != y)),
                WasmBytecode::I64LtS => bin!(f, o, |x, y: i64| b(x < y)),
                WasmBytecode::I64LtU => bin!(f, o, |x, y: u64| b(x < y)),
                WasmBytecode::I64GtS => bin!(f, o, |x, y: i64| b(x > y)),
                WasmBytecode::I64GtU => bin!(f, o, |x, y: u64| b(x > y)),
                WasmBytecode::I64LeS => bin!(f, o, |x, y: i64| b(x <= y)),
                WasmBytecode::I64LeU => bin!(f, o, |x, y: u64| b(x <= y)),
                WasmBytecode::I64GeS => bin!(f, o, |x, y: i64| b(x >= y)),
                WasmBytecode::I64GeU => bin!(f, o, |x, y: u64| b(x >= y)),
                WasmBytecode::F32Eq => bin!(f, o, |x, y: f32| b(x == y)),
                WasmBytecode::F32Ne => bin!(f, o, |x, y: f32| b(x != y)),
                WasmBytecode::F32Lt => bin!(f, o, |x, y: f32| b(x < y)),
                WasmBytecode::F32Gt => bin!(f, o, |x, y: f32| b(x > y)),
                WasmBytecode::F32Le => bin!(f, o, |x, y: f32| b(x <= y)),
                WasmBytecode::F32Ge => bin!(f, o, |x, y: f32| b(x >= y)),
                WasmBytecode::F64Eq => bin!(f, o, |x, y: f64| b(x == y)),
                WasmBytecode::F64Ne => bin!(f, o, |x, y: f64| b(x != y)),
                WasmBytecode::F64Lt => bin!(f, o, |x, y: f64| b(x < y)),
                WasmBytecode::F64Gt => bin!(f, o, |x, y: f64| b(x > y)),
                WasmBytecode::F64Le => bin!(f, o, |x, y: f64| b(x <= y)),
                WasmBytecode::F64Ge => bin!(f, o, |x, y: f64| b(x >= y)),
                WasmBytecode::F32Abs => un!(f, o, |x: u32| u32s(x & 0x7fff_ffff)),
                WasmBytecode::F32Neg => un!(f, o, |x: u32| u32s(x ^ 0x8000_0000)),
                WasmBytecode::F32Ceil => un!(f, o, |x: f32| f32s(if x.is_nan() { x + x } else { x.ceil() })),
                WasmBytecode::F32Floor => un!(f, o, |x: f32| f32s(if x.is_nan() { x + x } else { x.floor() })),
                WasmBytecode::F32Trunc => un!(f, o, |x: f32| f32s(if x.is_nan() { x + x } else { x.trunc() })),
                WasmBytecode::F32NearestInt => un!(f, o, |x: f32| f32s(if x.is_nan() { x + x } else { x.round_ties_even() })),
                WasmBytecode::F32Sqrt => un!(f, o, |x: f32| f32s(x.sqrt())),
                WasmBytecode::F32Add => bin!(f, o, |x, y: f32| f32s(x + y)),
                WasmBytecode::F32Sub => bin!(f, o, |x, y: f32| f32s(x - y)),
                WasmBytecode::F32Mul => bin!(f, o, |x, y: f32| f32s(x * y)),
                WasmBytecode::F32Div => bin!(f, o, |x, y: f32| f32s(x / y)),
                WasmBytecode::F32Min => bin!(f, o, |x, y: f32| f32s(Float::min32(x, y))),
                WasmBytecode::F32Max => bin!(f, o, |x, y: f32| f32s(Float::max32(x, y))),
                WasmBytecode::F32CopySign => bin!(f, o, |x, y: u32| u32s((x & 0x7fff_ffff) | (y & 0x8000_0000))),
                WasmBytecode::F64Abs => un!(f, o, |x: u64| x & 0x7fff_ffff_ffff_ffff),
                WasmBytecode::F64Neg => un!(f, o, |x: u64| x ^ 0x8000_0000_0000_0000),
                WasmBytecode::F64Ceil => un!(f, o, |x: f64| f64s(if x.is_nan() { x + x } else { x.ceil() })),
                WasmBytecode::F64Floor => un!(f, o, |x: f64| f64s(if x.is_nan() { x + x } else { x.floor() })),
                WasmBytecode::F64Trunc => un!(f, o, |x: f64| f64s(if x.is_nan() { x + x } else { x.trunc() })),
                WasmBytecode::F64NearestInt => un!(f, o, |x: f64| f64s(if x.is_nan() { x + x } else { x.round_ties_even() })),
                WasmBytecode::F64Sqrt => un!(f, o, |x: f64| f64s(x.sqrt())),
                WasmBytecode::F64Add => bin!(f, o, |x, y: f64| f64s(x + y)),
                WasmBytecode::F64Sub => bin!(f, o, |x, y: f64| f64s(x - y)),
                WasmBytecode::F64Mul => bin!(f, o, |x, y: f64| f64s(x * y)),
                WasmBytecode::F64Div => bin!(f, o, |x, y: f64| f64s(x / y)),
                WasmBytecode::F64Min => bin!(f, o, |x, y: f64| f64s(Float::min64(x, y))),
                WasmBytecode::F64Max => bin!(f, o, |x, y: f64| f64s(Float::max64(x, y))),
                WasmBytecode::F64CopySign => bin!(f, o, |x, y: u64| (x & 0x7fff_ffff_ffff_ffff) | (y & 0x8000_0000_0000_0000)),
                WasmBytecode::I32ConvertI64 => un!(f, o, |x: u64| x & u64::from(u32::MAX)),
                WasmBytecode::I32SConvertF32 => trunc!(f, o, f32, -2_147_483_904.0_f32, 2_147_483_648.0_f32, i32, i32s),
                WasmBytecode::I32UConvertF32 => trunc!(f, o, f32, -1.0_f32, 4_294_967_296.0_f32, u32, u32s),
                WasmBytecode::I32SConvertF64 => trunc!(f, o, f64, -2_147_483_649.0_f64, 2_147_483_648.0_f64, i32, i32s),
                WasmBytecode::I32UConvertF64 => trunc!(f, o, f64, -1.0_f64, 4_294_967_296.0_f64, u32, u32s),
                WasmBytecode::I64SConvertF32 => trunc!(f, o, f32, -9_223_373_136_366_403_584.0_f32, 9_223_372_036_854_775_808.0_f32, i64, |v: i64| v as u64),
                WasmBytecode::I64UConvertF32 => trunc!(f, o, f32, -1.0_f32, 18_446_744_073_709_551_616.0_f32, u64, |v: u64| v),
                WasmBytecode::I64SConvertF64 => trunc!(f, o, f64, -9_223_372_036_854_777_856.0_f64, 9_223_372_036_854_775_808.0_f64, i64, |v: i64| v as u64),
                WasmBytecode::I64UConvertF64 => trunc!(f, o, f64, -1.0_f64, 18_446_744_073_709_551_616.0_f64, u64, |v: u64| v),
                WasmBytecode::I32SConvertSatF32 => un!(f, o, |x: f32| i32s(x as i32)),
                WasmBytecode::I32UConvertSatF32 => un!(f, o, |x: f32| u32s(x as u32)),
                WasmBytecode::I32SConvertSatF64 => un!(f, o, |x: f64| i32s(x as i32)),
                WasmBytecode::I32UConvertSatF64 => un!(f, o, |x: f64| u32s(x as u32)),
                WasmBytecode::I64SConvertSatF32 => un!(f, o, |x: f32| (x as i64) as u64),
                WasmBytecode::I64UConvertSatF32 => un!(f, o, |x: f32| x as u64),
                WasmBytecode::I64SConvertSatF64 => un!(f, o, |x: f64| (x as i64) as u64),
                WasmBytecode::I64UConvertSatF64 => un!(f, o, |x: f64| x as u64),
                WasmBytecode::I64SConvertI32 => un!(f, o, |x: i32| i64::from(x) as u64),
                WasmBytecode::I64UConvertI32 => un!(f, o, |x: u32| u64::from(x)),
                WasmBytecode::F32SConvertI32 => un!(f, o, |x: i32| f32s(x as f32)),
                WasmBytecode::F32UConvertI32 => un!(f, o, |x: u32| f32s(x as f32)),
                WasmBytecode::F32SConvertI64 => un!(f, o, |x: i64| f32s(x as f32)),
                WasmBytecode::F32UConvertI64 => un!(f, o, |x: u64| f32s(x as f32)),
                WasmBytecode::F32ConvertF64 => un!(f, o, |x: f64| f32s(x as f32)),
                WasmBytecode::F64SConvertI32 => un!(f, o, |x: i32| f64s(f64::from(x))),
                WasmBytecode::F64UConvertI32 => un!(f, o, |x: u32| f64s(f64::from(x))),
                WasmBytecode::F64SConvertI64 => un!(f, o, |x: i64| f64s(x as f64)),
                WasmBytecode::F64UConvertI64 => un!(f, o, |x: u64| f64s(x as f64)),
                WasmBytecode::F64ConvertF32 => un!(f, o, |x: f32| f64s(f64::from(x))),
                WasmBytecode::I32ReinterpretF32 | WasmBytecode::F32ReinterpretI32 => un!(f, o, |x: u32| u32s(x)),
                WasmBytecode::I64ReinterpretF64 | WasmBytecode::F64ReinterpretI64 => un!(f, o, |x: u64| x),
                WasmBytecode::I32SExtendI8 => un!(f, o, |x: u32| i32s(i32::from(x as u8 as i8))),
                WasmBytecode::I32SExtendI16 => un!(f, o, |x: u32| i32s(i32::from(x as u16 as i16))),
                WasmBytecode::I64SExtendI8 => un!(f, o, |x: u64| i64::from(x as u8 as i8) as u64),
                WasmBytecode::I64SExtendI16 => un!(f, o, |x: u64| i64::from(x as u16 as i16) as u64),
                WasmBytecode::I64SExtendI32 => un!(f, o, |x: u64| i64::from(x as u32 as i32) as u64),
                WasmBytecode::I32LoadMem | WasmBytecode::F32LoadMem => load!(f, o, 4, |x| u32s(u32::from_le_bytes(x))),
                WasmBytecode::I64LoadMem | WasmBytecode::F64LoadMem => load!(f, o, 8, |x| u64::from_le_bytes(x)),
                WasmBytecode::I32LoadMem8S => load!(f, o, 1, |x| i32s(i32::from(x[0] as i8))),
                WasmBytecode::I32LoadMem8U | WasmBytecode::I64LoadMem8U => load!(f, o, 1, |x| u64::from(x[0])),
                WasmBytecode::I32LoadMem16S => load!(f, o, 2, |x| i32s(i32::from(i16::from_le_bytes(x)))),
                WasmBytecode::I32LoadMem16U | WasmBytecode::I64LoadMem16U => load!(f, o, 2, |x| u64::from(u16::from_le_bytes(x))),
                WasmBytecode::I64LoadMem8S => load!(f, o, 1, |x| i64::from(x[0] as i8) as u64),
                WasmBytecode::I64LoadMem16S => load!(f, o, 2, |x| i64::from(i16::from_le_bytes(x)) as u64),
                WasmBytecode::I64LoadMem32S => load!(f, o, 4, |x| i64::from(i32::from_le_bytes(x)) as u64),
                WasmBytecode::I64LoadMem32U => load!(f, o, 4, |x| u64::from(u32::from_le_bytes(x))),
                WasmBytecode::I32StoreMem | WasmBytecode::F32StoreMem | WasmBytecode::I64StoreMem32 => store!(f, o, 4),
                WasmBytecode::I64StoreMem | WasmBytecode::F64StoreMem => store!(f, o, 8),
                WasmBytecode::I32StoreMem8 | WasmBytecode::I64StoreMem8 => store!(f, o, 1),
                WasmBytecode::I32StoreMem16 | WasmBytecode::I64StoreMem16 => store!(f, o, 2),
                b if b.page_prefix().is_none() && (b as u16) >> 8 == 1 => {
                    simd::Vector { store: self, fp: f.fp, instance: f.instance }.run(d, &f.code.bits)?;
                }
                WasmBytecode::StructNew
                | WasmBytecode::StructNewDefault
                | WasmBytecode::StructGet
                | WasmBytecode::StructGetS
                | WasmBytecode::StructGetU
                | WasmBytecode::StructSet
                | WasmBytecode::ArrayNew
                | WasmBytecode::ArrayNewDefault
                | WasmBytecode::ArrayNewFixed
                | WasmBytecode::ArrayNewData
                | WasmBytecode::ArrayNewElem
                | WasmBytecode::ArrayGet
                | WasmBytecode::ArrayGetS
                | WasmBytecode::ArrayGetU
                | WasmBytecode::ArraySet
                | WasmBytecode::ArrayLen
                | WasmBytecode::ArrayFill
                | WasmBytecode::ArrayCopy
                | WasmBytecode::ArrayInitData
                | WasmBytecode::ArrayInitElem
                | WasmBytecode::RefTest
                | WasmBytecode::RefTestNull
                | WasmBytecode::RefCast
                | WasmBytecode::RefCastNull
                | WasmBytecode::AnyConvertExtern
                | WasmBytecode::ExternConvertAny
                | WasmBytecode::RefI31
                | WasmBytecode::I31GetS
                | WasmBytecode::I31GetU => {
                    Objects { store: self, fp: f.fp, instance: f.instance }.run(d)?;
                }
                _ => return Err(WasmTrap::Unsupported),
            }
        }
        Ok(())
    }
}

impl Slot {
    fn i64(self) -> i64 {
        self.0.cast_signed()
    }

    fn u64(self) -> u64 {
        self.0
    }
}

struct Float;

impl Float {
    fn min32(x: f32, y: f32) -> f32 {
        if x.is_nan() || y.is_nan() {
            x + y
        } else if x == y {
            f32::from_bits(x.to_bits() | y.to_bits())
        } else {
            x.min(y)
        }
    }

    fn max32(x: f32, y: f32) -> f32 {
        if x.is_nan() || y.is_nan() {
            x + y
        } else if x == y {
            f32::from_bits(x.to_bits() & y.to_bits())
        } else {
            x.max(y)
        }
    }

    fn min64(x: f64, y: f64) -> f64 {
        if x.is_nan() || y.is_nan() {
            x + y
        } else if x == y {
            f64::from_bits(x.to_bits() | y.to_bits())
        } else {
            x.min(y)
        }
    }

    fn max64(x: f64, y: f64) -> f64 {
        if x.is_nan() || y.is_nan() {
            x + y
        } else if x == y {
            f64::from_bits(x.to_bits() & y.to_bits())
        } else {
            x.max(y)
        }
    }
}
