use alloc::boxed::Box;
use alloc::vec::Vec;

use gc_arena::{Collect, Gc, Mutation, RefLock};

use crate::{FuncId, Value};

/// 堆上的字符串。
pub type JsString<'gc> = Gc<'gc, Box<str>>;

/// 堆上的对象（内部可变，写经 gc-arena 的写屏障）。
pub type JsObject<'gc> = Gc<'gc, RefLock<ObjectData<'gc>>>;

/// 堆上的作用域环境：被内层函数捕获的绑定放在这里，按槽位存。
pub type JsEnv<'gc> = Gc<'gc, RefLock<Env<'gc>>>;

/// 一层作用域环境，`parent` 指向外层。
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Env<'gc> {
    pub slots: Vec<Value<'gc>>,
    pub parent: Option<JsEnv<'gc>>,
}

impl<'gc> Env<'gc> {
    pub fn alloc(self, mc: &Mutation<'gc>) -> JsEnv<'gc> {
        Gc::new(mc, RefLock::new(self))
    }
}

/// 闭包：函数编号加创建时的环境。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Closure<'gc> {
    pub func: FuncId,
    pub env: Option<JsEnv<'gc>>,
}

/// 对象的种类：普通对象之外带的内部槽。
#[derive(Debug, Default, Collect)]
#[collect(no_drop)]
pub enum ObjectKind<'gc> {
    #[default]
    Ordinary,
    /// 数组元素（下标属性存在这里，`length` 就是它的长度）
    Array(Vec<Value<'gc>>),
    Function(Closure<'gc>),
}

/// 对象：种类、按插入顺序的属性表、原型。
#[derive(Debug, Default, Collect)]
#[collect(no_drop)]
pub struct ObjectData<'gc> {
    pub kind: ObjectKind<'gc>,
    pub props: Vec<(JsString<'gc>, Value<'gc>)>,
    pub proto: Option<JsObject<'gc>>,
}

impl<'gc> ObjectData<'gc> {
    /// 自有属性。
    pub fn own(&self, key: &str) -> Option<Value<'gc>> {
        self.props.iter().find_map(|&(k, v)| (**k == *key).then_some(v))
    }

    /// 设自有属性；键是新的才放上堆。
    pub fn set(&mut self, mc: &Mutation<'gc>, key: &str, value: Value<'gc>) {
        match self.props.iter_mut().find(|(k, _)| ***k == *key) {
            Some((_, v)) => *v = value,
            None => self.props.push((Gc::new(mc, Box::from(key)), value)),
        }
    }

    pub fn alloc(self, mc: &Mutation<'gc>) -> JsObject<'gc> {
        Gc::new(mc, RefLock::new(self))
    }
}

impl<'gc> From<ObjectKind<'gc>> for ObjectData<'gc> {
    fn from(kind: ObjectKind<'gc>) -> Self {
        Self { kind, ..Self::default() }
    }
}
