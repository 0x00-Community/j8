use alloc::boxed::Box;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WasmRefType {
    pub nullable: bool,
    pub heap: WasmHeapType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WasmValType {
    I32,
    I64,
    F32,
    F64,
    V128,
    Ref(WasmRefType),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct WasmFuncType {
    pub params: Box<[WasmValType]>,
    pub results: Box<[WasmValType]>,
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

#[derive(Clone, Debug, Default)]
pub struct WasmFunction {
    pub ty: u32,
    pub params: u32,
    pub results: u32,
    pub registers: u32,
    pub code: Box<[u8]>,
    pub bits: Box<[u64]>,
    pub tables: Box<[Box<[u32]>]>,
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
    pub types: Vec<WasmFuncType>,
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
}

impl WasmModule {
    pub fn imported(&self, kind: WasmExternKind) -> impl Iterator<Item = &WasmImport> {
        self.imports.iter().filter(move |i| WasmExternKind::from(&i.kind) == kind)
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
