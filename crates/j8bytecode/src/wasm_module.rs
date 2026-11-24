use alloc::boxed::Box;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WasmHeapType {
    Func,
    Extern,
    Any,
    Eq,
    I31,
    Struct,
    Array,
    Exn,
    None,
    NoFunc,
    NoExtern,
    NoExn,
    Concrete(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WasmRefType {
    pub nullable: bool,
    pub heap: WasmHeapType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WasmValType {
    I32,
    I64,
    F32,
    F64,
    V128,
    Ref(WasmRefType),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct WasmFuncType {
    pub params: Box<[WasmValType]>,
    pub results: Box<[WasmValType]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WasmStorageType {
    I8,
    I16,
    Val(WasmValType),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WasmFieldType {
    pub storage: WasmStorageType,
    pub mutable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WasmCompositeType {
    Func(WasmFuncType),
    Struct(Box<[WasmFieldType]>),
    Array(WasmFieldType),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WasmSubType {
    pub is_final: bool,
    pub supertype: Option<u32>,
    pub composite: WasmCompositeType,
}

impl WasmHeapType {
    #[must_use]
    pub fn map(self, f: &impl Fn(u32) -> u32) -> Self {
        if let Self::Concrete(i) = self { Self::Concrete(f(i)) } else { self }
    }
}

const HEAP_FUNC: i64 = -0x10;
const HEAP_EXTERN: i64 = -0x11;
const HEAP_ANY: i64 = -0x12;
const HEAP_EQ: i64 = -0x13;
const HEAP_I31: i64 = -0x14;
const HEAP_STRUCT: i64 = -0x15;
const HEAP_ARRAY: i64 = -0x16;
const HEAP_EXN: i64 = -0x17;
const HEAP_NOEXN: i64 = -0x0c;
const HEAP_NOFUNC: i64 = -0x0d;
const HEAP_NOEXTERN: i64 = -0x0e;
const HEAP_NONE: i64 = -0x0f;

impl From<WasmHeapType> for i64 {
    fn from(h: WasmHeapType) -> Self {
        match h {
            WasmHeapType::Func => HEAP_FUNC,
            WasmHeapType::Extern => HEAP_EXTERN,
            WasmHeapType::Any => HEAP_ANY,
            WasmHeapType::Eq => HEAP_EQ,
            WasmHeapType::I31 => HEAP_I31,
            WasmHeapType::Struct => HEAP_STRUCT,
            WasmHeapType::Array => HEAP_ARRAY,
            WasmHeapType::Exn => HEAP_EXN,
            WasmHeapType::NoExn => HEAP_NOEXN,
            WasmHeapType::NoFunc => HEAP_NOFUNC,
            WasmHeapType::NoExtern => HEAP_NOEXTERN,
            WasmHeapType::None => HEAP_NONE,
            WasmHeapType::Concrete(i) => i64::from(i),
        }
    }
}

impl From<i64> for WasmHeapType {
    fn from(v: i64) -> Self {
        match v {
            HEAP_FUNC => Self::Func,
            HEAP_EXTERN => Self::Extern,
            HEAP_ANY => Self::Any,
            HEAP_EQ => Self::Eq,
            HEAP_I31 => Self::I31,
            HEAP_STRUCT => Self::Struct,
            HEAP_ARRAY => Self::Array,
            HEAP_EXN => Self::Exn,
            HEAP_NOEXN => Self::NoExn,
            HEAP_NOFUNC => Self::NoFunc,
            HEAP_NOEXTERN => Self::NoExtern,
            _ => u32::try_from(v).map_or(Self::None, Self::Concrete),
        }
    }
}

impl WasmValType {
    #[must_use]
    pub fn map(self, f: &impl Fn(u32) -> u32) -> Self {
        if let Self::Ref(r) = self { Self::Ref(WasmRefType { nullable: r.nullable, heap: r.heap.map(f) }) } else { self }
    }
}

impl WasmFieldType {
    #[must_use]
    pub fn map(self, f: &impl Fn(u32) -> u32) -> Self {
        let storage = if let WasmStorageType::Val(v) = self.storage { WasmStorageType::Val(v.map(f)) } else { self.storage };
        Self { storage, mutable: self.mutable }
    }
}

impl WasmFuncType {
    #[must_use]
    pub fn map(&self, f: &impl Fn(u32) -> u32) -> Self {
        Self { params: self.params.iter().map(|t| t.map(f)).collect(), results: self.results.iter().map(|t| t.map(f)).collect() }
    }
}

impl WasmSubType {
    #[must_use]
    pub fn map(&self, f: &impl Fn(u32) -> u32) -> Self {
        let composite = match &self.composite {
            WasmCompositeType::Func(t) => WasmCompositeType::Func(t.map(f)),
            WasmCompositeType::Struct(fs) => WasmCompositeType::Struct(fs.iter().map(|x| x.map(f)).collect()),
            WasmCompositeType::Array(x) => WasmCompositeType::Array(x.map(f)),
        };
        Self { is_final: self.is_final, supertype: self.supertype.map(f), composite }
    }

    pub fn func(&self) -> Option<&WasmFuncType> {
        if let WasmCompositeType::Func(t) = &self.composite { Some(t) } else { None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WasmLimits {
    pub min: u64,
    pub max: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WasmTableType {
    pub element: WasmRefType,
    pub limits: WasmLimits,
    pub table64: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WasmMemoryType {
    pub limits: WasmLimits,
    pub memory64: bool,
    pub shared: bool,
    pub page_size_log2: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WasmGlobalType {
    pub content: WasmValType,
    pub mutable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmExternKind {
    Func,
    Table,
    Memory,
    Global,
    Tag,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WasmImportKind {
    Func(u32),
    Table(WasmTableType),
    Memory(WasmMemoryType),
    Global(WasmGlobalType),
    Tag(u32),
}

#[derive(Clone, Debug)]
pub struct WasmImport {
    pub module: Box<str>,
    pub name: Box<str>,
    pub kind: WasmImportKind,
}

#[derive(Clone, Debug)]
pub struct WasmExport {
    pub name: Box<str>,
    pub kind: WasmExternKind,
    pub index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmCatchKind {
    Catch,
    CatchRef,
    CatchAll,
    CatchAllRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WasmCatch {
    pub kind: WasmCatchKind,
    pub tag: u32,
    pub target: u32,
    pub land: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmHandler {
    pub start: u32,
    pub end: u32,
    pub catches: Box<[WasmCatch]>,
}

#[derive(Clone, Debug, Default)]
pub struct WasmFunction {
    pub ty: u32,
    pub params: u32,
    pub results: u32,
    pub registers: u32,
    pub code: Box<[u8]>,
    pub bits: Box<[u64]>,
    pub tables: Box<[Box<[u32]>]>,
    pub handlers: Box<[WasmHandler]>,
    pub vector: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmSegmentMode {
    Passive,
    Declared,
    Active { index: u32, offset: u32 },
}

#[derive(Clone, Debug)]
pub struct WasmElement {
    pub mode: WasmSegmentMode,
    pub element: WasmRefType,
    pub items: Box<[u32]>,
}

#[derive(Clone, Debug)]
pub struct WasmData {
    pub mode: WasmSegmentMode,
    pub bytes: Box<[u8]>,
}

#[derive(Clone, Debug)]
pub struct WasmCustom {
    pub name: Box<str>,
    pub bytes: Box<[u8]>,
}

#[derive(Clone, Debug)]
pub struct WasmGlobal {
    pub ty: WasmGlobalType,
    pub init: u32,
}

#[derive(Clone, Debug)]
pub struct WasmTable {
    pub ty: WasmTableType,
    pub init: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct WasmModule {
    pub types: Vec<WasmSubType>,
    pub groups: Vec<u32>,
    pub imports: Vec<WasmImport>,
    pub functions: Vec<WasmFunction>,
    pub tables: Vec<WasmTable>,
    pub memories: Vec<WasmMemoryType>,
    pub globals: Vec<WasmGlobal>,
    pub tags: Vec<u32>,
    pub exports: Vec<WasmExport>,
    pub start: Option<u32>,
    pub elements: Vec<WasmElement>,
    pub data: Vec<WasmData>,
    pub inits: Vec<WasmFunction>,
    pub customs: Vec<WasmCustom>,
}

impl WasmModule {
    pub fn imported(&self, kind: WasmExternKind) -> impl Iterator<Item = &WasmImport> {
        self.imports.iter().filter(move |i| WasmExternKind::from(&i.kind) == kind)
    }

    pub fn func_type(&self, ty: u32) -> &WasmFuncType {
        self.types[ty as usize].func().unwrap_or_else(|| unreachable!("type {ty} is validated as a function type"))
    }

    pub fn export(&self, name: &str) -> Option<&WasmExport> {
        self.exports.iter().find(|e| &*e.name == name)
    }
}

impl From<&WasmImportKind> for WasmExternKind {
    fn from(k: &WasmImportKind) -> Self {
        match k {
            WasmImportKind::Func(_) => Self::Func,
            WasmImportKind::Table(_) => Self::Table,
            WasmImportKind::Memory(_) => Self::Memory,
            WasmImportKind::Global(_) => Self::Global,
            WasmImportKind::Tag(_) => Self::Tag,
        }
    }
}
