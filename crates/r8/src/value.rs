use crate::{ObjId, StrId};

/// JS 值：标量直接存，字符串与对象是堆里的编号。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(StrId),
    Object(ObjId),
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<f64> for Value {
    fn from(x: f64) -> Self {
        Value::Number(x)
    }
}

impl From<StrId> for Value {
    fn from(s: StrId) -> Self {
        Value::String(s)
    }
}

impl From<ObjId> for Value {
    fn from(o: ObjId) -> Self {
        Value::Object(o)
    }
}
