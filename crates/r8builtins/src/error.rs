use r8frame::{Agent, Error, ErrorKind, Value};

use crate::{Args, Installer};

/// 各 Error 构造器与 `Error.prototype.toString`。
struct Errors;

impl Errors {
    /// `XxxError(message)`：第 `K` 种（按 [`ErrorKind::ALL`]）。
    fn construct<'gc, const K: usize>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let kind = ErrorKind::ALL[K];
        // AggregateError(errors, message, options)：实参往后错一位
        let all = args;
        let args = if kind == ErrorKind::Aggregate { all.get(1..).unwrap_or(&[]) } else { all };
        let proto = a.proto_from_ctor(a.new_target, a.realm.error_proto(kind))?;
        let e = a.error(kind, "");
        e.borrow_mut(a.mc).proto = Some(proto);
        e.borrow_mut(a.mc).remove("message");
        if args.at(0) != Value::Undefined {
            let m = a.to_string(args.at(0))?;
            e.borrow_mut(a.mc).define(a.mc, "message", r8frame::Property::hidden(m.into()));
        }
        // InstallErrorCause
        let options = args.at(1);
        if let Value::Object(opts) = options
            && a.has_key(opts, r8frame::Key::Str("cause"))?
        {
            let cause = a.get_named(options, "cause")?;
            e.borrow_mut(a.mc).define(a.mc, "cause", r8frame::Property::hidden(cause));
        }
        if kind == ErrorKind::Aggregate {
            let list = a.to_list(all.at(0))?;
            let arr = a.array(list);
            e.borrow_mut(a.mc).define(a.mc, "errors", r8frame::Property::hidden(arr.into()));
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
        let ctors: [r8frame::NativeFn; ErrorKind::ALL.len()] = [
            Errors::construct::<0>,
            Errors::construct::<1>,
            Errors::construct::<2>,
            Errors::construct::<3>,
            Errors::construct::<4>,
            Errors::construct::<5>,
            Errors::construct::<6>,
            Errors::construct::<7>,
        ];
        let mut base = None;
        for (kind, f) in ErrorKind::ALL.into_iter().zip(ctors) {
            let proto = self.realm.error_proto(kind);
            let ctor = self.constructor(kind.name(), f, proto, if kind == ErrorKind::Aggregate { 2 } else { 1 });
            // NativeError 构造器的原型是 Error 构造器
            match base {
                None => base = Some(ctor),
                Some(b) => ctor.borrow_mut(self.mc).proto = Some(b),
            }
            self.set(proto, "name", self.text(kind.name()));
            self.set(proto, "message", self.text(""));
        }
        self.method(self.realm.error_proto(ErrorKind::Error), "toString", Errors::to_string, 0);
    }
}
