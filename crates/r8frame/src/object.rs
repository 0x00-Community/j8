use alloc::boxed::Box;
use alloc::vec::Vec;

use gc_arena::{Collect, Gc, Mutation, RefLock};

use crate::Value;

/// 堆上的字符串。
pub type JsString<'gc> = Gc<'gc, Box<str>>;

/// 堆上的普通对象（内部可变，写经 gc-arena 的写屏障）。
pub type JsObject<'gc> = Gc<'gc, RefLock<ObjectData<'gc>>>;

/// 普通对象：按插入顺序的属性表加原型。
#[derive(Debug, Default, PartialEq, Collect)]
#[collect(no_drop)]
pub struct ObjectData<'gc> {
    pub props: Vec<(JsString<'gc>, Value<'gc>)>,
    pub proto: Option<JsObject<'gc>>,
}

impl<'gc> ObjectData<'gc> {
    pub fn get(&self, key: &str) -> Option<Value<'gc>> {
        self.props.iter().find_map(|&(k, v)| (**k == *key).then_some(v))
    }

    pub fn set(&mut self, key: JsString<'gc>, value: Value<'gc>) {
        match self.props.iter_mut().find(|(k, _)| ***k == **key) {
            Some((_, v)) => *v = value,
            None => self.props.push((key, value)),
        }
    }

    /// 放上堆。
    pub fn alloc(self, mc: &Mutation<'gc>) -> JsObject<'gc> {
        Gc::new(mc, RefLock::new(self))
    }
}
