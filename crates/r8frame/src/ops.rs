use alloc::boxed::Box;

use gc_arena::Gc;

use crate::{JsString, Scope, Value};

/// 二元算术运算（各层共用同一份语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl BinaryOp {
    pub fn name(self) -> &'static str {
        match self {
            BinaryOp::Add => "add",
            BinaryOp::Sub => "sub",
            BinaryOp::Mul => "mul",
            BinaryOp::Div => "div",
            BinaryOp::Rem => "rem",
        }
    }
}

impl<'gc> Scope<'gc, '_> {
    /// 字符串放上堆。
    pub fn string(&self, s: &str) -> JsString<'gc> {
        Gc::new(self.mc, Box::from(s))
    }

    /// ToNumber。对象还没有 ToPrimitive，先当 NaN。
    pub fn to_number(&self, v: Value<'gc>) -> f64 {
        match v {
            Value::Undefined | Value::Object(_) => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => f64::from(u8::from(b)),
            Value::Number(x) => x,
            Value::String(s) => match s.trim() {
                "" => 0.0,
                t => t.parse().unwrap_or(f64::NAN),
            },
        }
    }

    /// ToString。
    pub fn to_string(&self, v: Value<'gc>) -> JsString<'gc> {
        match v {
            Value::String(s) => s,
            Value::Undefined => self.string("undefined"),
            Value::Null => self.string("null"),
            Value::Bool(true) => self.string("true"),
            Value::Bool(false) => self.string("false"),
            Value::Object(_) => self.string("[object Object]"),
            Value::Number(x) => self.string(ryu_js::Buffer::new().format(x)),
        }
    }

    pub fn binary(&self, op: BinaryOp, lhs: Value<'gc>, rhs: Value<'gc>) -> Value<'gc> {
        match (op, lhs, rhs) {
            (BinaryOp::Add, Value::String(_), _) | (BinaryOp::Add, _, Value::String(_)) => {
                let (a, b) = (self.to_string(lhs), self.to_string(rhs));
                self.string(&[&**a, &**b].concat()).into()
            }
            _ => {
                let (a, b) = (self.to_number(lhs), self.to_number(rhs));
                match op {
                    BinaryOp::Add => a + b,
                    BinaryOp::Sub => a - b,
                    BinaryOp::Mul => a * b,
                    BinaryOp::Div => a / b,
                    BinaryOp::Rem => a % b,
                }
                .into()
            }
        }
    }
}
