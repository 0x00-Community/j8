use alloc::boxed::Box;
use alloc::vec::Vec;

use j8bytecode::{Decoded, OperandValue, WasmBytecode, WasmCompositeType, WasmHeapType, WasmStorageType, WasmValType};
use j8framework::paragc::{WORD_POINTER_MIN, word_pointer};
use j8framework::{Collect, IsoCell, Mutation, ScopeLocal, WordSlot, gc_from_raw};

use super::types::{Field, Hierarchy, Layout, Types};
use super::{Slot, WasmStore, WasmTrap};

const I31_TAG: u64 = 1 << 62;
const HOST_TAG: u64 = 1 << 61;
const I31_MASK: u64 = (1 << 31) - 1;
const I31_SIGN: u32 = 1 << 30;
const HOST_MASK: u64 = HOST_TAG - 1;
const BYTE_MASK: u64 = 0xff;
const SHORT_MASK: u64 = 0xffff;

#[derive(Collect)]
#[collect(no_drop)]
pub struct WasmCells<'scope>(pub Box<[WordSlot<'scope>]>);

j8framework::paragc::brand!(pub CellsBrand = for<'heap, 'scope> WasmCells<'scope>);

impl<'scope> WasmCells<'scope> {
    pub fn alloc(mc: &Mutation<'scope>, words: impl IntoIterator<Item = u64>) -> ScopeLocal<'scope, Self> {
        mc.alloc(Self(words.into_iter().map(WordSlot::new).collect()))
    }
}

#[derive(Collect)]
#[collect(no_drop)]
pub struct WasmObject<'scope> {
    head: u32,
    len: u32,
    refs: Box<[WordSlot<'scope>]>,
    data: Box<[IsoCell<u64>]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Word {
    Null,
    I31(i32),
    Host(u32),
    Object(u64),
    Func(u32),
}

impl Word {
    pub fn decode(w: u64, h: Hierarchy) -> Self {
        match w {
            0 => Self::Null,
            _ if h == Hierarchy::Func => Self::Func(u32::try_from(w - 1).unwrap_or(u32::MAX)),
            _ if w >= WORD_POINTER_MIN => Self::Object(w),
            _ if w & I31_TAG != 0 => {
                let v = u32::try_from(w & I31_MASK).unwrap_or(0);
                Self::I31(if v & I31_SIGN == 0 { v.cast_signed() } else { (v | !u32::try_from(I31_MASK).unwrap_or(0)).cast_signed() })
            }
            _ => Self::Host(u32::try_from(w & HOST_MASK).unwrap_or(u32::MAX)),
        }
    }
}

impl From<Word> for u64 {
    fn from(w: Word) -> Self {
        match w {
            Word::Null => 0,
            Word::I31(v) => I31_TAG | (u64::from(v.cast_unsigned()) & I31_MASK),
            Word::Host(n) => HOST_TAG | u64::from(n),
            Word::Object(w) => w,
            Word::Func(a) => u64::from(a) + 1,
        }
    }
}

impl<'scope> WasmObject<'scope> {
    pub fn get(w: u64) -> Option<&'scope Self> {
        word_pointer(w).map(|p| unsafe { gc_from_raw::<Self>(p.cast()) }.get())
    }

    pub fn head(&self) -> u32 {
        self.head
    }

    pub fn alloc(mc: &Mutation<'scope>, head: u32, len: u32, layout: &Layout) -> u64 {
        let n = len.max(1);
        let refs = (0..layout.refs * n).map(|_| WordSlot::new(0)).collect();
        let data = (0..layout.words * n).map(|_| IsoCell::new(0)).collect();
        let o = mc.alloc(Self { head, len, refs, data });
        WORD_POINTER_MIN | u64::try_from(o.as_ptr().expose_provenance()).unwrap_or(0)
    }

    fn at(f: Field, i: u32, layout: &Layout) -> usize {
        (f.index + i * if f.is_ref { layout.refs } else { layout.words }) as usize
    }

    pub fn read(&self, f: Field, i: u32, layout: &Layout) -> (u64, u128) {
        let at = Self::at(f, i, layout);
        if f.is_ref {
            (self.refs[at].get(), 0)
        } else if f.is_vector() {
            let (lo, hi) = (self.data[at].get(), self.data[at + 1].get());
            (lo, u128::from(lo) | (u128::from(hi) << u64::BITS))
        } else {
            (self.data[at].get(), 0)
        }
    }

    pub fn write(&self, mc: &Mutation<'scope>, f: Field, i: u32, layout: &Layout, (s, v): (u64, u128)) {
        let at = Self::at(f, i, layout);
        match f.storage {
            _ if f.is_ref => self.refs[at].set(mc.gc_scope(), s),
            WasmStorageType::I8 => self.data[at].set(s & BYTE_MASK),
            WasmStorageType::I16 => self.data[at].set(s & SHORT_MASK),
            WasmStorageType::Val(WasmValType::I32 | WasmValType::F32) => self.data[at].set(s & u64::from(u32::MAX)),
            WasmStorageType::Val(WasmValType::V128) => {
                self.data[at].set(u64::try_from(v & u128::from(u64::MAX)).unwrap_or(0));
                self.data[at + 1].set(u64::try_from(v >> u64::BITS).unwrap_or(0));
            }
            WasmStorageType::Val(_) => self.data[at].set(s),
        }
    }
}

pub struct Objects<'a, 'p, 's> {
    pub store: &'a mut WasmStore<'p>,
    pub mc: &'a Mutation<'s>,
    pub fp: usize,
    pub instance: u32,
}

impl Objects<'_, '_, '_> {
    fn reg(&self, r: OperandValue) -> (u64, u128) {
        let at = self.fp + usize::try_from(r).unwrap_or(0);
        (self.store.stack[at], self.store.vstack[at])
    }

    fn set(&mut self, r: OperandValue, (s, v): (u64, u128)) {
        let at = self.fp + usize::try_from(r).unwrap_or(0);
        self.store.stack[at] = s;
        self.store.vstack[at] = v;
    }

    fn ty(&self, t: OperandValue) -> u32 {
        self.store.instances[self.instance as usize].types[usize::try_from(t).unwrap_or(0)]
    }

    fn array(&self, t: OperandValue) -> (u32, Field) {
        let id = self.ty(t);
        (id, self.store.types.get(id).layout.fields[0])
    }

    fn object<'s>(w: u64, null: WasmTrap) -> Result<&'s WasmObject<'s>, WasmTrap> {
        WasmObject::get(w).ok_or(null)
    }

    fn range(len: u32, at: u64, n: u64) -> Result<(u32, u32), WasmTrap> {
        let (at, n) = (Slot(at).u32(), Slot(n).u32());
        match at.checked_add(n) {
            Some(e) if e <= len => Ok((at, n)),
            _ => Err(WasmTrap::OutOfBoundsArray),
        }
    }

    fn unpack(f: Field, s: u64, signed: bool) -> u64 {
        let (bits, mask) = match f.storage {
            WasmStorageType::I8 => (u8::BITS, BYTE_MASK),
            WasmStorageType::I16 => (u16::BITS, SHORT_MASK),
            WasmStorageType::Val(_) => return s,
        };
        let s = s & mask;
        if signed && s >> (bits - 1) != 0 { (s | !mask) & u64::from(u32::MAX) } else { s }
    }

    fn element(f: Field, bytes: &[u8]) -> (u64, u128) {
        let mut b = [0u8; 16];
        b[..bytes.len()].copy_from_slice(bytes);
        let v = u128::from_le_bytes(b);
        (u64::try_from(v & u128::from(u64::MAX)).unwrap_or(0), if f.is_vector() { v } else { 0 })
    }

    fn elems(&self, seg: OperandValue) -> Vec<u64> {
        let g = &self.store.instances[self.instance as usize].elems[usize::try_from(seg).unwrap_or(0)];
        self.mc.fetch(g).get().0.iter().map(WordSlot::get).collect()
    }

    #[expect(clippy::too_many_lines, reason = "one arm per GC bytecode")]
    pub fn run(mut self, d: &Decoded<WasmBytecode>) -> Result<(), WasmTrap> {
        let o = d.operands;
        let mc = self.mc;
        match d.bytecode {
            WasmBytecode::StructNew | WasmBytecode::StructNewDefault => {
                let id = self.ty(o[1]);
                let layout = self.store.types.get(id).layout.clone();
                let w = WasmObject::alloc(mc, id, 1, &layout);
                if let (WasmBytecode::StructNew, Some(obj)) = (d.bytecode, WasmObject::get(w)) {
                    for (i, &f) in layout.fields.iter().enumerate() {
                        obj.write(mc, f, 0, &layout, self.reg(o[2] + OperandValue::try_from(i).unwrap_or(0)));
                    }
                }
                self.set(o[0], (w, 0));
            }
            WasmBytecode::StructGet | WasmBytecode::StructGetS | WasmBytecode::StructGetU => {
                let obj = Self::object(self.reg(o[1]).0, WasmTrap::NullStructure)?;
                let layout = &self.store.types.get(obj.head).layout;
                let f = layout.fields[usize::try_from(o[3]).unwrap_or(0)];
                let (s, v) = obj.read(f, 0, layout);
                self.set(o[0], (Self::unpack(f, s, d.bytecode == WasmBytecode::StructGetS), v));
            }
            WasmBytecode::StructSet => {
                let obj = Self::object(self.reg(o[0]).0, WasmTrap::NullStructure)?;
                let layout = &self.store.types.get(obj.head).layout;
                let f = layout.fields[usize::try_from(o[3]).unwrap_or(0)];
                obj.write(mc, f, 0, layout, self.reg(o[1]));
            }
            WasmBytecode::ArrayNew | WasmBytecode::ArrayNewDefault => {
                let (len_reg, ty) = if d.bytecode == WasmBytecode::ArrayNew { (o[2], o[3]) } else { (o[1], o[2]) };
                let (id, f) = self.array(ty);
                let n = Slot(self.reg(len_reg).0).u32();
                let layout = self.store.types.get(id).layout.clone();
                let w = WasmObject::alloc(mc, id, n, &layout);
                if d.bytecode == WasmBytecode::ArrayNew
                    && let Some(obj) = WasmObject::get(w)
                {
                    let v = self.reg(o[1]);
                    for i in 0..n {
                        obj.write(mc, f, i, &layout, v);
                    }
                }
                self.set(o[0], (w, 0));
            }
            WasmBytecode::ArrayNewFixed => {
                let (id, f) = self.array(o[2]);
                let n = u32::try_from(o[3]).unwrap_or(0);
                let layout = self.store.types.get(id).layout.clone();
                let w = WasmObject::alloc(mc, id, n, &layout);
                if let Some(obj) = WasmObject::get(w) {
                    for i in 0..n {
                        obj.write(mc, f, i, &layout, self.reg(o[1] + OperandValue::from(i)));
                    }
                }
                self.set(o[0], (w, 0));
            }
            WasmBytecode::ArrayNewData | WasmBytecode::ArrayNewElem => {
                let (id, f) = self.array(o[2]);
                let (s, n) = (self.reg(o[1]).0, self.reg(o[1] + 1).0);
                let (s, n) = (Slot(s).u32(), Slot(n).u32());
                let layout = self.store.types.get(id).layout.clone();
                let items: Vec<(u64, u128)> = if d.bytecode == WasmBytecode::ArrayNewData {
                    let bytes = self.store.instances[self.instance as usize].data[usize::try_from(o[3]).unwrap_or(0)].clone();
                    let size = f.bytes();
                    let (start, end) = (s as usize, (s as usize).checked_add((n as usize).checked_mul(size).ok_or(WasmTrap::OutOfBoundsMemory)?).ok_or(WasmTrap::OutOfBoundsMemory)?);
                    let chunk = bytes.get(start..end).ok_or(WasmTrap::OutOfBoundsMemory)?;
                    chunk.chunks(size).map(|c| Self::element(f, c)).collect()
                } else {
                    let items = self.elems(o[3]);
                    let end = s.checked_add(n).ok_or(WasmTrap::OutOfBoundsTable)?;
                    items.get(s as usize..end as usize).ok_or(WasmTrap::OutOfBoundsTable)?.iter().map(|&w| (w, 0)).collect()
                };
                let w = WasmObject::alloc(mc, id, n, &layout);
                if let Some(obj) = WasmObject::get(w) {
                    for (i, &v) in (0..n).zip(&items) {
                        obj.write(mc, f, i, &layout, v);
                    }
                }
                self.set(o[0], (w, 0));
            }
            WasmBytecode::ArrayGet | WasmBytecode::ArrayGetS | WasmBytecode::ArrayGetU => {
                let obj = Self::object(self.reg(o[1]).0, WasmTrap::NullArray)?;
                let i = Self::range(obj.len, self.reg(o[2]).0, 1)?.0;
                let layout = &self.store.types.get(obj.head).layout;
                let f = layout.fields[0];
                let (s, v) = obj.read(f, i, layout);
                self.set(o[0], (Self::unpack(f, s, d.bytecode == WasmBytecode::ArrayGetS), v));
            }
            WasmBytecode::ArraySet => {
                let obj = Self::object(self.reg(o[0]).0, WasmTrap::NullArray)?;
                let i = Self::range(obj.len, self.reg(o[1]).0, 1)?.0;
                let layout = &self.store.types.get(obj.head).layout;
                obj.write(mc, layout.fields[0], i, layout, self.reg(o[2]));
            }
            WasmBytecode::ArrayLen => {
                let obj = Self::object(self.reg(o[1]).0, WasmTrap::NullArray)?;
                self.set(o[0], (u64::from(obj.len), 0));
            }
            WasmBytecode::ArrayFill => {
                let obj = Self::object(self.reg(o[0]).0, WasmTrap::NullArray)?;
                let (at, n) = Self::range(obj.len, self.reg(o[0] + 1).0, self.reg(o[0] + 3).0)?;
                let v = self.reg(o[0] + 2);
                let layout = &self.store.types.get(obj.head).layout;
                for i in at..at + n {
                    obj.write(mc, layout.fields[0], i, layout, v);
                }
            }
            WasmBytecode::ArrayCopy => {
                let dst = Self::object(self.reg(o[0]).0, WasmTrap::NullArray)?;
                let src = Self::object(self.reg(o[0] + 2).0, WasmTrap::NullArray)?;
                let n = self.reg(o[0] + 4).0;
                let (da, n) = Self::range(dst.len, self.reg(o[0] + 1).0, n)?;
                let (sa, _) = Self::range(src.len, self.reg(o[0] + 3).0, u64::from(n))?;
                let layout = &self.store.types.get(src.head).layout;
                let f = layout.fields[0];
                let items: Vec<(u64, u128)> = (sa..sa + n).map(|i| src.read(f, i, layout)).collect();
                let layout = &self.store.types.get(dst.head).layout;
                for (i, v) in (da..da + n).zip(items) {
                    dst.write(mc, layout.fields[0], i, layout, v);
                }
            }
            WasmBytecode::ArrayInitData | WasmBytecode::ArrayInitElem => {
                let obj = Self::object(self.reg(o[0]).0, WasmTrap::NullArray)?;
                let (da, s, n) = (self.reg(o[0] + 1).0, Slot(self.reg(o[0] + 2).0).u32(), self.reg(o[0] + 3).0);
                let (da, n) = Self::range(obj.len, da, n)?;
                let layout = self.store.types.get(obj.head).layout.clone();
                let f = layout.fields[0];
                let items: Vec<(u64, u128)> = if d.bytecode == WasmBytecode::ArrayInitData {
                    let bytes = self.store.instances[self.instance as usize].data[usize::try_from(o[2]).unwrap_or(0)].clone();
                    let size = f.bytes();
                    let end = (s as usize).checked_add(n as usize * size).ok_or(WasmTrap::OutOfBoundsMemory)?;
                    bytes.get(s as usize..end).ok_or(WasmTrap::OutOfBoundsMemory)?.chunks(size).map(|c| Self::element(f, c)).collect()
                } else {
                    let items = self.elems(o[2]);
                    let end = s.checked_add(n).ok_or(WasmTrap::OutOfBoundsTable)?;
                    items.get(s as usize..end as usize).ok_or(WasmTrap::OutOfBoundsTable)?.iter().map(|&w| (w, 0)).collect()
                };
                for (i, v) in (da..da + n).zip(items) {
                    obj.write(mc, f, i, &layout, v);
                }
            }
            WasmBytecode::RefTest | WasmBytecode::RefTestNull => {
                let ok = self.store.test(self.instance, self.reg(o[1]).0, o[2], d.bytecode == WasmBytecode::RefTestNull);
                self.set(o[0], (u64::from(ok), 0));
            }
            WasmBytecode::RefCast | WasmBytecode::RefCastNull => {
                let w = self.reg(o[1]).0;
                if !self.store.test(self.instance, w, o[2], d.bytecode == WasmBytecode::RefCastNull) {
                    return Err(WasmTrap::CastFailure);
                }
                self.set(o[0], (w, 0));
            }
            WasmBytecode::AnyConvertExtern | WasmBytecode::ExternConvertAny => {
                let w = self.reg(o[1]);
                self.set(o[0], w);
            }
            WasmBytecode::RefI31 => {
                let v = Slot(self.reg(o[1]).0).u32().cast_signed();
                self.set(o[0], (u64::from(Word::I31(v)), 0));
            }
            WasmBytecode::I31GetS | WasmBytecode::I31GetU => {
                let Word::I31(v) = Word::decode(self.reg(o[1]).0, Hierarchy::Any) else { return Err(WasmTrap::NullI31) };
                let v = if d.bytecode == WasmBytecode::I31GetS { u64::from(v.cast_unsigned()) } else { u64::from(v.cast_unsigned()) & I31_MASK };
                self.set(o[0], (v, 0));
            }
            _ => return Err(WasmTrap::Unsupported),
        }
        Ok(())
    }
}

impl WasmStore<'_> {
    pub fn test(&self, instance: u32, w: u64, ht: OperandValue, nullable: bool) -> bool {
        let heap = match WasmHeapType::from(ht) {
            WasmHeapType::Concrete(i) => WasmHeapType::Concrete(self.instances[instance as usize].types[i as usize]),
            h => h,
        };
        self.matches(w, heap, nullable)
    }

    pub fn matches(&self, w: u64, heap: WasmHeapType, nullable: bool) -> bool {
        let types: &Types = &self.types;
        let h = types.hierarchy(heap);
        let actual = match Word::decode(w, h) {
            Word::Null => return nullable,
            Word::I31(_) => WasmHeapType::I31,
            Word::Host(_) => return matches!(heap, WasmHeapType::Any | WasmHeapType::Extern),
            Word::Func(f) => WasmHeapType::Concrete(self.funcs[f as usize].canon),
            Word::Object(w) => match h {
                Hierarchy::Exn => WasmHeapType::Exn,
                Hierarchy::Extern => WasmHeapType::Extern,
                _ => WasmObject::get(w).map_or(WasmHeapType::None, |o| WasmHeapType::Concrete(o.head)),
            },
        };
        match h {
            Hierarchy::Extern => heap == WasmHeapType::Extern,
            Hierarchy::Exn => heap == WasmHeapType::Exn,
            _ => types.heap_sub(actual, heap),
        }
    }

    pub fn composite(&self, w: u64) -> Option<&WasmCompositeType> {
        WasmObject::get(w).map(|o| &self.types.get(o.head).sub.composite)
    }
}
