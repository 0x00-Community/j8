use gc_arena::{Collect, Gc};

use crate::{JsBigInt, JsObject, JsString, JsSymbol};

/// JS 值：标量直接存，字符串与对象是 GC 指针，活在 `'gc` 里。
#[derive(Clone, Copy, Debug, Default, Collect)]
#[collect(no_drop)]
pub enum Value<'gc> {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(JsString<'gc>),
    Symbol(JsSymbol<'gc>),
    BigInt(JsBigInt<'gc>),
    Object(JsObject<'gc>),
    /// 暂时性死区里还没初始化的 `let` / `const` / `class` 绑定（以及派生类 `super()` 前的 `this`）。
    /// 只在寄存器、环境槽与脚本作用域里出现，读写前检查掉，不会逃出到 JS。
    Uninit,
}

/// 严格相等（`===`）：字符串比内容，对象比身份，`NaN` 不等于自己。
impl PartialEq for Value<'_> {
    fn eq(&self, o: &Self) -> bool {
        match (self, o) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) | (Value::Uninit, Value::Uninit) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => **a == **b,
            (Value::Object(a), Value::Object(b)) => Gc::ptr_eq(*a, *b),
            (Value::Symbol(a), Value::Symbol(b)) => Gc::ptr_eq(*a, *b),
            (Value::BigInt(a), Value::BigInt(b)) => a.0 == b.0,
            _ => false,
        }
    }
}

impl Value<'_> {
    /// SameValue：同 `===`，但 `NaN` 等于自己、`+0` 不等于 `-0`。
    pub fn same_value(self, o: Self) -> bool {
        match (self, o) {
            (Value::Number(a), Value::Number(b)) => (a.is_nan() && b.is_nan()) || (a == b && a.is_sign_negative() == b.is_sign_negative()),
            _ => self == o,
        }
    }

    pub fn is_nullish(self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }

    /// 原始值包装对象 ⇒ 里面的原始值，别的原样。规范的 thisBooleanValue / thisNumberValue / thisStringValue 先过这一步。
    #[must_use]
    pub fn unwrapped(self) -> Self {
        match self {
            Value::Object(o) => match o.borrow().kind {
                crate::ObjectKind::Primitive(v) => v,
                _ => self,
            },
            _ => self,
        }
    }
}

impl From<bool> for Value<'_> {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<f64> for Value<'_> {
    fn from(x: f64) -> Self {
        Value::Number(x)
    }
}

impl<'gc> From<JsString<'gc>> for Value<'gc> {
    fn from(s: JsString<'gc>) -> Self {
        Value::String(s)
    }
}

impl<'gc> From<JsSymbol<'gc>> for Value<'gc> {
    fn from(s: JsSymbol<'gc>) -> Self {
        Value::Symbol(s)
    }
}

impl<'gc> From<JsObject<'gc>> for Value<'gc> {
    fn from(o: JsObject<'gc>) -> Self {
        Value::Object(o)
    }
}
