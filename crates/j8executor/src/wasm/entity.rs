use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::AtomicU32;

use j8bytecode::{Decoded, Decoder, OperandValue, WasmBytecode, WasmGlobalType, WasmHandler, WasmMemoryType, WasmModule, WasmTableType};
use j8framework::paragc::{WORD_POINTER_MIN, word_pointer};
use j8framework::{Collect, GcSlot, IsoCell, Mutation, ScopeLocal, Static, WordSlot, gc_from_raw};
use j8lockfree::{AtomicRefCell, OnceBox};

use super::{WasmMachine, WasmTrap};
use super::types::Types;

pub const OBJECT_TAG: u64 = 0xFFFE << 48;
const TAG_MASK: u64 = 0xFFFF << 48;

pub struct Code {
    pub ops: Vec<Decoded<WasmBytecode>>,
    pub at: Vec<u32>,
    pub bits: Box<[u64]>,
    pub tables: Box<[Box<[u32]>]>,
    pub handlers: Box<[WasmHandler]>,
    pub params: u32,
    pub results: u32,
    pub registers: u32,
    pub vector: bool,
    pub memory64: Option<bool>,
    pub jit: OnceBox<WasmMachine>,
    pub calls: AtomicU32,
    pub loops: AtomicU32,
}

impl Code {
    pub fn target(&self, d: &Decoded<WasmBytecode>, rel: OperandValue) -> usize {
        let byte = usize::try_from(OperandValue::try_from(d.offset).unwrap_or(0) + rel).unwrap_or(0);
        self.at.get(byte).map_or(usize::MAX, |&i| i as usize)
    }
}

impl Code {
    fn new(f: &j8bytecode::WasmFunction, memory64: Option<bool>) -> Self {
        let ops: Vec<Decoded<WasmBytecode>> = Decoder::<WasmBytecode>::from(&f.code[..]).map_while(Result::ok).collect();
        let mut at = alloc::vec![u32::MAX; f.code.len() + 1];
        for (i, d) in ops.iter().enumerate() {
            at[d.offset] = u32::try_from(i).unwrap_or(u32::MAX);
        }
        Self { ops, at, bits: f.bits.clone(), tables: f.tables.clone(), handlers: f.handlers.clone(), params: f.params, results: f.results, registers: f.registers, vector: f.vector, memory64, jit: OnceBox::default(), calls: AtomicU32::new(0), loops: AtomicU32::new(0) }
    }
}

pub struct WasmCompiled {
    pub module: WasmModule,
    code: Box<[OnceBox<Arc<Code>>]>,
    inits: Box<[OnceBox<Arc<Code>>]>,
}

impl From<WasmModule> for WasmCompiled {
    fn from(module: WasmModule) -> Self {
        let code = module.functions.iter().map(|_| OnceBox::default()).collect();
        let inits = module.inits.iter().map(|_| OnceBox::default()).collect();
        Self { module, code, inits }
    }
}

impl WasmCompiled {
    fn memory64(&self) -> Option<bool> {
        let imported = self.module.imports.iter().find_map(|i| if let j8bytecode::WasmImportKind::Memory(m) = i.kind { Some(m) } else { None });
        imported.or_else(|| self.module.memories.first().copied()).map(|m| m.memory64)
    }

    pub fn code(&self, index: u32) -> Arc<Code> {
        self.code[index as usize].get_or_init(|| Arc::new(Code::new(&self.module.functions[index as usize], self.memory64()))).clone()
    }

    pub fn init(&self, index: u32) -> Arc<Code> {
        self.inits[index as usize].get_or_init(|| Arc::new(Code::new(&self.module.inits[index as usize], self.memory64()))).clone()
    }
}

#[derive(Collect)]
#[collect(no_drop)]
pub struct WasmTypes(pub Static<AtomicRefCell<Types>>);

#[derive(Collect)]
#[collect(no_drop)]
pub struct WasmCells<'scope>(pub Box<[WordSlot<'scope>]>);

impl<'scope> WasmCells<'scope> {
    pub fn alloc(mc: &Mutation<'scope>, words: impl IntoIterator<Item = u64>) -> ScopeLocal<'scope, Self> {
        mc.alloc(Self(words.into_iter().map(WordSlot::new).collect()))
    }
}

#[derive(Collect)]
#[collect(no_drop)]
pub struct WasmFunc<'scope> {
    pub canon: u32,
    pub index: u32,
    pub instance: GcSlot<'scope, WasmInstance<'scope>>,
    pub host: WordSlot<'scope>,
    pub js: WordSlot<'scope>,
}

#[derive(Collect)]
#[collect(no_drop)]
pub struct WasmTable<'scope> {
    pub ty: Static<WasmTableType>,
    pub cells: GcSlot<'scope, WasmCells<'scope>>,
    pub js: WordSlot<'scope>,
}

impl<'scope> WasmTable<'scope> {
    pub fn cells(&self) -> &'scope [WordSlot<'scope>] {
        self.cells.get().map_or(&[], |c| &c.get().0)
    }

    pub fn index(&self, v: u64) -> u64 {
        if self.ty.table64 { v } else { v & u64::from(u32::MAX) }
    }
}

pub struct Bytes {
    data: UnsafeCell<Vec<u8>>,
}

unsafe impl Send for Bytes {}

unsafe impl Sync for Bytes {}

impl Bytes {
    #[expect(clippy::mut_from_ref, reason = "linear memory belongs to its isolate's single thread, like IsoCell; no two slices are held at once")]
    pub fn slice(&self) -> &mut [u8] {
        unsafe { &mut *self.data.get() }
    }

    pub fn resize(&self, len: usize) {
        unsafe { &mut *self.data.get() }.resize(len, 0);
    }
}

#[derive(Collect)]
#[collect(no_drop)]
pub struct WasmMemory<'scope> {
    pub ty: Static<WasmMemoryType>,
    pub bytes: Static<Arc<Bytes>>,
    pub buffer: WordSlot<'scope>,
    pub js: WordSlot<'scope>,
}

impl WasmMemory<'_> {
    pub fn new(ty: WasmMemoryType) -> Self {
        let n = usize::try_from(ty.limits.min << ty.page_size_log2).unwrap_or(0);
        Self { ty: Static(ty), bytes: Static(Arc::new(Bytes { data: UnsafeCell::new(alloc::vec![0; n]) })), buffer: WordSlot::new(0), js: WordSlot::new(0) }
    }

    pub fn data(&self) -> &mut [u8] {
        self.bytes.slice()
    }

    pub fn index(&self, v: u64) -> u64 {
        if self.ty.memory64 { v } else { v & u64::from(u32::MAX) }
    }

    pub fn pages(&self) -> u64 {
        u64::try_from(self.data().len()).unwrap_or(0) >> self.ty.page_size_log2
    }

    pub fn address(&self, addr: u64, offset: OperandValue, size: usize) -> Result<usize, WasmTrap> {
        let ea = u128::from(self.index(addr)) + u128::try_from(offset).unwrap_or(0);
        if ea + size as u128 > self.data().len() as u128 {
            return Err(WasmTrap::OutOfBoundsMemory);
        }
        usize::try_from(ea).map_err(|_| WasmTrap::OutOfBoundsMemory)
    }
}

#[derive(Collect)]
#[collect(no_drop)]
pub struct WasmGlobal<'scope> {
    pub ty: Static<WasmGlobalType>,
    pub value: IsoCell<u64>,
    pub high: IsoCell<u64>,
    pub cell: WordSlot<'scope>,
    pub js: WordSlot<'scope>,
}

impl<'scope> WasmGlobal<'scope> {
    pub fn new(ty: WasmGlobalType, v: (u64, u128)) -> Self {
        let mut g = Self { ty: Static(ty), value: IsoCell::new(0), high: IsoCell::new(0), cell: WordSlot::new(0), js: WordSlot::new(0) };
        if g.is_ref() {
            g.cell.set_mut(v.0);
        } else {
            g.store(v);
        }
        g
    }

    fn store(&self, (s, v): (u64, u128)) {
        if self.ty.content == j8bytecode::WasmValType::V128 {
            self.value.set(u64::try_from(v & u128::from(u64::MAX)).unwrap_or(0));
            self.high.set(u64::try_from(v >> u64::BITS).unwrap_or(0));
        } else {
            self.value.set(s);
        }
    }

    fn is_ref(&self) -> bool {
        matches!(self.ty.content, j8bytecode::WasmValType::Ref(_))
    }

    pub fn get(&self) -> (u64, u128) {
        if self.is_ref() {
            (self.cell.get(), 0)
        } else if self.ty.content == j8bytecode::WasmValType::V128 {
            let lo = self.value.get();
            (lo, u128::from(lo) | (u128::from(self.high.get()) << u64::BITS))
        } else {
            (self.value.get(), 0)
        }
    }

    pub fn set(&self, mc: &Mutation<'scope>, (s, v): (u64, u128)) {
        if self.is_ref() {
            self.cell.set(mc.gc_scope(), s);
        } else {
            self.store((s, v));
        }
    }
}

#[derive(Collect)]
#[collect(no_drop)]
pub struct WasmTag<'scope> {
    pub canon: u32,
    pub js: WordSlot<'scope>,
}

#[derive(Collect)]
#[collect(no_drop)]
pub struct WasmInstance<'scope> {
    pub module: Static<Arc<WasmCompiled>>,
    pub types: Static<Box<[u32]>>,
    pub funcs: Box<[GcSlot<'scope, WasmFunc<'scope>>]>,
    pub tables: Box<[GcSlot<'scope, WasmTable<'scope>>]>,
    pub memories: Box<[GcSlot<'scope, WasmMemory<'scope>>]>,
    pub globals: Box<[GcSlot<'scope, WasmGlobal<'scope>>]>,
    pub tags: Box<[GcSlot<'scope, WasmTag<'scope>>]>,
    pub elems: Box<[GcSlot<'scope, WasmCells<'scope>>]>,
    pub data: Box<[IsoCell<bool>]>,
    pub js: WordSlot<'scope>,
}

macro_rules! entity {
    ($($name:ident: $field:ident -> $ty:ident;)*) => {
        impl<'scope> WasmInstance<'scope> {
            $(
                pub fn $name(&self, i: impl TryInto<usize>) -> &'scope $ty<'scope> {
                    let i = i.try_into().unwrap_or(usize::MAX);
                    self.$field[i].get().unwrap_or_else(|| unreachable!("{} {i} is set during instantiation", stringify!($name))).get()
                }
            )*
        }
    };
}

entity! {
    func: funcs -> WasmFunc;
    table: tables -> WasmTable;
    memory: memories -> WasmMemory;
    global: globals -> WasmGlobal;
    tag: tags -> WasmTag;
}

impl<'scope> WasmInstance<'scope> {
    pub fn elem(&self, i: impl TryInto<usize>) -> &'scope [WordSlot<'scope>] {
        let i = i.try_into().unwrap_or(usize::MAX);
        self.elems[i].get().map_or(&[], |c| &c.get().0)
    }

    pub fn data(&self, i: impl TryInto<usize>) -> &[u8] {
        let i = i.try_into().unwrap_or(usize::MAX);
        if self.data[i].get() { &[] } else { &self.module.module.data[i].bytes }
    }

    pub fn ty(&self, i: impl TryInto<usize>) -> u32 {
        self.types[i.try_into().unwrap_or(usize::MAX)]
    }
}

pub struct Ptr;

impl Ptr {
    pub fn of<T>(p: ScopeLocal<'_, T>) -> u64 {
        OBJECT_TAG | u64::try_from(p.as_ptr().expose_provenance()).unwrap_or(0)
    }

    pub fn is_wasm(w: u64) -> bool {
        w >= WORD_POINTER_MIN && w & TAG_MASK == OBJECT_TAG
    }

    pub fn get<'scope, T>(w: u64) -> Option<&'scope T> {
        if !Self::is_wasm(w) {
            return None;
        }
        word_pointer(w).map(|p| unsafe { gc_from_raw::<T>(p.cast()) }.get())
    }

    pub fn local<'scope, T>(w: u64) -> Option<ScopeLocal<'scope, T>> {
        if !Self::is_wasm(w) {
            return None;
        }
        word_pointer(w).map(|p| unsafe { gc_from_raw::<T>(p.cast()) })
    }
}

j8framework::paragc::brand!(pub CellsBrand = for<'heap, 'scope> WasmCells<'scope>);
j8framework::paragc::brand!(pub TypesBrand = for<'heap, 'scope> WasmTypes);
j8framework::paragc::brand!(pub InstanceBrand = for<'heap, 'scope> WasmInstance<'scope>);
j8framework::paragc::brand!(pub FuncBrand = for<'heap, 'scope> WasmFunc<'scope>);
j8framework::paragc::brand!(pub TableBrand = for<'heap, 'scope> WasmTable<'scope>);
j8framework::paragc::brand!(pub MemoryBrand = for<'heap, 'scope> WasmMemory<'scope>);
j8framework::paragc::brand!(pub GlobalBrand = for<'heap, 'scope> WasmGlobal<'scope>);
j8framework::paragc::brand!(pub TagBrand = for<'heap, 'scope> WasmTag<'scope>);
