use num_bigint::BigInt;
use r8frame::{Agent, Error, ErrorKind, Hint, Value};

use crate::{Args, Installer};

/// `BigInt` 与原型方法。
struct BigInts;

impl BigInts {
    /// `BigInt(v)`：整数值的数、布尔、字符串（按 StringToBigInt）、BigInt。
    fn construct<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let prim = a.to_primitive(args.at(0), Hint::Number)?;
        let b = match prim {
            Value::Number(x) => Agent::number_to_bigint(x).ok_or_else(|| a.throw(ErrorKind::Range, &alloc::format!("The number {} cannot be converted to a BigInt because it is not an integer", ryu_js::Buffer::new().format(x))))?,
            Value::Bool(t) => BigInt::from(u8::from(t)),
            Value::BigInt(b) => return Ok(Value::BigInt(b)),
            Value::String(s) => Agent::parse_bigint(&s).ok_or_else(|| a.throw(ErrorKind::Syntax, &alloc::format!("Cannot convert {} to a BigInt", &**s)))?,
            other => {
                let what = match other {
                    Value::Symbol(s) => a.symbol_descriptive(s),
                    v => a.to_string(v)?,
                };
                return Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot convert {} to a BigInt", &**what)));
            }
        };
        Ok(a.bigint(b))
    }

    fn this<'gc>(a: &Agent<'gc, '_>, this: Value<'gc>) -> Result<r8frame::JsBigInt<'gc>, Error> {
        match this {
            Value::BigInt(b) => Ok(b),
            _ => Err(a.throw(ErrorKind::Type, "BigInt.prototype method called on a non-BigInt")),
        }
    }

    /// `n.toString(radix)`。
    fn to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let b = Self::this(a, this)?;
        let radix = match args.at(0) {
            Value::Undefined => 10,
            r => a.to_int32(r)?,
        };
        let radix = u32::try_from(radix).ok().filter(|r| (2..=36).contains(r)).ok_or_else(|| a.throw(ErrorKind::Range, "toString() radix must be between 2 and 36"))?;
        Ok(a.string(&b.0.to_str_radix(radix)).into())
    }

    fn value_of<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Ok(Value::BigInt(Self::this(a, this)?))
    }
}

impl Installer<'_, '_> {
    pub(crate) fn bigint(&mut self) {
        let proto = self.realm.bigint_proto;
        self.constructor("BigInt", BigInts::construct, proto);
        self.method(proto, "toString", BigInts::to_string);
        self.method(proto, "valueOf", BigInts::value_of);
    }
}
