use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use gc_arena::{Collect, Gc, Mutation, RefLock};

use crate::{Agent, Error, FuncId, Value};

/// 堆上的字符串。
pub type JsString<'gc> = Gc<'gc, Box<str>>;

/// 堆上的对象（内部可变，写经 gc-arena 的写屏障）。
pub type JsObject<'gc> = Gc<'gc, RefLock<ObjectData<'gc>>>;

/// 堆上的作用域环境：被内层函数捕获的绑定放在这里，按槽位存。
pub type JsEnv<'gc> = Gc<'gc, RefLock<Env<'gc>>>;

/// 宿主函数：`this` 与实参进，值或异常出。
pub type NativeFn = for<'gc, 'x> fn(&mut Agent<'gc, 'x>, Value<'gc>, &[Value<'gc>]) -> Result<Value<'gc>, Error>;

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

/// 宿主函数对象的本体。
#[derive(Clone, Copy, Collect)]
#[collect(require_static)]
pub struct Native(pub NativeFn);

impl fmt::Debug for Native {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Native")
    }
}

/// `Function.prototype.bind` 的结果：调用时换成目标，`this` 与前置实参固定。
#[derive(Debug, Collect)]
#[collect(no_drop)]
pub struct Bound<'gc> {
    pub target: Value<'gc>,
    pub this: Value<'gc>,
    pub args: Vec<Value<'gc>>,
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
    Native(Native),
    Bound(Bound<'gc>),
}

impl ObjectKind<'_> {
    pub fn is_callable(&self) -> bool {
        matches!(self, ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::Bound(_))
    }
}

/// 对象：种类、按插入顺序的属性表、原型。
#[derive(Default, Collect)]
#[collect(no_drop)]
pub struct ObjectData<'gc> {
    pub kind: ObjectKind<'gc>,
    pub props: Vec<(JsString<'gc>, Value<'gc>)>,
    pub proto: Option<JsObject<'gc>>,
}

/// 只打种类与键：对象图有环（`F.prototype.constructor`），不能往下展开。
impl fmt::Debug for ObjectData<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            ObjectKind::Ordinary => "Object",
            ObjectKind::Array(_) => "Array",
            ObjectKind::Function(_) | ObjectKind::Native(_) | ObjectKind::Bound(_) => "Function",
        };
        f.debug_list().entries(self.props.iter().map(|(k, _)| &**k)).finish()?;
        f.write_str(kind)
    }
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

    /// 删自有属性，返回删没删。
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.props.len();
        self.props.retain(|(k, _)| ***k != *key);
        before != self.props.len()
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
