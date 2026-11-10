use crate::{Heap, Value};

/// 二元算术运算（各执行层共用同一份语义）。
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

impl Heap {
    /// ToNumber。对象还没有 ToPrimitive，先当 NaN。
    pub fn number(&self, v: Value) -> f64 {
        match v {
            Value::Undefined | Value::Object(_) => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => f64::from(u8::from(b)),
            Value::Number(x) => x,
            Value::String(s) => match self.str(s).trim() {
                "" => 0.0,
                t => t.parse().unwrap_or(f64::NAN),
            },
        }
    }

    /// ToString，结果驻留。
    pub fn string(&mut self, v: Value) -> Value {
        let s = match v {
            Value::String(_) => return v,
            Value::Undefined => "undefined",
            Value::Null => "null",
            Value::Bool(true) => "true",
            Value::Bool(false) => "false",
            Value::Object(_) => "[object Object]",
            Value::Number(x) => return self.intern(ryu_js::Buffer::new().format(x)).into(),
        };
        self.intern(s).into()
    }

    pub fn binary(&mut self, op: BinaryOp, lhs: Value, rhs: Value) -> Value {
        match (op, lhs, rhs) {
            (BinaryOp::Add, Value::String(_), _) | (BinaryOp::Add, _, Value::String(_)) => {
                let (Value::String(a), Value::String(b)) = (self.string(lhs), self.string(rhs)) else { unreachable!("ToString yields a string") };
                let s = [self.str(a), self.str(b)].concat();
                self.intern(&s).into()
            }
            _ => {
                let (a, b) = (self.number(lhs), self.number(rhs));
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

impl TryFrom<oxc_syntax::operator::BinaryOperator> for BinaryOp {
    type Error = oxc_syntax::operator::BinaryOperator;

    fn try_from(op: oxc_syntax::operator::BinaryOperator) -> Result<Self, Self::Error> {
        use oxc_syntax::operator::BinaryOperator;
        Ok(match op {
            BinaryOperator::Addition => BinaryOp::Add,
            BinaryOperator::Subtraction => BinaryOp::Sub,
            BinaryOperator::Multiplication => BinaryOp::Mul,
            BinaryOperator::Division => BinaryOp::Div,
            BinaryOperator::Remainder => BinaryOp::Rem,
            op => return Err(op),
        })
    }
}
