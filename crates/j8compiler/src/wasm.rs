use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use j8bytecode::{Encoder, Opcode, OperandScale, OperandValue, WasmBytecode, WasmCatch, WasmCatchKind, WasmCompositeType, WasmCustom, WasmData, WasmElement, WasmExport, WasmExternKind, WasmFieldType, WasmFuncType, WasmFunction, WasmHandler, WasmStorageType, WasmSubType, WasmGlobal, WasmGlobalType, WasmHeapType, WasmImport, WasmImportKind, WasmLimits, WasmMemoryType, WasmModule, WasmOperandType, WasmRefType, WasmSegmentMode, WasmTable, WasmTableType, WasmValType};
use wasmparser::{AbstractHeapType, BinaryReaderError, BlockType, Catch, CompositeInnerType, StorageType, ConstExpr, DataKind, ElementItems, ElementKind, ExternalKind, FuncValidator, FunctionBody, HeapType, MemoryType, Operator, Parser, Payload, RefType, TableInit, TableType, TypeRef, ValType, ValidPayload, Validator, ValidatorResources, WasmFeatures, WasmModuleResources};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmErrorKind {
    Invalid,
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct WasmError {
    pub kind: WasmErrorKind,
    pub message: Box<str>,
    pub offset: usize,
}

impl From<BinaryReaderError> for WasmError {
    fn from(e: BinaryReaderError) -> Self {
        Self { kind: WasmErrorKind::Invalid, message: e.message().into(), offset: usize::try_from(e.offset()).unwrap_or(usize::MAX) }
    }
}

impl WasmError {
    fn unsupported(offset: usize, what: impl Into<String>) -> Self {
        Self { kind: WasmErrorKind::Unsupported, message: what.into().into_boxed_str(), offset }
    }
}

impl core::fmt::Display for WasmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?} at {:#x}: {}", self.kind, self.offset, self.message)
    }
}

trait Convert<T> {
    fn convert(self) -> T;
}

pub trait CompileWasm: Sized {
    fn compile_wasm(bytes: &[u8]) -> Result<Self, WasmError>;
}

impl Convert<WasmRefType> for RefType {
    fn convert(self) -> WasmRefType {
        let r = self;
        let heap = match r.heap_type() {
            HeapType::Concrete(i) | HeapType::Exact(i) => WasmHeapType::Concrete(i.as_module_index().unwrap_or(u32::MAX)),
            HeapType::Abstract { ty, .. } => match ty {
                AbstractHeapType::Func => WasmHeapType::Func,
                AbstractHeapType::Extern => WasmHeapType::Extern,
                AbstractHeapType::Any => WasmHeapType::Any,
                AbstractHeapType::Eq => WasmHeapType::Eq,
                AbstractHeapType::I31 => WasmHeapType::I31,
                AbstractHeapType::Struct => WasmHeapType::Struct,
                AbstractHeapType::Array => WasmHeapType::Array,
                AbstractHeapType::Exn => WasmHeapType::Exn,
                AbstractHeapType::NoFunc => WasmHeapType::NoFunc,
                AbstractHeapType::NoExtern => WasmHeapType::NoExtern,
                AbstractHeapType::NoExn => WasmHeapType::NoExn,
                _ => WasmHeapType::None,
            },
        };
        WasmRefType { nullable: r.is_nullable(), heap }
    }
}

impl Convert<OperandValue> for HeapType {
    fn convert(self) -> OperandValue {
        OperandValue::from(RefType::new(true, self).map_or(WasmHeapType::None, |r| r.convert().heap))
    }
}

impl Convert<WasmValType> for ValType {
    fn convert(self) -> WasmValType {
        match self {
            ValType::I32 => WasmValType::I32,
            ValType::I64 => WasmValType::I64,
            ValType::F32 => WasmValType::F32,
            ValType::F64 => WasmValType::F64,
            ValType::V128 => WasmValType::V128,
            ValType::Ref(r) => WasmValType::Ref(r.convert()),
        }
    }
}

impl Convert<WasmFuncType> for &wasmparser::FuncType {
    fn convert(self) -> WasmFuncType {
        let f = self;
        WasmFuncType { params: f.params().iter().map(|&t| t.convert()).collect(), results: f.results().iter().map(|&t| t.convert()).collect() }
    }
}

impl Convert<WasmFieldType> for wasmparser::FieldType {
    fn convert(self) -> WasmFieldType {
        let storage = match self.element_type {
            StorageType::I8 => WasmStorageType::I8,
            StorageType::I16 => WasmStorageType::I16,
            StorageType::Val(v) => WasmStorageType::Val(v.convert()),
        };
        WasmFieldType { storage, mutable: self.mutable }
    }
}

impl Convert<WasmSubType> for &wasmparser::SubType {
    fn convert(self) -> WasmSubType {
        let composite = match &self.composite_type.inner {
            CompositeInnerType::Func(f) => WasmCompositeType::Func(f.convert()),
            CompositeInnerType::Struct(st) => WasmCompositeType::Struct(st.fields.iter().map(|f| f.convert()).collect()),
            CompositeInnerType::Array(a) => WasmCompositeType::Array(a.0.convert()),
            CompositeInnerType::Cont(_) => WasmCompositeType::Struct(Box::new([])),
        };
        WasmSubType { is_final: self.is_final, supertype: self.supertype_idxs.first().and_then(wasmparser::PackedIndex::as_module_index), composite }
    }
}

impl Convert<WasmTableType> for TableType {
    fn convert(self) -> WasmTableType {
        let t = self;
        WasmTableType { element: t.element_type.convert(), limits: WasmLimits { min: t.initial, max: t.maximum }, table64: t.table64 }
    }
}

impl Convert<WasmMemoryType> for MemoryType {
    fn convert(self) -> WasmMemoryType {
        let m = self;
        WasmMemoryType { limits: WasmLimits { min: m.initial, max: m.maximum }, memory64: m.memory64, shared: m.shared, page_size_log2: u8::try_from(m.page_size_log2()).unwrap_or(16) }
    }
}

impl Convert<WasmGlobalType> for wasmparser::GlobalType {
    fn convert(self) -> WasmGlobalType {
        let g = self;
        WasmGlobalType { content: g.content_type.convert(), mutable: g.mutable }
    }
}

impl Convert<WasmExternKind> for ExternalKind {
    fn convert(self) -> WasmExternKind {
        match self {
            ExternalKind::Func | ExternalKind::FuncExact => WasmExternKind::Func,
            ExternalKind::Table => WasmExternKind::Table,
            ExternalKind::Memory => WasmExternKind::Memory,
            ExternalKind::Global => WasmExternKind::Global,
            ExternalKind::Tag => WasmExternKind::Tag,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Frame {
    Block,
    Loop,
    If,
    Else,
}

struct Control {
    frame: Frame,
    base: u32,
    params: u32,
    results: u32,
    start: usize,
    exits: Vec<usize>,
    otherwise: Option<usize>,
    catches: Option<Vec<WasmCatch>>,
}

impl Control {
    fn arity(&self) -> u32 {
        if self.frame == Frame::Loop { self.params } else { self.results }
    }
}

const PLACEHOLDER: OperandValue = 0x7fff_ffff;
const MEMORY_OFFSET_LIMIT: OperandValue = 0xffff_ffff;

struct Emitter<'m> {
    module: &'m [u8],
    enc: Encoder,
    bits: Vec<u64>,
    tables: Vec<Box<[u32]>>,
    handlers: Vec<WasmHandler>,
    locals: u32,
    registers: u32,
    controls: Vec<Control>,
    alias: Vec<Option<u32>>,
    last: Option<Defined>,
}

struct Defined {
    start: usize,
    bytecode: WasmBytecode,
    operands: Vec<OperandValue>,
}

impl<'m> Emitter<'m> {
    fn new(module: &'m [u8], locals: u32) -> Self {
        Self { module, enc: Encoder::default(), bits: Vec::new(), tables: Vec::new(), handlers: Vec::new(), locals, registers: locals, controls: Vec::new(), alias: Vec::new(), last: None }
    }

    fn reg(&mut self, slot: u32) -> OperandValue {
        if let Some(a) = self.alias.get_mut(slot as usize) {
            *a = None;
        }
        let r = self.locals + slot;
        self.registers = self.registers.max(r + 1);
        OperandValue::from(r)
    }

    fn src(&mut self, slot: u32) -> OperandValue {
        match self.alias.get(slot as usize).copied().flatten() {
            Some(local) => OperandValue::from(local),
            None => self.reg(slot),
        }
    }

    fn alias(&mut self, slot: u32, local: u32) {
        let i = slot as usize;
        if self.alias.len() <= i {
            self.alias.resize(i + 1, None);
        }
        self.alias[i] = Some(local);
    }

    fn spill(&mut self, local: u32, below: u32) -> Result<(), WasmError> {
        for s in 0..below.min(u32::try_from(self.alias.len()).unwrap_or(0)) {
            if self.alias[s as usize] == Some(local) {
                let d = self.reg(s);
                self.emit(WasmBytecode::Mov, &[d, OperandValue::from(local)])?;
            }
        }
        Ok(())
    }

    fn materialize(&mut self, below: u32) -> Result<(), WasmError> {
        for s in 0..below.min(u32::try_from(self.alias.len()).unwrap_or(0)) {
            if let Some(local) = self.alias[s as usize] {
                let d = self.reg(s);
                self.emit(WasmBytecode::Mov, &[d, OperandValue::from(local)])?;
            }
        }
        self.forget();
        Ok(())
    }

    fn forget(&mut self) {
        self.alias.clear();
        self.last = None;
    }

    fn emit(&mut self, b: WasmBytecode, ops: &[OperandValue]) -> Result<usize, WasmError> {
        self.last = None;
        self.enc.emit(b, ops).map_err(|_| WasmError::unsupported(0, format!("{b:?}: operands")))
    }

    fn define(&mut self, b: WasmBytecode, ops: &[OperandValue]) -> Result<(), WasmError> {
        let start = self.enc.code.len();
        self.emit(b, ops)?;
        if (b as u16) >> u8::BITS == 0 {
            self.last = Some(Defined { start, bytecode: b, operands: ops.to_vec() });
        }
        Ok(())
    }

    fn set_local(&mut self, local: u32, top: u32, tee: bool) -> Result<(), WasmError> {
        let l = OperandValue::from(local);
        if let Some(m) = self.alias.get(top as usize).copied().flatten() {
            self.spill(local, top)?;
            if m != local {
                self.emit(WasmBytecode::Mov, &[l, OperandValue::from(m)])?;
            }
        } else {
            let t = self.reg(top);
            match self.last.take().filter(|d| d.operands.first() == Some(&t)) {
                Some(mut d) => {
                    self.enc.code.truncate(d.start);
                    self.spill(local, top)?;
                    d.operands[0] = l;
                    self.emit(d.bytecode, &d.operands)?;
                }
                None => {
                    self.spill(local, top)?;
                    self.emit(WasmBytecode::Mov, &[l, t])?;
                }
            }
        }
        if tee {
            self.alias(top, local);
        } else if let Some(a) = self.alias.get_mut(top as usize) {
            *a = None;
        }
        Ok(())
    }

    fn here(&self) -> usize {
        self.enc.code.len()
    }

    fn jump(&mut self, b: WasmBytecode, ops: &[OperandValue], target: Option<usize>) -> Result<usize, WasmError> {
        self.last = None;
        let at = self.here();
        let mut all: Vec<OperandValue> = ops.to_vec();
        all.push(target.map_or(PLACEHOLDER, |t| OperandValue::try_from(t).unwrap_or(0) - OperandValue::try_from(at).unwrap_or(0)));
        if target.is_none() {
            self.enc.code.push(WasmBytecode::ExtraWide as u8);
            self.enc.code.push((b as u16).to_be_bytes()[1]);
            for (v, info) in all.iter().zip(b.operand_infos()) {
                let n = info.size(OperandScale::Quadruple) as usize;
                self.enc.code.extend_from_slice(&i32::try_from(*v).unwrap_or(0).to_le_bytes()[..n]);
            }
            return Ok(at);
        }
        self.emit(b, &all)?;
        Ok(at)
    }

    fn patch(&mut self, at: usize, target: usize) {
        self.last = None;
        let b = WasmBytecode::from_page(0, self.enc.code[at + 1]).unwrap_or(WasmBytecode::Illegal);
        let n = b.operand_types().len() - 1;
        let pos = at + 2 + b.operand_infos().take(n).map(|i| i.size(OperandScale::Quadruple) as usize).sum::<usize>();
        let rel = i32::try_from(target).unwrap_or(0) - i32::try_from(at).unwrap_or(0);
        self.enc.code[pos..pos + 4].copy_from_slice(&rel.to_le_bytes());
    }

    fn moves(&mut self, from: u32, to: u32, n: u32) -> Result<(), WasmError> {
        if from != to {
            for i in 0..n {
                let (d, s) = (self.reg(to + i), self.reg(from + i));
                self.emit(WasmBytecode::Mov, &[d, s])?;
            }
        }
        Ok(())
    }

    fn branch(&mut self, depth: u32, height: u32) -> Result<(), WasmError> {
        let c = &self.controls[self.controls.len() - 1 - depth as usize];
        let (n, base, frame, start) = (c.arity(), c.base, c.frame, c.start);
        self.moves(height - n, base, n)?;
        if frame == Frame::Loop {
            self.jump(WasmBytecode::Jump, &[], Some(start))?;
        } else {
            let at = self.jump(WasmBytecode::Jump, &[], None)?;
            let i = self.controls.len() - 1 - depth as usize;
            self.controls[i].exits.push(at);
        }
        Ok(())
    }

    fn needs_moves(&self, depth: u32, height: u32) -> bool {
        let c = &self.controls[self.controls.len() - 1 - depth as usize];
        c.arity() > 0 && height - c.arity() != c.base
    }

    fn conditional(&mut self, on: WasmBytecode, off: WasmBytecode, cond: &[OperandValue], depth: u32, height: u32) -> Result<(), WasmError> {
        let i = self.controls.len() - 1 - depth as usize;
        if !self.needs_moves(depth, height) {
            if self.controls[i].frame == Frame::Loop {
                let start = self.controls[i].start;
                self.jump(on, cond, Some(start))?;
            } else {
                let at = self.jump(on, cond, None)?;
                self.controls[i].exits.push(at);
            }
            return Ok(());
        }
        let skip = self.jump(off, cond, None)?;
        self.branch(depth, height)?;
        let here = self.here();
        self.patch(skip, here);
        Ok(())
    }

    fn raw(&self, at: usize) -> (u32, usize) {
        let b = self.module[at];
        if matches!(b, 0xfb..=0xfe) {
            let (op, n) = Leb::u64(&self.module[at + 1..]);
            let op = u32::try_from(op).unwrap_or(u32::MAX);
            let code = if op < 0x100 { (u32::from(b) << 8) | op } else { (u32::from(b) << 12) | op };
            (code, at + 1 + n)
        } else {
            (u32::from(b), at + 1)
        }
    }

    fn immediates(&self, b: WasmBytecode, mut at: usize, regs: usize) -> Vec<OperandValue> {
        let mut out = Vec::new();
        let types = &b.operand_types()[regs..];
        let mut i = 0;
        while i < types.len() {
            match types[i] {
                WasmOperandType::Memory if types.get(i + 1) == Some(&WasmOperandType::MemOffset) => {
                    let (align, n) = Leb::u64(&self.module[at..]);
                    at += n;
                    let memory = if align & 0x40 != 0 {
                        let (m, n) = Leb::u64(&self.module[at..]);
                        at += n;
                        m
                    } else {
                        0
                    };
                    let (offset, n) = Leb::u64(&self.module[at..]);
                    at += n;
                    out.push(OperandValue::try_from(memory).unwrap_or(0));
                    out.push(OperandValue::try_from(offset).unwrap_or(OperandValue::MAX));
                    i += 2;
                    continue;
                }
                WasmOperandType::HeapType => {
                    let (v, n) = Leb::s64(&self.module[at..]);
                    at += n;
                    out.push(v);
                }
                WasmOperandType::Lane | WasmOperandType::Flag8 => {
                    out.push(OperandValue::from(self.module[at]));
                    at += 1;
                }
                _ => {
                    let (v, n) = Leb::u64(&self.module[at..]);
                    at += n;
                    out.push(OperandValue::try_from(v).unwrap_or(0));
                }
            }
            i += 1;
        }
        if matches!(b, WasmBytecode::TableInit | WasmBytecode::MemoryInit) {
            let n = out.len();
            out.swap(n - 2, n - 1);
        }
        out
    }
}

struct Leb;

impl Leb {
    fn u64(b: &[u8]) -> (u64, usize) {
        let mut v = 0u64;
        for (i, &x) in b.iter().enumerate().take(10) {
            v |= u64::from(x & 0x7f) << (7 * i);
            if x & 0x80 == 0 {
                return (v, i + 1);
            }
        }
        (v, b.len().min(10))
    }

    fn s64(b: &[u8]) -> (i64, usize) {
        let (v, n) = Self::u64(b);
        let bits = u32::try_from(7 * n).unwrap_or(64);
        if bits < 64 && b.get(n - 1).is_some_and(|x| x & 0x40 != 0) {
            ((v | (u64::MAX << bits)).cast_signed(), n)
        } else {
            (v.cast_signed(), n)
        }
    }
}

struct Body<'a, 'm> {
    emitter: Emitter<'m>,
    v: FuncValidator<ValidatorResources>,
    func: FunctionBody<'a>,
}

impl Body<'_, '_> {
    fn arity(&self, bt: BlockType) -> (u32, u32) {
        match bt {
            BlockType::Empty => (0, 0),
            BlockType::Type(_) => (0, 1),
            BlockType::FuncType(i) => self.v.resources().sub_type_at(i).map_or((0, 0), |t| {
                let f = t.unwrap_func();
                (u32::try_from(f.params().len()).unwrap_or(0), u32::try_from(f.results().len()).unwrap_or(0))
            }),
        }
    }

    fn fields(&self, type_index: u32) -> u32 {
        self.v.resources().sub_type_at(type_index).map_or(0, |t| match &t.composite_type.inner {
            CompositeInnerType::Struct(st) => u32::try_from(st.fields.len()).unwrap_or(0),
            _ => 0,
        })
    }

    fn signature(&self, type_index: u32) -> (u32, u32) {
        self.arity(BlockType::FuncType(type_index))
    }

    fn height(&self) -> u32 {
        self.v.operand_stack_height()
    }

    fn reachable(&self) -> bool {
        self.v.get_control_frame(0).is_some_and(|f| !f.unreachable)
    }

    fn shaped(op: &Operator<'_>) -> bool {
        matches!(
            op,
            Operator::Block { .. }
                | Operator::Loop { .. }
                | Operator::If { .. }
                | Operator::TryTable { .. }
                | Operator::Else
                | Operator::End
                | Operator::Unreachable
                | Operator::Br { .. }
                | Operator::BrIf { .. }
                | Operator::BrOnNull { .. }
                | Operator::BrOnNonNull { .. }
                | Operator::BrTable { .. }
                | Operator::Return
                | Operator::Call { .. }
                | Operator::ReturnCall { .. }
                | Operator::CallIndirect { .. }
                | Operator::ReturnCallIndirect { .. }
                | Operator::CallRef { .. }
                | Operator::ReturnCallRef { .. }
                | Operator::MemoryInit { .. }
                | Operator::MemoryCopy { .. }
                | Operator::MemoryFill { .. }
                | Operator::TableFill { .. }
                | Operator::ArrayFill { .. }
                | Operator::ArrayCopy { .. }
                | Operator::ArrayInitData { .. }
                | Operator::ArrayInitElem { .. }
                | Operator::V128Const { .. }
                | Operator::I8x16Shuffle { .. }
                | Operator::Throw { .. }
                | Operator::ThrowRef
                | Operator::StructNew { .. }
                | Operator::ArrayNewFixed { .. }
                | Operator::ArrayNewData { .. }
                | Operator::ArrayNewElem { .. }
                | Operator::BrOnCast { .. }
                | Operator::BrOnCastFail { .. }
        )
    }

    fn compile(mut self, ty: u32, params: u32, results: u32) -> Result<WasmFunction, WasmError> {
        let mut locals = self.func.get_locals_reader()?;
        for _ in 0..locals.get_count() {
            let at = locals.original_position();
            let (n, t) = locals.read()?;
            self.v.define_locals(at, n, t)?;
        }
        let total = self.v.len_locals();
        let mut vector = (0..total).any(|i| self.v.get_local_type(i) == Some(ValType::V128));
        self.emitter.locals = total;
        self.emitter.registers = total;
        self.emitter.controls.push(Control { frame: Frame::Block, base: 0, params: 0, results, start: 0, exits: Vec::new(), otherwise: None, catches: None });
        let mut ops = self.func.get_operators_reader()?;
        while !ops.eof() {
            let (op, pos) = ops.read_with_offset()?;
            let at = usize::try_from(pos).unwrap_or(usize::MAX);
            let h = self.height();
            let live = self.reachable();
            self.v.op(pos, &op)?;
            vector |= self.v.get_operand_type(0) == Some(Some(ValType::V128));
            self.operator(&op, at, h, live)?;
        }
        if self.v.control_stack_height() != 0 {
            return Err(WasmError { kind: WasmErrorKind::Invalid, message: "function body does not end".into(), offset: usize::try_from(ops.original_position()).unwrap_or(usize::MAX) });
        }
        let e = self.emitter;
        Ok(WasmFunction { ty, params, results, registers: e.registers + 1, code: e.enc.code.into(), bits: e.bits.into(), tables: e.tables.into(), handlers: e.handlers.into(), vector })
    }

    #[expect(clippy::too_many_lines, reason = "one arm per WebAssembly control and special-shape operator")]
    fn operator(&mut self, op: &Operator<'_>, at: usize, h: u32, live: bool) -> Result<(), WasmError> {
        let e = &mut self.emitter;
        if !live {
            e.forget();
        } else if Body::shaped(op) {
            e.materialize(h)?;
        }
        match *op {
            Operator::Block { blockty } | Operator::Loop { blockty } | Operator::If { blockty } => {
                let (p, r) = self.arity(blockty);
                let e = &mut self.emitter;
                let is_if = matches!(op, Operator::If { .. });
                let top = if is_if { h.saturating_sub(1) } else { h };
                let otherwise = if is_if && live {
                    let c = e.reg(h - 1);
                    Some(e.jump(WasmBytecode::JumpIfZero, &[c], None)?)
                } else {
                    None
                };
                let frame = match op {
                    Operator::Loop { .. } => Frame::Loop,
                    Operator::If { .. } => Frame::If,
                    _ => Frame::Block,
                };
                let start = e.here();
                e.controls.push(Control { frame, base: top.saturating_sub(p), params: p, results: r, start, exits: Vec::new(), otherwise, catches: None });
            }
            Operator::TryTable { ref try_table } => {
                let (p, r) = self.arity(try_table.ty);
                let land = h - p;
                let mut catches = Vec::new();
                let mut over = None;
                if live {
                    over = Some(self.emitter.jump(WasmBytecode::Jump, &[], None)?);
                    for c in &try_table.catches {
                        let (kind, tag, label) = match *c {
                            Catch::One { tag, label } => (WasmCatchKind::Catch, tag, label),
                            Catch::OneRef { tag, label } => (WasmCatchKind::CatchRef, tag, label),
                            Catch::All { label } => (WasmCatchKind::CatchAll, 0, label),
                            Catch::AllRef { label } => (WasmCatchKind::CatchAllRef, 0, label),
                        };
                        let params = match kind {
                            WasmCatchKind::Catch | WasmCatchKind::CatchRef => self.v.resources().tag_at(tag).map_or(0, |t| u32::try_from(t.params().len()).unwrap_or(0)),
                            _ => 0,
                        };
                        let n = params + u32::from(matches!(kind, WasmCatchKind::CatchRef | WasmCatchKind::CatchAllRef));
                        let e = &mut self.emitter;
                        let target = u32::try_from(e.here()).unwrap_or(0);
                        let reg = e.reg(land);
                        e.reg(land + n);
                        e.branch(label, land + n)?;
                        catches.push(WasmCatch { kind, tag, target, land: u32::try_from(reg).unwrap_or(0) });
                    }
                }
                let e = &mut self.emitter;
                let start = e.here();
                if let Some(o) = over {
                    e.patch(o, start);
                }
                e.controls.push(Control { frame: Frame::Block, base: land, params: p, results: r, start, exits: Vec::new(), otherwise: None, catches: live.then_some(catches) });
            }
            Operator::Else => {
                if live {
                    let j = e.jump(WasmBytecode::Jump, &[], None)?;
                    let c = e.controls.last_mut().ok_or_else(|| WasmError::unsupported(at, "else"))?;
                    c.exits.push(j);
                }
                let here = e.here();
                let c = e.controls.last_mut().ok_or_else(|| WasmError::unsupported(at, "else"))?;
                c.frame = Frame::Else;
                if let Some(o) = c.otherwise.take() {
                    e.patch(o, here);
                }
            }
            Operator::End => {
                let c = e.controls.pop().ok_or_else(|| WasmError::unsupported(at, "end"))?;
                let here = e.here();
                if let Some(catches) = c.catches {
                    e.handlers.push(WasmHandler { start: u32::try_from(c.start).unwrap_or(0), end: u32::try_from(here).unwrap_or(0), catches: catches.into() });
                }
                for x in c.exits {
                    e.patch(x, here);
                }
                if let Some(o) = c.otherwise {
                    e.patch(o, here);
                }
                if e.controls.is_empty() {
                    let base = e.reg(0);
                    e.emit(WasmBytecode::Return, &[base, OperandValue::from(c.results)])?;
                }
            }
            _ if !live => {}
            Operator::Nop | Operator::Drop => {}
            Operator::Unreachable => {
                e.emit(WasmBytecode::Unreachable, &[])?;
            }
            Operator::Br { relative_depth } => e.branch(relative_depth, h)?,
            Operator::BrIf { relative_depth } => {
                let c = e.reg(h - 1);
                e.conditional(WasmBytecode::JumpIfNonZero, WasmBytecode::JumpIfZero, &[c], relative_depth, h - 1)?;
            }
            Operator::BrOnNull { relative_depth } => {
                let r = e.reg(h - 1);
                e.conditional(WasmBytecode::BrOnNull, WasmBytecode::BrOnNonNull, &[r], relative_depth, h - 1)?;
            }
            Operator::BrOnNonNull { relative_depth } => {
                let r = e.reg(h - 1);
                e.conditional(WasmBytecode::BrOnNonNull, WasmBytecode::BrOnNull, &[r], relative_depth, h)?;
            }
            Operator::BrTable { ref targets } => {
                let index = e.reg(h - 1);
                let slot = OperandValue::try_from(e.tables.len()).unwrap_or(0);
                e.tables.push(Box::new([]));
                e.emit(WasmBytecode::BrTable, &[index, slot])?;
                let mut depths: Vec<u32> = targets.targets().collect::<Result<_, _>>()?;
                depths.push(targets.default());
                let mut stubs: Vec<(u32, u32)> = Vec::new();
                let mut entries = Vec::with_capacity(depths.len());
                for d in depths {
                    let stub = match stubs.iter().find(|(x, _)| *x == d) {
                        Some(&(_, s)) => s,
                        None => {
                            let s = u32::try_from(e.here()).unwrap_or(0);
                            e.branch(d, h - 1)?;
                            stubs.push((d, s));
                            s
                        }
                    };
                    entries.push(stub);
                }
                e.tables[usize::try_from(slot).unwrap_or(0)] = entries.into();
            }
            Operator::Return => {
                let n = e.controls[0].results;
                let base = e.reg(h - n);
                e.emit(WasmBytecode::Return, &[base, OperandValue::from(n)])?;
            }
            Operator::LocalGet { local_index } => {
                e.reg(h);
                e.alias(h, local_index);
            }
            Operator::LocalSet { local_index } => e.set_local(local_index, h - 1, false)?,
            Operator::LocalTee { local_index } => e.set_local(local_index, h - 1, true)?,
            Operator::Select | Operator::TypedSelect { .. } | Operator::TypedSelectMulti { .. } => {
                let (a, b, c) = (e.src(h - 3), e.src(h - 2), e.src(h - 1));
                let d = e.reg(h - 3);
                e.define(WasmBytecode::Select, &[d, a, b, c])?;
            }
            Operator::I32Const { value } => {
                let d = e.reg(h);
                e.define(WasmBytecode::I32Const, &[d, OperandValue::from(value)])?;
            }
            Operator::I64Const { value } => {
                let d = e.reg(h);
                if let Ok(v) = i32::try_from(value) {
                    e.define(WasmBytecode::I64Const, &[d, OperandValue::from(v)])?;
                } else {
                    let i = OperandValue::try_from(e.bits.len()).unwrap_or(0);
                    e.bits.push(u64::from_ne_bytes(value.to_ne_bytes()));
                    e.define(WasmBytecode::I64ConstPool, &[d, i])?;
                }
            }
            Operator::F32Const { value } => {
                let d = e.reg(h);
                e.define(WasmBytecode::F32Const, &[d, OperandValue::from(i32::from_ne_bytes(value.bits().to_ne_bytes()))])?;
            }
            Operator::F64Const { value } => {
                let d = e.reg(h);
                let i = OperandValue::try_from(e.bits.len()).unwrap_or(0);
                e.bits.push(value.bits());
                e.define(WasmBytecode::F64Const, &[d, i])?;
            }
            Operator::GlobalGet { global_index } => {
                let d = e.reg(h);
                e.define(WasmBytecode::GlobalGet, &[d, OperandValue::from(global_index)])?;
            }
            Operator::GlobalSet { global_index } => {
                let s = e.src(h - 1);
                e.emit(WasmBytecode::GlobalSet, &[OperandValue::from(global_index), s])?;
            }
            Operator::RefNull { .. } => {
                let d = e.reg(h);
                e.emit(WasmBytecode::RefNull, &[d])?;
            }
            Operator::RefFunc { function_index } => {
                let d = e.reg(h);
                e.emit(WasmBytecode::RefFunc, &[d, OperandValue::from(function_index)])?;
            }
            Operator::Call { function_index } => {
                let ti = self.v.resources().type_index_of_function(function_index).unwrap_or(0);
                let (p, r) = self.signature(ti);
                let e = &mut self.emitter;
                let base = e.reg(h - p);
                e.reg(h - p + r.max(p));
                e.emit(WasmBytecode::CallFunction, &[OperandValue::from(function_index), base])?;
            }
            Operator::ReturnCall { function_index } => {
                let ti = self.v.resources().type_index_of_function(function_index).unwrap_or(0);
                let (p, _) = self.signature(ti);
                let e = &mut self.emitter;
                let base = e.reg(h - p);
                e.emit(WasmBytecode::ReturnCall, &[OperandValue::from(function_index), base])?;
            }
            Operator::CallIndirect { type_index, table_index } | Operator::ReturnCallIndirect { type_index, table_index } => {
                let (p, r) = self.signature(type_index);
                let e = &mut self.emitter;
                let callee = e.reg(h - 1);
                let base = e.reg(h - 1 - p);
                e.reg(h - 1 - p + r.max(p));
                let b = if matches!(op, Operator::CallIndirect { .. }) { WasmBytecode::CallIndirect } else { WasmBytecode::ReturnCallIndirect };
                e.emit(b, &[OperandValue::from(type_index), OperandValue::from(table_index), callee, base])?;
            }
            Operator::CallRef { type_index } | Operator::ReturnCallRef { type_index } => {
                let (p, r) = self.signature(type_index);
                let e = &mut self.emitter;
                let callee = e.reg(h - 1);
                let base = e.reg(h - 1 - p);
                e.reg(h - 1 - p + r.max(p));
                let b = if matches!(op, Operator::CallRef { .. }) { WasmBytecode::CallRef } else { WasmBytecode::ReturnCallRef };
                e.emit(b, &[OperandValue::from(type_index), callee, base])?;
            }
            Operator::MemoryInit { .. } | Operator::MemoryCopy { .. } | Operator::MemoryFill { .. } | Operator::TableFill { .. } | Operator::ArrayFill { .. } | Operator::ArrayCopy { .. } | Operator::ArrayInitData { .. } | Operator::ArrayInitElem { .. } => {
                let (code, imm) = e.raw(at);
                let b = WasmBytecode::from_wasm(code).ok_or_else(|| WasmError::unsupported(at, "bulk operator"))?;
                let pops = match op {
                    Operator::ArrayCopy { .. } => 5,
                    Operator::ArrayFill { .. } | Operator::ArrayInitData { .. } | Operator::ArrayInitElem { .. } => 4,
                    _ => 3,
                };
                let base = e.reg(h - pops);
                e.reg(h - 1);
                let mut ops = alloc::vec![base];
                ops.extend(e.immediates(b, imm, 1));
                e.emit(b, &ops)?;
            }
            Operator::V128Const { value } => {
                let d = e.reg(h);
                let i = OperandValue::try_from(e.bits.len()).unwrap_or(0);
                let v = value.i128().cast_unsigned();
                e.bits.extend([u64::try_from(v & u128::from(u64::MAX)).unwrap_or(0), u64::try_from(v >> 64).unwrap_or(0)]);
                e.emit(WasmBytecode::S128Const, &[d, i])?;
            }
            Operator::I8x16Shuffle { lanes } => {
                let (dst, a, b) = (e.reg(h - 2), e.reg(h - 2), e.reg(h - 1));
                let i = OperandValue::try_from(e.bits.len()).unwrap_or(0);
                let v = u128::from_le_bytes(lanes);
                e.bits.extend([u64::try_from(v & u128::from(u64::MAX)).unwrap_or(0), u64::try_from(v >> 64).unwrap_or(0)]);
                e.emit(WasmBytecode::I8x16Shuffle, &[dst, a, b, i])?;
            }
            Operator::Throw { tag_index } => {
                let n = self.v.resources().tag_at(tag_index).map_or(0, |t| u32::try_from(t.params().len()).unwrap_or(0));
                let e = &mut self.emitter;
                let base = e.reg(h - n);
                e.emit(WasmBytecode::Throw, &[OperandValue::from(tag_index), base])?;
            }
            Operator::ThrowRef => {
                let r = e.reg(h - 1);
                e.emit(WasmBytecode::ThrowRef, &[r])?;
            }
            Operator::StructNew { struct_type_index } => {
                let n = self.fields(struct_type_index);
                let e = &mut self.emitter;
                let base = e.reg(h - n);
                e.emit(WasmBytecode::StructNew, &[base, OperandValue::from(struct_type_index), base])?;
            }
            Operator::ArrayNewFixed { array_type_index, array_size } => {
                let base = e.reg(h - array_size);
                e.emit(WasmBytecode::ArrayNewFixed, &[base, base, OperandValue::from(array_type_index), OperandValue::from(array_size)])?;
            }
            Operator::ArrayNewData { .. } | Operator::ArrayNewElem { .. } => {
                let (code, imm) = e.raw(at);
                let b = WasmBytecode::from_wasm(code).ok_or_else(|| WasmError::unsupported(at, "array operator"))?;
                let base = e.reg(h - 2);
                let mut ops = alloc::vec![base, base];
                ops.extend(e.immediates(b, imm, 2));
                e.emit(b, &ops)?;
            }
            Operator::BrOnCast { relative_depth, from_ref_type, to_ref_type } | Operator::BrOnCastFail { relative_depth, from_ref_type, to_ref_type } => {
                let r = e.reg(h - 1);
                let flags = OperandValue::from(u8::from(from_ref_type.is_nullable()) | (u8::from(to_ref_type.is_nullable()) << 1));
                let cond = [r, flags, from_ref_type.heap_type().convert(), to_ref_type.heap_type().convert()];
                let (on, off) = if matches!(op, Operator::BrOnCast { .. }) { (WasmBytecode::BrOnCast, WasmBytecode::BrOnCastFail) } else { (WasmBytecode::BrOnCastFail, WasmBytecode::BrOnCast) };
                e.conditional(on, off, &cond, relative_depth, h)?;
            }
            _ => {
                let (code, imm) = e.raw(at);
                let b = WasmBytecode::from_wasm(code).ok_or_else(|| WasmError::unsupported(at, format!("{op:?}")))?;
                let types = b.operand_types();
                let k = types.iter().take_while(|t| **t == WasmOperandType::Reg).count();
                let after = i64::from(self.height());
                let d = after - i64::from(h);
                let k64 = i64::try_from(k).unwrap_or(0);
                let n = (k64 - d) / 2;
                let m = n + d;
                let n = u32::try_from(n).map_err(|_| WasmError::unsupported(at, format!("{op:?}: shape")))?;
                let m = u32::try_from(m).map_err(|_| WasmError::unsupported(at, format!("{op:?}: shape")))?;
                let e = &mut self.emitter;
                let sources: Vec<OperandValue> = (0..n).map(|i| e.src(h - n + i)).collect();
                let mut ops: Vec<OperandValue> = (0..m).map(|i| e.reg(h - n + i)).collect();
                ops.extend(sources);
                let imms = e.immediates(b, imm, k);
                ops.extend(imms.into_iter().map(|v| v.min(MEMORY_OFFSET_LIMIT)));
                if m == 1 {
                    e.define(b, &ops)?;
                } else {
                    e.emit(b, &ops)?;
                }
            }
        }
        Ok(())
    }
}

struct ModuleBuilder<'m> {
    bytes: &'m [u8],
    module: WasmModule,
    functions: Vec<u32>,
}

impl ModuleBuilder<'_> {
    #[expect(clippy::too_many_lines, reason = "one arm per constant-expression operator")]
    fn init(&mut self, expr: &ConstExpr<'_>) -> Result<u32, WasmError> {
        let mut e = Emitter::new(self.bytes, 0);
        let mut h = 0u32;
        let mut ops = expr.get_operators_reader();
        while !ops.eof() {
            let (op, pos) = ops.read_with_offset()?;
            let at = usize::try_from(pos).unwrap_or(usize::MAX);
            match op {
                Operator::End => {}
                Operator::I32Const { value } => {
                    let d = e.reg(h);
                    e.emit(WasmBytecode::I32Const, &[d, OperandValue::from(value)])?;
                    h += 1;
                }
                Operator::I64Const { value } => {
                    let d = e.reg(h);
                    let i = OperandValue::try_from(e.bits.len()).unwrap_or(0);
                    e.bits.push(u64::from_ne_bytes(value.to_ne_bytes()));
                    e.emit(WasmBytecode::I64ConstPool, &[d, i])?;
                    h += 1;
                }
                Operator::F32Const { value } => {
                    let d = e.reg(h);
                    e.emit(WasmBytecode::F32Const, &[d, OperandValue::from(i32::from_ne_bytes(value.bits().to_ne_bytes()))])?;
                    h += 1;
                }
                Operator::F64Const { value } => {
                    let d = e.reg(h);
                    let i = OperandValue::try_from(e.bits.len()).unwrap_or(0);
                    e.bits.push(value.bits());
                    e.emit(WasmBytecode::F64Const, &[d, i])?;
                    h += 1;
                }
                Operator::GlobalGet { global_index } => {
                    let d = e.reg(h);
                    e.emit(WasmBytecode::GlobalGet, &[d, OperandValue::from(global_index)])?;
                    h += 1;
                }
                Operator::V128Const { value } => {
                    let d = e.reg(h);
                    let i = OperandValue::try_from(e.bits.len()).unwrap_or(0);
                    let v = value.i128().cast_unsigned();
                    e.bits.extend([u64::try_from(v & u128::from(u64::MAX)).unwrap_or(0), u64::try_from(v >> 64).unwrap_or(0)]);
                    e.emit(WasmBytecode::S128Const, &[d, i])?;
                    h += 1;
                }
                Operator::RefNull { .. } => {
                    let d = e.reg(h);
                    e.emit(WasmBytecode::RefNull, &[d])?;
                    h += 1;
                }
                Operator::RefFunc { function_index } => {
                    let d = e.reg(h);
                    e.emit(WasmBytecode::RefFunc, &[d, OperandValue::from(function_index)])?;
                    h += 1;
                }
                Operator::I32Add | Operator::I32Sub | Operator::I32Mul | Operator::I64Add | Operator::I64Sub | Operator::I64Mul => {
                    let (code, _) = e.raw(at);
                    let b = WasmBytecode::from_wasm(code).ok_or_else(|| WasmError::unsupported(at, "constant operator"))?;
                    let (d, x, y) = (e.reg(h - 2), e.reg(h - 2), e.reg(h - 1));
                    e.emit(b, &[d, x, y])?;
                    h -= 1;
                }
                Operator::StructNew { struct_type_index } => {
                    let n = match &self.module.types[struct_type_index as usize].composite {
                        WasmCompositeType::Struct(fs) => u32::try_from(fs.len()).unwrap_or(0),
                        _ => 0,
                    };
                    let base = e.reg(h - n);
                    e.emit(WasmBytecode::StructNew, &[base, OperandValue::from(struct_type_index), base])?;
                    h = h - n + 1;
                }
                Operator::StructNewDefault { struct_type_index } => {
                    let d = e.reg(h);
                    e.emit(WasmBytecode::StructNewDefault, &[d, OperandValue::from(struct_type_index)])?;
                    h += 1;
                }
                Operator::ArrayNew { array_type_index } => {
                    let (d, v, n) = (e.reg(h - 2), e.reg(h - 2), e.reg(h - 1));
                    e.emit(WasmBytecode::ArrayNew, &[d, v, n, OperandValue::from(array_type_index)])?;
                    h -= 1;
                }
                Operator::ArrayNewDefault { array_type_index } => {
                    let (d, n) = (e.reg(h - 1), e.reg(h - 1));
                    e.emit(WasmBytecode::ArrayNewDefault, &[d, n, OperandValue::from(array_type_index)])?;
                }
                Operator::ArrayNewFixed { array_type_index, array_size } => {
                    let base = e.reg(h - array_size);
                    e.emit(WasmBytecode::ArrayNewFixed, &[base, base, OperandValue::from(array_type_index), OperandValue::from(array_size)])?;
                    h = h - array_size + 1;
                }
                Operator::RefI31 | Operator::AnyConvertExtern | Operator::ExternConvertAny => {
                    let (code, _) = e.raw(at);
                    let b = WasmBytecode::from_wasm(code).ok_or_else(|| WasmError::unsupported(at, "constant operator"))?;
                    let (d, x) = (e.reg(h - 1), e.reg(h - 1));
                    e.emit(b, &[d, x])?;
                }
                other => return Err(WasmError::unsupported(at, format!("constant expression {other:?}"))),
            }
        }
        let base = e.reg(0);
        e.emit(WasmBytecode::Return, &[base, 1])?;
        let f = WasmFunction { ty: u32::MAX, params: 0, results: 1, registers: e.registers + 1, code: e.enc.code.into(), bits: e.bits.into(), tables: e.tables.into(), handlers: Box::new([]), vector: true };
        self.module.inits.push(f);
        Ok(u32::try_from(self.module.inits.len() - 1).unwrap_or(0))
    }

    fn function_ref(&mut self, f: u32) -> u32 {
        let mut e = Emitter::new(self.bytes, 0);
        let d = e.reg(0);
        let _ = e.emit(WasmBytecode::RefFunc, &[d, OperandValue::from(f)]);
        let _ = e.emit(WasmBytecode::Return, &[d, 1]);
        self.module.inits.push(WasmFunction { ty: u32::MAX, params: 0, results: 1, registers: 2, code: e.enc.code.into(), ..WasmFunction::default() });
        u32::try_from(self.module.inits.len() - 1).unwrap_or(0)
    }

    #[expect(clippy::too_many_lines, reason = "one arm per module section")]
    fn payload(&mut self, validator: &mut Validator, payload: Payload<'_>) -> Result<(), WasmError> {
        if let ValidPayload::Func(func, body) = validator.payload(&payload)? {
            let index = u32::try_from(self.module.functions.len()).unwrap_or(0);
            let ty = self.functions.get(index as usize).copied().unwrap_or(0);
            let sig = self.module.func_type(ty);
            let (params, results) = (u32::try_from(sig.params.len()).unwrap_or(0), u32::try_from(sig.results.len()).unwrap_or(0));
            let v = func.into_validator(wasmparser::FuncValidatorAllocations::default());
            let f = Body { emitter: Emitter::new(self.bytes, 0), v, func: body }.compile(ty, params, results)?;
            self.module.functions.push(f);
            return Ok(());
        }
        match payload {
            Payload::TypeSection(r) => {
                for group in r {
                    let mut n = 0u32;
                    for (_, sub) in group?.into_types_and_offsets() {
                        self.module.types.push(sub.convert());
                        n += 1;
                    }
                    self.module.groups.push(n);
                }
            }
            Payload::ImportSection(r) => {
                for imp in r.into_imports() {
                    let imp = imp?;
                    let kind = match imp.ty {
                        TypeRef::Func(t) | TypeRef::FuncExact(t) => WasmImportKind::Func(t),
                        TypeRef::Table(t) => WasmImportKind::Table(t.convert()),
                        TypeRef::Memory(m) => WasmImportKind::Memory(m.convert()),
                        TypeRef::Global(g) => WasmImportKind::Global(g.convert()),
                        TypeRef::Tag(t) => WasmImportKind::Tag(t.func_type_idx),
                    };
                    if let WasmImportKind::Func(t) = kind {
                        self.functions.push(t);
                        self.module.functions.push(WasmFunction { ty: t, ..WasmFunction::default() });
                    }
                    self.module.imports.push(WasmImport { module: imp.module.into(), name: imp.name.into(), kind });
                }
            }
            Payload::FunctionSection(r) => {
                for t in r {
                    self.functions.push(t?);
                }
            }
            Payload::TableSection(r) => {
                for t in r {
                    let t = t?;
                    let init = match t.init {
                        TableInit::RefNull => None,
                        TableInit::Expr(e) => Some(self.init(&e)?),
                    };
                    self.module.tables.push(WasmTable { ty: t.ty.convert(), init });
                }
            }
            Payload::MemorySection(r) => {
                for m in r {
                    self.module.memories.push(m?.convert());
                }
            }
            Payload::TagSection(r) => {
                for t in r {
                    self.module.tags.push(t?.func_type_idx);
                }
            }
            Payload::GlobalSection(r) => {
                for g in r {
                    let g = g?;
                    let init = self.init(&g.init_expr)?;
                    self.module.globals.push(WasmGlobal { ty: g.ty.convert(), init });
                }
            }
            Payload::ExportSection(r) => {
                for x in r {
                    let x = x?;
                    self.module.exports.push(WasmExport { name: x.name.into(), kind: x.kind.convert(), index: x.index });
                }
            }
            Payload::StartSection { func, .. } => self.module.start = Some(func),
            Payload::CustomSection(c) => self.module.customs.push(WasmCustom { name: c.name().into(), bytes: c.data().into() }),
            Payload::ElementSection(r) => {
                for el in r {
                    let el = el?;
                    let mode = match el.kind {
                        ElementKind::Passive => WasmSegmentMode::Passive,
                        ElementKind::Declared => WasmSegmentMode::Declared,
                        ElementKind::Active { table_index, offset_expr } => WasmSegmentMode::Active { index: table_index.unwrap_or(0), offset: self.init(&offset_expr)? },
                    };
                    let (element, items) = match el.items {
                        ElementItems::Functions(fs) => {
                            let mut items = Vec::new();
                            for f in fs {
                                items.push(self.function_ref(f?));
                            }
                            (WasmRefType { nullable: false, heap: WasmHeapType::Func }, items)
                        }
                        ElementItems::Expressions(t, es) => {
                            let mut items = Vec::new();
                            for x in es {
                                items.push(self.init(&x?)?);
                            }
                            (t.convert(), items)
                        }
                    };
                    self.module.elements.push(WasmElement { mode, element, items: items.into() });
                }
            }
            Payload::DataSection(r) => {
                for d in r {
                    let d = d?;
                    let mode = match d.kind {
                        DataKind::Passive => WasmSegmentMode::Passive,
                        DataKind::Active { memory_index, offset_expr } => WasmSegmentMode::Active { index: memory_index, offset: self.init(&offset_expr)? },
                    };
                    self.module.data.push(WasmData { mode, bytes: d.data.into() });
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl CompileWasm for WasmModule {
    fn compile_wasm(bytes: &[u8]) -> Result<Self, WasmError> {
        let mut validator = Validator::new_with_features(WasmFeatures::WASM3);
        let mut b = ModuleBuilder { bytes, module: WasmModule::default(), functions: Vec::new() };
        for payload in Parser::new(0).parse_all(bytes) {
            b.payload(&mut validator, payload?)?;
        }
        Ok(b.module)
    }
}
