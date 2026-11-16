use j8framework::ValueKind;
use alloc::string::String;
use alloc::vec::Vec;

use j8framework::{Agent, CodeKind, Error, Native, ObjectKind, Property, Slot, Value};

use crate::{Args, Installer};

/// The `Function` family of constructors and global `eval` (dynamic code goes through [`Agent::eval_code`]).
struct Functions;

impl Functions {
    /// CreateDynamicFunction: join parameters and body into `(prefix anonymous(params\n) {\nbody\n})`, then compile and
    /// evaluate it.
    fn dynamic<'gc>(a: &mut Agent<'gc, '_>, args: &[Value<'gc>], prefix: &str) -> Result<Value<'gc>, Error> {
        let (params, body) = match args.split_last() {
            Some((&body, params)) => (params, body),
            None => (&[][..], Value::Undefined),
        };
        let params = params.iter().map(|&p| a.to_string(p).map(|s| String::from(&**s))).collect::<Result<Vec<_>, _>>()?;
        let body = match body.kind() {
            ValueKind::Undefined => String::new(),
            b => String::from(&**a.to_string(Value::from(b))?),
        };
        let src = alloc::format!("({prefix} anonymous({}\n) {{\n{body}\n}})", params.join(","));
        let new_target = a.new_target;
        let f = a.eval_code(&src, CodeKind::Eval)?;
        // When constructing a subclass (`class F extends Function`), the prototype comes from new.target.prototype
        if let Some(fo) = f.as_object()
            && new_target != Value::Undefined
        {
            let fallback = fo.borrow().proto.unwrap_or(a.realm.function_proto);
            let proto = a.proto_from_ctor(new_target, fallback)?;
            fo.borrow_mut(a.mc).proto = Some(proto);
        }
        Ok(f)
    }

    fn function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, "function")
    }

    fn generator_function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, "function*")
    }

    fn async_function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, "async function")
    }

    fn async_generator_function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, "async function*")
    }

    /// Global `eval` (indirect): non-strings are returned as is; strings run as global code.
    fn eval<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        match args.at(0).kind() {
            ValueKind::String(s) => {
                let src = String::from(&**s);
                a.eval_code(&src, CodeKind::Eval)
            }
            v => Ok(Value::from(v)),
        }
    }
}

impl<'gc> Installer<'_, 'gc> {
    pub(crate) fn functions(&mut self) {
        let r = self.realm;
        let ctor = self.constructor("Function", Functions::function, r.function_proto, 1);
        // Generator / async function constructors aren't globals: they're reached via the `constructor` of their
        // prototypes
        let kinds: [(&str, j8framework::NativeFn, j8framework::JsObject<'gc>, Option<j8framework::JsObject<'gc>>); 3] = [
            ("GeneratorFunction", Functions::generator_function, r.generator_function_proto, Some(r.generator_proto)),
            ("AsyncFunction", Functions::async_function, r.async_function_proto, None),
            ("AsyncGeneratorFunction", Functions::async_generator_function, r.async_generator_function_proto, Some(r.async_generator_proto)),
        ];
        for (name, f, proto, instances) in kinds {
            let c = r.named_native(self.mc, f, name, 1);
            if let ObjectKind::Native(n) = &mut c.borrow_mut(self.mc).kind {
                n.constructor = true;
            }
            let fixed = |v: Value<'gc>| Property { slot: Slot::Data(v), enumerable: false, writable: false, configurable: false };
            c.borrow_mut(self.mc).define("prototype", fixed(proto.into()));
            c.borrow_mut(self.mc).proto = Some(ctor);
            proto.borrow_mut(self.mc).define("constructor", Property { writable: false, ..Property::hidden(c.into()) });
            if let Some(i) = instances {
                proto.borrow_mut(self.mc).define("prototype", Property { writable: false, ..Property::hidden(i.into()) });
                i.borrow_mut(self.mc).define("constructor", Property { writable: false, ..Property::hidden(proto.into()) });
            }
        }
        // %eval% is the reserved object in the Realm (the executor uses it to recognize direct eval)
        r.eval.borrow_mut(self.mc).kind = ObjectKind::Native(Native::function(Functions::eval));
        r.name_function(self.mc, r.eval, "eval", Some(1));
        r.global.borrow_mut(self.mc).define("eval", Property::hidden(r.eval.into()));
    }
}
