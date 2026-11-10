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

/// 闭包：函数编号、创建时的环境、方法的主对象（`super` 从它的原型找）、类构造器的实例字段初始化函数。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Closure<'gc> {
    pub func: FuncId,
    pub env: Option<JsEnv<'gc>>,
    pub home: Option<JsObject<'gc>>,
    pub fields: Option<JsObject<'gc>>,
}

impl<'gc> Closure<'gc> {
    pub fn new(func: FuncId, env: Option<JsEnv<'gc>>) -> Self {
        Self { func, env, home: None, fields: None }
    }
}

/// 属性的内容：数据，或一对访问器。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum Slot<'gc> {
    Data(Value<'gc>),
    Accessor { get: Option<JsObject<'gc>>, set: Option<JsObject<'gc>> },
}

/// 一个属性：内容加特性。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub struct Property<'gc> {
    pub slot: Slot<'gc>,
    pub enumerable: bool,
    pub writable: bool,
    pub configurable: bool,
}

impl<'gc> Property<'gc> {
    /// 普通赋值建出来的属性：可枚举、可写、可配置。
    pub fn data(v: Value<'gc>) -> Self {
        Self { slot: Slot::Data(v), enumerable: true, writable: true, configurable: true }
    }

    /// 内建对象与类方法的属性：不可枚举。
    pub fn hidden(v: Value<'gc>) -> Self {
        Self { enumerable: false, ..Self::data(v) }
    }

    pub fn value(&self) -> Option<Value<'gc>> {
        match self.slot {
            Slot::Data(v) => Some(v),
            Slot::Accessor { .. } => None,
        }
    }
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
    pub props: Vec<(JsString<'gc>, Property<'gc>)>,
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
    pub fn get_own(&self, key: &str) -> Option<Property<'gc>> {
        self.props.iter().find_map(|&(k, p)| (**k == *key).then_some(p))
    }

    /// 自有数据属性的值（访问器给 `None`）。
    pub fn own(&self, key: &str) -> Option<Value<'gc>> {
        self.get_own(key).and_then(|p| p.value())
    }

    /// 赋值：已有数据属性就改值（不可写就不动），没有就建一个普通属性。访问器由调用方先处理。
    pub fn set(&mut self, mc: &Mutation<'gc>, key: &str, value: Value<'gc>) {
        match self.props.iter_mut().find(|(k, _)| ***k == *key) {
            Some((_, p)) => {
                if p.writable {
                    p.slot = Slot::Data(value);
                }
            }
            None => self.props.push((Gc::new(mc, Box::from(key)), Property::data(value))),
        }
    }

    /// 定义：整个替换（或新建）这个属性。
    pub fn define(&mut self, mc: &Mutation<'gc>, key: &str, prop: Property<'gc>) {
        match self.props.iter_mut().find(|(k, _)| ***k == *key) {
            Some((_, p)) => *p = prop,
            None => self.props.push((Gc::new(mc, Box::from(key)), prop)),
        }
    }

    /// 删自有属性：不可配置的删不掉，返回删没删成（不存在也算成）。
    pub fn remove(&mut self, key: &str) -> bool {
        match self.props.iter().position(|(k, _)| ***k == *key) {
            Some(i) if self.props[i].1.configurable => {
                self.props.remove(i);
                true
            }
            Some(_) => false,
            None => true,
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
