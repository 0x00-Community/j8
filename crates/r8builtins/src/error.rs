use r8frame::{Agent, Error, ErrorKind, Value};

use crate::{Args, Installer};

/// 各 Error 构造器与 `Error.prototype.toString`。
struct Errors;

impl Errors {
    /// `XxxError(message)`：第 `K` 种（按 [`ErrorKind::ALL`]）。
    fn construct<'gc, const K: usize>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let kind = ErrorKind::ALL[K];
        let e = match args.at(0) {
            Value::Undefined => a.error(kind, ""),
            m => {
                let m = a.to_string(m)?;
                a.error(kind, &m)
            }
        };
        if args.at(0) == Value::Undefined {
            e.borrow_mut(a.mc).remove("message");
        }
        Ok(e.into())
    }

    /// `name: message`，有一边空就只给另一边。
    fn to_string<'gc>(a: &mut Agent<'gc, '_>, this: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if !matches!(this, Value::Object(_)) {
            return Err(a.throw(ErrorKind::Type, "Error.prototype.toString called on non-object"));
        }
        let name = match a.get_named(this, "name")? {
            Value::Undefined => a.string("Error"),
            v => a.to_string(v)?,
        };
        let msg = match a.get_named(this, "message")? {
            Value::Undefined => a.string(""),
            v => a.to_string(v)?,
        };
        Ok(match (name.is_empty(), msg.is_empty()) {
            (_, true) => name.into(),
            (true, false) => msg.into(),
            (false, false) => a.string(&alloc::format!("{}: {}", &**name, &**msg)).into(),
        })
    }
}

impl Installer<'_, '_> {
    pub(crate) fn errors(&mut self) {
        let ctors: [r8frame::NativeFn; ErrorKind::ALL.len()] = [Errors::construct::<0>, Errors::construct::<1>, Errors::construct::<2>, Errors::construct::<3>, Errors::construct::<4>];
        for (kind, f) in ErrorKind::ALL.into_iter().zip(ctors) {
            let proto = self.realm.error_proto(kind);
            self.constructor(kind.name(), f, proto);
            self.set(proto, "name", self.text(kind.name()));
            self.set(proto, "message", self.text(""));
        }
        self.method(self.realm.error_proto(ErrorKind::Error), "toString", Errors::to_string);
    }
}
