use gc_arena::Collect;

use crate::{JsObject, JsString};

/// JS 值：标量直接存，字符串与对象是 GC 指针，活在 `'gc` 里。
#[derive(Clone, Copy, Debug, PartialEq, Default, Collect)]
#[collect(no_drop)]
pub enum Value<'gc> {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(JsString<'gc>),
    Object(JsObject<'gc>),
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

impl<'gc> From<JsObject<'gc>> for Value<'gc> {
    fn from(o: JsObject<'gc>) -> Self {
        Value::Object(o)
    }
}
