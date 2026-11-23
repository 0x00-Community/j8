use j8framework::ValueKind;
use num_bigint::BigInt;
use j8framework::{Agent, Error, ErrorKind, Hint, Value};

use crate::{Args, Installer};

struct BigInts;

impl BigInts {
    fn construct<'scope>(a: &mut Agent<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let prim = a.to_primitive(args.at(0), Hint::Number)?;
        let b = match prim.kind() {
            ValueKind::Number(x) => Agent::number_to_bigint(x).ok_or_else(|| a.throw(ErrorKind::Range, &alloc::format!("The number {} cannot be converted to a BigInt because it is not an integer", ryu_js::Buffer::new().format(x))))?,
            ValueKind::Bool(t) => BigInt::from(u8::from(t)),
            ValueKind::BigInt(b) => return Ok(Value::BigInt(b)),
            ValueKind::String(s) => Agent::parse_bigint(&s).ok_or_else(|| a.throw(ErrorKind::Syntax, &alloc::format!("Cannot convert {} to a BigInt", &**s)))?,
            other => {
                let what = match other {
                    ValueKind::Symbol(s) => a.symbol_descriptive(s),
                    v => a.to_string(Value::from(v))?,
                };
                return Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot convert {} to a BigInt", &**what)));
            }
        };
        Ok(a.bigint(b))
    }

    fn this<'scope>(a: &Agent<'scope, '_>, this: Value<'scope>) -> Result<j8framework::JsBigInt<'scope>, Error> {
        match this.unwrapped().kind() {
            ValueKind::BigInt(b) => Ok(b),
            _ => Err(a.throw(ErrorKind::Type, "BigInt.prototype method called on a non-BigInt")),
        }
    }

    fn to_string<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let b = Self::this(a, this)?;
        let radix = match args.at(0).kind() {
            ValueKind::Undefined => 10,
            r => a.to_int32(Value::from(r))?,
        };
        let radix = u32::try_from(radix).ok().filter(|r| (2..=36).contains(r)).ok_or_else(|| a.throw(ErrorKind::Range, "toString() radix must be between 2 and 36"))?;
        Ok(a.string(&b.0.to_str_radix(radix)).into())
    }

    fn value_of<'scope>(a: &mut Agent<'scope, '_>, this: Value<'scope>, _: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        Ok(Value::BigInt(Self::this(a, this)?))
    }
}

impl Installer<'_, '_> {
    pub(crate) fn bigint(&mut self) {
        let proto = self.context.bigint_proto;
        let ctor = self.constructor("BigInt", BigInts::construct, proto, 1);
        self.not_constructor(ctor);
        self.method(proto, "toString", BigInts::to_string, 0);
        self.method(proto, "valueOf", BigInts::value_of, 0);
    }
}
