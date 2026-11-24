use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use j8bytecode::{WasmCompositeType, WasmFieldType, WasmFuncType, WasmHeapType, WasmModule, WasmRefType, WasmStorageType, WasmSubType, WasmValType};

const REC: u32 = 1 << 31;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hierarchy {
    Func,
    Extern,
    Any,
    Exn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub is_ref: bool,
    pub index: u32,
    pub storage: WasmStorageType,
}

impl Field {
    pub fn is_vector(self) -> bool {
        self.storage == WasmStorageType::Val(WasmValType::V128)
    }

    pub fn bytes(self) -> usize {
        match self.storage {
            WasmStorageType::I8 => 1,
            WasmStorageType::I16 => 2,
            WasmStorageType::Val(WasmValType::I32 | WasmValType::F32) => 4,
            WasmStorageType::Val(WasmValType::V128) => 16,
            WasmStorageType::Val(_) => 8,
        }
    }

    pub fn words(self) -> u32 {
        if self.is_vector() { 2 } else { 1 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub fields: Box<[Field]>,
    pub refs: u32,
    pub words: u32,
}

impl<'a> FromIterator<&'a WasmStorageType> for Layout {
    fn from_iter<I: IntoIterator<Item = &'a WasmStorageType>>(storages: I) -> Self {
        let (mut refs, mut words) = (0, 0);
        let fields = storages
            .into_iter()
            .map(|&storage| {
                let is_ref = matches!(storage, WasmStorageType::Val(WasmValType::Ref(_)));
                let slot = if is_ref { &mut refs } else { &mut words };
                let f = Field { is_ref, index: *slot, storage };
                *slot += f.words();
                f
            })
            .collect();
        Self { fields, refs, words }
    }
}

pub struct TypeInfo {
    pub sub: WasmSubType,
    pub layout: Arc<Layout>,
}

#[derive(Default)]
pub struct Types {
    list: Vec<TypeInfo>,
    groups: BTreeMap<Box<[WasmSubType]>, u32>,
}

impl Types {
    pub fn get(&self, id: u32) -> &TypeInfo {
        &self.list[id as usize]
    }

    pub fn canonicalize(&mut self, module: &WasmModule) -> Vec<u32> {
        let mut ids: Vec<u32> = Vec::with_capacity(module.types.len());
        let mut start = 0u32;
        for &n in &module.groups {
            let end = start + n;
            let key: Box<[WasmSubType]> = module.types[start as usize..end as usize].iter().map(|t| t.map(&|i| if (start..end).contains(&i) { REC | (i - start) } else { ids[i as usize] })).collect();
            let base = self.group(key);
            ids.extend(base..base + n);
            start = end;
        }
        ids
    }

    pub fn func(&mut self, ty: &WasmFuncType) -> u32 {
        self.group(Box::new([WasmSubType { is_final: true, supertype: None, composite: WasmCompositeType::Func(ty.clone()) }]))
    }

    fn group(&mut self, key: Box<[WasmSubType]>) -> u32 {
        if let Some(&base) = self.groups.get(&key) {
            return base;
        }
        let base = u32::try_from(self.list.len()).unwrap_or(u32::MAX);
        for t in &key {
            let sub = t.map(&|i| if i & REC == 0 { i } else { base + (i & !REC) });
            let layout = match &sub.composite {
                WasmCompositeType::Struct(fs) => fs.iter().map(|f: &WasmFieldType| &f.storage).collect(),
                WasmCompositeType::Array(f) => core::iter::once(&f.storage).collect(),
                WasmCompositeType::Func(_) => Layout::default(),
            };
            self.list.push(TypeInfo { sub, layout: Arc::new(layout) });
        }
        self.groups.insert(key, base);
        base
    }

    pub fn sub(&self, a: u32, b: u32) -> bool {
        let mut t = Some(a);
        while let Some(x) = t {
            if x == b {
                return true;
            }
            t = self.get(x).sub.supertype;
        }
        false
    }

    pub fn hierarchy(&self, h: WasmHeapType) -> Hierarchy {
        match h {
            WasmHeapType::Func | WasmHeapType::NoFunc => Hierarchy::Func,
            WasmHeapType::Extern | WasmHeapType::NoExtern => Hierarchy::Extern,
            WasmHeapType::Exn | WasmHeapType::NoExn => Hierarchy::Exn,
            WasmHeapType::Concrete(i) if matches!(self.get(i).sub.composite, WasmCompositeType::Func(_)) => Hierarchy::Func,
            _ => Hierarchy::Any,
        }
    }

    pub fn abstract_of(&self, id: u32) -> WasmHeapType {
        match self.get(id).sub.composite {
            WasmCompositeType::Func(_) => WasmHeapType::Func,
            WasmCompositeType::Struct(_) => WasmHeapType::Struct,
            WasmCompositeType::Array(_) => WasmHeapType::Array,
        }
    }

    pub fn heap_sub(&self, a: WasmHeapType, b: WasmHeapType) -> bool {
        use WasmHeapType as H;
        match (a, b) {
            _ if a == b => true,
            (H::Concrete(x), H::Concrete(y)) => self.sub(x, y),
            (H::Concrete(x), _) => self.heap_sub(self.abstract_of(x), b),
            (H::None, _) => self.hierarchy(b) == Hierarchy::Any,
            (H::NoFunc, _) => self.hierarchy(b) == Hierarchy::Func,
            (H::NoExtern, H::Extern) | (H::NoExn, H::Exn) | (H::I31 | H::Struct | H::Array, H::Eq) | (H::I31 | H::Struct | H::Array | H::Eq, H::Any) => true,
            _ => false,
        }
    }

    pub fn ref_sub(&self, a: WasmRefType, b: WasmRefType) -> bool {
        (!a.nullable || b.nullable) && self.heap_sub(a.heap, b.heap)
    }

    pub fn val_sub(&self, a: WasmValType, b: WasmValType) -> bool {
        match (a, b) {
            (WasmValType::Ref(x), WasmValType::Ref(y)) => self.ref_sub(x, y),
            _ => a == b,
        }
    }
}
