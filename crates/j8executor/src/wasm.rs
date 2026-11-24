use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::OnceCell;

mod simd;

use j8bytecode::{Decoded, Decoder, OperandValue, WasmBytecode, WasmExternKind, WasmFuncType, WasmGlobalType, WasmHeapType, WasmImportKind, WasmMemoryType, WasmModule, WasmSegmentMode, WasmTableType, WasmValType};

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
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    V128(u128),
    FuncRef(Option<u32>),
    ExternRef(Option<u32>),
    Ref(Option<u64>),
}

impl WasmValue {
    fn vector(self) -> u128 {
        if let Self::V128(v) = self { v } else { 0 }
    }

    fn slot(self) -> u64 {
        match self {
            Self::I32(v) => u64::from(v.cast_unsigned()),
            Self::I64(v) => v.cast_unsigned(),
            Self::F32(v) => u64::from(v),
            Self::F64(v) => v,
            Self::V128(v) => u64::try_from(v & u128::from(u64::MAX)).unwrap_or(0),
            Self::FuncRef(r) | Self::ExternRef(r) => r.map_or(0, |a| u64::from(a) + 1),
            Self::Ref(r) => r.map_or(0, |a| a + 1),
        }
    }

    fn typed(ty: WasmValType, s: u64, v: u128) -> Self {
        let r = |s: u64| if s == 0 { None } else { u32::try_from(s - 1).ok() };
        match ty {
            WasmValType::I32 => Self::I32(Slot(s).i32()),
            WasmValType::I64 => Self::I64(s.cast_signed()),
            WasmValType::F32 => Self::F32(Slot(s).u32()),
            WasmValType::F64 => Self::F64(s),
            WasmValType::V128 => Self::V128(v),
            WasmValType::Ref(t) => match t.heap {
                WasmHeapType::Func | WasmHeapType::NoFunc | WasmHeapType::Concrete(_) => Self::FuncRef(r(s)),
                WasmHeapType::Extern | WasmHeapType::NoExtern => Self::ExternRef(r(s)),
                _ => Self::Ref(if s == 0 { None } else { Some(s - 1) }),
            },
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmExtern {
    Func(u32),
    Table(u32),
    Memory(u32),
    Global(u32),
    Tag(u32),
}

impl From<WasmExtern> for WasmExternKind {
    fn from(e: WasmExtern) -> Self {
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
    Trap(WasmTrap),
}

impl From<WasmTrap> for WasmInstantiateError {
    fn from(t: WasmTrap) -> Self {
        Self::Trap(t)
    }
}

pub type WasmHost = Box<dyn Fn(&[WasmValue]) -> Result<Vec<WasmValue>, WasmTrap>>;

struct Code {
    ops: Vec<Decoded<WasmBytecode>>,
    at: Vec<u32>,
    bits: Box<[u64]>,
    tables: Box<[Box<[u32]>]>,
}

impl Code {
    fn target(&self, d: &Decoded<WasmBytecode>, rel: OperandValue) -> usize {
        let byte = usize::try_from(OperandValue::try_from(d.offset).unwrap_or(0) + rel).unwrap_or(0);
        self.at.get(byte).map_or(usize::MAX, |&i| i as usize)
    }
}

enum FuncKind {
    Wasm { instance: u32, index: u32, code: OnceCell<Rc<Code>> },
    Host(WasmHost),
}

struct Func {
    ty: WasmFuncType,
    kind: FuncKind,
}

struct Table {
    ty: WasmTableType,
    elems: Vec<u64>,
}

struct Memory {
    ty: WasmMemoryType,
    bytes: Vec<u8>,
}

struct Global {
    ty: WasmGlobalType,
    value: u64,
    vector: u128,
}

struct Instance {
    module: Rc<WasmModule>,
    funcs: Vec<u32>,
    tables: Vec<u32>,
    memories: Vec<u32>,
    globals: Vec<u32>,
    elems: Vec<Rc<[u64]>>,
    data: Vec<Rc<[u8]>>,
}

struct Activation {
    instance: u32,
    code: Rc<Code>,
    fp: usize,
    pc: usize,
}

const MAX_DEPTH: usize = 50_000;
const MAX_PAGES: u64 = 65_536;
const FUNC_INIT: u32 = u32::MAX;

#[derive(Default)]
pub struct WasmStore {
    funcs: Vec<Func>,
    tables: Vec<Table>,
    memories: Vec<Memory>,
    globals: Vec<Global>,
    instances: Vec<Instance>,
    stack: Vec<u64>,
    vstack: Vec<u128>,
}

impl WasmStore {
    pub fn host_func(&mut self, ty: WasmFuncType, f: WasmHost) -> u32 {
        self.funcs.push(Func { ty, kind: FuncKind::Host(f) });
        u32::try_from(self.funcs.len() - 1).unwrap_or(0)
    }

    pub fn host_table(&mut self, ty: WasmTableType) -> u32 {
        let n = usize::try_from(ty.limits.min).unwrap_or(0);
        self.tables.push(Table { ty, elems: vec![0; n] });
        u32::try_from(self.tables.len() - 1).unwrap_or(0)
    }

    pub fn host_memory(&mut self, ty: WasmMemoryType) -> u32 {
        let n = usize::try_from(ty.limits.min << ty.page_size_log2).unwrap_or(0);
        self.memories.push(Memory { ty, bytes: vec![0; n] });
        u32::try_from(self.memories.len() - 1).unwrap_or(0)
    }

    pub fn host_global(&mut self, ty: WasmGlobalType, value: WasmValue) -> u32 {
        self.globals.push(Global { ty, value: value.slot(), vector: value.vector() });
        u32::try_from(self.globals.len() - 1).unwrap_or(0)
    }

    pub fn export(&self, instance: u32, name: &str) -> Option<WasmExtern> {
        let inst = &self.instances[instance as usize];
        let e = inst.module.export(name)?;
        let i = e.index as usize;
        Some(match e.kind {
            WasmExternKind::Func => WasmExtern::Func(inst.funcs[i]),
            WasmExternKind::Table => WasmExtern::Table(inst.tables[i]),
            WasmExternKind::Memory => WasmExtern::Memory(inst.memories[i]),
            WasmExternKind::Global => WasmExtern::Global(inst.globals[i]),
            WasmExternKind::Tag => WasmExtern::Tag(e.index),
        })
    }

    pub fn global_value(&self, g: u32) -> WasmValue {
        let g = &self.globals[g as usize];
        WasmValue::typed(g.ty.content, g.value, g.vector)
    }

    pub fn func_type(&self, f: u32) -> &WasmFuncType {
        &self.funcs[f as usize].ty
    }

    fn limits_match(have: (u64, Option<u64>), want: (u64, Option<u64>)) -> bool {
        have.0 >= want.0 && match (have.1, want.1) {
            (_, None) => true,
            (Some(h), Some(w)) => h <= w,
            (None, Some(_)) => false,
        }
    }

    fn link(&self, kind: &WasmImportKind, ext: WasmExtern, module: &WasmModule) -> Result<(), WasmLinkError> {
        let ok = match (kind, ext) {
            (WasmImportKind::Func(t), WasmExtern::Func(f)) => self.funcs[f as usize].ty == module.types[*t as usize],
            (WasmImportKind::Table(t), WasmExtern::Table(x)) => {
                let have = &self.tables[x as usize];
                let size = u64::try_from(have.elems.len()).unwrap_or(0);
                have.ty.element == t.element && have.ty.table64 == t.table64 && Self::limits_match((size, have.ty.limits.max), (t.limits.min, t.limits.max))
            }
            (WasmImportKind::Memory(t), WasmExtern::Memory(x)) => {
                let have = &self.memories[x as usize];
                let pages = u64::try_from(have.bytes.len()).unwrap_or(0) >> have.ty.page_size_log2;
                have.ty.memory64 == t.memory64 && have.ty.page_size_log2 == t.page_size_log2 && Self::limits_match((pages, have.ty.limits.max), (t.limits.min, t.limits.max))
            }
            (WasmImportKind::Global(t), WasmExtern::Global(g)) => self.globals[g as usize].ty == *t,
            (WasmImportKind::Tag(_), WasmExtern::Tag(_)) => true,
            _ => false,
        };
        if ok { Ok(()) } else { Err(WasmLinkError::IncompatibleImport) }
    }

    pub fn instantiate(&mut self, module: &Rc<WasmModule>, imports: &[WasmExtern]) -> Result<u32, WasmInstantiateError> {
        if imports.len() != module.imports.len() {
            return Err(WasmInstantiateError::Link(WasmLinkError::MissingImport));
        }
        for (imp, &ext) in module.imports.iter().zip(imports) {
            self.link(&imp.kind, ext, module).map_err(WasmInstantiateError::Link)?;
        }
        let id = u32::try_from(self.instances.len()).unwrap_or(0);
        let mut inst = Instance { module: module.clone(), funcs: Vec::new(), tables: Vec::new(), memories: Vec::new(), globals: Vec::new(), elems: Vec::new(), data: Vec::new() };
        for &ext in imports {
            match ext {
                WasmExtern::Func(f) => inst.funcs.push(f),
                WasmExtern::Table(t) => inst.tables.push(t),
                WasmExtern::Memory(m) => inst.memories.push(m),
                WasmExtern::Global(g) => inst.globals.push(g),
                WasmExtern::Tag(_) => {}
            }
        }
        let imported = inst.funcs.len();
        for (i, f) in module.functions.iter().enumerate().skip(imported) {
            self.funcs.push(Func { ty: module.types[f.ty as usize].clone(), kind: FuncKind::Wasm { instance: id, index: u32::try_from(i).unwrap_or(0), code: OnceCell::new() } });
            inst.funcs.push(u32::try_from(self.funcs.len() - 1).unwrap_or(0));
        }
        self.instances.push(inst);
        for g in &module.globals {
            let (value, vector) = self.eval(id, g.init)?;
            self.globals.push(Global { ty: g.ty, value, vector });
            let a = u32::try_from(self.globals.len() - 1).unwrap_or(0);
            self.instances[id as usize].globals.push(a);
        }
        for t in &module.tables {
            let init = match t.init {
                Some(i) => self.eval(id, i)?.0,
                None => 0,
            };
            let n = usize::try_from(t.ty.limits.min).unwrap_or(0);
            self.tables.push(Table { ty: t.ty, elems: vec![init; n] });
            let a = u32::try_from(self.tables.len() - 1).unwrap_or(0);
            self.instances[id as usize].tables.push(a);
        }
        for m in &module.memories {
            let a = self.host_memory(*m);
            self.instances[id as usize].memories.push(a);
        }
        for el in &module.elements {
            let mut items = Vec::with_capacity(el.items.len());
            for &i in &el.items {
                items.push(self.eval(id, i)?.0);
            }
            self.instances[id as usize].elems.push(items.into());
        }
        for d in &module.data {
            self.instances[id as usize].data.push(d.bytes.clone().into());
        }
        for (i, el) in module.elements.iter().enumerate() {
            match el.mode {
                WasmSegmentMode::Active { index, offset } => {
                    let at = self.eval(id, offset)?.0;
                    let n = u64::try_from(el.items.len()).unwrap_or(0);
                    self.table_init(id, index, u32::try_from(i).unwrap_or(0), at, 0, n)?;
                    self.instances[id as usize].elems[i] = Rc::from([]);
                }
                WasmSegmentMode::Declared => self.instances[id as usize].elems[i] = Rc::from([]),
                WasmSegmentMode::Passive => {}
            }
        }
        for (i, d) in module.data.iter().enumerate() {
            if let WasmSegmentMode::Active { index, offset } = d.mode {
                let at = self.eval(id, offset)?.0;
                let n = u64::try_from(d.bytes.len()).unwrap_or(0);
                self.memory_init(id, index, u32::try_from(i).unwrap_or(0), at, 0, n)?;
                self.instances[id as usize].data[i] = Rc::from([]);
            }
        }
        if let Some(s) = module.start {
            let f = self.instances[id as usize].funcs[s as usize];
            self.invoke(f, &[])?;
        }
        Ok(id)
    }

    fn eval(&mut self, instance: u32, init: u32) -> Result<(u64, u128), WasmTrap> {
        let fp = self.stack.len();
        self.call(FUNC_INIT, instance, init, fp)?;
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

    pub fn invoke(&mut self, func: u32, args: &[WasmValue]) -> Result<Vec<WasmValue>, WasmTrap> {
        let ty = self.funcs[func as usize].ty.clone();
        let fp = self.stack.len();
        self.stack.extend(args.iter().map(|a| a.slot()));
        self.vstack.extend(args.iter().map(|a| a.vector()));
        self.reserve(fp + ty.results.len());
        let r = self.enter(func, fp);
        let out = r.map(|()| ty.results.iter().enumerate().map(|(i, &t)| WasmValue::typed(t, self.stack[fp + i], self.vstack[fp + i])).collect());
        self.truncate(fp);
        out
    }

    fn enter(&mut self, func: u32, fp: usize) -> Result<(), WasmTrap> {
        match &self.funcs[func as usize].kind {
            FuncKind::Wasm { instance, index, .. } => {
                let (instance, index) = (*instance, *index);
                self.call(func, instance, index, fp)
            }
            FuncKind::Host(_) => self.host(func, fp),
        }
    }

    fn host(&mut self, func: u32, fp: usize) -> Result<(), WasmTrap> {
        let f = &self.funcs[func as usize];
        let args: Vec<WasmValue> = f.ty.params.iter().enumerate().map(|(i, &t)| WasmValue::typed(t, self.stack.get(fp + i).copied().unwrap_or(0), self.vstack.get(fp + i).copied().unwrap_or(0))).collect();
        let FuncKind::Host(h) = &f.kind else { return Err(WasmTrap::Unsupported) };
        let results = h(&args)?;
        self.reserve(fp + results.len());
        for (i, r) in results.iter().enumerate() {
            self.stack[fp + i] = r.slot();
            self.vstack[fp + i] = r.vector();
        }
        Ok(())
    }

    fn code(&self, func: u32, instance: u32, index: u32) -> Rc<Code> {
        let build = || {
            let m = &self.instances[instance as usize].module;
            let f = if func == FUNC_INIT { &m.inits[index as usize] } else { &m.functions[index as usize] };
            let ops: Vec<Decoded<WasmBytecode>> = Decoder::<WasmBytecode>::from(&f.code[..]).map_while(Result::ok).collect();
            let mut at = vec![u32::MAX; f.code.len() + 1];
            for (i, d) in ops.iter().enumerate() {
                at[d.offset] = u32::try_from(i).unwrap_or(u32::MAX);
            }
            Rc::new(Code { ops, at, bits: f.bits.clone(), tables: f.tables.clone() })
        };
        if func == FUNC_INIT {
            return build();
        }
        match &self.funcs[func as usize].kind {
            FuncKind::Wasm { code, .. } => code.get_or_init(build).clone(),
            FuncKind::Host(_) => Rc::new(Code { ops: Vec::new(), at: Vec::new(), bits: Box::new([]), tables: Box::new([]) }),
        }
    }

    fn function(&self, func: u32, instance: u32, index: u32) -> (u32, u32) {
        let m = &self.instances[instance as usize].module;
        let f = if func == FUNC_INIT { &m.inits[index as usize] } else { &m.functions[index as usize] };
        (f.params, f.registers)
    }

    fn frame(&mut self, func: u32, instance: u32, index: u32, fp: usize) -> Activation {
        let (params, registers) = self.function(func, instance, index);
        let end = fp + registers as usize;
        self.reserve(end);
        self.stack[fp + params as usize..end].fill(0);
        self.vstack[fp + params as usize..end].fill(0);
        Activation { instance, code: self.code(func, instance, index), fp, pc: 0 }
    }

    fn mem(&self, instance: u32, m: OperandValue) -> usize {
        self.instances[instance as usize].memories[usize::try_from(m).unwrap_or(0)] as usize
    }

    fn table(&self, instance: u32, t: OperandValue) -> usize {
        self.instances[instance as usize].tables[usize::try_from(t).unwrap_or(0)] as usize
    }

    fn address(&self, mem: usize, addr: u64, offset: OperandValue, size: usize) -> Result<usize, WasmTrap> {
        let m = &self.memories[mem];
        let addr = if m.ty.memory64 { addr } else { addr & u64::from(u32::MAX) };
        let ea = u128::from(addr) + u128::try_from(offset).unwrap_or(0);
        let end = ea + size as u128;
        if end > m.bytes.len() as u128 {
            return Err(WasmTrap::OutOfBoundsMemory);
        }
        usize::try_from(ea).map_err(|_| WasmTrap::OutOfBoundsMemory)
    }

    fn index(&self, mem: usize, v: u64) -> u64 {
        if self.memories[mem].ty.memory64 { v } else { v & u64::from(u32::MAX) }
    }

    fn table_index(&self, t: usize, v: u64) -> u64 {
        if self.tables[t].ty.table64 { v } else { v & u64::from(u32::MAX) }
    }

    fn table_init(&mut self, instance: u32, table: u32, elem: u32, d: u64, s: u64, n: u64) -> Result<(), WasmTrap> {
        let t = self.table(instance, OperandValue::from(table));
        let items = self.instances[instance as usize].elems[elem as usize].clone();
        let (d, s, n) = (usize::try_from(d).unwrap_or(usize::MAX), usize::try_from(s).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
        if s.checked_add(n).is_none_or(|e| e > items.len()) || d.checked_add(n).is_none_or(|e| e > self.tables[t].elems.len()) {
            return Err(WasmTrap::OutOfBoundsTable);
        }
        self.tables[t].elems[d..d + n].copy_from_slice(&items[s..s + n]);
        Ok(())
    }

    fn memory_init(&mut self, instance: u32, memory: u32, data: u32, d: u64, s: u64, n: u64) -> Result<(), WasmTrap> {
        let m = self.mem(instance, OperandValue::from(memory));
        let bytes = self.instances[instance as usize].data[data as usize].clone();
        let (s, n) = (usize::try_from(s).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
        let d = usize::try_from(self.index(m, d)).unwrap_or(usize::MAX);
        if s.checked_add(n).is_none_or(|e| e > bytes.len()) || d.checked_add(n).is_none_or(|e| e > self.memories[m].bytes.len()) {
            return Err(WasmTrap::OutOfBoundsMemory);
        }
        self.memories[m].bytes[d..d + n].copy_from_slice(&bytes[s..s + n]);
        Ok(())
    }

    fn grow_memory(&mut self, m: usize, delta: u64) -> u64 {
        let mem = &mut self.memories[m];
        let page = 1u64 << mem.ty.page_size_log2;
        let old = u64::try_from(mem.bytes.len()).unwrap_or(0) / page;
        let limit = mem.ty.limits.max.unwrap_or(u64::MAX).min(if mem.ty.page_size_log2 == 16 { MAX_PAGES } else { u64::from(u32::MAX) });
        let new = old.checked_add(delta);
        match new {
            Some(n) if n <= limit => {
                mem.bytes.resize(usize::try_from(n * page).unwrap_or(0), 0);
                old
            }
            _ => u64::MAX,
        }
    }

    fn grow_table(&mut self, t: usize, init: u64, delta: u64) -> u64 {
        let table = &mut self.tables[t];
        let old = u64::try_from(table.elems.len()).unwrap_or(0);
        let limit = table.ty.limits.max.unwrap_or(u64::from(u32::MAX)).min(10_000_000);
        match old.checked_add(delta) {
            Some(n) if n <= limit => {
                table.elems.resize(usize::try_from(n).unwrap_or(0), init);
                old
            }
            _ => u64::MAX,
        }
    }

    fn callee(&self, instance: u32, type_index: OperandValue, r: u64) -> Result<u32, WasmTrap> {
        if r == 0 {
            return Err(WasmTrap::UninitializedElement);
        }
        let f = u32::try_from(r - 1).map_err(|_| WasmTrap::UndefinedElement)?;
        let want = &self.instances[instance as usize].module.types[usize::try_from(type_index).unwrap_or(0)];
        if self.funcs[f as usize].ty == *want { Ok(f) } else { Err(WasmTrap::IndirectCallTypeMismatch) }
    }

    #[expect(clippy::too_many_lines, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap, clippy::cast_precision_loss, clippy::float_cmp, reason = "the interpreter loop: one arm per bytecode, with WebAssembly's wrapping and bit-level numeric semantics")]
    fn call(&mut self, func: u32, instance: u32, index: u32, fp: usize) -> Result<(), WasmTrap> {
        let mut frames: Vec<Activation> = vec![self.frame(func, instance, index, fp)];
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
                let m = self.mem($f.instance, $o[2]);
                let at = self.address(m, r!($f, $o[1]), $o[3], $n)?;
                let mut $b = [0u8; $n];
                $b.copy_from_slice(&self.memories[m].bytes[at..at + $n]);
                r!($f, $o[0]) = $e;
            }};
        }
        macro_rules! store {
            ($f:expr, $o:expr, $n:expr) => {{
                let m = self.mem($f.instance, $o[2]);
                let at = self.address(m, r!($f, $o[0]), $o[3], $n)?;
                let v = r!($f, $o[1]).to_le_bytes();
                self.memories[m].bytes[at..at + $n].copy_from_slice(&v[..$n]);
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
                        WasmBytecode::CallFunction | WasmBytecode::ReturnCall => (self.instances[inst as usize].funcs[o[0] as usize], o[1]),
                        WasmBytecode::CallIndirect | WasmBytecode::ReturnCallIndirect => {
                            let t = self.table(inst, o[1]);
                            let i = self.table_index(t, r!(f, o[2]));
                            let e = *self.tables[t].elems.get(usize::try_from(i).unwrap_or(usize::MAX)).ok_or(WasmTrap::UndefinedElement)?;
                            (self.callee(inst, o[0], e)?, o[3])
                        }
                        _ => {
                            let e = r!(f, o[1]);
                            if e == 0 {
                                return Err(WasmTrap::NullReference);
                            }
                            (u32::try_from(e - 1).unwrap_or(0), o[2])
                        }
                    };
                    let tail = matches!(d.bytecode, WasmBytecode::ReturnCall | WasmBytecode::ReturnCallIndirect | WasmBytecode::ReturnCallRef);
                    let mut fp = f.fp + base as usize;
                    if tail {
                        let n = self.funcs[target as usize].ty.params.len();
                        self.copy(fp, f.fp, n);
                        fp = f.fp;
                        frames.pop();
                    }
                    match &self.funcs[target as usize].kind {
                        FuncKind::Host(_) => self.host(target, fp)?,
                        FuncKind::Wasm { instance, index, .. } => {
                            let (i, x) = (*instance, *index);
                            if frames.len() >= MAX_DEPTH {
                                return Err(WasmTrap::CallStackExhausted);
                            }
                            let a = self.frame(target, i, x, fp);
                            frames.push(a);
                        }
                    }
                }
                WasmBytecode::Select => {
                    let from = if Slot(r!(f, o[3])).u32() != 0 { o[1] } else { o[2] };
                    self.copy(f.fp + from as usize, f.fp + o[0] as usize, 1);
                }
                WasmBytecode::GlobalGet => {
                    let g = self.instances[f.instance as usize].globals[o[1] as usize];
                    r!(f, o[0]) = self.globals[g as usize].value;
                    self.vstack[f.fp + o[0] as usize] = self.globals[g as usize].vector;
                }
                WasmBytecode::GlobalSet => {
                    let g = self.instances[f.instance as usize].globals[o[0] as usize];
                    self.globals[g as usize].value = r!(f, o[1]);
                    self.globals[g as usize].vector = self.vstack[f.fp + o[1] as usize];
                }
                WasmBytecode::TableGet => {
                    let t = self.table(f.instance, o[2]);
                    let i = self.table_index(t, r!(f, o[1]));
                    r!(f, o[0]) = *self.tables[t].elems.get(usize::try_from(i).unwrap_or(usize::MAX)).ok_or(WasmTrap::OutOfBoundsTable)?;
                }
                WasmBytecode::TableSet => {
                    let t = self.table(f.instance, o[2]);
                    let i = self.table_index(t, r!(f, o[0]));
                    let v = r!(f, o[1]);
                    *self.tables[t].elems.get_mut(usize::try_from(i).unwrap_or(usize::MAX)).ok_or(WasmTrap::OutOfBoundsTable)? = v;
                }
                WasmBytecode::TableSize => {
                    let t = self.table(f.instance, o[1]);
                    r!(f, o[0]) = self.tables[t].elems.len() as u64;
                }
                WasmBytecode::TableGrow => {
                    let t = self.table(f.instance, o[3]);
                    let (init, n) = (r!(f, o[1]), self.table_index(t, r!(f, o[2])));
                    let old = self.grow_table(t, init, n);
                    r!(f, o[0]) = if self.tables[t].ty.table64 { old } else { old & u64::from(u32::MAX) };
                }
                WasmBytecode::TableFill => {
                    let t = self.table(f.instance, o[1]);
                    let base = o[0] as usize;
                    let (i, v, n) = (self.table_index(t, self.stack[f.fp + base]), self.stack[f.fp + base + 1], self.table_index(t, self.stack[f.fp + base + 2]));
                    let (i, n) = (usize::try_from(i).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
                    if i.checked_add(n).is_none_or(|e| e > self.tables[t].elems.len()) {
                        return Err(WasmTrap::OutOfBoundsTable);
                    }
                    self.tables[t].elems[i..i + n].fill(v);
                }
                WasmBytecode::TableCopy => {
                    let (dt, st) = (self.table(f.instance, o[3]), self.table(f.instance, o[4]));
                    let (dd, s, n) = (self.table_index(dt, r!(f, o[0])), self.table_index(st, r!(f, o[1])), r!(f, o[2]));
                    let n = if self.tables[dt].ty.table64 && self.tables[st].ty.table64 { n } else { n & u64::from(u32::MAX) };
                    let (dd, s, n) = (usize::try_from(dd).unwrap_or(usize::MAX), usize::try_from(s).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
                    if s.checked_add(n).is_none_or(|e| e > self.tables[st].elems.len()) || dd.checked_add(n).is_none_or(|e| e > self.tables[dt].elems.len()) {
                        return Err(WasmTrap::OutOfBoundsTable);
                    }
                    if dt == st {
                        self.tables[dt].elems.copy_within(s..s + n, dd);
                    } else {
                        let src: Vec<u64> = self.tables[st].elems[s..s + n].to_vec();
                        self.tables[dt].elems[dd..dd + n].copy_from_slice(&src);
                    }
                }
                WasmBytecode::TableInit => {
                    let t = self.table(f.instance, o[3]);
                    let (dd, s, n) = (self.table_index(t, r!(f, o[0])), Slot(r!(f, o[1])).u32(), Slot(r!(f, o[2])).u32());
                    let inst = f.instance;
                    self.table_init(inst, u32::try_from(o[3]).unwrap_or(0), u32::try_from(o[4]).unwrap_or(0), dd, u64::from(s), u64::from(n))?;
                }
                WasmBytecode::ElemDrop => {
                    self.instances[f.instance as usize].elems[o[0] as usize] = Rc::from([]);
                }
                WasmBytecode::DataDrop => {
                    self.instances[f.instance as usize].data[o[0] as usize] = Rc::from([]);
                }
                WasmBytecode::MemoryInit => {
                    let base = f.fp + o[0] as usize;
                    let (dd, s, n) = (self.stack[base], Slot(self.stack[base + 1]).u32(), Slot(self.stack[base + 2]).u32());
                    let inst = f.instance;
                    self.memory_init(inst, u32::try_from(o[1]).unwrap_or(0), u32::try_from(o[2]).unwrap_or(0), dd, u64::from(s), u64::from(n))?;
                }
                WasmBytecode::MemoryCopy => {
                    let (dm, sm) = (self.mem(f.instance, o[1]), self.mem(f.instance, o[2]));
                    let base = f.fp + o[0] as usize;
                    let (dd, s, n) = (self.index(dm, self.stack[base]), self.index(sm, self.stack[base + 1]), self.stack[base + 2]);
                    let n = if self.memories[dm].ty.memory64 && self.memories[sm].ty.memory64 { n } else { n & u64::from(u32::MAX) };
                    let (dd, s, n) = (usize::try_from(dd).unwrap_or(usize::MAX), usize::try_from(s).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
                    if s.checked_add(n).is_none_or(|e| e > self.memories[sm].bytes.len()) || dd.checked_add(n).is_none_or(|e| e > self.memories[dm].bytes.len()) {
                        return Err(WasmTrap::OutOfBoundsMemory);
                    }
                    if dm == sm {
                        self.memories[dm].bytes.copy_within(s..s + n, dd);
                    } else {
                        let src: Vec<u8> = self.memories[sm].bytes[s..s + n].to_vec();
                        self.memories[dm].bytes[dd..dd + n].copy_from_slice(&src);
                    }
                }
                WasmBytecode::MemoryFill => {
                    let m = self.mem(f.instance, o[1]);
                    let base = f.fp + o[0] as usize;
                    let (dd, v, n) = (self.index(m, self.stack[base]), self.stack[base + 1] as u8, self.index(m, self.stack[base + 2]));
                    let (dd, n) = (usize::try_from(dd).unwrap_or(usize::MAX), usize::try_from(n).unwrap_or(usize::MAX));
                    if dd.checked_add(n).is_none_or(|e| e > self.memories[m].bytes.len()) {
                        return Err(WasmTrap::OutOfBoundsMemory);
                    }
                    self.memories[m].bytes[dd..dd + n].fill(v);
                }
                WasmBytecode::MemorySize => {
                    let m = self.mem(f.instance, o[1]);
                    let mem = &self.memories[m];
                    r!(f, o[0]) = (mem.bytes.len() as u64) >> mem.ty.page_size_log2;
                }
                WasmBytecode::MemoryGrow => {
                    let m = self.mem(f.instance, o[2]);
                    let n = self.index(m, r!(f, o[1]));
                    let old = self.grow_memory(m, n);
                    r!(f, o[0]) = if self.memories[m].ty.memory64 { old } else { old & u64::from(u32::MAX) };
                }
                WasmBytecode::I32Const | WasmBytecode::F32Const => r!(f, o[0]) = u32s(o[1] as u32),
                WasmBytecode::I64Const => r!(f, o[0]) = o[1] as u64,
                WasmBytecode::I64ConstPool | WasmBytecode::F64Const => {
                    r!(f, o[0]) = f.code.bits[o[1] as usize];
                }
                WasmBytecode::RefNull => r!(f, o[0]) = 0,
                WasmBytecode::RefIsNull => r!(f, o[0]) = b(r!(f, o[1]) == 0),
                WasmBytecode::RefFunc => r!(f, o[0]) = u64::from(self.instances[f.instance as usize].funcs[o[1] as usize]) + 1,
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
