use indexmap::IndexSet;

use crate::Value;

/// 驻留字符串的编号。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StrId(u32);

/// 堆对象的编号。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjId(u32);

/// 普通对象：按插入顺序的属性表加原型。
#[derive(Debug, Default)]
pub struct Object {
    pub props: Vec<(StrId, Value)>,
    pub proto: Option<ObjId>,
}

impl Object {
    pub fn get(&self, key: StrId) -> Option<Value> {
        self.props.iter().find_map(|&(k, v)| (k == key).then_some(v))
    }

    pub fn set(&mut self, key: StrId, value: Value) {
        match self.props.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => *v = value,
            None => self.props.push((key, value)),
        }
    }
}

/// 一个 isolate 的堆：字符串驻留表（一份拷贝，下标即编号）加对象表。
#[derive(Debug, Default)]
pub struct Heap {
    strings: IndexSet<Box<str>>,
    objects: Vec<Object>,
}

impl Heap {
    pub fn intern(&mut self, s: &str) -> StrId {
        let i = self.strings.get_index_of(s).unwrap_or_else(|| self.strings.insert_full(s.into()).0);
        StrId(u32::try_from(i).expect("fewer than 2^32 strings"))
    }

    pub fn str(&self, id: StrId) -> &str {
        &self.strings[id.0 as usize]
    }

    pub fn alloc(&mut self, o: Object) -> ObjId {
        self.objects.push(o);
        ObjId(u32::try_from(self.objects.len() - 1).expect("fewer than 2^32 objects"))
    }

    pub fn object(&self, id: ObjId) -> &Object {
        &self.objects[id.0 as usize]
    }

    pub fn object_mut(&mut self, id: ObjId) -> &mut Object {
        &mut self.objects[id.0 as usize]
    }
}
